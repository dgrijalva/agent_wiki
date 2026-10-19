//! The host's hook into parsing and drawing, and the parser: a block phase,
//! then an inline phase.

mod block;
mod inline;
mod stream;

use maud::Markup;

pub use self::stream::{BlockStream, InlineStream};
use crate::document::Document;
use crate::draw::{DrawChildren, DrawCtx, Element};
use crate::template::{DefaultTemplate, Template};
use crate::tree::{BlockKind, Children, InlineKind, Parsed};

/// A host: its block and inline element types and its page template.
/// Element types are `Send + Sync` because a parsed page is held across the
/// page handler's awaits.
pub trait Host: Sized + Send + Sync + 'static {
    type Block: Element<Self> + Send + Sync;
    type Inline: Element<Self> + Send + Sync;
    type Template: Template<Self>;

    /// Called at the start of every line, including inside containers and
    /// mid-paragraph (a claim ends the paragraph), but not inside a fenced
    /// code block, an HTML block or frontmatter, not on an indented code
    /// block's continuation lines, and not on a table's delimiter row.
    /// `None` = not mine: the library rewinds the stream and tries its
    /// built-ins.
    ///
    /// A line can be offered more than once. A line that would continue a
    /// paragraph lazily, and a list item's line after the first item, are
    /// first offered as a probe. A claim there is kept, and is the block
    /// where the line is then parsed; on `None`, the line may be offered
    /// again there. A claim on a line that starts with a list marker is an
    /// ordinary block in place of that line: it ends the list.
    fn parse_block(&self, s: &mut BlockStream<'_, Self>) -> Option<Parsed<BlockKind<Self>>> {
        let _ = s;
        None
    }

    /// Called at each position in an inline section, after escapes and
    /// code spans, before the built-ins. Same contract: `None` = not mine,
    /// and the library rewinds the stream. The hook is pure, so a `None` is
    /// remembered: the position isn't offered again at the same nesting.
    fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
        let _ = s;
        None
    }
}

/// How deep the parser nests block containers (quotes, list items, a
/// host's `blocks_until`), inline sections (link text, a host's
/// `inlines_until` or `inlines_in`) and emphasis within a section. Past it,
/// the syntax is text, so that pathological input can't overflow the stack.
const MAX_NESTING: usize = 32;

/// Parses `source` with `host`'s hooks.
pub(crate) fn parse<P: Host>(host: &P, source: &str) -> Document<P> {
    let mut s = BlockStream::new(host, source.lines().collect());
    let (frontmatter, body) = s.document();
    inline::inlines(host, &mut s.tree, &s.refs);
    Document {
        frontmatter,
        tree: s.tree,
        body: Children::new(body),
    }
}

/// The default: a wiki with no host syntax.
pub struct Plain;

/// A host element type with no values, for hosts that add no blocks or no
/// inlines.
pub enum Never {}

impl Host for Plain {
    type Block = Never;
    type Inline = Never;
    type Template = DefaultTemplate;
}

impl<P: Host> Element<P> for Never {
    fn draw(&self, _: &DrawCtx<'_>, _: DrawChildren<'_, P>) -> Markup {
        match *self {}
    }
}

#[cfg(test)]
mod spec_tests;
