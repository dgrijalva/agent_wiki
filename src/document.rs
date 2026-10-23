//! A parsed document: its frontmatter and its body tree, and what one walk
//! of that tree extracts from it.

use std::collections::{HashMap, HashSet};

use indextree::NodeId;
use serde_json::Value;

use crate::draw::Element;
use crate::parse::Host;
use crate::tree::{BlockKind, Children, InlineKind, Node, NodeRef, Tree};

/// A parsed document: its frontmatter and the tree of its body. Read-only;
/// what hosts walk to extract records.
pub struct DocumentTree<P: Host> {
    /// `None` when absent or not well formed.
    pub(crate) frontmatter: Option<Value>,
    pub(crate) tree: Tree<P>,
    pub(crate) body: Children<BlockKind<P>>,
}

/// What the index stores about a document, and the ordinals its wikilinks
/// and tags are drawn with.
#[derive(Debug, Default)]
pub(crate) struct Extracted {
    pub(crate) title_from_frontmatter: Option<String>,
    pub(crate) first_heading: Option<String>,
    /// The text of the first paragraph, as the fallback summary.
    pub(crate) first_paragraph: Option<String>,
    /// Frontmatter `description`, else frontmatter `summary`.
    pub(crate) summary_from_frontmatter: Option<String>,
    /// Wikilink targets in document order, as written (a `#Heading`
    /// fragment included). None from inside images.
    pub(crate) wikilinks: Vec<String>,
    /// Inline tags in document order, lowercased. None from inside links,
    /// wikilinks or images.
    pub(crate) inline_tags: Vec<String>,
    /// Frontmatter `tags`, leading `#` stripped.
    pub(crate) frontmatter_tags: Vec<String>,
    /// Text, code and code-block literals and host elements' own text, in
    /// document order, separated by spaces: what body search indexes. No
    /// frontmatter, raw HTML or URLs.
    pub(crate) body_text: String,
    /// The ordinal of each counted wikilink and inline tag node: its index
    /// in `wikilinks` or `inline_tags`.
    pub(crate) ordinals: HashMap<NodeId, usize>,
    /// Each heading's id: its text's [`slug`], unique on the page.
    pub(crate) headings: HashMap<NodeId, String>,
    /// Every heading, in document order.
    pub(crate) outline: Vec<OutlineHeading>,
}

/// A heading as the outline lists it: a top-level one, not inside a
/// block quote or list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineHeading {
    pub level: u8,
    pub text: String,
    /// The heading's id on the page.
    pub anchor: String,
    /// The line it starts on; `None` for a heading a host built.
    pub line: Option<usize>,
}

impl Extracted {
    /// The frontmatter title, else the first heading, else `stem`.
    pub(crate) fn title(&self, stem: &str) -> String {
        self.title_from_frontmatter
            .clone()
            .or_else(|| self.first_heading.clone())
            .unwrap_or_else(|| stem.to_string())
    }

    /// The frontmatter description or summary, else the first paragraph's
    /// text, cut to [`SUMMARY_CHARS`]; empty when there is none.
    pub(crate) fn summary(&self) -> String {
        let text = self
            .summary_from_frontmatter
            .as_deref()
            .or(self.first_paragraph.as_deref())
            .unwrap_or("");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        match text.char_indices().nth(SUMMARY_CHARS) {
            Some((end, _)) => format!("{}…", text[..end].trim_end()),
            None => text,
        }
    }
}

/// The longest summary, in characters, before the `…`.
const SUMMARY_CHARS: usize = 280;

impl<P: Host> DocumentTree<P> {
    /// `None` when the document has no frontmatter, or it isn't well formed.
    pub fn frontmatter(&self) -> Option<&Value> {
        self.frontmatter.as_ref()
    }

    /// The top-level blocks, in order.
    pub fn blocks(&self) -> impl Iterator<Item = NodeRef<'_, P>> {
        self.body.ids.iter().map(|&id| NodeRef::new(&self.tree, id))
    }

    /// Every node of the body, depth first in document order: each block,
    /// then its contents.
    pub fn nodes(&self) -> impl Iterator<Item = NodeRef<'_, P>> {
        self.blocks().flat_map(|block| block.descendants())
    }

    /// One walk of the body. Nodes left in the arena but never attached
    /// (from a host hook that returned `None`, or a failed bracket) are not
    /// reached.
    pub(crate) fn extract(&self) -> Extracted {
        let mut walk = Walk {
            tree: &self.tree,
            ex: Extracted::default(),
            slugs: HashSet::new(),
        };
        // Depth first, in document order, without recursion: a node's
        // children are pushed in reverse.
        let mut stack: Vec<(NodeId, Under)> = self
            .body
            .ids
            .iter()
            .rev()
            .map(|&id| (id, Under::default()))
            .collect();
        while let Some((id, under)) = stack.pop() {
            let under = walk.node(id, under);
            let children = id.children(&self.tree.arena).rev();
            stack.extend(children.map(|child| (child, under)));
        }
        let mut ex = walk.ex;
        if let Some(Value::Object(map)) = &self.frontmatter {
            ex.title_from_frontmatter = map
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            ex.summary_from_frontmatter = ["description", "summary"]
                .iter()
                .filter_map(|key| map.get(*key).and_then(Value::as_str))
                .map(str::trim)
                .find(|t| !t.is_empty())
                .map(str::to_string);
            let tags = match map.get("tags") {
                Some(Value::String(s)) => vec![s.as_str()],
                Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            ex.frontmatter_tags = tags
                .into_iter()
                .map(|t| t.trim().trim_start_matches('#').to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
        ex
    }
}

/// What a node is inside of.
#[derive(Clone, Copy, Default)]
struct Under {
    link: bool,
    image: bool,
}

struct Walk<'a, P: Host> {
    tree: &'a Tree<P>,
    ex: Extracted,
    /// The heading ids taken so far.
    slugs: HashSet<String>,
}

impl<P: Host> Walk<'_, P> {
    /// Takes in node `id`; returns what its children are inside of.
    fn node(&mut self, id: NodeId, mut under: Under) -> Under {
        match self.tree.arena[id].get() {
            Node::Block(kind) => match kind {
                BlockKind::Heading { level } => {
                    let mut text = String::new();
                    plain_text(self.tree, id, &mut text);
                    let slug = self.unique(slug(&text));
                    self.ex.headings.insert(id, slug.clone());
                    // Headings inside a block quote or list item still
                    // draw as headings, but aren't in the outline.
                    if self.tree.arena[id].parent().is_none() {
                        self.ex.outline.push(OutlineHeading {
                            level: *level,
                            text: text.clone(),
                            anchor: slug,
                            line: self.tree.lines.get(&id).copied(),
                        });
                    }
                    self.ex.first_heading.get_or_insert(text);
                }
                BlockKind::Paragraph if self.ex.first_paragraph.is_none() => {
                    let mut text = String::new();
                    plain_text(self.tree, id, &mut text);
                    if !text.trim().is_empty() {
                        self.ex.first_paragraph = Some(text);
                    }
                }
                BlockKind::Code { body, .. } => self.body(body),
                BlockKind::Host(element) => self.host_text(element),
                _ => {}
            },
            Node::Inline(kind) => match kind {
                InlineKind::Text(text) | InlineKind::Code(text) => self.body(text),
                InlineKind::Link { .. } => under.link = true,
                InlineKind::Image { .. } => under.image = true,
                InlineKind::WikiLink { target, label } => {
                    if !under.image {
                        self.ex.ordinals.insert(id, self.ex.wikilinks.len());
                        self.ex.wikilinks.push(target.to_string());
                    }
                    self.body(label.as_deref().unwrap_or(target));
                }
                InlineKind::Embed { target, alt } => {
                    self.body(target);
                    if let Some(alt) = alt {
                        self.body(alt);
                    }
                }
                InlineKind::Tag(tag) => {
                    if !under.link && !under.image {
                        self.ex.ordinals.insert(id, self.ex.inline_tags.len());
                        self.ex.inline_tags.push(tag.to_lowercase());
                    }
                    self.body(&format!("#{tag}"));
                }
                InlineKind::Host(element) => self.host_text(element),
                _ => {}
            },
        }
        under
    }

    /// `slug`, or the first of `slug-1`, `slug-2`, … not yet taken on the
    /// page and not one the library draws itself.
    fn unique(&mut self, slug: String) -> String {
        let mut id = slug.clone();
        let mut n = 0;
        while reserved(&id) || !self.slugs.insert(id.clone()) {
            n += 1;
            id = format!("{slug}-{n}");
        }
        id
    }

    fn host_text(&mut self, element: &impl Element<P>) {
        if let Some(text) = element.text() {
            self.body(&text);
        }
    }

    fn body(&mut self, literal: &str) {
        let body = &mut self.ex.body_text;
        if !body.is_empty() {
            body.push(' ');
        }
        body.push_str(literal);
    }
}

/// A GitHub-style slug of a heading's text: lowercased, each space a `-`,
/// and everything but letters, digits, `-` and `_` dropped.
pub(crate) fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// True for an id a heading can't have: empty, or one the library draws
/// (`htmx-root`, `wl-{n}`, `tag-{n}`).
fn reserved(id: &str) -> bool {
    let numbered = |prefix: &str| {
        id.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    id.is_empty() || id == "htmx-root" || numbered("wl-") || numbered("tag-")
}

/// The text of `id`'s descendants, as a heading's title.
pub(crate) fn plain_text<P: Host>(tree: &Tree<P>, id: NodeId, out: &mut String) {
    for child in id.descendants(&tree.arena).skip(1) {
        if let Node::Inline(kind) = tree.arena[child].get() {
            match kind {
                InlineKind::Text(t) | InlineKind::Code(t) => out.push_str(t),
                InlineKind::SoftBreak | InlineKind::HardBreak => out.push(' '),
                InlineKind::WikiLink { target, label } => {
                    out.push_str(label.as_deref().unwrap_or(target))
                }
                InlineKind::Embed { target, alt } => out.push_str(alt.as_deref().unwrap_or(target)),
                InlineKind::Tag(tag) => {
                    out.push('#');
                    out.push_str(tag);
                }
                InlineKind::Host(element) => {
                    if let Some(text) = element.text() {
                        out.push_str(&text);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use maud::{Markup, html};
    use nom::bytes::complete::{tag, take_while1};
    use nom::combinator::rest;
    use nom::sequence::{delimited, preceded};

    use super::*;
    use crate::draw::{DrawCtx, DrawCx, draw_blocks};
    use crate::parse::{BlockStream, InlineStream, Plain, parse};
    use crate::template::DefaultTemplate;
    use crate::tree::{Content, Parsed};

    fn extract<P: Host>(host: &P, md: &str) -> Extracted {
        parse(host, md).extract()
    }

    fn plain(md: &str) -> Extracted {
        extract(&Plain, md)
    }

    #[test]
    fn pathological_nesting_parses_extracts_and_draws_on_a_worker_stack() {
        let nested_images = format!(
            "{}x #t [[W]]{}",
            format!("{}![", "**".repeat(40)).repeat(40),
            format!("](i.png){}", "**".repeat(40)).repeat(40)
        );
        let inputs = [
            format!("{} hi #t [[W]]\n", ">".repeat(5000)),
            (0..1000)
                .map(|i| format!("{}- x\n", "  ".repeat(i)))
                .collect(),
            format!("{}a{}\n", "*".repeat(20_000), "*".repeat(20_000)),
            format!("{}a{}\n", "[a](".repeat(5000), ")".repeat(5000)),
            format!("{}{nested_images}\n", "> - ".repeat(40)),
        ];
        for md in inputs {
            // The stack a tokio worker thread has.
            let (ex, html) = std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(move || {
                    let doc = parse(&Plain, &md);
                    let html = draw_blocks(&doc.tree, &doc.body, &DrawCx::default());
                    (doc.extract(), html.into_string())
                })
                .unwrap()
                .join()
                .unwrap();
            assert!(!html.is_empty());
            assert!(!ex.body_text.is_empty());
        }
    }

    #[test]
    fn embeds_are_neither_tags_nor_wikilinks() {
        let ex = plain("![[a.png|Alt]] #t ![[Note]] x![[b.PNG]]\n");
        assert_eq!(ex.inline_tags, ["t"]);
        assert!(ex.wikilinks.is_empty());
    }

    #[test]
    fn summary_is_frontmatter_else_first_paragraph() {
        let summary = |md: &str| plain(md).summary();
        assert_eq!(
            summary("# T\n\nFirst *para*\nwraps.\n\nSecond.\n"),
            "First para wraps."
        );
        assert_eq!(summary("---\nsummary: S\n---\nBody.\n"), "S");
        assert_eq!(
            summary("---\nsummary: S\ndescription: D\n---\nBody.\n"),
            "D"
        );
        assert_eq!(summary("---\ndescription: '  '\n---\nBody.\n"), "Body.");
        assert_eq!(summary("# Only a heading\n"), "");
        let long = summary(&"word ".repeat(100));
        assert_eq!(long.chars().count(), 280);
        assert!(long.ends_with("word…"), "{long}");
    }

    #[test]
    fn body_text_has_code_but_not_frontmatter() {
        let ex = plain(
            "---\nsecretkey: hidden value\n---\n# Head\n\nSome `inline code` and [link text](http://url.example).\n\n```rust\nfn fenced() {}\n```\n\n<div>raw html</div>\n",
        );
        for want in ["Head", "Some", "inline code", "link text", "fn fenced() {}"] {
            assert!(ex.body_text.contains(want), "{want}: {:?}", ex.body_text);
        }
        for unwanted in ["secretkey", "hidden value", "url.example", "raw html"] {
            assert!(
                !ex.body_text.contains(unwanted),
                "{unwanted}: {:?}",
                ex.body_text
            );
        }
    }

    #[test]
    fn no_tags_in_code_links_or_wikilinks() {
        let ex = plain(
            "Real #one and `#code` and [#linktext](x) and [[Foo#b|#wl]].\n\n```\n#fenced\n```\n\nThen [[Bar]] #Two.\n",
        );
        assert_eq!(ex.inline_tags, ["one", "two"]);
        assert_eq!(ex.wikilinks, ["Foo#b", "Bar"]);
    }

    #[test]
    fn image_alt_text_holds_no_tags_or_wikilinks() {
        let ex = plain("![alt #hidden [[X]]](i.png) #shown [[Y]]\n");
        assert_eq!(ex.inline_tags, ["shown"]);
        assert_eq!(ex.wikilinks, ["Y"]);
    }

    #[test]
    fn ordinals_number_wikilinks_and_tags_in_document_order() {
        let doc = parse(&Plain, "[[A]] #x ![a [[Skip]]](i.png)\n\n> [[B]] #y\n");
        let ex = doc.extract();
        let mut found: Vec<(String, usize)> = ex
            .ordinals
            .iter()
            .map(|(&id, &n)| (n_kind(&doc.tree, id), n))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("tag x".to_string(), 0),
                ("tag y".to_string(), 1),
                ("wl A".to_string(), 0),
                ("wl B".to_string(), 1),
            ]
        );
    }

    #[test]
    fn slugs_are_github_style() {
        assert_eq!(slug("Hello, World!"), "hello-world");
        assert_eq!(slug("A - B  c"), "a---b--c");
        assert_eq!(slug("snake_case & kebab-case"), "snake_case--kebab-case");
        assert_eq!(slug("Café Ünïcode 日本"), "café-ünïcode-日本");
        assert_eq!(slug("`code` *em* 1.2"), "code-em-12");
    }

    #[test]
    fn heading_ids_are_unique_on_the_page() {
        let doc = parse(
            &Plain,
            "# Foo\n\n## Foo\n\n### Foo 1\n\n> # Foo\n\n## ???\n\n## !!\n\n## WL 0\n\n## Tag 3\n\n## htmx root\n\n## wl-x\n",
        );
        let ex = doc.extract();
        let ids: Vec<&str> = doc
            .body
            .ids
            .iter()
            .flat_map(|&id| id.descendants(&doc.tree.arena))
            .filter_map(|id| ex.headings.get(&id).map(String::as_str))
            .collect();
        assert_eq!(
            ids,
            [
                "foo",
                "foo-1",
                "foo-1-1",
                "foo-2",
                "-1",
                "-2",
                "wl-0-1",
                "tag-3-1",
                "htmx-root-1",
                "wl-x"
            ]
        );
        assert_eq!(ex.first_heading.as_deref(), Some("Foo"));
    }

    fn n_kind<P: Host>(tree: &Tree<P>, id: NodeId) -> String {
        match tree.arena[id].get() {
            Node::Inline(InlineKind::WikiLink { target, .. }) => format!("wl {target}"),
            Node::Inline(InlineKind::Tag(tag)) => format!("tag {tag}"),
            _ => "other".to_string(),
        }
    }

    #[test]
    fn frontmatter() {
        let doc = parse(
            &Plain,
            "---\ntitle: Welcome Home\nstatus: draft\ntags: [meta, '#Other']\n---\n# Heading\n",
        );
        let status = doc.frontmatter.as_ref().and_then(|fm| fm.get("status"));
        assert_eq!(status.and_then(Value::as_str), Some("draft"));
        let ex = doc.extract();
        assert_eq!(ex.title_from_frontmatter.as_deref(), Some("Welcome Home"));
        assert_eq!(ex.frontmatter_tags, ["meta", "Other"]);
    }

    #[test]
    fn frontmatter_string_tag_and_not_well_formed() {
        assert_eq!(
            plain("---\ntags: solo\n---\nbody\n").frontmatter_tags,
            ["solo"]
        );

        // Not well formed: no frontmatter, and its text is a code block.
        let doc = parse(&Plain, "---\n- not\n- a mapping\ntitle: x\n---\nbody\n");
        assert!(doc.frontmatter.is_none());
        let ex = doc.extract();
        assert_eq!(ex.title_from_frontmatter, None);
        assert!(ex.frontmatter_tags.is_empty());
        assert!(ex.body_text.contains("- a mapping"), "{:?}", ex.body_text);

        assert!(parse(&Plain, "body\n").frontmatter.is_none());
    }

    #[test]
    fn titles() {
        let ex = plain("---\ntitle: From FM\n---\n# Heading\n");
        assert_eq!(ex.title("stem"), "From FM");

        let ex = plain("intro\n\n## Sub `code`\n\n# Main\n");
        assert_eq!(ex.title("stem"), "Sub code");

        let ex = plain("> # [[Page|Quoted]] *em* #t\n");
        assert_eq!(ex.title("stem"), "Quoted em #t");

        let ex = plain("---\ntitle: ''\n---\nno heading\n");
        assert_eq!(ex.title("2024-03-05 Daily"), "2024-03-05 Daily");
    }

    /// `[[ABC-1]]` is a ticket with its own text; `[OPEN: …]` a marker with
    /// none; `[GHOST: …]` builds a node, then gives up without a `]`; `!! …`
    /// a block with none.
    struct TestHost;

    enum TestInline {
        Ticket(String),
        Marker,
        Ghost,
    }

    enum TestBlock {
        Banner,
    }

    impl Element<TestHost> for TestInline {
        fn draw(&self, cx: &DrawCtx<'_, TestHost>) -> Markup {
            html! { span { (cx.draw_children()) } }
        }

        fn text(&self) -> Option<String> {
            match self {
                TestInline::Ticket(id) => Some(id.clone()),
                TestInline::Marker => None,
                TestInline::Ghost => Some("ghosttext".to_string()),
            }
        }
    }

    impl Element<TestHost> for TestBlock {
        fn draw(&self, cx: &DrawCtx<'_, TestHost>) -> Markup {
            html! { header { (cx.draw_children()) } }
        }
    }

    impl Host for TestHost {
        type Block = TestBlock;
        type Inline = TestInline;
        type Template = DefaultTemplate;

        fn parse_block(&self, s: &mut BlockStream<'_, Self>) -> Option<Parsed<BlockKind<Self>>> {
            let text = s.try_parse(preceded(tag("!! "), rest))?;
            let text = s.inlines_in(text);
            Some(s.node(BlockKind::Host(TestBlock::Banner), Content::Inlines(text)))
        }

        fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
            let id = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-';
            if let Some(id) = s.try_parse(delimited(tag("[["), take_while1(id), tag("]]"))) {
                return Some(s.leaf(InlineKind::Host(TestInline::Ticket(id.to_string()))));
            }
            if s.try_parse(tag("[OPEN: ")).is_some() {
                let body = s.inlines_until(tag("]"))?;
                return Some(s.node(InlineKind::Host(TestInline::Marker), body));
            }
            if s.try_parse(tag("[GHOST: ")).is_some() {
                s.leaf(InlineKind::Host(TestInline::Ghost));
                s.leaf(InlineKind::Tag("ghosttag".to_string()));
                s.leaf(InlineKind::WikiLink {
                    target: "Ghost".to_string(),
                    label: None,
                });
                let body = s.inlines_until(tag("]"))?;
                return Some(s.node(InlineKind::Host(TestInline::Ghost), body));
            }
            None
        }
    }

    #[test]
    fn host_nodes_contribute_their_text_and_their_childrens() {
        let ex = extract(
            &TestHost,
            "# [[ABC-1]] plan\n\nSee [[ABC-1]] and [[Page]]. [OPEN: **inside** [[Inner]] #t]\n\n!! banner words #b\n",
        );
        // A claimed `[[ABC-1]]` is not a wikilink; a host node's children
        // are walked for wikilinks and tags.
        assert_eq!(ex.wikilinks, ["Page", "Inner"]);
        assert_eq!(ex.inline_tags, ["t", "b"]);
        assert_eq!(ex.title("stem"), "ABC-1 plan");
        // The ticket's `text()` and the marker's and banner's children.
        for want in ["ABC-1", "inside", "Inner", "banner words"] {
            assert!(ex.body_text.contains(want), "{want}: {:?}", ex.body_text);
        }
        // The marker and the banner have the default `text()`: only their
        // children's text.
        assert_eq!(
            ex.body_text,
            "ABC-1  plan See  ABC-1  and  Page .  inside   Inner   #t banner words  #b"
        );
    }

    #[test]
    fn nodes_never_attached_are_not_extracted() {
        let ex = extract(&TestHost, "a [GHOST: no close #real\n");
        assert_eq!(ex.inline_tags, ["real"]);
        assert!(ex.wikilinks.is_empty());
        assert!(!ex.body_text.contains("ghost"), "{:?}", ex.body_text);
    }
}
