//! The inline phase. At each position of an inline section: backslash
//! escapes and code spans first, then the host's hook, then the built-ins.
//! Brackets are parsed by recursive descent, and emphasis is resolved with
//! the CommonMark delimiter algorithm within each section.

use std::collections::{HashMap, HashSet};

use indextree::NodeId;

use super::block::{LinkRef, close_tag, label_key, link_dest, link_title, open_tag, skip_space};
use super::stream::InlineStream;
use super::{Host, MAX_NESTING};
use crate::names::image_type_of;
use crate::tree::{BlockKind, Children, InlineKind, Node, Tree};

/// Parses every inline section the block phase left into the node it was
/// given to. Sections no node was given (a host's hook built them, then
/// returned `None`) are skipped.
pub(super) fn inlines<P: Host>(host: &P, tree: &mut Tree<P>, refs: &[LinkRef]) {
    let mut defs = HashMap::new();
    for r in refs {
        // The first definition of a label wins.
        defs.entry(r.label.clone()).or_insert_with(|| {
            let title = r.title.as_deref().map(unescape);
            (unescape(&r.dest), title)
        });
    }
    let sections = std::mem::take(&mut tree.sections);
    for (text, node) in &sections {
        let Some(node) = *node else {
            continue;
        };
        let item = tree
            .item_starts
            .contains(&node)
            .then(|| tree.arena[node].parent())
            .flatten();
        let mut s = InlineStream {
            host,
            tree: &mut *tree,
            refs: &defs,
            src: text,
            pos: 0,
            in_link: false,
            links: 0,
            not_links: HashSet::new(),
            not_hosts: HashSet::new(),
            found: HashMap::new(),
            depth: 0,
            hashes: HashMap::new(),
            item,
        };
        for id in s.section(None).unwrap_or_default() {
            node.append(id, &mut tree.arena);
        }
    }
}

/// Where a nested section ends: the length of the end at the start of the
/// input, if it is there.
pub(super) type End<'e, 'a> = &'e mut dyn FnMut(&'a str) -> Option<usize>;

/// A section being parsed: its nodes so far, and text not yet in one.
#[derive(Default)]
struct Run {
    ids: Vec<NodeId>,
    text: String,
    delims: Vec<Delim>,
    /// `[`s drawn as text whose `]` is still to come; that `]` is text too.
    brackets: usize,
    /// Where in `text` a `#` came from an escape or a character reference,
    /// so can't start a tag.
    hashes: Vec<usize>,
}

/// A run of `*`, `_` or `~`, held as a text node until emphasis is
/// resolved.
struct Delim {
    id: NodeId,
    /// Where the node is in the section's nodes.
    node: usize,
    ch: char,
    count: usize,
    /// The run's length as written.
    len: usize,
    open: bool,
    close: bool,
}

/// A list of the indices `0..n`, linked both ways, so that indices can be
/// taken out of the middle in constant time.
struct Links {
    prev: Vec<Option<usize>>,
    next: Vec<Option<usize>>,
    head: Option<usize>,
}

impl Links {
    fn new(n: usize) -> Self {
        Links {
            prev: (0..n).map(|i| i.checked_sub(1)).collect(),
            next: (1..=n).map(|i| (i < n).then_some(i)).collect(),
            head: (n > 0).then_some(0),
        }
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.head, |&i| self.next[i])
    }

    /// The indices after `a`, up to `b`.
    fn between(&self, a: usize, b: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.next[a], |&i| self.next[i]).take_while(move |&i| i != b)
    }

    /// Takes out the indices between `a` and `b`, which comes after it.
    fn join(&mut self, a: usize, b: usize) {
        self.next[a] = Some(b);
        self.prev[b] = Some(a);
    }

    /// Adds the next index after `a`.
    fn insert_after(&mut self, a: usize) {
        let i = self.next.len();
        self.prev.push(Some(a));
        self.next.push(self.next[a]);
        if let Some(n) = self.next[a] {
            self.prev[n] = Some(i);
        }
        self.next[a] = Some(i);
    }

    /// Takes out `i`. Its own links are kept, so the list can still be
    /// followed on from it.
    fn unlink(&mut self, i: usize) {
        match self.prev[i] {
            Some(p) => self.next[p] = self.next[i],
            None => self.head = self.next[i],
        }
        if let Some(n) = self.next[i] {
            self.prev[n] = self.prev[i];
        }
    }
}

impl<'a, P: Host> InlineStream<'a, P> {
    /// Inlines until `end` matches (consumed) or, with no `end`, to the end
    /// of the input. `None` when `end` never matches.
    pub(super) fn section(&mut self, end: Option<End<'_, 'a>>) -> Option<Vec<NodeId>> {
        if self.depth >= MAX_NESTING {
            return None;
        }
        self.depth += 1;
        let ids = self.run(end);
        self.depth -= 1;
        ids
    }

    fn run(&mut self, mut end: Option<End<'_, 'a>>) -> Option<Vec<NodeId>> {
        let mut run = Run::default();
        loop {
            let rest = &self.src[self.pos..];
            let Some(c) = rest.chars().next() else {
                if end.is_some() {
                    return None;
                }
                break;
            };
            if self.escape(&mut run) || self.code_span(&mut run) {
                continue;
            }
            if c == ']' && run.brackets > 0 {
                run.brackets -= 1;
                run.text.push(']');
                self.pos += 1;
                continue;
            }
            if let Some(end) = end.as_mut()
                && let Some(len) = end(rest)
            {
                self.pos += len;
                break;
            }
            if self.hook(&mut run) || self.builtin(&mut run) {
                continue;
            }
            run.text.push(c);
            self.pos += c.len_utf8();
        }
        self.flush(&mut run);
        self.emphasis(&mut run);
        let ids = self.merge(run.ids);
        Some(if self.in_link { ids } else { self.tags(ids) })
    }

    /// Text so far becomes a node.
    fn flush(&mut self, run: &mut Run) {
        if !run.text.is_empty() {
            let text = std::mem::take(&mut run.text);
            let id = self.tree.leaf(InlineKind::Text(text)).id;
            if !run.hashes.is_empty() {
                self.hashes.insert(id, std::mem::take(&mut run.hashes));
            }
            run.ids.push(id);
        }
    }

    /// `text` as text that can't start a tag.
    fn push_literal(&mut self, run: &mut Run, text: &str) {
        let at = run.text.len();
        run.hashes
            .extend(text.match_indices('#').map(|(i, _)| at + i));
        run.text.push_str(text);
    }

    fn push(&mut self, run: &mut Run, id: NodeId) {
        self.flush(run);
        run.ids.push(id);
    }

    fn push_leaf(&mut self, run: &mut Run, kind: InlineKind<P>) {
        let id = self.tree.leaf(kind).id;
        self.push(run, id);
    }

    /// A link whose text is `text` as written.
    fn push_link(&mut self, run: &mut Run, dest: String, text: &str) {
        let text = self.tree.leaf(InlineKind::Text(text.to_string()));
        let link = InlineKind::Link { dest, title: None };
        let id = self.tree.inline(link, Children::new(vec![text.id])).id;
        self.push(run, id);
    }

    /// Offers the position to the host, unless it returned `None` there
    /// before. On `None` the stream is rewound. A claim that consumes
    /// nothing leaves the position to the built-ins.
    fn hook(&mut self, run: &mut Run) -> bool {
        let (pos, links) = (self.pos, self.links);
        let at = (pos, self.depth, self.in_link);
        if self.not_hosts.contains(&at) {
            return false;
        }
        let host = self.host;
        match host.parse_inline(self) {
            Some(parsed) => {
                self.push(run, parsed.id);
                self.pos > pos
            }
            None => {
                self.pos = pos;
                self.links = links;
                self.not_hosts.insert(at);
                false
            }
        }
    }

    fn builtin(&mut self, run: &mut Run) -> bool {
        self.checkbox()
            || self.autolink(run)
            || self.raw_html(run)
            || self.embed(run)
            || self.wikilink(run)
            || self.bracket(run)
            || self.entity(run)
            || self.extended_autolink(run)
            || self.delimiter(run)
            || self.line_break(run, false)
    }

    /// At a list item's start, `[ ]`, `[x]` or `[X]`, then whitespace and
    /// more on the line: the item is a task. The checkbox and the
    /// whitespace are consumed.
    fn checkbox(&mut self) -> bool {
        let Some(item) = self.item.filter(|_| self.at_item_start()) else {
            return false;
        };
        let rest = self.rest();
        let checked = match rest.get(..3) {
            Some("[ ]") => false,
            Some("[x]" | "[X]") => true,
            _ => return false,
        };
        let line = rest[3..].split('\n').next().unwrap_or_default();
        if !line.starts_with([' ', '\t']) || line.trim().is_empty() {
            return false;
        }
        if let Node::Block(BlockKind::ListItem { task }) = self.tree.arena[item].get_mut() {
            *task = Some(checked);
        }
        self.pos = self.src.len() - rest[3..].trim_start_matches([' ', '\t']).len();
        true
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    /// `\` before ASCII punctuation: the character as text. Before a line
    /// ending: a hard break.
    fn escape(&mut self, run: &mut Run) -> bool {
        let Some(after) = self.rest().strip_prefix('\\') else {
            return false;
        };
        match after.chars().next() {
            Some('\n') => {
                self.pos += 1;
                self.line_break(run, true)
            }
            Some(c) if c.is_ascii_punctuation() => {
                self.push_literal(run, &after[..1]);
                self.pos += 2;
                true
            }
            _ => false,
        }
    }

    /// A code span; a backtick run with no closing run of the same length is
    /// text.
    fn code_span(&mut self, run: &mut Run) -> bool {
        let rest = self.rest();
        let n = rest.bytes().take_while(|&b| b == b'`').count();
        if n == 0 {
            return false;
        }
        let bytes = rest.as_bytes();
        let mut i = n;
        while i < bytes.len() {
            let m = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if m == n {
                let code = rest[n..i].replace('\n', " ");
                let code = match code.strip_prefix(' ').and_then(|c| c.strip_suffix(' ')) {
                    Some(inner) if code.bytes().any(|b| b != b' ') => inner.to_string(),
                    _ => code,
                };
                self.pos += i + m;
                self.push_leaf(run, InlineKind::Code(code));
                return true;
            }
            i += m.max(1);
        }
        run.text.push_str(&rest[..n]);
        self.pos += n;
        true
    }

    /// `<scheme:…>` or `<address@domain>`.
    fn autolink(&mut self, run: &mut Run) -> bool {
        let Some(inner) = self.rest().strip_prefix('<') else {
            return false;
        };
        let Some(len) = inner
            .find(['<', '>'])
            .filter(|&i| inner.as_bytes()[i] == b'>')
        else {
            return false;
        };
        let text = &inner[..len];
        let dest = if uri(text) {
            text.to_string()
        } else if email(text) {
            format!("mailto:{text}")
        } else {
            return false;
        };
        self.pos += len + 2;
        self.push_link(run, dest, text);
        true
    }

    /// An open or closing tag, comment, processing instruction, declaration
    /// or CDATA section.
    fn raw_html(&mut self, run: &mut Run) -> bool {
        let rest = self.rest();
        if !rest.starts_with('<') {
            return false;
        }
        let len = open_tag(rest)
            .or_else(|| close_tag(rest))
            .or_else(|| {
                ["<!-->", "<!--->"]
                    .into_iter()
                    .find(|c| rest.starts_with(c))
                    .map(str::len)
            })
            .or_else(|| self.ends("<!--", "-->"))
            .or_else(|| self.ends("<?", "?>"))
            .or_else(|| self.ends("<![CDATA[", "]]>"))
            .or_else(|| {
                let name = rest.strip_prefix("<!")?;
                name.starts_with(|c: char| c.is_ascii_alphabetic())
                    .then(|| self.ends("<!", ">"))
                    .flatten()
            });
        let Some(len) = len else {
            return false;
        };
        self.pos += len;
        self.push_leaf(run, InlineKind::Html(rest[..len].to_string()));
        true
    }

    /// The length of `start`, then text up to and including `end`, at the
    /// position.
    fn ends(&mut self, start: &str, end: &'static str) -> Option<usize> {
        self.rest().strip_prefix(start)?;
        let from = self.pos + start.len();
        let at = self.find(from, end)?;
        Some(at + end.len() - self.pos)
    }

    /// Where `end` first is in the input at or after `from`. The answer is
    /// kept, so that looking again from a later position up to it doesn't
    /// scan the input again.
    fn find(&mut self, from: usize, end: &'static str) -> Option<usize> {
        if let Some(&(after, at)) = self.found.get(end)
            && after <= from
            && at.is_none_or(|at| from <= at)
        {
            return at;
        }
        let at = self.src[from..].find(end).map(|i| from + i);
        self.found.insert(end, (from, at));
        at
    }

    /// `![[target]]` or `![[target|alt]]`, where the target, without any
    /// `#…`, names an image. Not in link text.
    fn embed(&mut self, run: &mut Run) -> bool {
        let Some(rest) = self.rest().strip_prefix("![[") else {
            return false;
        };
        if self.in_link {
            return false;
        }
        let Some(end) = rest.find("]]") else {
            return false;
        };
        let inner = &rest[..end];
        if inner.contains("[[") {
            return false;
        }
        let (target, alt) = match inner.split_once('|') {
            Some((target, alt)) => (target.trim(), Some(alt.trim()).filter(|a| !a.is_empty())),
            None => (inner.trim(), None),
        };
        if image_type_of(target.split('#').next().unwrap_or("")).is_none() {
            return false;
        }
        self.pos += "![[".len() + end + "]]".len();
        let embed = InlineKind::Embed {
            target: target.to_string(),
            alt: alt.map(str::to_string),
        };
        self.push_leaf(run, embed);
        true
    }

    /// `[[target]]` or `[[target|label]]`; neither part holds `[`, `]` or
    /// `|`.
    fn wikilink(&mut self, run: &mut Run) -> bool {
        let rest = self.rest();
        let Some(inner) = rest.strip_prefix("[[") else {
            return false;
        };
        let part = |s: &str| {
            let b = s.as_bytes();
            let mut i = 0;
            while let Some(&c) = b.get(i) {
                match c {
                    b'[' | b']' | b'|' => break,
                    b'\\' if b.get(i + 1).is_some_and(u8::is_ascii_punctuation) => i += 2,
                    _ => i += 1,
                }
            }
            (i <= 1000).then_some(i)
        };
        let Some(t) = part(inner) else {
            return false;
        };
        let (label, len) = match &inner[t..] {
            after if after.starts_with("]]") => (None, t + 2),
            after if after.starts_with('|') => {
                let Some(l) = part(&after[1..]).filter(|&l| after[1 + l..].starts_with("]]"))
                else {
                    return false;
                };
                (Some(unescape(after[1..1 + l].trim())), t + 1 + l + 2)
            }
            _ => return false,
        };
        self.pos += 2 + len;
        let link = InlineKind::WikiLink {
            target: unescape(inner[..t].trim()),
            label,
        };
        self.push_leaf(run, link);
        true
    }

    /// `[text](…)`, `[text][label]`, `[label][]` or `[label]`, and the same
    /// after `!` for an image. Otherwise the bracket is text, and so is its
    /// `]`.
    fn bracket(&mut self, run: &mut Run) -> bool {
        let rest = self.rest();
        let image = rest.starts_with("![");
        if !image && !rest.starts_with('[') {
            return false;
        }
        let open = if image { 2 } else { 1 };
        let (start, links) = (self.pos, self.links);
        let at = (start, self.depth, self.in_link);
        // With no `]` ahead, the text can't end.
        if !self.not_links.contains(&at) && self.find(start + open, "]").is_some() {
            self.pos += open;
            let in_link = std::mem::replace(&mut self.in_link, true);
            let text = self.section(Some(&mut |s: &str| s.starts_with(']').then_some(1)));
            self.in_link = in_link;
            // A link's text can't hold another link; an image's can.
            if let Some(text) = text.filter(|_| image || self.links == links) {
                let label = &self.src[start + open..self.pos - 1];
                if let Some((dest, title)) = self.destination(label) {
                    let kind = if image {
                        InlineKind::Image { dest, title }
                    } else {
                        self.links += 1;
                        InlineKind::Link { dest, title }
                    };
                    let id = self.tree.inline(kind, Children::new(text)).id;
                    self.push(run, id);
                    return true;
                }
            }
            self.not_links.insert(at);
        }
        self.pos = start + open;
        self.links = links;
        run.text.push_str(&rest[..open]);
        run.brackets += 1;
        true
    }

    /// What follows a link's text: an inline destination and title, or a
    /// reference to a definition. `label` is the text as written.
    fn destination(&mut self, label: &str) -> Option<(String, Option<String>)> {
        let rest = self.rest();
        if let Some(inline) = rest.strip_prefix('(')
            && let Some((dest, title, len)) = inline_destination(inline)
        {
            self.pos += 1 + len;
            return Some((unescape(dest), title.map(unescape)));
        }
        let (key, len) = match reference_label(rest) {
            // `[]`, or no label: the text is the label.
            Some(("", len)) => (label, len),
            Some((key, len)) => (key, len),
            None => (label, 0),
        };
        if key.len() > 999 {
            return None;
        }
        let (dest, title) = self.refs.get(&label_key(key))?.clone();
        self.pos += len;
        Some((dest, title))
    }

    /// `&name;`, `&#digits;` or `&#xhex;`: the character as text.
    fn entity(&mut self, run: &mut Run) -> bool {
        let Some((decoded, len)) = entity(self.rest()) else {
            return false;
        };
        self.push_literal(run, &decoded);
        self.pos += len;
        true
    }

    /// GFM's extended autolinks: `www.…`, `http://…`, `https://…`,
    /// `ftp://…` and email addresses, outside link text.
    fn extended_autolink(&mut self, run: &mut Run) -> bool {
        if self.in_link {
            return false;
        }
        let prev = self.src[..self.pos].chars().next_back();
        let rest = self.rest();
        let Some((len, dest)) = www(prev, rest)
            .or_else(|| url(prev, rest))
            .or_else(|| email_address(prev, rest))
        else {
            return false;
        };
        self.pos += len;
        self.push_link(run, dest, &rest[..len]);
        true
    }

    /// A run of `*`, `_` (emphasis) or `~` (strikethrough), held for
    /// [`Self::emphasis`]. A run of more than two `~` is text.
    fn delimiter(&mut self, run: &mut Run) -> bool {
        let rest = self.rest();
        let Some(ch @ ('*' | '_' | '~')) = rest.chars().next() else {
            return false;
        };
        let len = rest.bytes().take_while(|&b| b == ch as u8).count();
        self.pos += len;
        if ch == '~' && len > 2 {
            run.text.push_str(&rest[..len]);
            return true;
        }
        let before = self.src[..self.pos - len]
            .chars()
            .next_back()
            .unwrap_or('\n');
        let after = rest[len..].chars().next().unwrap_or('\n');
        let left = !after.is_whitespace()
            && (!punctuation(after) || before.is_whitespace() || punctuation(before));
        let right = !before.is_whitespace()
            && (!punctuation(before) || after.is_whitespace() || punctuation(after));
        let (open, close) = match ch {
            '_' => (
                left && (!right || punctuation(before)),
                right && (!left || punctuation(after)),
            ),
            _ => (left, right),
        };
        let id = self.tree.leaf(InlineKind::Text(rest[..len].to_string())).id;
        self.push(run, id);
        run.delims.push(Delim {
            id,
            node: run.ids.len() - 1,
            ch,
            count: len,
            len,
            open,
            close,
        });
        true
    }

    /// A line ending: a hard break after a backslash (`escaped`) or two or
    /// more spaces, otherwise a soft break. Spaces around it are dropped.
    fn line_break(&mut self, run: &mut Run, escaped: bool) -> bool {
        if !self.rest().starts_with('\n') {
            return false;
        }
        let hard = escaped || run.text.ends_with("  ");
        run.text.truncate(run.text.trim_end_matches(' ').len());
        self.pos += 1;
        self.pos += self.rest().len() - self.rest().trim_start_matches([' ', '\t']).len();
        let kind = if hard {
            InlineKind::HardBreak
        } else {
            InlineKind::SoftBreak
        };
        self.push_leaf(run, kind);
        true
    }

    /// CommonMark's "process emphasis" over the section's delimiter runs:
    /// each closer is matched with the nearest opener of its kind, and the
    /// nodes between them become an `Emph`, `Strong` or `Strike` node. A
    /// match that would nest emphasis past the nesting limit is not made;
    /// its delimiters stay text. The nodes and the delimiter stack are
    /// linked lists, so a match costs what it wraps.
    fn emphasis(&mut self, run: &mut Run) {
        let delims = &mut run.delims;
        let mut ids = std::mem::take(&mut run.ids);
        let mut nodes = Links::new(ids.len());
        let mut stack = Links::new(delims.len());
        // Openers below this index can't match a closer of this kind.
        let mut bottom: HashMap<(char, bool, usize), usize> = HashMap::new();
        // How deep each node made here nests emphasis.
        let mut nesting: HashMap<NodeId, usize> = HashMap::new();
        let mut next = stack.head;
        while let Some(c) = next {
            if !delims[c].close {
                next = stack.next[c];
                continue;
            }
            let closer = &delims[c];
            let key = (closer.ch, closer.open, closer.len % 3);
            let floor = bottom.get(&key).copied().unwrap_or(0);
            let fits = |d: &Delim| {
                d.open
                    && d.ch == closer.ch
                    && if d.ch == '~' {
                        d.count == closer.count
                    } else {
                        // The rule of 3.
                        !((d.close || closer.open)
                            && (d.len + closer.len).is_multiple_of(3)
                            && !(d.len.is_multiple_of(3) && closer.len.is_multiple_of(3)))
                    }
            };
            let mut opener = stack.prev[c];
            while let Some(o) = opener
                && o >= floor
                && !fits(&delims[o])
            {
                opener = stack.prev[o];
            }
            let Some(o) = opener.filter(|&o| o >= floor) else {
                bottom.insert(key, c);
                next = stack.next[c];
                if !delims[c].open {
                    stack.unlink(c);
                }
                continue;
            };
            let (n, kind) = match delims[c].ch {
                '~' => (delims[c].count, InlineKind::Strike),
                _ if delims[o].count >= 2 && delims[c].count >= 2 => (2, InlineKind::Strong),
                _ => (1, InlineKind::Emph),
            };
            let (on, cn) = (delims[o].node, delims[c].node);
            let inner: Vec<NodeId> = nodes.between(on, cn).map(|i| ids[i]).collect();
            let depth = 1 + inner
                .iter()
                .filter_map(|id| nesting.get(id))
                .max()
                .unwrap_or(&0);
            if depth > MAX_NESTING {
                next = stack.next[c];
                if !delims[c].open {
                    stack.unlink(c);
                }
                continue;
            }
            let inner = self.merge(inner);
            let node = self.tree.inline(kind, Children::new(inner)).id;
            nesting.insert(node, depth);
            nodes.join(on, cn);
            nodes.insert_after(on);
            ids.push(node);
            stack.join(o, c);
            for i in [o, c] {
                let d = &mut delims[i];
                d.count -= n;
                if d.count == 0 {
                    stack.unlink(i);
                    nodes.unlink(d.node);
                    d.id.remove(&mut self.tree.arena);
                } else if let Node::Inline(InlineKind::Text(t)) = self.tree.arena[d.id].get_mut() {
                    t.truncate(d.count);
                }
            }
            if delims[c].count == 0 {
                next = stack.next[c];
            }
        }
        run.ids = nodes.iter().map(|i| ids[i]).collect();
    }

    /// `ids` with each run of adjacent text nodes joined into one.
    fn merge(&mut self, ids: Vec<NodeId>) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(&last) = out.last()
                && let Some(len) = self.text(last).map(str::len)
                && self.text(id).is_some()
            {
                if let Node::Inline(InlineKind::Text(more)) = self.tree.arena[id].get_mut() {
                    let more = std::mem::take(more);
                    if let Node::Inline(InlineKind::Text(t)) = self.tree.arena[last].get_mut() {
                        t.push_str(&more);
                    }
                }
                if let Some(hashes) = self.hashes.remove(&id) {
                    let shifted = hashes.into_iter().map(|i| len + i);
                    self.hashes.entry(last).or_default().extend(shifted);
                }
                id.remove(&mut self.tree.arena);
                continue;
            }
            out.push(id);
        }
        out
    }

    fn text(&self, id: NodeId) -> Option<&str> {
        match self.tree.arena[id].get() {
            Node::Inline(InlineKind::Text(t)) => Some(t),
            _ => None,
        }
    }

    /// `ids` with `#tag`s split out of their text, as today's renderer
    /// finds them in each text node: a `#` at the start of a run of text or
    /// after whitespace, then one or more tag characters (alphanumerics,
    /// `_`, `-` and `/`), not all digits. A run of text continues across
    /// image embeds; after an embed or a tag, the next `#` doesn't start
    /// one. Emphasis made in this section is looked into; links and host
    /// elements are not.
    fn tags(&mut self, ids: Vec<NodeId>) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(ids.len());
        let mut after_space = true;
        for id in ids {
            match self.tree.arena[id].get() {
                Node::Inline(InlineKind::Text(_)) => {
                    after_space = self.split_tags(id, after_space, &mut out);
                }
                Node::Inline(InlineKind::Embed { .. }) => {
                    out.push(id);
                    after_space = false;
                }
                Node::Inline(InlineKind::Emph | InlineKind::Strong | InlineKind::Strike) => {
                    let children: Vec<NodeId> = id.children(&self.tree.arena).collect();
                    for &child in &children {
                        child.detach(&mut self.tree.arena);
                    }
                    for child in self.tags(children) {
                        id.append(child, &mut self.tree.arena);
                    }
                    out.push(id);
                    after_space = true;
                }
                _ => {
                    out.push(id);
                    after_space = true;
                }
            }
        }
        out
    }

    /// Text node `id` split into text and tags, onto `out`; whether its
    /// last character was whitespace.
    fn split_tags(&mut self, id: NodeId, mut after_space: bool, out: &mut Vec<NodeId>) -> bool {
        let hashes = self.hashes.remove(&id).unwrap_or_default();
        let text = self.text(id).unwrap_or_default().to_string();
        let mut pieces = Vec::new();
        let mut plain = 0;
        let mut i = 0;
        while let Some(c) = text[i..].chars().next() {
            if c == '#' && after_space && !hashes.contains(&i) {
                let body = &text[i + 1..];
                let len = body
                    .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                    .unwrap_or(body.len());
                let tag = &body[..len];
                if !tag.is_empty() && !tag.chars().all(char::is_numeric) {
                    pieces.push((plain, i, Some(tag)));
                    i += 1 + len;
                    plain = i;
                    after_space = false;
                    continue;
                }
            }
            after_space = c.is_whitespace();
            i += c.len_utf8();
        }
        if pieces.is_empty() {
            out.push(id);
            return after_space;
        }
        pieces.push((plain, text.len(), None));
        for (start, end, tag) in pieces {
            if start < end {
                let text = InlineKind::Text(text[start..end].to_string());
                out.push(self.tree.leaf(text).id);
            }
            if let Some(tag) = tag {
                out.push(self.tree.leaf(InlineKind::Tag(tag.to_string())).id);
            }
        }
        id.remove(&mut self.tree.arena);
        after_space
    }
}

/// Unicode punctuation and symbols, for delimiter runs.
fn punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
}

/// `s` with backslash escapes and entity and numeric character references
/// resolved.
pub(super) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        if c == '\\'
            && let Some(&next) = s.as_bytes().get(i + 1)
            && next.is_ascii_punctuation()
        {
            out.push(next as char);
            i += 2;
        } else if c == '&'
            && let Some((decoded, len)) = entity(&s[i..])
        {
            out.push_str(&decoded);
            i += len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// The entity or numeric character reference `s` starts with: its text and
/// length.
fn entity(s: &str) -> Option<(String, usize)> {
    let body = s.strip_prefix('&')?;
    if let Some(num) = body.strip_prefix('#') {
        let (radix, digits, max) = match num.strip_prefix(['x', 'X']) {
            Some(hex) => (16, hex, 6),
            None => (10, num, 7),
        };
        let len = digits
            .bytes()
            .take_while(|b| (*b as char).is_digit(radix))
            .count();
        if len == 0 || len > max || digits.as_bytes().get(len) != Some(&b';') {
            return None;
        }
        let n = u32::from_str_radix(&digits[..len], radix).ok()?;
        let c = char::from_u32(n)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}');
        return Some((c.to_string(), s.len() - digits.len() + len + 1));
    }
    let len = body.bytes().take_while(u8::is_ascii_alphanumeric).count();
    if len == 0 || body.as_bytes().get(len) != Some(&b';') {
        return None;
    }
    let name = &s[..len + 2];
    let found = entities::ENTITIES.iter().find(|e| e.entity == name)?;
    Some((found.characters.to_string(), name.len()))
}

/// `(dest "title")` after the `(`: the destination and title as written,
/// and the length up to and including the `)`.
fn inline_destination(s: &str) -> Option<(&str, Option<&str>, usize)> {
    let start = skip_space(s);
    let (dest, after) = if start.starts_with(')') {
        ("", start)
    } else {
        link_dest(start)?
    };
    let spaced = skip_space(after);
    let (title, after) = match (spaced.len() < after.len())
        .then(|| link_title(spaced))
        .flatten()
    {
        Some((title, rest)) => (Some(title), skip_space(rest)),
        None => (None, spaced),
    };
    let after = after.strip_prefix(')')?;
    Some((dest, title, s.len() - after.len()))
}

/// The `[label]` `s` starts with, as written, and its length. The label
/// holds no unescaped brackets.
fn reference_label(s: &str) -> Option<(&str, usize)> {
    let inner = s.strip_prefix('[')?;
    let b = inner.as_bytes();
    let mut i = 0;
    loop {
        match *b.get(i)? {
            b'\\' => i += 2,
            b'[' => return None,
            b']' => break,
            _ => i += 1,
        }
    }
    let label = inner.get(..i)?;
    (label.is_empty() || !label.trim().is_empty()).then_some((label, i + 2))
}

/// An absolute URI: a scheme of 2 to 32 characters, `:`, and no spaces or
/// controls.
fn uri(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    (2..=32).contains(&scheme.len())
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
        && !rest.bytes().any(|b| b <= b' ' || b == 0x7f)
}

/// An email address as CommonMark's autolinks take one.
fn email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b));
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    local_ok && domain.split('.').all(label_ok)
}

/// `www.` and a domain: the length and the destination.
fn www(prev: Option<char>, s: &str) -> Option<(usize, String)> {
    if prev.is_some_and(|c| !c.is_whitespace() && !matches!(c, '*' | '_' | '~' | '(' | '[')) {
        return None;
    }
    let domain = s.strip_prefix("www.")?;
    let end = autolink_end(s, 4 + check_domain(domain)?);
    Some((end, format!("http://{}", &s[..end])))
}

/// `http://`, `https://` or `ftp://` and a domain: the length and the
/// destination.
fn url(prev: Option<char>, s: &str) -> Option<(usize, String)> {
    if prev.is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let scheme = ["http://", "https://", "ftp://"]
        .into_iter()
        .find(|p| s.starts_with(p))?;
    let end = autolink_end(s, scheme.len() + check_domain(&s[scheme.len()..])?);
    Some((end, s[..end].to_string()))
}

/// An email address not preceded by one of its own characters: the length
/// and the destination.
fn email_address(prev: Option<char>, s: &str) -> Option<(usize, String)> {
    let local_char = |b: u8| b.is_ascii_alphanumeric() || b".+-_".contains(&b);
    if prev.is_some_and(|c| c.is_ascii() && local_char(c as u8)) {
        return None;
    }
    let local = s.bytes().take_while(|&b| local_char(b)).count();
    let domain = s[local..].strip_prefix('@')?;
    let b = domain.as_bytes();
    let mut end = 0;
    let mut periods = 0;
    while let Some(&c) = b.get(end) {
        match c {
            b'@' => return None,
            b'.' if b.get(end + 1).is_some_and(u8::is_ascii_alphanumeric) => periods += 1,
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' => {}
            _ => break,
        }
        end += 1;
    }
    if local == 0 || end == 0 || periods == 0 {
        return None;
    }
    let last = b[end - 1];
    if !last.is_ascii_alphabetic() && last != b'.' {
        return None;
    }
    let end = local + 1 + autolink_delim(domain, end);
    Some((end, format!("mailto:{}", &s[..end])))
}

/// The length of a valid domain at the start of `s`: segments of
/// alphanumerics, `_` and `-` separated by `.`, at least one `.`, and no
/// `_` in the last two segments.
fn check_domain(s: &str) -> Option<usize> {
    let (mut periods, mut under_last, mut under_this) = (0, 0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '\\' if i + 1 < s.len() => {}
            '_' => under_this += 1,
            '.' => {
                under_last = under_this;
                under_this = 0;
                periods += 1;
            }
            '-' => {}
            c if c.is_alphanumeric() => {}
            _ => {
                return (under_last == 0 && under_this == 0 && periods > 0).then_some(i);
            }
        }
    }
    ((under_last == 0 && under_this == 0) && periods > 0).then_some(s.len())
}

/// Where an extended autolink whose domain ends at `domain` ends: at the
/// next whitespace, less trailing punctuation.
fn autolink_end(s: &str, domain: usize) -> usize {
    let end = s[domain..]
        .find(char::is_whitespace)
        .map_or(s.len(), |i| domain + i);
    autolink_delim(s, end)
}

/// `end` moved back past trailing punctuation, unmatched `)`, and a
/// trailing entity-like `&name;`; and to the first `<`.
fn autolink_delim(s: &str, mut end: usize) -> usize {
    let b = s.as_bytes();
    if let Some(lt) = b[..end].iter().position(|&c| c == b'<') {
        end = lt;
    }
    let open = b[..end].iter().filter(|&&c| c == b'(').count();
    let mut close = b[..end].iter().filter(|&&c| c == b')').count();
    while end > 0 {
        match b[end - 1] {
            b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"' => end -= 1,
            b';' => {
                let name = b[..end - 1]
                    .iter()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .count();
                let amp = end - 1 - name;
                if name > 0 && amp > 0 && b[amp - 1] == b'&' {
                    end = amp - 1;
                } else {
                    end -= 1;
                }
            }
            b')' if close > open => {
                close -= 1;
                end -= 1;
            }
            _ => break,
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use maud::{Markup, html};
    use nom::bytes::complete::{tag, take_while1};
    use nom::character::complete::not_line_ending;
    use nom::sequence::{delimited, preceded};

    use super::*;
    use crate::draw::{DrawCtx, DrawCx, Element, draw_blocks};
    use crate::parse::{Never, Plain, parse};
    use crate::template::DefaultTemplate;
    use crate::tree::Parsed;

    fn draw<P: Host>(host: &P, md: &str) -> String {
        let doc = parse(host, md);
        draw_blocks(&doc.tree, &doc.body, &DrawCx::default()).into_string()
    }

    /// One paragraph's contents.
    fn plain(md: &str) -> String {
        let html = draw(&Plain, md);
        let inner = html
            .strip_prefix("<p>")
            .and_then(|h| h.strip_suffix("</p>\n"));
        inner.unwrap_or(&html).to_string()
    }

    #[test]
    fn escapes_and_code_spans_come_first() {
        assert_eq!(plain(r"\*a\* \#b \[c] \q"), r"*a* #b [c] \q");
        assert_eq!(
            plain("`a` `` b ` c `` ` d ` `\ne`"),
            "<code>a</code> <code>b ` c</code> <code>d</code> <code> e</code>"
        );
        // No closing run of the same length: the whole run is text.
        assert_eq!(plain("```a`` *b*"), "```a`` <em>b</em>");
        // A code span binds more tightly than a link's brackets.
        assert_eq!(
            plain("[a`]`](https://u)"),
            "<a href=\"https://u\">a<code>]</code></a>"
        );
    }

    #[test]
    fn autolinks_and_raw_html() {
        assert_eq!(
            plain("<https://e.org/a?b> <me@e.org> <a@b"),
            "<a href=\"https://e.org/a?b\">https://e.org/a?b</a> \
             <a href=\"mailto:me@e.org\">me@e.org</a> &lt;a@b"
        );
        assert_eq!(
            plain("a <span class=\"x\">b</span> <!-- c --> <?p?> <!X y> <![CDATA[z]]> <3"),
            "a <span class=\"x\">b</span> <!-- c --> <?p?> <!X y> <![CDATA[z]]> &lt;3"
        );
    }

    #[test]
    fn embeds_and_wikilinks() {
        assert_eq!(
            plain("![[a.png|Alt]] ![[Note]] [[Foo#b|the label]] [[ Bar ]] [[a[b]]"),
            "<img src=\"/a.png\" alt=\"Alt\" /> ![[Note]] \
             <a href=\"Foo#b\" class=\"wikilink\">the label</a> \
             <a href=\"Bar\" class=\"wikilink\">Bar</a> [[a[b]]"
        );
        let doc = parse(&Plain, "[[Foo#b|label]]\n");
        let link = doc.tree.arena.iter().find_map(|n| match n.get() {
            Node::Inline(InlineKind::WikiLink { target, label }) => Some((target, label)),
            _ => None,
        });
        assert_eq!(
            link,
            Some((&"Foo#b".to_string(), &Some("label".to_string())))
        );
    }

    #[test]
    fn links_and_images() {
        assert_eq!(
            plain("[a *b*](https://e.org \"T\") [c](<https://x y>) [d]()"),
            "<a href=\"https://e.org\" title=\"T\">a <em>b</em></a> \
             <a href=\"https://x%20y\">c</a> <a href=\"\">d</a>"
        );
        // References, defined after their use: full, collapsed, shortcut.
        assert_eq!(
            draw(
                &Plain,
                "[x][Ref] [ref][] [REF] [y][none]\n\n[ref]: https://r.org 'R'\n"
            ),
            "<p><a href=\"https://r.org\" title=\"R\">x</a> \
             <a href=\"https://r.org\" title=\"R\">ref</a> \
             <a href=\"https://r.org\" title=\"R\">REF</a> [y][none]</p>\n"
        );
        // Brackets nest; a link's text can't hold a link, an image's can.
        assert_eq!(
            plain("[a [b] c](https://u) [a [b](https://u) c](https://v)"),
            "<a href=\"https://u\">a [b] c</a> [a <a href=\"https://u\">b</a> c](https://v)"
        );
        assert_eq!(
            plain("![a [b](https://u) *c*](https://i.png \"t\") ![nope]"),
            "<img src=\"https://i.png\" alt=\"a b c\" title=\"t\" /> ![nope]"
        );
    }

    #[test]
    fn deep_nesting_is_text_not_a_stack_overflow() {
        let md = "[".repeat(5000);
        assert_eq!(plain(&md), md);
        let deep = format!("{}x{}", "[a ".repeat(100), "]".repeat(100));
        assert_eq!(plain(&deep), deep);
    }

    #[test]
    fn a_link_after_brackets_past_the_nesting_limit_is_a_link() {
        for n in [20, MAX_NESTING - 1, MAX_NESTING, 100] {
            let open = "[".repeat(n);
            assert_eq!(
                plain(&format!("{open}[a](https://u)")),
                format!("{open}<a href=\"https://u\">a</a>"),
                "{n}"
            );
        }
    }

    #[test]
    fn emphasis_past_the_nesting_limit_is_text() {
        let n = 20_000;
        let html = plain(&format!("{}a{}", "*".repeat(n), "*".repeat(n)));
        let stars = "*".repeat(n - 2 * MAX_NESTING);
        let strong = "<strong>".repeat(MAX_NESTING);
        let end = "</strong>".repeat(MAX_NESTING);
        assert_eq!(html, format!("{stars}{strong}a{end}{stars}"));
        // Separate runs, each closed inside the last.
        let html = plain(&format!("{}a{}", "*b ".repeat(1000), " b*".repeat(1000)));
        assert_eq!(html.matches("<em>").count(), MAX_NESTING);
    }

    #[test]
    fn entities() {
        assert_eq!(
            plain("&amp; &copy; &#35;x &#x41; &#0; &bogus; &#xZ;"),
            "&amp; © #x A \u{FFFD} &amp;bogus; &amp;#xZ;"
        );
        // Entities are text: not emphasis, not a tag.
        assert_eq!(plain("&#42;a&#42;"), "*a*");
    }

    #[test]
    fn tags() {
        assert_eq!(
            plain("#a x#b #12 \\#c [#d](https://u) `#e` **#f** h*#g `x`#h"),
            "<a class=\"tag\" href=\"/_wiki/tag/a\">#a</a> x#b #12 #c \
             <a href=\"https://u\">#d</a> <code>#e</code> \
             <strong><a class=\"tag\" href=\"/_wiki/tag/f\">#f</a></strong> h*#g \
             <code>x</code><a class=\"tag\" href=\"/_wiki/tag/h\">#h</a>"
        );
        assert_eq!(
            plain("#Project/Alpha-1, ![#no](https://i.png) [[W|#no]]"),
            "<a class=\"tag\" href=\"/_wiki/tag/project/alpha-1\">#Project/Alpha-1</a>, \
             <img src=\"https://i.png\" alt=\"#no\" /> <a href=\"W\" class=\"wikilink\">#no</a>"
        );
    }

    /// The inlines in document order, as `Text(…)`, `Tag(…)`, `Embed(…)` or
    /// the kind's name.
    fn inlines_of(md: &str) -> Vec<String> {
        fn walk(tree: &Tree<Plain>, id: NodeId, out: &mut Vec<String>) {
            match tree.arena[id].get() {
                Node::Inline(InlineKind::Text(t)) => out.push(format!("Text({t})")),
                Node::Inline(InlineKind::Tag(t)) => out.push(format!("Tag({t})")),
                Node::Inline(InlineKind::Embed { target, .. }) => {
                    out.push(format!("Embed({target})"))
                }
                Node::Inline(InlineKind::Code(_)) => out.push("Code".to_string()),
                _ => {}
            }
            for child in id.children(&tree.arena) {
                walk(tree, child, out);
            }
        }
        let doc = parse(&Plain, md);
        let mut out = Vec::new();
        for &id in &doc.body.ids {
            walk(&doc.tree, id, &mut out);
        }
        out
    }

    fn tags_of(md: &str) -> Vec<String> {
        inlines_of(md)
            .into_iter()
            .filter_map(|i| Some(i.strip_prefix("Tag(")?.strip_suffix(')')?.to_string()))
            .collect()
    }

    /// Ported from `markdown.rs`'s `segments` tests: today's tag rule.
    #[test]
    fn tags_follow_todays_rule() {
        assert_eq!(
            inlines_of("![[a.png|Alt]] #t ![[Note]] x![[b.PNG]]"),
            [
                "Embed(a.png)",
                "Text( )",
                "Tag(t)",
                "Text( ![[Note]] x)",
                "Embed(b.PNG)"
            ]
        );
        assert_eq!(
            tags_of("see #project/alpha and x#no #123 #Status."),
            ["project/alpha", "Status"]
        );
        assert_eq!(inlines_of("a #b c"), ["Text(a )", "Tag(b)", "Text( c)"]);
        assert_eq!(
            tags_of(
                "Real #one and `#code` and [#linktext](x) and [[Foo#b|#wl]]. Then [[Bar]] #Two."
            ),
            ["one", "Two"]
        );
        assert_eq!(
            tags_of("![alt #hidden [[X]]](i.png) #shown [[Y]]"),
            ["shown"]
        );
        // After an embed or a tag, `#` doesn't start one; after another
        // inline, a run of text starts again.
        assert_eq!(
            tags_of("![[a.png]]#t #a#b `c`#d **#e** x*#f*"),
            ["a", "d", "e"]
        );
        // A `#` from an escape or a character reference is text (escapes
        // now run first).
        assert_eq!(tags_of("\\#a &#35;b &num;c #d"), ["d"]);
    }

    #[test]
    fn extended_autolinks() {
        assert_eq!(
            plain("www.e.org/a. (https://e.org/q?(1)) me@e.org. xwww.no.org"),
            "<a href=\"http://www.e.org/a\">www.e.org/a</a>. \
             (<a href=\"https://e.org/q?(1)\">https://e.org/q?(1)</a>) \
             <a href=\"mailto:me@e.org\">me@e.org</a>. xwww.no.org"
        );
        // Not in link text.
        assert_eq!(
            plain("[www.e.org](https://u)"),
            "<a href=\"https://u\">www.e.org</a>"
        );
    }

    #[test]
    fn strikethrough_and_emphasis() {
        assert_eq!(
            plain("~~a~~ ~b~ ~~~c~~~ ~~d~"),
            "<del>a</del> <del>b</del> ~~~c~~~ ~~d~"
        );
        assert_eq!(
            plain("*a* **b** ***c*** _d_ foo_bar_ *e **f** g* **h*"),
            "<em>a</em> <strong>b</strong> <em><strong>c</strong></em> <em>d</em> \
             foo_bar_ <em>e <strong>f</strong> g</em> *<em>h</em>"
        );
    }

    #[test]
    fn hard_and_soft_breaks() {
        assert_eq!(plain("a  \nb\\\nc\n  d"), "a<br />\nb<br />\nc<br />\nd");
        let doc = parse(&Plain, "a  \nb\nc\\\nd\n");
        let breaks: Vec<_> = doc
            .tree
            .arena
            .iter()
            .filter_map(|n| match n.get() {
                Node::Inline(InlineKind::HardBreak) => Some("hard"),
                Node::Inline(InlineKind::SoftBreak) => Some("soft"),
                _ => None,
            })
            .collect();
        assert_eq!(breaks, ["hard", "soft", "hard"]);
    }

    /// Modelled on the sketch's `SampleHost`.
    struct SampleHost;

    enum SampleInline {
        Ticket(String),
        Marker(String),
        /// `[ ] text` at a list item's start, to the end of the line.
        Todo,
    }

    impl Element<SampleHost> for SampleInline {
        fn draw(&self, cx: &DrawCtx<'_, SampleHost>) -> Markup {
            match self {
                SampleInline::Ticket(id) => html! { span.ticket { (id) } },
                SampleInline::Marker(kind) => {
                    html! { span class={ "marker-" (kind) } { (cx.draw_children()) } }
                }
                SampleInline::Todo => html! { span.todo { (cx.draw_children()) } },
            }
        }
    }

    impl Host for SampleHost {
        type Block = Never;
        type Inline = SampleInline;
        type Template = DefaultTemplate;

        fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
            if s.at_item_start()
                && let Some(line) = s.try_parse(preceded(tag("[ ] "), not_line_ending))
            {
                let text = s.inlines_in(line);
                return Some(s.node(InlineKind::Host(SampleInline::Todo), text));
            }
            let id = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-';
            if let Some(id) = s.try_parse(delimited(tag("[["), take_while1(id), tag("]]"))) {
                return Some(s.leaf(InlineKind::Host(SampleInline::Ticket(id.to_string()))));
            }
            let kind = |c: char| c.is_ascii_uppercase();
            if let Some(kind) = s.try_parse(delimited(tag("["), take_while1(kind), tag(": "))) {
                let body = s.inlines_until(tag("]"))?;
                let marker = SampleInline::Marker(kind.to_lowercase());
                return Some(s.node(InlineKind::Host(marker), body));
            }
            None
        }
    }

    #[test]
    fn host_inlines_come_before_the_built_ins_and_nest() {
        assert_eq!(
            draw(
                &SampleHost,
                "See [[ABC-1]] and [[Page]]. [OPEN: **foo** _bar_ [[ABC-2]] #t]\n"
            ),
            "<p>See <span class=\"ticket\">ABC-1</span> and \
             <a href=\"Page\" class=\"wikilink\">Page</a>. \
             <span class=\"marker-open\"><strong>foo</strong> <em>bar</em> \
             <span class=\"ticket\">ABC-2</span> \
             <a class=\"tag\" href=\"/_wiki/tag/t\">#t</a></span></p>\n"
        );
        // Escapes and code spans come before the host.
        assert_eq!(
            draw(&SampleHost, "\\[[ABC-1]] `[[ABC-2]]`\n"),
            "<p>[[ABC-1]] <code>[[ABC-2]]</code></p>\n"
        );
        // No `]`: the hook's `None` rewinds, and the built-ins run.
        assert_eq!(
            draw(&SampleHost, "[OPEN: *a*\n"),
            "<p>[OPEN: <em>a</em></p>\n"
        );
    }

    #[test]
    fn a_list_items_start_is_offered_to_the_host_before_the_checkbox() {
        // Claimed and built-in tasks, sub-items and plain items share one
        // list; the claim ends at the end of its line.
        assert_eq!(
            draw(
                &SampleHost,
                "- [ ] a *b* [[ABC-1]]\n  c\n  - [ ] sub\n- [x] done\n- plain\n"
            ),
            "<ul>\n<li><span class=\"todo\">a <em>b</em> \
             <span class=\"ticket\">ABC-1</span></span><br />\nc\n<ul>\n\
             <li><span class=\"todo\">sub</span></li>\n</ul>\n</li>\n\
             <li><input type=\"checkbox\" checked=\"\" disabled=\"\" /> done</li>\n\
             <li>plain</li>\n</ul>\n"
        );
        // Only at the start of an item's content, never of a paragraph.
        assert_eq!(
            draw(&SampleHost, "[ ] a\n\n- b [ ] c\n- [?] d\n-\n  [ ] e\n"),
            "<p>[ ] a</p>\n<ul>\n<li>b [ ] c</li>\n<li>[?] d</li>\n<li>[ ] e</li>\n</ul>\n"
        );
    }

    #[test]
    fn host_nesting_past_the_limit_is_text() {
        let md = format!("{}x{}\n", "[OPEN: ".repeat(100), "]".repeat(100));
        let html = draw(&SampleHost, &md);
        // The outer levels are markers; past the limit, the rest is text.
        assert!(
            html.starts_with("<p><span class=\"marker-open\">"),
            "{html}"
        );
        assert!(html.contains("[OPEN: [OPEN: "), "{html}");
        assert_eq!(html.matches("<span").count(), MAX_NESTING - 1);
    }

    /// `md` drawn with `host`, in well under a second even in a debug
    /// build.
    fn draw_quickly<P: Host>(host: &P, md: &str) -> String {
        let start = Instant::now();
        let html = draw(host, md);
        let took = start.elapsed();
        assert!(took < Duration::from_secs(1), "{took:?}");
        html
    }

    #[test]
    fn a_host_none_is_not_asked_again() {
        // Each unclosed marker's `None` would otherwise be asked again from
        // the link text of the `[` it falls back to, doubling the work.
        let md = "[OPEN: a ".repeat(25);
        assert_eq!(
            draw_quickly(&SampleHost, &md),
            format!("<p>{}</p>\n", md.trim_end())
        );
    }

    #[test]
    fn long_emphasis_and_unclosed_raw_html_are_linear() {
        let pairs = "*a* ".repeat(20_000);
        let html = draw_quickly(&Plain, &pairs);
        assert_eq!(html.matches("<em>a</em>").count(), 20_000);
        let openers = format!("{}a\n", "*".repeat(20_000));
        assert!(draw_quickly(&Plain, &openers).contains("**a"));
        for open in ["<![CDATA[", "<!--", "<?", "<!X"] {
            let md = format!("a {}\n", open.repeat(5000));
            assert!(!draw_quickly(&Plain, &md).contains("<!"), "{open}");
        }
    }

    #[test]
    fn emphasis_does_not_cross_a_host_boundary() {
        assert_eq!(
            draw(&SampleHost, "*a [OPEN: b* c] **d [OPEN: e** f]**\n"),
            "<p>*a <span class=\"marker-open\">b* c</span> \
             <strong>d <span class=\"marker-open\">e** f</span></strong></p>\n"
        );
    }

    #[test]
    fn headings_and_table_cells_are_inline_sections() {
        assert_eq!(
            draw(
                &Plain,
                "# A *b* #c\n\n| `x` | **y** |\n|---|---|\n| [z](https://u) | ~~w~~ |\n"
            ),
            "<h1>A <em>b</em> <a class=\"tag\" href=\"/_wiki/tag/c\">#c</a></h1>\n\
             <table>\n<thead>\n<tr>\n<th><code>x</code></th>\n<th><strong>y</strong></th>\n\
             </tr>\n</thead>\n<tbody>\n<tr>\n<td><a href=\"https://u\">z</a></td>\n\
             <td><del>w</del></td>\n</tr>\n</tbody>\n</table>\n"
        );
    }
}
