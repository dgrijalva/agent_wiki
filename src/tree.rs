//! The document tree: node kinds, which hold no children, and an arena that
//! holds the nodes and their order. Hosts see only kinds and the opaque
//! [`Children`] and [`Parsed`] handles.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use indextree::{Arena, NodeId};

use crate::parse::Host;

/// What a block node is, without its children.
pub enum BlockKind<P: Host> {
    Paragraph,
    Heading {
        level: u8,
    },
    ThematicBreak,
    Quote,
    List {
        ordered: bool,
        /// The first item's number; only meaningful when `ordered`.
        start: u32,
        /// No blank lines between or inside the items: their paragraphs
        /// are drawn without `<p>`.
        tight: bool,
    },
    ListItem {
        /// `Some(checked)` for a task list item.
        task: Option<bool>,
    },
    Code {
        info: String,
        body: String,
    },
    Html(String),
    Table {
        /// One entry per column.
        align: Vec<Align>,
    },
    TableRow {
        header: bool,
    },
    TableCell,
    Host(P::Block),
}

/// A table column's alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// What an inline node is, without its children.
pub enum InlineKind<P: Host> {
    Text(String),
    SoftBreak,
    HardBreak,
    Strong,
    Emph,
    Strike,
    Code(String),
    Html(String),
    /// The link text is the node's children.
    Link {
        dest: String,
        title: Option<String>,
    },
    /// The alt text is the node's children.
    Image {
        dest: String,
        title: Option<String>,
    },
    /// `[[target|label]]`; `target` keeps any `#heading` suffix.
    WikiLink {
        target: String,
        label: Option<String>,
    },
    /// `![[target|alt]]`, an image embed.
    Embed {
        target: String,
        alt: Option<String>,
    },
    /// `#tag`, without the `#`, as written.
    Tag(String),
    Host(P::Inline),
}

/// A list of nodes in the library's arena. Hosts get these from the stream
/// and hand them back; they never build or open one.
pub struct Children<K> {
    pub(crate) ids: Vec<NodeId>,
    /// An inline section still to parse, as an index into the tree's
    /// sections: the inline phase parses it into the node it is given to.
    section: Option<usize>,
    kind: PhantomData<fn() -> K>,
}

impl<K> Children<K> {
    pub(crate) fn new(ids: Vec<NodeId>) -> Self {
        Children {
            ids,
            section: None,
            kind: PhantomData,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_parsed(nodes: impl IntoIterator<Item = Parsed<K>>) -> Self {
        Children::new(nodes.into_iter().map(|p| p.id).collect())
    }
}

/// What a block holds.
pub enum Content<P: Host> {
    Blocks(Children<BlockKind<P>>),
    Inlines(Children<InlineKind<P>>),
    Leaf,
}

/// A node the stream built for the host; returned from a parse hook.
pub struct Parsed<K> {
    pub(crate) id: NodeId,
    kind: PhantomData<fn() -> K>,
}

impl<K> Parsed<K> {
    fn new(id: NodeId) -> Self {
        Parsed {
            id,
            kind: PhantomData,
        }
    }
}

/// A node of a parsed document, for reading: its kind and its place in the
/// tree.
pub struct NodeRef<'a, P: Host> {
    pub(crate) tree: &'a Tree<P>,
    pub(crate) id: NodeId,
}

// Derived `Clone` and `Copy` would require `P: Clone`.
impl<P: Host> Clone for NodeRef<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Host> Copy for NodeRef<'_, P> {}

/// What a node is: a block or an inline kind.
pub enum KindRef<'a, P: Host> {
    Block(&'a BlockKind<P>),
    Inline(&'a InlineKind<P>),
}

impl<'a, P: Host> NodeRef<'a, P> {
    pub(crate) fn new(tree: &'a Tree<P>, id: NodeId) -> Self {
        NodeRef { tree, id }
    }

    pub fn kind(&self) -> KindRef<'a, P> {
        match self.tree.arena[self.id].get() {
            Node::Block(kind) => KindRef::Block(kind),
            Node::Inline(kind) => KindRef::Inline(kind),
        }
    }

    pub fn children(&self) -> impl Iterator<Item = NodeRef<'a, P>> + use<'a, P> {
        let tree = self.tree;
        self.id
            .children(&tree.arena)
            .map(move |id| NodeRef::new(tree, id))
    }

    /// This node, then every node inside it, depth first in document order.
    pub fn descendants(&self) -> impl Iterator<Item = NodeRef<'a, P>> + use<'a, P> {
        let tree = self.tree;
        self.id
            .descendants(&tree.arena)
            .map(move |id| NodeRef::new(tree, id))
    }

    pub fn parent(&self) -> Option<NodeRef<'a, P>> {
        let id = self.tree.arena[self.id].parent()?;
        Some(NodeRef::new(self.tree, id))
    }

    pub fn next_sibling(&self) -> Option<NodeRef<'a, P>> {
        let id = self.tree.arena[self.id].next_sibling()?;
        Some(NodeRef::new(self.tree, id))
    }

    /// The line a block starts on in the source file, 1-based, counting
    /// frontmatter; `None` for inlines and for blocks a host built inside
    /// its claim.
    pub fn line(&self) -> Option<usize> {
        self.tree.lines.get(&self.id).copied()
    }

    /// The plain text inside the node, as the outline reads a heading.
    pub fn text(&self) -> String {
        let mut text = String::new();
        crate::document::plain_text(self.tree, self.id, &mut text);
        text
    }
}

/// An arena entry: one block or inline node.
pub(crate) enum Node<P: Host> {
    Block(BlockKind<P>),
    Inline(InlineKind<P>),
}

/// The arena every node of one document lives in. A node's children are
/// attached when its parent is built, in order.
pub(crate) struct Tree<P: Host> {
    pub(crate) arena: Arena<Node<P>>,
    /// The block phase's inline sections, in order: the text, and the node
    /// it was given to. The inline phase parses each into its node, once
    /// every link reference definition is known.
    pub(crate) sections: Vec<(String, Option<NodeId>)>,
    /// Paragraphs that start a list item's content, on the marker's line.
    pub(crate) item_starts: HashSet<NodeId>,
    /// The line each block starts on in the source file, 1-based and
    /// counting frontmatter: every block the block phase reaches at a line
    /// start, each list item and table row, and each host claim. Blocks a
    /// host builds inside its claim with `node` have none.
    pub(crate) lines: HashMap<NodeId, usize>,
}

impl<P: Host> Tree<P> {
    pub(crate) fn new() -> Self {
        Tree {
            arena: Arena::new(),
            sections: Vec::new(),
            item_starts: HashSet::new(),
            lines: HashMap::new(),
        }
    }

    pub(crate) fn block(
        &mut self,
        kind: BlockKind<P>,
        content: Content<P>,
    ) -> Parsed<BlockKind<P>> {
        let id = self.arena.new_node(Node::Block(kind));
        let children = match content {
            Content::Blocks(children) => children.ids,
            Content::Inlines(children) => {
                if let Some(n) = children.section {
                    self.sections[n].1 = Some(id);
                }
                children.ids
            }
            Content::Leaf => Vec::new(),
        };
        self.attach(id, children);
        Parsed::new(id)
    }

    pub(crate) fn inline(
        &mut self,
        kind: InlineKind<P>,
        children: Children<InlineKind<P>>,
    ) -> Parsed<InlineKind<P>> {
        let id = self.arena.new_node(Node::Inline(kind));
        self.attach(id, children.ids);
        Parsed::new(id)
    }

    pub(crate) fn leaf(&mut self, kind: InlineKind<P>) -> Parsed<InlineKind<P>> {
        Parsed::new(self.arena.new_node(Node::Inline(kind)))
    }

    /// An inline section, parsed in the inline phase into the block it is
    /// given to.
    pub(crate) fn section(&mut self, text: &str) -> Children<InlineKind<P>> {
        self.sections.push((text.to_string(), None));
        Children {
            section: Some(self.sections.len() - 1),
            ..Children::new(Vec::new())
        }
    }

    fn attach(&mut self, parent: NodeId, children: Vec<NodeId>) {
        for child in children {
            parent.append(child, &mut self.arena);
        }
    }
}
