//! The block phase: frontmatter, containers (block quotes, list items), leaf
//! blocks, and the host block hook, which is offered every line start
//! before the built-ins. Inline sections are left for the inline phase.

use indextree::NodeId;
use serde_json::Value;

use super::inline::unescape;
use super::stream::{BlockStream, Claim, Probed};
use super::{Host, MAX_NESTING};
use crate::tree::{Align, BlockKind, Children, Content, Fold, InlineKind, Node};

/// A line, or what is left of it once container prefixes are stripped.
#[derive(Debug, Clone, Copy)]
pub(super) struct Line<'a> {
    /// The text left, after any tab partly consumed as indentation.
    pub(super) rest: &'a str,
    /// The column `rest` starts at, for tab stops.
    col: usize,
    /// Columns left over from a tab partly consumed as indentation; they
    /// count as spaces before `rest`.
    virt: usize,
}

impl<'a> Line<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Line {
            rest: text,
            col: 0,
            virt: 0,
        }
    }

    pub(super) fn is_blank(&self) -> bool {
        self.rest.bytes().all(|b| b == b' ' || b == b'\t')
    }

    /// Columns of leading whitespace, tabs to the next multiple of 4.
    fn indent(&self) -> usize {
        let mut col = self.col;
        for b in self.rest.bytes() {
            match b {
                b' ' => col += 1,
                b'\t' => col += 4 - col % 4,
                _ => break,
            }
        }
        self.virt + col - self.col
    }

    /// The line with up to `n` columns of leading whitespace removed.
    fn strip(mut self, mut n: usize) -> Self {
        let virt = n.min(self.virt);
        self.virt -= virt;
        n -= virt;
        while n > 0 {
            match self.rest.as_bytes().first() {
                Some(b' ') => n -= 1,
                Some(b'\t') => {
                    let width = 4 - self.col % 4;
                    self.col += width - 1;
                    if width > n {
                        self.virt = width - n;
                        n = 0;
                    } else {
                        n -= width;
                    }
                }
                _ => break,
            }
            self.rest = &self.rest[1..];
            self.col += 1;
        }
        self
    }

    fn trim_start(self) -> Self {
        self.strip(usize::MAX)
    }

    /// The line past `n` bytes of a marker (ASCII, no tabs).
    fn skip(mut self, n: usize) -> Self {
        if n == 0 {
            return self;
        }
        self.rest = &self.rest[n..];
        self.col += n;
        self.virt = 0;
        self
    }

    /// Appends the text, leftover tab columns as spaces, and a newline.
    fn push_to(&self, out: &mut String) {
        out.extend(std::iter::repeat_n(' ', self.virt));
        out.push_str(self.rest);
        out.push('\n');
    }
}

/// A line through the open containers.
pub(super) enum Strip<'a> {
    /// Inside them all: the line without their prefixes.
    Open(Line<'a>),
    /// The container at this depth doesn't continue on the line; the line
    /// has the prefixes of the containers outside it stripped.
    Closed(usize, Line<'a>),
}

/// An open container.
pub(super) enum Frame {
    Quote,
    Item {
        /// Columns of indentation that continue the item.
        width: usize,
        first: Option<First>,
    },
}

/// A list item's first line, where the marker takes the place of the
/// indentation.
#[derive(Clone, Copy)]
pub(super) struct First {
    line: usize,
    marker: Marker,
}

impl Frame {
    /// `line`, line `i` of the document, without this container's prefix;
    /// `None` when the container doesn't continue on it.
    pub(super) fn strip<'a>(&self, line: Line<'a>, i: usize) -> Option<Line<'a>> {
        match self {
            Frame::Quote => quote(line),
            Frame::Item { first: Some(f), .. } if f.line == i => Some(f.marker.content(line)),
            Frame::Item { width, .. } => {
                (line.is_blank() || line.indent() >= *width).then(|| line.strip(*width))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListKind {
    Bullet(u8),
    /// By delimiter, `.` or `)`.
    Ordered(u8),
}

/// A list item's marker, as the start of a line.
#[derive(Debug, Clone, Copy)]
pub(super) struct Marker {
    pub(super) kind: ListKind,
    start: u32,
    /// Columns of indentation before the marker.
    pre: usize,
    /// Bytes of marker.
    len: usize,
    /// Columns between the marker and the content.
    gap: usize,
    /// Nothing follows the marker.
    blank: bool,
}

impl Marker {
    /// Columns of indentation that continue the item.
    pub(super) fn width(&self) -> usize {
        self.pre + self.len + self.gap
    }

    /// The item's first line, `line`, after the marker.
    fn content<'a>(&self, line: Line<'a>) -> Line<'a> {
        let after = line.trim_start().skip(self.len);
        if self.blank {
            after.trim_start()
        } else {
            after.strip(self.gap)
        }
    }
}

/// The list item marker `line` starts with. A thematic break is not one.
pub(super) fn list_marker(line: Line<'_>) -> Option<Marker> {
    let pre = line.indent();
    if pre > 3 || thematic_break(line) {
        return None;
    }
    let line = line.trim_start();
    let bytes = line.rest.as_bytes();
    let (kind, start, len) = match *bytes.first()? {
        c @ (b'-' | b'+' | b'*') => (ListKind::Bullet(c), 1, 1),
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            let delim = *bytes.get(digits)?;
            if digits > 9 || !matches!(delim, b'.' | b')') {
                return None;
            }
            let start = line.rest[..digits].parse().ok()?;
            (ListKind::Ordered(delim), start, digits + 1)
        }
        _ => return None,
    };
    let after = line.skip(len);
    if !matches!(after.rest.as_bytes().first(), None | Some(b' ' | b'\t')) {
        return None;
    }
    let blank = after.is_blank();
    let ws = after.indent();
    Some(Marker {
        kind,
        start,
        pre,
        len,
        gap: if blank || ws >= 5 { 1 } else { ws },
        blank,
    })
}

/// `line` without a block quote marker, if it starts with one.
fn quote(line: Line<'_>) -> Option<Line<'_>> {
    if line.indent() > 3 {
        return None;
    }
    let line = line.trim_start();
    line.rest.starts_with('>').then(|| {
        let line = line.skip(1);
        if line.rest.starts_with([' ', '\t']) {
            line.strip(1)
        } else {
            line
        }
    })
}

/// The callout header `line`, a block quote's first line, is: `[!type]`,
/// then `+` or `-` to fold, then the title. Gives the type lowercase, the
/// fold and the title.
fn callout_header(line: Line<'_>) -> Option<(String, Option<Fold>, &str)> {
    if line.indent() > 3 {
        return None;
    }
    let t = line.trim_start().rest.strip_prefix("[!")?;
    let len = t.find(|c: char| c == ']' || c.is_whitespace())?;
    if len == 0 || !t[len..].starts_with(']') {
        return None;
    }
    let rest = &t[len + 1..];
    let (fold, rest) = match rest.as_bytes().first() {
        Some(b'+') => (Some(Fold::Open), &rest[1..]),
        Some(b'-') => (Some(Fold::Closed), &rest[1..]),
        _ => (None, rest),
    };
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    Some((
        t[..len].to_lowercase(),
        fold,
        rest.trim_matches([' ', '\t']),
    ))
}

fn thematic_break(line: Line<'_>) -> bool {
    if line.indent() > 3 {
        return false;
    }
    let t = line.trim_start().rest;
    let Some(c @ (b'-' | b'*' | b'_')) = t.bytes().next() else {
        return false;
    };
    t.bytes().all(|b| b == c || b == b' ' || b == b'\t')
        && t.bytes().filter(|&b| b == c).count() >= 3
}

/// An ATX heading's level and text.
fn atx(line: Line<'_>) -> Option<(u8, &str)> {
    if line.indent() > 3 {
        return None;
    }
    let t = line.trim_start().rest;
    let level = t.bytes().take_while(|&b| b == b'#').count();
    let after = &t[level..];
    if !(1..=6).contains(&level) || !matches!(after.as_bytes().first(), None | Some(b' ' | b'\t')) {
        return None;
    }
    let text = after.trim_matches([' ', '\t']);
    let unclosed = text.trim_end_matches('#');
    let text = if unclosed.is_empty() {
        unclosed
    } else if unclosed.ends_with([' ', '\t']) {
        unclosed.trim_end_matches([' ', '\t'])
    } else {
        text
    };
    Some((level as u8, text))
}

/// The level a setext underline gives the paragraph above it.
fn setext(line: Line<'_>) -> Option<u8> {
    if line.indent() > 3 {
        return None;
    }
    let t = line.trim_start().rest.trim_end_matches([' ', '\t']);
    if !t.is_empty() && t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if !t.is_empty() && t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

struct Fence {
    ch: u8,
    len: usize,
    indent: usize,
    info: String,
}

impl Fence {
    fn open(line: Line<'_>) -> Option<Fence> {
        let indent = line.indent();
        if indent > 3 {
            return None;
        }
        let t = line.trim_start().rest;
        let ch = *t.as_bytes().first().filter(|&&b| b == b'`' || b == b'~')?;
        let len = t.bytes().take_while(|&b| b == ch).count();
        let info = t[len..].trim_matches([' ', '\t']);
        if len < 3 || (ch == b'`' && info.contains('`')) {
            return None;
        }
        Some(Fence {
            ch,
            len,
            indent,
            info: unescape(info),
        })
    }

    fn closes(&self, line: Line<'_>) -> bool {
        if line.indent() > 3 {
            return false;
        }
        let t = line.trim_start().rest;
        let len = t.bytes().take_while(|&b| b == self.ch).count();
        len >= self.len && t[len..].bytes().all(|b| b == b' ' || b == b'\t')
    }
}

/// How an HTML block ends.
enum HtmlEnd {
    /// At the first line containing one of these (lowercase).
    Contains(&'static [&'static str]),
    /// At the line holding the matching closing tag of this block-level
    /// element; with none in reach, as `Blank`.
    Close(String),
    /// Before the next blank line.
    Blank,
}

/// The HTML block `t` (a line without its indentation) starts, if any. A
/// paragraph can't be interrupted by a complete tag of another name
/// (CommonMark's type 7).
fn html_start(t: &str, interrupting: bool) -> Option<HtmlEnd> {
    let rest = t.strip_prefix('<')?;
    let lower = rest.to_ascii_lowercase();
    let name_ends = |s: &str| s.is_empty() || s.starts_with([' ', '\t', '>']);
    for name in ["pre", "script", "style", "textarea"] {
        if lower.strip_prefix(name).is_some_and(name_ends) {
            return Some(HtmlEnd::Contains(&[
                "</pre>",
                "</script>",
                "</style>",
                "</textarea>",
            ]));
        }
    }
    if rest.starts_with("!--") {
        return Some(HtmlEnd::Contains(&["-->"]));
    }
    if rest.starts_with('?') {
        return Some(HtmlEnd::Contains(&["?>"]));
    }
    if rest.starts_with("![CDATA[") {
        return Some(HtmlEnd::Contains(&["]]>"]));
    }
    if rest
        .strip_prefix('!')
        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_alphabetic()))
    {
        return Some(HtmlEnd::Contains(&[">"]));
    }
    let (closing, body) = match lower.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, lower.as_str()),
    };
    let len = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let after = &body[len..];
    if BLOCK_NAMES.contains(&&body[..len]) && (name_ends(after) || after.starts_with("/>")) {
        return Some(if closing {
            HtmlEnd::Blank
        } else {
            HtmlEnd::Close(body[..len].to_string())
        });
    }
    let tag = open_tag(t).or_else(|| close_tag(t))?;
    (!interrupting && t[tag..].trim_matches([' ', '\t']).is_empty()).then_some(HtmlEnd::Blank)
}

/// The length of the complete open tag `s` starts with.
pub(super) fn open_tag(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 1 + tag_name(s.strip_prefix('<')?)?;
    loop {
        let ws = b[i..]
            .iter()
            .take_while(|c| matches!(c, b' ' | b'\t' | b'\n'))
            .count();
        let name = b
            .get(i + ws)
            .filter(|c| c.is_ascii_alphabetic() || matches!(c, b'_' | b':'));
        if ws == 0 || name.is_none() {
            i += ws;
            break;
        }
        i += ws;
        i += b[i..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
            .count();
        // An optional value.
        let ws = b[i..]
            .iter()
            .take_while(|c| matches!(c, b' ' | b'\t' | b'\n'))
            .count();
        if b.get(i + ws) != Some(&b'=') {
            continue;
        }
        i += ws + 1;
        i += b[i..]
            .iter()
            .take_while(|c| matches!(c, b' ' | b'\t' | b'\n'))
            .count();
        i += match b.get(i)? {
            q @ (b'"' | b'\'') => b[i + 1..].iter().position(|c| c == q)? + 2,
            _ => {
                let n = b[i..]
                    .iter()
                    .take_while(|c| {
                        !matches!(
                            c,
                            b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`'
                        )
                    })
                    .count();
                (n > 0).then_some(n)?
            }
        };
    }
    if b.get(i) == Some(&b'/') {
        i += 1;
    }
    (b.get(i) == Some(&b'>')).then_some(i + 1)
}

/// The length of the complete closing tag `s` starts with.
pub(super) fn close_tag(s: &str) -> Option<usize> {
    let mut i = 2 + tag_name(s.strip_prefix("</")?)?;
    i += s.as_bytes()[i..]
        .iter()
        .take_while(|c| matches!(c, b' ' | b'\t' | b'\n'))
        .count();
    (s.as_bytes().get(i) == Some(&b'>')).then_some(i + 1)
}

/// The length of the tag name `s` starts with.
fn tag_name(s: &str) -> Option<usize> {
    s.starts_with(|c: char| c.is_ascii_alphabetic()).then(|| {
        s.find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .unwrap_or(s.len())
    })
}

/// The tag names that start an HTML block of CommonMark's type 6.
const BLOCK_NAMES: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// A link reference definition, as written; the inline phase resolves
/// escapes and entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkRef {
    /// Normalized: whitespace collapsed, case folded.
    pub(crate) label: String,
    pub(crate) dest: String,
    pub(crate) title: Option<String>,
}

/// The link reference definitions at the start of a paragraph's text, and
/// the text after them.
fn link_refs(mut text: &str) -> (Vec<LinkRef>, &str) {
    let mut refs = Vec::new();
    while let Some((r, rest)) = link_ref(text) {
        refs.push(r);
        text = rest;
    }
    (refs, text)
}

fn link_ref(s: &str) -> Option<(LinkRef, &str)> {
    let s = s.strip_prefix('[')?;
    let b = s.as_bytes();
    let mut i = 0;
    loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b'[' => return None,
            b']' => break,
            _ => i += 1,
        }
    }
    let label = &s[..i];
    if i > 999 || label.trim().is_empty() {
        return None;
    }
    let s = skip_space(s[i + 1..].strip_prefix(':')?);
    let (dest, s) = link_dest(s)?;
    let spaced = skip_space(s);
    let titled = (spaced.len() < s.len())
        .then(|| link_title(spaced))
        .flatten()
        .and_then(|(title, rest)| Some((title, line_end(rest)?)));
    let (title, rest) = match titled {
        Some((title, rest)) => (Some(title), rest),
        None => (None, line_end(s)?),
    };
    Some((
        LinkRef {
            label: label_key(label),
            dest: dest.to_string(),
            title: title.map(str::to_string),
        },
        rest,
    ))
}

/// A link label as it is matched: whitespace collapsed, case folded.
pub(super) fn label_key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .to_uppercase()
}

/// `s` past spaces and tabs, and at most one line ending.
pub(super) fn skip_space(s: &str) -> &str {
    let s = s.trim_start_matches([' ', '\t']);
    s.strip_prefix('\n')
        .map_or(s, |s| s.trim_start_matches([' ', '\t']))
}

/// The rest of `s` after spaces, tabs and a line ending; `None` if anything
/// else is on the line.
fn line_end(s: &str) -> Option<&str> {
    let s = s.trim_start_matches([' ', '\t']);
    if s.is_empty() {
        Some(s)
    } else {
        s.strip_prefix('\n')
    }
}

pub(super) fn link_dest(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if let Some(inner) = s.strip_prefix('<') {
        let mut i = 0;
        loop {
            match inner.as_bytes().get(i)? {
                b'\\' => i += 2,
                b'>' => return Some((&inner[..i], &inner[i + 1..])),
                b'<' | b'\n' => return None,
                _ => i += 1,
            }
        }
    }
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        match c {
            b'\\' if b.get(i + 1).is_some_and(u8::is_ascii_punctuation) => i += 1,
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            c if c <= b' ' || c == 0x7f => break,
            _ => {}
        }
        i += 1;
    }
    (i > 0 && depth == 0).then(|| (&s[..i], &s[i..]))
}

pub(super) fn link_title(s: &str) -> Option<(&str, &str)> {
    let close = match s.as_bytes().first()? {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let b = s.as_bytes();
    let mut i = 1;
    loop {
        match *b.get(i)? {
            b'\\' => i += 2,
            c if c == close => return Some((&s[1..i], &s[i + 1..])),
            b'(' if close == b')' => return None,
            _ => i += 1,
        }
    }
}

/// A table's delimiter row: one alignment per cell.
fn delimiter_row(s: &str) -> Option<Vec<Align>> {
    row_cells(s)
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':') && cell.len() > 1;
            let dashes = &cell[left as usize..cell.len() - right as usize];
            (!dashes.is_empty() && dashes.bytes().all(|b| b == b'-')).then_some(
                match (left, right) {
                    (true, true) => Align::Center,
                    (true, false) => Align::Left,
                    (false, true) => Align::Right,
                    (false, false) => Align::None,
                },
            )
        })
        .collect()
}

/// A table row's cells, trimmed, with `\|` as `|`.
fn row_cells(s: &str) -> Vec<String> {
    let s = s.trim_matches([' ', '\t']);
    let s = s.strip_prefix('|').unwrap_or(s);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('|') => cell.push('|'),
                Some(next) => {
                    cell.push('\\');
                    cell.push(next);
                }
                None => cell.push('\\'),
            },
            '|' => cells.push(
                std::mem::take(&mut cell)
                    .trim_matches([' ', '\t'])
                    .to_string(),
            ),
            _ => cell.push(c),
        }
    }
    if !cell.trim_matches([' ', '\t']).is_empty() || cells.is_empty() {
        cells.push(cell.trim_matches([' ', '\t']).to_string());
    }
    cells
}

/// Whether `line`, where a paragraph could continue, starts another block
/// instead. When it would continue the paragraph inside its containers
/// (`paragraph`), an ordered list must start at 1 and an item can't be
/// empty; a lazy continuation line is held to no such rule.
fn starts_block(line: Line<'_>, paragraph: bool) -> bool {
    if line.indent() > 3 {
        return false;
    }
    Fence::open(line).is_some()
        || html_start(line.trim_start().rest, true).is_some()
        || atx(line).is_some()
        || thematic_break(line)
        || quote(line).is_some()
        || list_marker(line).is_some_and(|m| {
            !paragraph || (!m.blank && (matches!(m.kind, ListKind::Bullet(_)) || m.start == 1))
        })
}

/// The blocks of one container.
#[derive(Default)]
pub(super) struct Section {
    pub(super) ids: Vec<NodeId>,
    /// A blank line separates two of them.
    blank_between: bool,
    /// The last line was blank.
    ends_blank: bool,
}

impl<'a, P: Host> BlockStream<'a, P> {
    /// The whole document: frontmatter, and the body's blocks.
    pub(super) fn document(&mut self) -> (Option<Value>, Vec<NodeId>) {
        let (frontmatter, raw) = self.frontmatter();
        let mut ids: Vec<NodeId> = raw.into_iter().collect();
        ids.extend(self.blocks().ids);
        (frontmatter, ids)
    }

    /// A leading `---`…`---` block. When it isn't well formed (YAML that
    /// doesn't parse, isn't a mapping, or has a key that isn't a string),
    /// there is no frontmatter and its text is a yaml code block.
    fn frontmatter(&mut self) -> (Option<Value>, Option<NodeId>) {
        let is_fence = |l: &&str| l.trim_end() == "---";
        if !self.lines.first().is_some_and(is_fence) {
            return (None, None);
        }
        let Some(close) = self.lines.iter().skip(1).position(is_fence) else {
            return (None, None);
        };
        let raw: String = self.lines[1..=close]
            .iter()
            .flat_map(|l| [*l, "\n"])
            .collect();
        self.pos = close + 2;
        let yaml = serde_norway::from_str::<serde_norway::Value>(&raw).ok();
        let value = yaml
            .filter(|v| v.is_mapping() && string_keys(v))
            .and_then(|v| serde_json::to_value(v).ok());
        match value {
            Some(value) => (Some(value), None),
            None => {
                let code = BlockKind::Code {
                    info: "yaml".to_string(),
                    body: raw,
                };
                let id = self.add(code, Content::Leaf);
                self.tree.lines.insert(id, 1);
                (None, Some(id))
            }
        }
    }

    fn add(&mut self, kind: BlockKind<P>, content: Content<P>) -> NodeId {
        self.tree.block(kind, content).id
    }

    /// Blocks until the open containers end.
    pub(super) fn blocks(&mut self) -> Section {
        self.levels.push(self.frames.len());
        let section = self.level_blocks();
        self.levels.pop();
        section
    }

    /// Blocks until the open containers end, in the block section open
    /// around the position.
    fn level_blocks(&mut self) -> Section {
        let mut section = Section::default();
        let mut blank = false;
        while let Some(Strip::Open(line)) = self.peek() {
            if line.is_blank() {
                self.pos += 1;
                blank = true;
                continue;
            }
            section.blank_between |= blank && !section.ids.is_empty();
            let start = self.pos;
            let (ids, ends_blank) = self.block(line);
            // A claim, and a paragraph after link reference definitions,
            // have their line already.
            if let Some(&first) = ids.first() {
                self.tree.lines.entry(first).or_insert(start + 1);
            }
            section.ids.extend(ids);
            blank = ends_blank;
        }
        section.ends_blank = blank;
        section
    }

    /// The block starting at `line`, the current line: the host's claim, or
    /// a built-in. Returns the nodes (a paragraph can be followed by the
    /// claim that ended it) and whether they ended with a blank line.
    fn block(&mut self, line: Line<'a>) -> (Vec<NodeId>, bool) {
        if let Some(claim) = self.claim(line) {
            return (vec![claim.id], claim.ends_blank);
        }
        if line.indent() >= 4 {
            return (vec![self.indented_code()], false);
        }
        if let Some(fence) = Fence::open(line) {
            return (vec![self.fenced_code(fence)], false);
        }
        if let Some(end) = html_start(line.trim_start().rest, false) {
            return (vec![self.html(end)], false);
        }
        if let Some((level, text)) = atx(line) {
            self.pos += 1;
            let inlines = self.inlines_in(text);
            let heading = BlockKind::Heading { level };
            return (vec![self.add(heading, Content::Inlines(inlines))], false);
        }
        if thematic_break(line) {
            self.pos += 1;
            return (
                vec![self.add(BlockKind::ThematicBreak, Content::Leaf)],
                false,
            );
        }
        // Past the nesting limit, a container's marker is text.
        let nest = self.levels.len() < MAX_NESTING;
        if nest && quote(line).is_some() {
            return (vec![self.block_quote()], false);
        }
        if nest && let Some(marker) = list_marker(line) {
            return self.list(marker);
        }
        if let Some(align) = self.table_start(line) {
            return self.table(line, align);
        }
        self.paragraph(line)
    }

    /// A block quote, or a callout when its first line is a callout header
    /// the host doesn't claim.
    fn block_quote(&mut self) -> NodeId {
        self.frames.push(Frame::Quote);
        self.levels.push(self.frames.len());
        let start = self.pos;
        let header = self.callout_header();
        let body = self.level_blocks();
        self.levels.pop();
        self.frames.pop();
        let Some((kind, fold, title)) = header else {
            return self.add(BlockKind::Quote, Content::Blocks(Children::new(body.ids)));
        };
        let title = self.add(BlockKind::CalloutTitle, Content::Inlines(title));
        self.tree.lines.insert(title, start + 1);
        let ids = std::iter::once(title).chain(body.ids).collect();
        let callout = BlockKind::Callout { kind, fold };
        self.add(callout, Content::Blocks(Children::new(ids)))
    }

    /// The callout header on the current line, the first inside a block
    /// quote, consumed: the type, the fold and the title.
    fn callout_header(&mut self) -> Option<(String, Option<Fold>, Children<InlineKind<P>>)> {
        let Some(Strip::Open(line)) = self.peek() else {
            return None;
        };
        let (kind, fold, title) = callout_header(line)?;
        if self.claims(self.frames.len(), line) {
            return None;
        }
        self.pos += 1;
        let title = if title.is_empty() {
            Children::new(Vec::new())
        } else {
            self.inlines_in(title)
        };
        Some((kind, fold, title))
    }

    fn indented_code(&mut self) -> NodeId {
        let mut body = String::new();
        let mut blanks = String::new();
        let mut end = self.pos;
        while let Some(Strip::Open(line)) = self.peek() {
            if line.is_blank() {
                line.strip(4).push_to(&mut blanks);
            } else if line.indent() >= 4 {
                body.push_str(&blanks);
                blanks.clear();
                line.strip(4).push_to(&mut body);
                end = self.pos + 1;
            } else {
                break;
            }
            self.pos += 1;
        }
        // Trailing blank lines are not part of it.
        self.pos = end;
        let code = BlockKind::Code {
            info: String::new(),
            body,
        };
        self.add(code, Content::Leaf)
    }

    fn fenced_code(&mut self, fence: Fence) -> NodeId {
        self.pos += 1;
        let mut body = String::new();
        while let Some(Strip::Open(line)) = self.peek() {
            self.pos += 1;
            if fence.closes(line) {
                break;
            }
            line.strip(fence.indent).push_to(&mut body);
        }
        let code = BlockKind::Code {
            info: fence.info,
            body,
        };
        self.add(code, Content::Leaf)
    }

    fn html(&mut self, end: HtmlEnd) -> NodeId {
        let close = match &end {
            HtmlEnd::Close(name) => self.closing_tag(name),
            _ => None,
        };
        let mut raw = String::new();
        while let Some(Strip::Open(line)) = self.peek() {
            let blank_ends = matches!(end, HtmlEnd::Blank | HtmlEnd::Close(_)) && close.is_none();
            if blank_ends && line.is_blank() {
                break;
            }
            line.push_to(&mut raw);
            self.pos += 1;
            let done = match &end {
                HtmlEnd::Contains(ends) => {
                    let lower = line.rest.to_ascii_lowercase();
                    ends.iter().any(|e| lower.contains(e))
                }
                HtmlEnd::Close(_) => close == Some(self.pos - 1),
                HtmlEnd::Blank => false,
            };
            if done {
                break;
            }
        }
        self.add(BlockKind::Html(raw), Content::Leaf)
    }

    /// The line, from the current one on and inside the open containers,
    /// that closes the `<name>` element the current line opens: nested
    /// `<name>`s are counted. An opening line that closes its own tags
    /// closes the element.
    fn closing_tag(&self, name: &str) -> Option<usize> {
        let mut depth = 0usize;
        for i in self.pos..self.limit {
            let Strip::Open(line) = self.strip(i) else {
                return None;
            };
            let lower = line.rest.to_ascii_lowercase();
            let mut rest = lower.as_str();
            while let Some(at) = rest.find('<') {
                rest = &rest[at + 1..];
                let (closing, tag) = match rest.strip_prefix('/') {
                    Some(tag) => (true, tag),
                    None => (false, rest),
                };
                let Some(after) = tag.strip_prefix(name) else {
                    continue;
                };
                if closing && after.starts_with(['>', ' ', '\t']) {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                } else if !closing && (after.is_empty() || after.starts_with(['>', ' ', '\t', '/']))
                {
                    let self_closing = after.find('>').is_some_and(|e| after[..e].ends_with('/'));
                    depth += usize::from(!self_closing);
                }
            }
            if depth == 0 {
                return Some(i);
            }
        }
        None
    }

    /// A list, from its first item's `marker`. A line the host claims ends
    /// it.
    fn list(&mut self, marker: Marker) -> (Vec<NodeId>, bool) {
        let mut items = Vec::new();
        let mut tight = true;
        let mut blank = false;
        let mut next = Some(marker);
        while let Some(m) = next.take() {
            tight &= !blank;
            let (id, blank_between, ends_blank) = self.item(m);
            tight &= !blank_between;
            blank = ends_blank;
            items.push(id);
            while let Some(Strip::Open(line)) = self.peek()
                && line.is_blank()
            {
                self.pos += 1;
                blank = true;
            }
            let Some(Strip::Open(line)) = self.peek() else {
                break;
            };
            let Some(m) = list_marker(line).filter(|m| m.kind == marker.kind) else {
                break;
            };
            if self.claims(self.frames.len(), line) {
                break;
            }
            next = Some(m);
        }
        let list = BlockKind::List {
            ordered: matches!(marker.kind, ListKind::Ordered(_)),
            start: marker.start,
            tight,
        };
        (
            vec![self.add(list, Content::Blocks(Children::new(items)))],
            blank,
        )
    }

    /// A list item: the node, whether a blank line separates two of its
    /// blocks, and whether it ended with a blank line.
    fn item(&mut self, marker: Marker) -> (NodeId, bool, bool) {
        let start = self.pos;
        self.frames.push(Frame::Item {
            width: marker.width(),
            first: Some(First {
                line: self.pos,
                marker,
            }),
        });
        // An item can start with at most one blank line.
        let empty = marker.blank && {
            self.pos += 1;
            !matches!(self.peek(), Some(Strip::Open(l)) if !l.is_blank())
        };
        let body = if empty {
            Section::default()
        } else {
            self.blocks()
        };
        self.frames.pop();
        // The inline phase offers its start to the host, then reads a task
        // checkbox there.
        if let Some(&first) = body.ids.first()
            && !marker.blank
            && matches!(
                self.tree.arena[first].get(),
                Node::Block(BlockKind::Paragraph)
            )
        {
            self.tree.item_starts.insert(first);
        }
        let id = self.add(
            BlockKind::ListItem { task: None },
            Content::Blocks(Children::new(body.ids)),
        );
        self.tree.lines.insert(id, start + 1);
        (id, body.blank_between, body.ends_blank)
    }

    /// The table `line` heads: its delimiter row's alignments.
    fn table_start(&self, line: Line<'_>) -> Option<Vec<Align>> {
        if line.indent() > 3 || self.pos + 1 >= self.limit {
            return None;
        }
        let Strip::Open(next) = self.strip(self.pos + 1) else {
            return None;
        };
        if next.indent() > 3 || setext(next).is_some() {
            return None;
        }
        delimiter_row(next.rest).filter(|align| row_cells(line.rest).len() == align.len())
    }

    fn table(&mut self, header: Line<'a>, align: Vec<Align>) -> (Vec<NodeId>, bool) {
        let mut rows = vec![self.row(header.rest, align.len(), true)];
        self.pos += 2;
        let mut claim = None;
        while let Some(Strip::Open(line)) = self.peek() {
            if line.is_blank() {
                break;
            }
            if let Some(c) = self.claim(line) {
                claim = Some(c);
                break;
            }
            if starts_block(line, false) {
                break;
            }
            rows.push(self.row(line.rest, align.len(), false));
            self.pos += 1;
        }
        let table = self.add(
            BlockKind::Table { align },
            Content::Blocks(Children::new(rows)),
        );
        then_claimed(table, claim)
    }

    /// A table row of `n` cells, on the current line: missing ones empty,
    /// extra ones dropped.
    fn row(&mut self, text: &str, n: usize, header: bool) -> NodeId {
        let mut cells = row_cells(text);
        cells.resize(n, String::new());
        let cells = cells
            .iter()
            .map(|cell| {
                let inlines = self.inlines_in(cell);
                self.add(BlockKind::TableCell, Content::Inlines(inlines))
            })
            .collect();
        let id = self.add(
            BlockKind::TableRow { header },
            Content::Blocks(Children::new(cells)),
        );
        self.tree.lines.insert(id, self.pos + 1);
        id
    }

    /// Whether the host claims `line`, the current line, inside the
    /// containers outside `depth`, in the block section where the claim
    /// itself is then made. Asked of a line that would otherwise continue a
    /// paragraph lazily or start a list item. The claim is kept for that
    /// section, so the line isn't parsed twice.
    fn claims(&mut self, depth: usize, line: Line<'a>) -> bool {
        let (pos, refs) = (self.pos, self.refs.len());
        let inner = self.frames.split_off(depth);
        let level = self
            .levels
            .iter()
            .rposition(|&n| n == depth)
            .map_or(self.levels.len(), |i| i + 1);
        let deeper = self.levels.split_off(level);
        let at = self.at();
        let claim = self.claim(line);
        let claimed = claim.is_some();
        if let Some(claim) = claim {
            self.probed = Some(Probed {
                at,
                claim,
                pos: self.pos,
                refs: self.refs.split_off(refs),
            });
        }
        self.levels.extend(deeper);
        self.frames.extend(inner);
        self.pos = pos;
        self.refs.truncate(refs);
        claimed
    }

    /// A paragraph, or a setext heading. Link reference definitions at its
    /// start are taken out. A host claim ends it.
    fn paragraph(&mut self, first: Line<'a>) -> (Vec<NodeId>, bool) {
        let start = self.pos;
        let mut lines = vec![first.trim_start().rest];
        self.pos += 1;
        let mut claim = None;
        let mut level = None;
        loop {
            let line = match self.peek() {
                Some(Strip::Open(line)) if !line.is_blank() => {
                    if let Some(c) = self.claim(line) {
                        claim = Some(c);
                        break;
                    }
                    if let Some(l) = setext(line) {
                        // An underline under definitions alone is not one.
                        if !link_refs(&lines.join("\n")).1.is_empty() {
                            self.pos += 1;
                            level = Some(l);
                        }
                        break;
                    }
                    if starts_block(line, true) || self.table_start(line).is_some() {
                        break;
                    }
                    line
                }
                // A lazy continuation line, unless the host claims it where
                // it is.
                Some(Strip::Closed(depth, line))
                    if !line.is_blank()
                        && !starts_block(line, false)
                        && !self.claims(depth, line) =>
                {
                    line
                }
                _ => break,
            };
            lines.push(line.trim_start().rest);
            self.pos += 1;
        }
        let joined = lines.join("\n");
        let (refs, text) = link_refs(&joined);
        self.refs.extend(refs);
        // The text starts on the line after the definitions.
        let start = start + joined[..joined.len() - text.len()].matches('\n').count();
        let text = text.trim_end_matches([' ', '\t']);
        let kind = match level {
            Some(level) => BlockKind::Heading { level },
            None if text.is_empty() => {
                return match claim {
                    Some(claim) => (vec![claim.id], claim.ends_blank),
                    None => (Vec::new(), false),
                };
            }
            None => BlockKind::Paragraph,
        };
        let inlines = self.inlines_in(text);
        let id = self.add(kind, Content::Inlines(inlines));
        self.tree.lines.insert(id, start + 1);
        then_claimed(id, claim)
    }
}

/// `id`, then the claim that ended it, if any.
fn then_claimed(id: NodeId, claim: Option<Claim>) -> (Vec<NodeId>, bool) {
    match claim {
        Some(claim) => (vec![id, claim.id], claim.ends_blank),
        None => (vec![id], false),
    }
}

/// Every mapping key in `v` is a string.
fn string_keys(v: &serde_norway::Value) -> bool {
    match v {
        serde_norway::Value::Mapping(map) => {
            map.iter().all(|(k, v)| k.is_string() && string_keys(v))
        }
        serde_norway::Value::Sequence(seq) => seq.iter().all(string_keys),
        serde_norway::Value::Tagged(tagged) => string_keys(&tagged.value),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use maud::{Markup, html};
    use nom::branch::alt;
    use nom::bytes::complete::tag;
    use nom::character::complete::{alpha1, digit1, space0};
    use nom::combinator::{eof, rest, value};
    use nom::sequence::preceded;

    use super::*;
    use crate::draw::{DrawCtx, DrawCx, Element, draw_blocks};
    use crate::parse::{Never, Plain, parse};
    use crate::template::DefaultTemplate;
    use crate::tree::Parsed;

    fn draw<P: Host>(host: &P, md: &str) -> String {
        let doc = parse(host, md);
        draw_blocks(&doc.tree, &doc.body, &DrawCx::default()).into_string()
    }

    fn plain(md: &str) -> String {
        draw(&Plain, md)
    }

    /// Drawn with each block's source line.
    fn lined<P: Host>(host: &P, md: &str) -> String {
        let doc = parse(host, md);
        let cx = DrawCx {
            lines: true,
            ..DrawCx::default()
        };
        draw_blocks(&doc.tree, &doc.body, &cx).into_string()
    }

    #[test]
    fn blocks_carry_their_source_line_through_containers() {
        // Lines count from the top of the file, frontmatter included.
        let md = "---\ntitle: x\n---\n# H\n\npara\ncont\n\n- a\n  - b\n    c\n- d\n\n\
                  > q\n> > r\n>\n> - s\n>   t\n\n```rust\nx\n```\n\n\
                  | a | b |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |\n\n<div>\nx\n</div>\n\n\
                  [r]: /u\nafter\n\n***\n    code\n\nSet\n===\n-\n  late\n1. [ ] task\n\
                  > lazy\nline\n";
        assert_eq!(
            lined(&Plain, md),
            "<h1 data-line=\"4\">H</h1>\n\
             <p data-line=\"6\">para<br />\n\
             cont</p>\n\
             <ul data-line=\"9\">\n\
             <li data-line=\"9\">a\n\
             <ul data-line=\"10\">\n\
             <li data-line=\"10\">b<br />\n\
             c</li>\n\
             </ul>\n\
             </li>\n\
             <li data-line=\"12\">d</li>\n\
             </ul>\n\
             <blockquote data-line=\"14\">\n\
             <p data-line=\"14\">q</p>\n\
             <blockquote data-line=\"15\">\n\
             <p data-line=\"15\">r</p>\n\
             </blockquote>\n\
             <ul data-line=\"17\">\n\
             <li data-line=\"17\">s<br />\n\
             t</li>\n\
             </ul>\n\
             </blockquote>\n\
             <pre data-line=\"20\"><code class=\"language-rust\">x\n\
             </code></pre>\n\
             <table data-line=\"24\">\n\
             <thead>\n\
             <tr data-line=\"24\">\n\
             <th>a</th>\n\
             <th>b</th>\n\
             </tr>\n\
             </thead>\n\
             <tbody>\n\
             <tr data-line=\"26\">\n\
             <td>1</td>\n\
             <td>2</td>\n\
             </tr>\n\
             <tr data-line=\"27\">\n\
             <td>3</td>\n\
             <td>4</td>\n\
             </tr>\n\
             </tbody>\n\
             </table>\n\
             <div data-line=\"29\">\n\
             x\n\
             </div>\n\
             <p data-line=\"34\">after</p>\n\
             <hr data-line=\"36\" />\n\
             <pre data-line=\"37\"><code>code\n\
             </code></pre>\n\
             <h1 data-line=\"39\">Set</h1>\n\
             <ul data-line=\"41\">\n\
             <li data-line=\"41\">late</li>\n\
             </ul>\n\
             <ol data-line=\"43\" class=\"contains-task-list\">\n\
             <li data-line=\"43\" class=\"task-list-item\" data-task=\" \">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" /> task</li>\n\
             </ol>\n\
             <blockquote data-line=\"44\">\n\
             <p data-line=\"44\">lazy<br />\n\
             line</p>\n\
             </blockquote>\n"
        );
        // Host claims, and the library's blocks inside them.
        assert_eq!(
            lined(
                &TestHost,
                "para\n!! banner\n> :::note\n> x\n> !! y\n> :::\n- [ ] t\n"
            ),
            "<p data-line=\"1\">para</p>\n\
             <header data-line=\"2\">banner</header>\n\
             <blockquote data-line=\"3\">\n\
             <aside data-line=\"3\" class=\"note\">\n\
             <p data-line=\"4\">x</p>\n\
             <header data-line=\"5\">y</header>\n\
             </aside>\n\
             </blockquote>\n\
             <p data-line=\"7\" class=\"task\">t</p>\n"
        );
        // A callout and its title are on the header's line.
        assert_eq!(
            no_svg(&lined(&Plain, "p\n\n> [!warning]- T\n> x\n>\n> y\n")),
            "<p data-line=\"1\">p</p>\n\
             <details data-line=\"3\" class=\"markdown-alert markdown-alert-warning\">\n\
             <summary data-line=\"3\" class=\"markdown-alert-title\"><svg/><span>T</span></summary>\n\
             <p data-line=\"4\">x</p>\n\
             <p data-line=\"6\">y</p>\n\
             </details>\n"
        );
        // Frontmatter that isn't well formed is a code block on line 1.
        assert_eq!(
            lined(&Plain, "---\n- bad\n---\nbody\n"),
            "<pre data-line=\"1\"><code class=\"language-yaml\">- bad\n\
             </code></pre>\n\
             <p data-line=\"4\">body</p>\n"
        );
        // Without lines (the spec harness), nothing changes.
        assert_eq!(
            plain("# H\n\n- a\n"),
            "<h1>H</h1>\n<ul>\n<li>a</li>\n</ul>\n"
        );
    }

    #[test]
    fn containers_strip_their_prefixes_and_nest() {
        assert_eq!(
            plain("> a\n> > b\n> - c\n>   d\n- e\n  > f\n\t> g\n"),
            "<blockquote>\n<p>a</p>\n<blockquote>\n<p>b</p>\n</blockquote>\n\
             <ul>\n<li>c<br />\nd</li>\n</ul>\n</blockquote>\n\
             <ul>\n<li>e\n<blockquote>\n<p>f<br />\ng</p>\n</blockquote>\n</li>\n</ul>\n"
        );
        // A lazy continuation line continues the paragraph, not the quote.
        assert_eq!(
            plain("> a\nb\n> c\n\nd\n"),
            "<blockquote>\n<p>a<br />\nb<br />\nc</p>\n</blockquote>\n<p>d</p>\n"
        );
        assert_eq!(
            plain("> a\n---\n"),
            "<blockquote>\n<p>a</p>\n</blockquote>\n<hr />\n"
        );
    }

    /// `html` with each `<svg …>…</svg>` as `<svg/>`.
    fn no_svg(html: &str) -> String {
        let mut out = String::new();
        let mut rest = html;
        while let Some(at) = rest.find("<svg") {
            out.push_str(&rest[..at]);
            out.push_str("<svg/>");
            let end = rest[at..].find("</svg>").expect("closed svg");
            rest = &rest[at + end + "</svg>".len()..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn callouts_are_quotes_with_a_header_line() {
        let callout = |md: &str| no_svg(&plain(md));
        // GitHub's alerts, as GitHub draws them; the type is case-insensitive.
        assert_eq!(
            callout("> [!NOTE]\n> Read *this*.\n"),
            "<div class=\"markdown-alert markdown-alert-note\">\n\
             <p class=\"markdown-alert-title\"><svg/>Note</p>\n\
             <p>Read <em>this</em>.</p>\n</div>\n"
        );
        for kind in ["tip", "Important", "WARNING", "caution"] {
            let lower = kind.to_lowercase();
            assert!(
                callout(&format!("> [!{kind}]\n> x\n")).starts_with(&format!(
                    "<div class=\"markdown-alert markdown-alert-{lower}\">\n"
                )),
                "{kind}"
            );
        }
        // An Obsidian type keeps its type beside the alert it is drawn as,
        // and its title is inline content.
        assert_eq!(
            callout("> [!Info] A **bold** [[Title]]\n> body\n>\n> - item\n"),
            "<div class=\"markdown-alert markdown-alert-note\" data-callout=\"info\">\n\
             <p class=\"markdown-alert-title\"><svg/><span>A <strong>bold</strong> \
             <a href=\"Title\" class=\"wikilink\">Title</a></span></p>\n\
             <p>body</p>\n<ul>\n<li>item</li>\n</ul>\n</div>\n"
        );
        // Foldable: `+` starts open, `-` closed.
        assert_eq!(
            callout("> [!faq]- Why?\n> Because.\n"),
            "<details class=\"markdown-alert markdown-alert-warning\" data-callout=\"faq\">\n\
             <summary class=\"markdown-alert-title\"><svg/><span>Why?</span></summary>\n\
             <p>Because.</p>\n</details>\n"
        );
        assert_eq!(
            callout("> [!tip]+\n"),
            "<details class=\"markdown-alert markdown-alert-tip\" open=\"\">\n\
             <summary class=\"markdown-alert-title\"><svg/>Tip</summary>\n</details>\n"
        );
        // An unknown type is drawn as a note, titled with its type.
        assert_eq!(
            callout("> [!my-type]\n"),
            "<div class=\"markdown-alert markdown-alert-note\" data-callout=\"my-type\">\n\
             <p class=\"markdown-alert-title\"><svg/>My-type</p>\n</div>\n"
        );
        // Callouts nest, and sit in other containers.
        assert_eq!(
            callout("> [!question] Outer\n> > [!bug] Inner\n> > in\n> out\n"),
            "<div class=\"markdown-alert markdown-alert-warning\" data-callout=\"question\">\n\
             <p class=\"markdown-alert-title\"><svg/><span>Outer</span></p>\n\
             <div class=\"markdown-alert markdown-alert-caution\" data-callout=\"bug\">\n\
             <p class=\"markdown-alert-title\"><svg/><span>Inner</span></p>\n\
             <p>in<br />\nout</p>\n</div>\n</div>\n"
        );
        assert_eq!(
            callout("- > [!example]\n  > x\n"),
            "<ul>\n<li><div class=\"markdown-alert markdown-alert-important\" \
             data-callout=\"example\">\n\
             <p class=\"markdown-alert-title\"><svg/>Example</p>\n<p>x</p>\n</div>\n</li>\n</ul>\n"
        );
        // The body is blocks from the line after the header, so the header
        // isn't a paragraph the next line continues.
        assert_eq!(
            callout("> [!note]\n> 2. two\n"),
            "<div class=\"markdown-alert markdown-alert-note\">\n\
             <p class=\"markdown-alert-title\"><svg/>Note</p>\n\
             <ol start=\"2\">\n<li>two</li>\n</ol>\n</div>\n"
        );
        assert_eq!(
            callout("> [!note] T\nnext\n"),
            "<div class=\"markdown-alert markdown-alert-note\">\n\
             <p class=\"markdown-alert-title\"><svg/><span>T</span></p>\n</div>\n<p>next</p>\n"
        );
    }

    #[test]
    fn not_callouts() {
        for md in [
            "> [!note]x\n",
            "> [!]\n",
            "> [! note]\n",
            "> [!note\n",
            "> [!note]: /url\n> x\n",
            "> text\n> [!note]\n",
            "[!note] text\n",
            ">     [!note]\n",
            "    > [!note]\n",
        ] {
            assert!(!plain(md).contains("markdown-alert"), "{md}");
        }
        // The host is offered the header line first.
        assert_eq!(
            host("> [!banner] mine\n> x\n"),
            "<blockquote>\n<header>mine</header>\n<p>x</p>\n</blockquote>\n"
        );
    }

    #[test]
    fn lists_ordered_tight_loose_and_tasks() {
        assert_eq!(
            plain("3. a\n4. b\n\n- [ ] t\n- [x] u\n\n* x\n\n* y\n  1) n\n"),
            "<ol start=\"3\">\n<li>a</li>\n<li>b</li>\n</ol>\n\
             <ul class=\"contains-task-list\">\n\
             <li class=\"task-list-item\" data-task=\" \">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" /> t</li>\n\
             <li class=\"task-list-item\" data-task=\"x\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" checked=\"\" \
             disabled=\"\" /> u</li>\n</ul>\n\
             <ul>\n<li><p>x</p>\n</li>\n<li><p>y</p>\n<ol>\n<li>n</li>\n</ol>\n</li>\n</ul>\n"
        );
        // Any one character but a bracket or whitespace other than a space is a
        // status.
        let statuses = |md: &str| {
            let html = plain(md);
            html.split("data-task=\"")
                .skip(1)
                .map(|rest| rest.split('"').next().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            statuses("- [X] a\n- [/] b\n- [-] c\n- [>] d\n- [é] e\n"),
            ["x", "/", "-", "&gt;", "é"]
        );
        assert_eq!(
            statuses("- [ab] a\n- [] b\n- [\t] c\n- []] d\n- [/]\n- [-]x\n"),
            Vec::<String>::new()
        );
        // An ordered list must start at 1 to interrupt a paragraph.
        assert_eq!(plain("a\n2. b\n"), "<p>a<br />\n2. b</p>\n");
        assert_eq!(plain("a\n1. b\n"), "<p>a</p>\n<ol>\n<li>b</li>\n</ol>\n");
    }

    #[test]
    fn leaf_blocks() {
        assert_eq!(
            plain(
                "# One #\nSet\n===\n***\n    code\n\n      more\n```rust x\nfn f() {}\n```\n~~~\nopen"
            ),
            "<h1>One</h1>\n<h1>Set</h1>\n<hr />\n<pre><code>code\n\n  more\n</code></pre>\n\
             <pre><code class=\"language-rust\">fn f() {}\n</code></pre>\n\
             <pre><code>open\n</code></pre>\n"
        );
        assert_eq!(
            plain("| a | b \\| c |\n|:-|-:|\n| 1 |\nnext | x | y\n\npara\n"),
            "<table>\n<thead>\n<tr>\n<th align=\"left\">a</th>\n<th align=\"right\">b | c</th>\n\
             </tr>\n</thead>\n<tbody>\n<tr>\n<td align=\"left\">1</td>\n<td align=\"right\"></td>\n\
             </tr>\n<tr>\n<td align=\"left\">next</td>\n<td align=\"right\">x</td>\n</tr>\n\
             </tbody>\n</table>\n<p>para</p>\n"
        );
        // A table can interrupt a paragraph; `---` under a line is a heading.
        assert_eq!(
            plain("p\n| a |\n| - |\nq\n---\n"),
            "<p>p</p>\n<table>\n<thead>\n<tr>\n<th>a</th>\n</tr>\n</thead>\n<tbody>\n\
             <tr>\n<td>q</td>\n</tr>\n</tbody>\n</table>\n<hr />\n"
        );
    }

    #[test]
    fn link_reference_definitions_are_collected_not_drawn() {
        let md = "[a]: /url \"t\"\n[B  c]:\n  <x y>\npara\n\n[d]: /e 'bad' junk\n";
        let mut s = BlockStream::new(&Plain, md.lines().collect());
        s.document();
        let refs: Vec<_> = s
            .refs
            .iter()
            .map(|r| (r.label.as_str(), r.dest.as_str(), r.title.as_deref()))
            .collect();
        assert_eq!(refs, [("A", "/url", Some("t")), ("B C", "x y", None)]);
        assert_eq!(plain(md), "<p>para</p>\n<p>[d]: /e 'bad' junk</p>\n");
    }

    #[test]
    fn html_blocks_end_at_their_closing_tag() {
        // Blank lines don't end it; markdown after the closing tag is parsed.
        assert_eq!(
            plain("<div class=\"x\">\n\n*x*\n\n</div>\nafter\n"),
            "<div class=\"x\">\n\n*x*\n\n</div>\n<p>after</p>\n"
        );
        // Nested elements of the same name are counted.
        assert_eq!(
            plain("<div>\n<div>\n</div>\ninner\n</DIV>\n# after\n"),
            "<div>\n<div>\n</div>\ninner\n</DIV>\n<h1>after</h1>\n"
        );
        // One that closes on its own line.
        assert_eq!(plain("<p>one</p>\ntwo\n"), "<p>one</p>\n<p>two</p>\n");
        // No closing tag in reach: CommonMark's blank line rule.
        assert_eq!(plain("<div>\na\n\nb\n"), "<div>\na\n<p>b</p>\n");
        assert_eq!(
            plain("> <div>\n> a\n\n</div>\n"),
            "<blockquote>\n<div>\na\n</blockquote>\n</div>\n"
        );
        // Other kinds keep their CommonMark end conditions.
        assert_eq!(
            plain("<!--\n\nx\n-->\nafter\n"),
            "<!--\n\nx\n-->\n<p>after</p>\n"
        );
        assert_eq!(plain("<span>\na\n\nb\n"), "<span>\na\n<p>b</p>\n");
    }

    #[test]
    fn a_table_with_no_blank_lines_is_followed_by_markdown() {
        // The shape of a Notion export: an HTML table with no blank lines around it.
        let md = "## 2. Browse people\n\
                  The layout has four parts:\n\
                  <table header-row=\"true\">\n\
                  <colgroup>\n\
                  <col>\n\
                  <col width=\"463\">\n\
                  </colgroup>\n\
                  <tr>\n\
                  <td>Part</td>\n\
                  </tr>\n\
                  </table>\n\
                  #### Sections\n\
                  - one\n\
                  - two\n";
        assert_eq!(
            plain(md),
            "<h2>2. Browse people</h2>\n<p>The layout has four parts:</p>\n\
             <table header-row=\"true\">\n<colgroup>\n<col>\n<col width=\"463\">\n</colgroup>\n\
             <tr>\n<td>Part</td>\n</tr>\n</table>\n\
             <h4>Sections</h4>\n<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n"
        );
    }

    #[test]
    fn frontmatter_is_parsed_when_well_formed() {
        let doc = parse(&Plain, "---\ntitle: Hi\ntags: [a]\n---\n# H\n");
        assert_eq!(
            doc.frontmatter,
            Some(serde_json::json!({"title": "Hi", "tags": ["a"]}))
        );
        assert_eq!(
            draw_blocks(&doc.tree, &doc.body, &DrawCx::default()).into_string(),
            "<h1>H</h1>\n"
        );
        // Not at the start, or not closed: no frontmatter.
        assert_eq!(parse(&Plain, "x\n---\na: b\n---\n").frontmatter, None);
        assert_eq!(plain("---\na: b\n"), "<hr />\n<p>a: b</p>\n");
    }

    #[test]
    fn frontmatter_not_well_formed_is_a_yaml_code_block() {
        for yaml in [
            "a: [b\n",
            "- a\n- b\n",
            "just text\n",
            "1: a\n",
            "a:\n  2: b\n",
        ] {
            let md = format!("---\n{yaml}---\nbody\n");
            let doc = parse(&Plain, &md);
            assert_eq!(doc.frontmatter, None, "{yaml:?}");
            assert_eq!(
                draw_blocks(&doc.tree, &doc.body, &DrawCx::default()).into_string(),
                format!("<pre><code class=\"language-yaml\">{yaml}</code></pre>\n<p>body</p>\n"),
                "{yaml:?}"
            );
        }
    }

    struct TestHost;

    enum TestBlock {
        /// `!! text`: one line, its text inline.
        Banner,
        /// `:::kind` … `:::`: blocks.
        Callout(String),
        /// `- [ ] text`: one line.
        Task { done: bool, text: String },
    }

    impl Element<TestHost> for TestBlock {
        fn draw(&self, cx: &DrawCtx<'_, TestHost>) -> Markup {
            match self {
                TestBlock::Banner => html! { header { (cx.draw_children()) } "\n" },
                TestBlock::Callout(kind) => {
                    html! { aside class=(kind) { "\n" (cx.draw_children()) } "\n" }
                }
                TestBlock::Task { done, text } => html! { p.task.done[*done] { (text) } "\n" },
            }
        }
    }

    impl Host for TestHost {
        type Block = TestBlock;
        type Inline = Never;
        type Template = DefaultTemplate;

        fn parse_block(&self, s: &mut BlockStream<'_, Self>) -> Option<Parsed<BlockKind<Self>>> {
            if let Some(text) = s.try_parse(preceded(alt((tag("!! "), tag("[!banner] "))), rest)) {
                let text = s.inlines_in(text);
                return Some(s.node(BlockKind::Host(TestBlock::Banner), Content::Inlines(text)));
            }
            if let Some(kind) = s.try_parse(preceded(tag(":::"), alpha1)) {
                let body = s.blocks_until((tag(":::"), eof))?;
                let callout = TestBlock::Callout(kind.to_string());
                return Some(s.node(BlockKind::Host(callout), Content::Blocks(body)));
            }
            let check = alt((value(false, tag(" ")), value(true, tag("x"))));
            if let Some((_, _, done, _)) = s.try_parse((space0, tag("- ["), check, tag("] "))) {
                let text = s.line().to_string();
                let task = TestBlock::Task { done, text };
                return Some(s.node(BlockKind::Host(task), Content::Leaf));
            }
            None
        }
    }

    fn host(md: &str) -> String {
        draw(&TestHost, md)
    }

    #[test]
    fn host_blocks_interrupt_paragraphs() {
        assert_eq!(
            host("para\n!! banner\nmore\n"),
            "<p>para</p>\n<header>banner</header>\n<p>more</p>\n"
        );
    }

    #[test]
    fn host_blocks_inside_containers_and_nesting_through_the_library() {
        assert_eq!(
            host("> a\n> !! b\n> :::note\n> > x\n> !! y\n> :::\n> c\n"),
            "<blockquote>\n<p>a</p>\n<header>b</header>\n\
             <aside class=\"note\">\n<blockquote>\n<p>x</p>\n</blockquote>\n<header>y</header>\n</aside>\n\
             <p>c</p>\n</blockquote>\n"
        );
        // No end in reach: the hook's `None` rewinds, and the built-ins run.
        assert_eq!(
            host("> :::note\n> x\n:::\n"),
            "<blockquote>\n<p>:::note<br />\nx<br />\n:::</p>\n</blockquote>\n"
        );
        // Not offered lines inside a fenced code block.
        assert_eq!(host("```\n!! x\n```\n"), "<pre><code>!! x\n</code></pre>\n");
    }

    #[test]
    fn a_claimed_list_marker_line_is_a_block_that_ends_the_list() {
        assert_eq!(
            host("- plain\n- [ ] one\n- [x] two\n* other\n"),
            "<ul>\n<li>plain</li>\n</ul>\n<p class=\"task\">one</p>\n\
             <p class=\"task done\">two</p>\n<ul>\n<li>other</li>\n</ul>\n"
        );
        // No list is opened for a claim, after a paragraph or in an item.
        assert_eq!(
            host("para\n- [ ] t\n- u\n"),
            "<p>para</p>\n<p class=\"task\">t</p>\n<ul>\n<li>u</li>\n</ul>\n"
        );
        assert_eq!(
            host("- a\n\n  - [ ] t\n"),
            "<ul>\n<li><p>a</p>\n<p class=\"task\">t</p>\n</li>\n</ul>\n"
        );
    }

    #[test]
    fn containers_past_the_nesting_limit_are_text() {
        // Each `>` past the limit stays in the paragraph's text.
        let quotes = format!("{} hi\n", ">".repeat(5000));
        let html = plain(&quotes);
        assert_eq!(html.matches("<blockquote>").count(), MAX_NESTING - 1);
        assert!(html.contains(&format!("<p>{} hi</p>", "&gt;".repeat(5001 - MAX_NESTING))));
        // List items past the limit are the innermost item's text.
        let items: String = (0..1000)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        let html = plain(&items);
        assert_eq!(html.matches("<li>").count(), MAX_NESTING - 1);
        assert_eq!(html.matches("- x").count(), 1001 - MAX_NESTING);
        // A host's `blocks_until` is refused at the limit.
        let callout = |n: usize| {
            let q = "> ".repeat(n);
            host(&format!("{q}:::note\n{q}x\n{q}:::\n"))
        };
        assert!(callout(MAX_NESTING - 2).contains("<aside"));
        assert!(!callout(MAX_NESTING - 1).contains("<aside"));
    }

    struct NestHost;

    thread_local! {
        /// Lines offered to [`NestHost`] on this thread.
        static OFFERED: Cell<usize> = const { Cell::new(0) };
    }

    /// `md` drawn with [`NestHost`], and the lines offered to it.
    fn draw_counted(md: &str) -> (String, usize) {
        OFFERED.set(0);
        let html = draw(&NestHost, md);
        (html, OFFERED.get())
    }

    /// `:::nK` … `:::/nK` or `- !openK` … `- !closeK`: blocks, each with its
    /// own closer, so they nest.
    struct Nest;

    impl Element<NestHost> for Nest {
        fn draw(&self, cx: &DrawCtx<'_, NestHost>) -> Markup {
            html! { section { "\n" (cx.draw_children()) } "\n" }
        }
    }

    impl Host for NestHost {
        type Block = Nest;
        type Inline = Never;
        type Template = DefaultTemplate;

        fn parse_block(&self, s: &mut BlockStream<'_, Self>) -> Option<Parsed<BlockKind<Self>>> {
            OFFERED.set(OFFERED.get() + 1);
            for (open, close) in [(":::n", ":::/n"), ("- !open", "- !close")] {
                if let Some(k) = s.try_parse(preceded(tag(open), digit1)) {
                    let body = s.blocks_until((tag(close), tag(k), eof))?;
                    return Some(s.node(BlockKind::Host(Nest), Content::Blocks(body)));
                }
            }
            None
        }
    }

    #[test]
    fn a_probed_line_is_claimed_once() {
        // The line after a list item, or after a quote's last line, is
        // probed and then claimed. Parsing a nested claim twice at each
        // level would double the work per level.
        let nested = |n: usize, before: &str, open: &str, close: &str| {
            let opens: String = (0..n).map(|k| format!("{before}\n{open}{k}\n")).collect();
            let closes: String = (0..n).rev().map(|k| format!("{close}{k}\n")).collect();
            format!("{opens}x\n{closes}")
        };
        for (before, open, close) in [("- a", "- !open", "- !close"), ("> a", ":::n", ":::/n")] {
            let (_, fewer) = draw_counted(&nested(12, before, open, close));
            let (html, more) = draw_counted(&nested(13, before, open, close));
            assert!(2 * more <= 3 * fewer, "{fewer} offers, then {more}");
            assert_eq!(html.matches("<section>").count(), 13, "{html}");
            assert!(html.contains("<section>\n<p>x</p>\n</section>"), "{html}");
        }
    }
}
