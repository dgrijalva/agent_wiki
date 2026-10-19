//! The streams a host parses from. They hold `&P`, so nested parsing goes
//! back through the host first.

use std::collections::{HashMap, HashSet};

use indextree::NodeId;

use super::block::{Frame, Line, LinkRef, Strip};
use super::{Host, MAX_NESTING};
use crate::tree::{BlockKind, Children, Content, InlineKind, Parsed, Tree};

/// The block phase's input, one line at a time, with container prefixes
/// (`> `, list indentation) stripped. A host's [`Host::parse_block`] reads
/// the line it was offered from here and builds its node with it.
pub struct BlockStream<'a, P: Host> {
    pub(super) host: &'a P,
    pub(super) lines: Vec<&'a str>,
    pub(super) tree: Tree<P>,
    /// The open containers, outermost first.
    pub(super) frames: Vec<Frame>,
    /// Block sections open around the position, outermost first: the
    /// document's, and one per container or `blocks_until`. Each holds the
    /// number of containers open in it.
    pub(super) levels: Vec<usize>,
    /// The line being parsed.
    pub(super) pos: usize,
    /// Lines from here on are out of reach: the end of a `blocks_until`.
    pub(super) limit: usize,
    /// While a hook runs: what is left of line `pos`.
    cursor: Option<Cursor<'a>>,
    pub(super) refs: Vec<LinkRef>,
    /// A probe's claim, kept for the claim that follows it.
    pub(super) probed: Option<Probed>,
}

struct Cursor<'a> {
    rest: &'a str,
    /// The line belongs to the claim: it is the line the hook was offered,
    /// or `try_parse` consumed some of it.
    touched: bool,
}

/// A probe's claim of a line: where it was made, and what it did.
pub(super) struct Probed {
    pub(super) at: At,
    pub(super) claim: Claim,
    /// The line after the claim.
    pub(super) pos: usize,
    /// Link reference definitions inside the claim.
    pub(super) refs: Vec<LinkRef>,
}

/// Where a line is offered: the line, the containers and block sections
/// open around it, and the limit. A pure host claims it the same way each
/// time.
pub(super) type At = (usize, usize, usize, usize);

/// A line the host claimed.
pub(super) struct Claim {
    pub(super) id: NodeId,
    /// The claim ended with a blank line.
    pub(super) ends_blank: bool,
}

impl<'a, P: Host> BlockStream<'a, P> {
    pub(super) fn new(host: &'a P, lines: Vec<&'a str>) -> Self {
        BlockStream {
            host,
            limit: lines.len(),
            lines,
            tree: Tree::new(),
            frames: Vec::new(),
            levels: Vec::new(),
            pos: 0,
            cursor: None,
            refs: Vec::new(),
            probed: None,
        }
    }

    /// Run a nom parser at the current position in the line; advance on
    /// success.
    pub fn try_parse<O>(
        &mut self,
        mut p: impl nom::Parser<&'a str, Output = O, Error = nom::error::Error<&'a str>>,
    ) -> Option<O> {
        let cursor = self.cursor.as_mut()?;
        let (rest, out) = p.parse(cursor.rest).ok()?;
        cursor.rest = rest;
        cursor.touched = true;
        Some(out)
    }

    /// What is left of the current line: container prefixes (`> `, list
    /// indentation) stripped, and whatever `try_parse` consumed.
    pub fn line(&self) -> &'a str {
        self.cursor.as_ref().map_or("", |c| c.rest)
    }

    /// Parse blocks (host first, then built-ins) from the next line until
    /// `end` matches at a line start; consumes `end`'s line. `None` when the
    /// enclosing containers end first, or past the nesting limit.
    pub fn blocks_until<O>(
        &mut self,
        mut end: impl nom::Parser<&'a str, Output = O, Error = nom::error::Error<&'a str>>,
    ) -> Option<Children<BlockKind<P>>> {
        if self.levels.len() >= MAX_NESTING {
            return None;
        }
        let from = self.next_line();
        let stop = (from..self.limit).find_map(|i| match self.strip(i) {
            Strip::Open(line) => end.parse(line.rest).is_ok().then_some(Some(i)),
            Strip::Closed(..) => Some(None),
        })??;
        let limit = std::mem::replace(&mut self.limit, stop);
        self.pos = from;
        let body = self.blocks();
        self.limit = limit;
        self.move_to(stop + 1);
        Some(Children::new(body.ids))
    }

    /// Parse `text` as an inline section, e.g. a host block's title line.
    /// The inlines are parsed once the block phase is done, so that link
    /// reference definitions anywhere in the document are known.
    pub fn inlines_in(&mut self, text: &str) -> Children<InlineKind<P>> {
        self.tree.section(text)
    }

    pub fn node(&mut self, kind: BlockKind<P>, content: Content<P>) -> Parsed<BlockKind<P>> {
        self.tree.block(kind, content)
    }

    /// The first line a nested parse starts at: the current line, unless it
    /// belongs to the claim already.
    fn next_line(&self) -> usize {
        match &self.cursor {
            Some(Cursor { touched: true, .. }) => self.pos + 1,
            _ => self.pos,
        }
    }

    /// Makes line `i` the current one, untouched.
    fn move_to(&mut self, i: usize) {
        self.pos = i;
        self.cursor = match self.peek() {
            Some(Strip::Open(line)) => Some(Cursor {
                rest: line.rest,
                touched: false,
            }),
            _ => None,
        };
    }

    /// Where the current line is offered.
    pub(super) fn at(&self) -> At {
        (self.pos, self.frames.len(), self.levels.len(), self.limit)
    }

    /// Offers `line`, the current line, to the host, unless a probe just
    /// claimed it where it is. On `None` the stream is rewound.
    pub(super) fn claim(&mut self, line: Line<'a>) -> Option<Claim> {
        if let Some(p) = self.probed.take().filter(|p| p.at == self.at()) {
            self.pos = p.pos;
            self.refs.extend(p.refs);
            return Some(p.claim);
        }
        let start = self.pos;
        let refs = self.refs.len();
        let outer = self.cursor.take();
        self.cursor = Some(Cursor {
            rest: line.rest,
            touched: true,
        });
        let host = self.host;
        let parsed = host.parse_block(self);
        let touched = self.cursor.as_ref().is_some_and(|c| c.touched);
        self.cursor = outer;
        let Some(parsed) = parsed else {
            self.pos = start;
            self.refs.truncate(refs);
            return None;
        };
        self.tree.lines.insert(parsed.id, start + 1);
        if touched {
            self.pos += 1;
        }
        let ends_blank = self.pos > start + 1
            && matches!(self.strip(self.pos - 1), Strip::Open(l) if l.is_blank());
        Some(Claim {
            id: parsed.id,
            ends_blank,
        })
    }

    /// Line `pos`, through the open containers; `None` past the end.
    pub(super) fn peek(&self) -> Option<Strip<'a>> {
        (self.pos < self.limit).then(|| self.strip(self.pos))
    }

    /// Line `i` with the open containers' prefixes stripped.
    pub(super) fn strip(&self, i: usize) -> Strip<'a> {
        let mut line = Line::new(self.lines[i]);
        for (depth, frame) in self.frames.iter().enumerate() {
            match frame.strip(line, i) {
                Some(rest) => line = rest,
                None => return Strip::Closed(depth, line),
            }
        }
        Strip::Open(line)
    }
}

/// An inline section's input. A host's [`Host::parse_inline`] reads from
/// the position it was offered and builds its node with it.
pub struct InlineStream<'a, P: Host> {
    pub(super) host: &'a P,
    pub(super) tree: &'a mut Tree<P>,
    /// Link reference definitions by label, destination and title resolved.
    pub(super) refs: &'a HashMap<String, (String, Option<String>)>,
    /// The section's text.
    pub(super) src: &'a str,
    pub(super) pos: usize,
    /// Inside link, wikilink or image text: no tags or extended autolinks.
    pub(super) in_link: bool,
    /// Links built so far; a link's text can't hold another.
    pub(super) links: usize,
    /// Where a `[` or `![` was found not to start a link or image: the
    /// position, the sections open around it, and whether it was in link
    /// text. Past the nesting limit a bracket fails that might not fail
    /// shallower, so the failure only holds where it was found.
    pub(super) not_links: HashSet<(usize, usize, bool)>,
    /// Where the host returned `None`: the position, the sections open
    /// around it, and whether it was in link text. A pure host returns
    /// `None` there again, so it isn't asked again.
    pub(super) not_hosts: HashSet<(usize, usize, bool)>,
    /// For each end looked for (`]`, and raw HTML's `-->`, `?>`, `]]>` and
    /// `>`): where it was last looked for from, and where it was found.
    pub(super) found: HashMap<&'static str, (usize, Option<usize>)>,
    /// Sections open around the position.
    pub(super) depth: usize,
    /// For a text node, where in its text a `#` came from an escape or a
    /// character reference, so can't start a tag.
    pub(super) hashes: HashMap<NodeId, Vec<usize>>,
    /// The list item whose content the section starts.
    pub(super) item: Option<NodeId>,
}

impl<'a, P: Host> InlineStream<'a, P> {
    /// Run a nom parser at the current position; advance on success.
    pub fn try_parse<O>(
        &mut self,
        mut p: impl nom::Parser<&'a str, Output = O, Error = nom::error::Error<&'a str>>,
    ) -> Option<O> {
        let (rest, out) = p.parse(&self.src[self.pos..]).ok()?;
        self.pos = self.src.len() - rest.len();
        Some(out)
    }

    /// Library → host → library: parse inline content (host first, then
    /// built-ins) until `end` matches; consumes `end`. Emphasis is resolved
    /// within the returned children, so it never crosses a host boundary.
    /// `None` when the section ends first.
    pub fn inlines_until<O>(
        &mut self,
        mut end: impl nom::Parser<&'a str, Output = O, Error = nom::error::Error<&'a str>>,
    ) -> Option<Children<InlineKind<P>>> {
        let mut end = |s: &'a str| end.parse(s).ok().map(|(rest, _)| s.len() - rest.len());
        self.section(Some(&mut end)).map(Children::new)
    }

    /// Parse `text` as an inline section (host first, then built-ins), e.g.
    /// the part of the input a host element keeps as its children. Past the
    /// nesting limit, `text` is one text node.
    pub fn inlines_in(&mut self, text: &str) -> Children<InlineKind<P>> {
        let mut s = InlineStream {
            host: self.host,
            tree: &mut *self.tree,
            refs: self.refs,
            src: text,
            pos: 0,
            in_link: self.in_link,
            links: self.links,
            not_links: HashSet::new(),
            not_hosts: HashSet::new(),
            found: HashMap::new(),
            depth: self.depth,
            hashes: HashMap::new(),
            item: None,
        };
        let ids = s.section(None);
        self.links = s.links;
        let ids = ids.unwrap_or_else(|| vec![self.leaf(InlineKind::Text(text.to_string())).id]);
        Children::new(ids)
    }

    /// Whether the position is the start of a list item's
    /// content: the start of the item's first paragraph, when that begins
    /// on the marker's line. A task checkbox is read here after the host.
    pub fn at_item_start(&self) -> bool {
        self.item.is_some() && self.pos == 0 && self.depth == 1
    }

    pub fn node(
        &mut self,
        kind: InlineKind<P>,
        children: Children<InlineKind<P>>,
    ) -> Parsed<InlineKind<P>> {
        self.tree.inline(kind, children)
    }

    pub fn leaf(&mut self, kind: InlineKind<P>) -> Parsed<InlineKind<P>> {
        self.tree.leaf(kind)
    }
}
