//! Drawing the tree to HTML: parsing in reverse. Built-in kinds are drawn
//! here; host elements draw themselves through [`Element`].

use std::collections::HashMap;

use indextree::NodeId;
use maud::{Markup, PreEscaped, Render};

use crate::document::slug;
use crate::links::{Resolved, local_href, resolve};
use crate::names::encode_path;
use crate::parse::Host;
use crate::tree::{Align, BlockKind, Children, InlineKind, Node, TaskStatus, Tree};

/// Anything that draws itself: a host's block and inline elements, and the
/// library's page components. Plain data; what `cx` doesn't give it, it
/// carries in its own state.
pub trait Element<P: Host> {
    /// Draws the element, synchronously: async work happens when it is
    /// built. `cx.draw_children()` draws the node's children inside it.
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup;

    /// Text this element contributes to search, besides its children's.
    fn text(&self) -> Option<String> {
        None
    }
}

/// What an element is drawn with: the base path, and the node it is drawn
/// for, if it is in a tree.
pub struct DrawCtx<'a, P: Host> {
    base: &'a str,
    node: Option<(Draw<'a, P>, NodeId)>,
}

impl<'a, P: Host> DrawCtx<'a, P> {
    /// For an element that isn't in a tree, which has no children.
    pub(crate) fn detached(base: &'a str) -> Self {
        DrawCtx { base, node: None }
    }

    /// Where the wiki is mounted, without a trailing slash: `""` at the
    /// root, e.g. `"/docs"` otherwise.
    pub fn base_path(&self) -> &str {
        self.base
    }

    /// The node's children, drawn when rendered (maud `Render`), so they
    /// drop into `html!` as `(cx.draw_children())`. Nothing for an element
    /// that isn't in a tree.
    pub fn draw_children(&self) -> DrawChildren<'a, P> {
        DrawChildren { node: self.node }
    }
}

/// A node's children, drawn when rendered.
pub struct DrawChildren<'a, P: Host> {
    node: Option<(Draw<'a, P>, NodeId)>,
}

impl<P: Host> Render for DrawChildren<'_, P> {
    fn render_to(&self, out: &mut String) {
        if let Some((draw, node)) = self.node {
            draw.children(node, out);
        }
    }
}

/// How the `n`th wikilink on a page is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WikiHref {
    pub(crate) href: String,
    pub(crate) resolved: bool,
}

/// What drawing needs beyond the tree.
#[derive(Debug, Clone, Default)]
pub(crate) struct DrawCx {
    /// Base path without trailing slash (`""` at root).
    pub(crate) base: String,
    /// The page's folder rel (`""` at root); ordinary links resolve from it.
    pub(crate) folder: String,
    /// One entry per wikilink ordinal. `None` (empty target, e.g.
    /// `[[#Heading]]`) draws an ordinary link to the target as written.
    pub(crate) wikilinks: Vec<Option<WikiHref>>,
    /// Each resolved image embed: resolved, drawn as an image of `href`;
    /// unresolved, as an unresolved wikilink to `href`. An embed missing
    /// here resolves from `folder`, as an ordinary image does.
    pub(crate) embeds: HashMap<NodeId, WikiHref>,
    /// The ordinal of each wikilink and tag node, drawn as its `wl-{n}` or
    /// `tag-{n}` id.
    pub(crate) ordinals: HashMap<NodeId, usize>,
    /// Each heading's id, the slug its anchor links to.
    pub(crate) headings: HashMap<NodeId, String>,
    /// Draw each block's source line as `data-line`, for `#:N` links.
    pub(crate) lines: bool,
    /// The source page's URL, when drawn off its own page: in-page links
    /// (`#…`) go to it.
    pub(crate) page: Option<String>,
}

impl DrawCx {
    /// The href of `fragment` (`#…`) on the source page.
    fn in_page(&self, fragment: &str) -> String {
        format!("{}{fragment}", self.page.as_deref().unwrap_or(""))
    }
}

/// Draws a list of top-level blocks.
pub(crate) fn draw_blocks<P: Host>(
    tree: &Tree<P>,
    blocks: &Children<BlockKind<P>>,
    cx: &DrawCx,
) -> Markup {
    let draw = Draw { tree, cx, depth: 0 };
    let mut out = String::new();
    for &id in &blocks.ids {
        draw.node(id, &mut out);
    }
    PreEscaped(out)
}

/// How deep drawing nests before the rest of a subtree is drawn as its
/// text, so that no tree can overflow the stack.
const MAX_DEPTH: usize = 256;

/// Built-in kinds are written straight into one buffer, so drawing is
/// linear in the output.
struct Draw<'a, P: Host> {
    tree: &'a Tree<P>,
    cx: &'a DrawCx,
    /// Ancestors of the nodes being drawn.
    depth: usize,
}

// Derived `Clone` would require `P: Clone`.
impl<P: Host> Clone for Draw<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Host> Copy for Draw<'_, P> {}

impl<'a, P: Host> Draw<'a, P> {
    fn host(self, id: NodeId, element: &impl Element<P>, out: &mut String) {
        let cx = DrawCtx {
            base: &self.cx.base,
            node: Some((self, id)),
        };
        let drawn = element.draw(&cx);
        match self.line(id) {
            Some(n) => out.push_str(&with_line(&drawn.0, n)),
            None => drawn.render_to(out),
        }
    }

    /// The block's source line, when lines are drawn.
    fn line(self, id: NodeId) -> Option<usize> {
        self.cx
            .lines
            .then(|| self.tree.lines.get(&id).copied())
            .flatten()
    }

    /// `<{name}`, the block's `data-line`, and `rest` (e.g. `">"`).
    fn open(self, id: NodeId, name: &str, rest: &str, out: &mut String) {
        out.push('<');
        out.push_str(name);
        if let Some(n) = self.line(id) {
            attr(out, "data-line", n);
        }
        out.push_str(rest);
    }

    fn children(self, node: NodeId, out: &mut String) {
        let draw = Draw {
            depth: self.depth + 1,
            ..self
        };
        for child in node.children(&self.tree.arena) {
            draw.node(child, out);
        }
    }

    /// `open`, the node's children, then `close`.
    fn around(self, id: NodeId, open: &str, close: &str, out: &mut String) {
        out.push_str(open);
        self.children(id, out);
        out.push_str(close);
    }

    fn node(self, id: NodeId, out: &mut String) {
        if self.depth > MAX_DEPTH {
            let mut text = String::new();
            self.plain_text(id, &mut text);
            text.render_to(out);
            return;
        }
        match self.tree.arena[id].get() {
            Node::Block(kind) => self.block(id, kind, out),
            Node::Inline(kind) => self.inline(id, kind, out),
        }
    }

    fn block(self, id: NodeId, kind: &BlockKind<P>, out: &mut String) {
        match kind {
            BlockKind::Paragraph if self.in_tight_item(id) => {
                if let Some(status) = self.item_task(id) {
                    checkbox(status, out);
                }
                self.children(id, out);
                if self.tree.arena[id].next_sibling().is_some() {
                    out.push('\n');
                }
            }
            BlockKind::Paragraph => {
                self.open(id, "p", ">", out);
                if let Some(status) = self.item_task(id) {
                    checkbox(status, out);
                }
                self.around(id, "", "</p>\n", out);
            }
            BlockKind::Heading { level } => {
                let level = (*level).clamp(1, 6);
                self.open(id, &format!("h{level}"), "", out);
                let Some(slug) = self.cx.headings.get(&id) else {
                    self.around(id, ">", &format!("</h{level}>\n"), out);
                    return;
                };
                attr(out, "id", slug.as_str());
                out.push('>');
                self.children(id, out);
                out.push_str("<a class=\"anchor\"");
                attr(
                    out,
                    "href",
                    href_attr(&self.cx.in_page(&format!("#{slug}"))),
                );
                out.push_str(">#</a>");
                out.push_str(&format!("</h{level}>\n"));
            }
            BlockKind::ThematicBreak => self.open(id, "hr", " />\n", out),
            BlockKind::Quote => {
                self.open(id, "blockquote", ">\n", out);
                self.around(id, "", "</blockquote>\n", out);
            }
            BlockKind::List { ordered, start, .. } => {
                let name = if *ordered { "ol" } else { "ul" };
                self.open(id, name, "", out);
                if *ordered && *start != 1 {
                    attr(out, "start", start);
                }
                if self.has_tasks(id) {
                    attr(out, "class", "contains-task-list");
                }
                out.push_str(">\n");
                self.around(id, "", &format!("</{name}>\n"), out);
            }
            BlockKind::ListItem { task: None } => {
                self.open(id, "li", ">", out);
                self.around(id, "", "</li>\n", out);
            }
            BlockKind::ListItem { task: Some(status) } => {
                self.open(id, "li", " class=\"task-list-item\"", out);
                attr(out, "data-task", status.symbol());
                out.push('>');
                // The item's first paragraph draws the checkbox, as GitHub does.
                let first = self.tree.arena[id].first_child();
                if !first.is_some_and(|p| {
                    matches!(self.tree.arena[p].get(), Node::Block(BlockKind::Paragraph))
                }) {
                    checkbox(*status, out);
                }
                self.around(id, "", "</li>\n", out);
            }
            BlockKind::Code { info, body } => {
                let lang = info.split_whitespace().next();
                if lang == Some("mermaid") {
                    self.open(id, "pre", " class=\"mermaid\">", out);
                    body.render_to(out);
                    out.push_str("</pre>\n");
                } else {
                    self.open(id, "pre", "><code", out);
                    if let Some(lang) = lang {
                        attr(out, "class", format!("language-{lang}"));
                    }
                    out.push('>');
                    body.render_to(out);
                    out.push_str("</code></pre>\n");
                }
            }
            BlockKind::Html(raw) => match self.line(id) {
                Some(n) => out.push_str(&with_line(raw, n)),
                None => out.push_str(raw),
            },
            BlockKind::Table { align } => self.table(id, align, out),
            // Rows and cells are drawn by their table, which knows the
            // column alignments; these arms are reached only for a row or
            // cell outside a table.
            BlockKind::TableRow { .. } => {
                self.open(id, "tr", ">", out);
                self.around(id, "", "</tr>\n", out);
            }
            BlockKind::TableCell => self.around(id, "<td>", "</td>", out),
            BlockKind::Host(element) => self.host(id, element, out),
        }
    }

    /// The status of the task item whose first child is `id`.
    fn item_task(self, id: NodeId) -> Option<TaskStatus> {
        let item = self.tree.arena[id].parent()?;
        if self.tree.arena[item].first_child() != Some(id) {
            return None;
        }
        match self.tree.arena[item].get() {
            Node::Block(BlockKind::ListItem { task }) => *task,
            _ => None,
        }
    }

    /// True when one of the list `id`'s items is a task.
    fn has_tasks(self, id: NodeId) -> bool {
        id.children(&self.tree.arena).any(|item| {
            matches!(
                self.tree.arena[item].get(),
                Node::Block(BlockKind::ListItem { task: Some(_) })
            )
        })
    }

    /// True when `id`'s parent is an item of a tight list.
    fn in_tight_item(self, id: NodeId) -> bool {
        let arena = &self.tree.arena;
        let Some(item) = arena[id].parent() else {
            return false;
        };
        let Some(list) = arena[item].parent() else {
            return false;
        };
        matches!(
            (arena[item].get(), arena[list].get()),
            (
                Node::Block(BlockKind::ListItem { .. }),
                Node::Block(BlockKind::List { tight: true, .. })
            )
        )
    }

    fn table(self, id: NodeId, align: &[Align], out: &mut String) {
        let arena = &self.tree.arena;
        let (head, body): (Vec<NodeId>, Vec<NodeId>) = id.children(arena).partition(|&row| {
            matches!(
                arena[row].get(),
                Node::Block(BlockKind::TableRow { header: true })
            )
        });
        self.open(id, "table", ">\n", out);
        for (rows, section, header) in [(head, "thead", true), (body, "tbody", false)] {
            if rows.is_empty() {
                continue;
            }
            out.push_str(&format!("<{section}>\n"));
            for row in rows {
                self.row(row, align, header, out);
            }
            out.push_str(&format!("</{section}>\n"));
        }
        out.push_str("</table>\n");
    }

    fn row(self, row: NodeId, align: &[Align], header: bool, out: &mut String) {
        let cell = if header { "th" } else { "td" };
        self.open(row, "tr", ">\n", out);
        for (i, id) in row.children(&self.tree.arena).enumerate() {
            out.push('<');
            out.push_str(cell);
            match align.get(i) {
                Some(Align::Left) => attr(out, "align", "left"),
                Some(Align::Center) => attr(out, "align", "center"),
                Some(Align::Right) => attr(out, "align", "right"),
                Some(Align::None) | None => {}
            }
            out.push('>');
            self.children(id, out);
            out.push_str(&format!("</{cell}>\n"));
        }
        out.push_str("</tr>\n");
    }

    fn inline(self, id: NodeId, kind: &InlineKind<P>, out: &mut String) {
        let cx = self.cx;
        match kind {
            InlineKind::Text(text) => text.render_to(out),
            InlineKind::SoftBreak | InlineKind::HardBreak => out.push_str("<br />\n"),
            InlineKind::Strong => self.around(id, "<strong>", "</strong>", out),
            InlineKind::Emph => self.around(id, "<em>", "</em>", out),
            InlineKind::Strike => self.around(id, "<del>", "</del>", out),
            InlineKind::Code(code) => {
                out.push_str("<code>");
                code.render_to(out);
                out.push_str("</code>");
            }
            InlineKind::Html(raw) => out.push_str(raw),
            InlineKind::Link { dest, title } => {
                match target_href(cx, dest) {
                    Some(href) => {
                        out.push_str("<a");
                        attr(out, "href", href_attr(&href));
                    }
                    None => out.push_str("<a class=\"unresolved\""),
                }
                if let Some(title) = title {
                    attr(out, "title", title);
                }
                self.around(id, ">", "</a>", out);
            }
            InlineKind::Image { dest, title } => {
                let mut alt = String::new();
                self.plain_text(id, &mut alt);
                img(
                    target_href(cx, dest).as_deref(),
                    &alt,
                    title.as_deref(),
                    (None, None),
                    out,
                );
            }
            InlineKind::WikiLink { target, label } => {
                let n = cx.ordinals.get(&id).copied();
                let (href, class) = match n.and_then(|n| cx.wikilinks.get(n).cloned().flatten()) {
                    Some(WikiHref {
                        href,
                        resolved: true,
                    }) => (href, "wikilink"),
                    Some(WikiHref {
                        href,
                        resolved: false,
                    }) => (href, "wikilink unresolved"),
                    // `[[#Heading]]`: the heading on this page.
                    None => match target.split_once('#') {
                        Some((name, heading)) if name.trim().is_empty() => {
                            (cx.in_page(&format!("#{}", slug(heading))), "wikilink")
                        }
                        _ => (target.clone(), "wikilink"),
                    },
                };
                out.push_str("<a");
                attr(out, "href", href_attr(&href));
                if let Some(n) = n {
                    attr(out, "id", format!("wl-{n}"));
                }
                attr(out, "class", class);
                out.push('>');
                label.as_deref().unwrap_or(target).render_to(out);
                out.push_str("</a>");
            }
            InlineKind::Embed {
                target,
                alt,
                width,
                height,
            } => {
                let alt = alt.as_deref().unwrap_or(target);
                let src = match cx.embeds.get(&id) {
                    Some(WikiHref {
                        href,
                        resolved: false,
                    }) => {
                        out.push_str("<a");
                        attr(out, "href", href_attr(href));
                        out.push_str(" class=\"wikilink unresolved\">");
                        alt.render_to(out);
                        out.push_str("</a>");
                        return;
                    }
                    Some(WikiHref { href, .. }) => Some(href.clone()),
                    None => target_href(cx, target),
                };
                img(src.as_deref(), alt, None, (*width, *height), out);
            }
            InlineKind::Tag(tag) => {
                out.push_str("<a class=\"tag\"");
                if let Some(n) = cx.ordinals.get(&id) {
                    attr(out, "id", format!("tag-{n}"));
                }
                attr(out, "href", href_attr(&tag_href(&cx.base, tag)));
                out.push_str(">#");
                tag.render_to(out);
                out.push_str("</a>");
            }
            InlineKind::Host(element) => self.host(id, element, out),
        }
    }

    /// The text of `id` and its descendants, as an image's alt text.
    fn plain_text(self, id: NodeId, out: &mut String) {
        for node in id.descendants(&self.tree.arena) {
            if let Node::Inline(kind) = self.tree.arena[node].get() {
                match kind {
                    InlineKind::Text(t) | InlineKind::Code(t) => out.push_str(t),
                    InlineKind::SoftBreak | InlineKind::HardBreak => out.push(' '),
                    InlineKind::WikiLink { target, label } => {
                        out.push_str(label.as_deref().unwrap_or(target))
                    }
                    InlineKind::Embed { target, alt, .. } => {
                        out.push_str(alt.as_deref().unwrap_or(target))
                    }
                    InlineKind::Tag(tag) => {
                        out.push('#');
                        out.push_str(tag);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// ` name="value"`, the value escaped as maud escapes it (a `PreEscaped`
/// value as is).
fn attr(out: &mut String, name: &str, value: impl Render) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    value.render_to(out);
    out.push('"');
}

/// A task's disabled checkbox, and a space.
fn checkbox(status: TaskStatus, out: &mut String) {
    out.push_str("<input type=\"checkbox\" class=\"task-list-item-checkbox\"");
    if status == TaskStatus::Done {
        out.push_str(" checked=\"\"");
    }
    out.push_str(" disabled=\"\"");
    match status {
        TaskStatus::InProgress => attr(out, "title", "In progress"),
        TaskStatus::Canceled => attr(out, "title", "Canceled"),
        _ => {}
    }
    out.push_str(" /> ");
}

/// `html` with ` data-line="{n}"` after the name of the open tag it starts
/// with (after whitespace); unchanged when it doesn't start with one, e.g.
/// text, a comment or a closing tag.
fn with_line(html: &str, n: usize) -> String {
    let start = html.len() - html.trim_start().len();
    let name = html[start..]
        .strip_prefix('<')
        .filter(|t| t.starts_with(|c: char| c.is_ascii_alphabetic()))
        .map(|t| {
            t.find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(t.len())
        });
    let Some(len) = name else {
        return html.to_string();
    };
    let at = start + 1 + len;
    if !html[at..].starts_with([' ', '\t', '\n', '\r', '>', '/']) {
        return html.to_string();
    }
    format!("{} data-line=\"{n}\"{}", &html[..at], &html[at..])
}

/// `<img src alt title width height />`, or without `src` when the target
/// is outside the root. Written out for the ` />` that maud doesn't
/// produce.
fn img(
    src: Option<&str>,
    alt: &str,
    title: Option<&str>,
    (width, height): (Option<u32>, Option<u32>),
    out: &mut String,
) {
    out.push_str("<img");
    if let Some(src) = src {
        attr(out, "src", href_attr(src));
    }
    attr(out, "alt", alt);
    if let Some(title) = title {
        attr(out, "title", title);
    }
    if let Some(width) = width {
        attr(out, "width", width);
    }
    if let Some(height) = height {
        attr(out, "height", height);
    }
    out.push_str(" />");
}

/// The href or src for a link or image URL on the page: untouched, resolved
/// under the base, or `None` when it points outside the root.
fn target_href(cx: &DrawCx, url: &str) -> Option<String> {
    match resolve(&cx.folder, url) {
        Resolved::Unchanged if url.starts_with('#') => Some(cx.in_page(url)),
        Resolved::Unchanged => Some(url.to_string()),
        Resolved::OutsideRoot => None,
        Resolved::Local {
            rel,
            suffix,
            folder,
        } => Some(local_href(&cx.base, &rel, &suffix, folder)),
    }
}

/// `{base}/_wiki/tag/{tag}`, tag lowercased, each segment percent-encoded.
pub(crate) fn tag_href(base: &str, tag: &str) -> String {
    format!(
        "{base}/_wiki{}",
        encode_path(&format!("tag/{}", tag.to_lowercase()))
    )
}

/// `url` escaped for an `href` or `src` attribute: bytes outside the URL-safe
/// set percent-encoded (existing `%XX` escapes kept), `&` and `'` as
/// entities.
fn href_attr(url: &str) -> PreEscaped<String> {
    let mut out = String::with_capacity(url.len());
    let bytes = url.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => out.push(b as char),
            b'-' | b'_' | b'.' | b'+' | b'!' | b'*' | b'(' | b')' | b',' | b'#' | b'@' | b'?'
            | b'=' | b';' | b':' | b'/' | b'$' | b'~' => out.push(b as char),
            b'&' => out.push_str("&amp;"),
            b'\'' => out.push_str("&#x27;"),
            b'%' if bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit) =>
            {
                out.push('%');
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
        i += 1;
    }
    PreEscaped(out)
}

#[cfg(test)]
mod tests {
    use maud::html;

    use super::*;
    use crate::parse::Plain;
    use crate::template::DefaultTemplate;
    use crate::tree::{Content, Parsed};

    type B<P> = Parsed<BlockKind<P>>;
    type I<P> = Parsed<InlineKind<P>>;

    fn text<P: Host>(t: &mut Tree<P>, s: &str) -> I<P> {
        t.leaf(InlineKind::Text(s.to_string()))
    }

    fn para<P: Host>(t: &mut Tree<P>, inlines: Vec<I<P>>) -> B<P> {
        t.block(
            BlockKind::Paragraph,
            Content::Inlines(Children::from_parsed(inlines)),
        )
    }

    fn cx() -> DrawCx {
        DrawCx {
            base: "/b".to_string(),
            folder: "Projects/Alpha".to_string(),
            ..DrawCx::default()
        }
    }

    fn draw<P: Host>(t: &Tree<P>, blocks: Vec<B<P>>, cx: &DrawCx) -> String {
        draw_blocks(t, &Children::from_parsed(blocks), cx).into_string()
    }

    #[test]
    fn inline_formatting_and_breaks() {
        let mut t = Tree::<Plain>::new();
        let a = text(&mut t, "a <&> \"q\"");
        let strong_text = text(&mut t, "s");
        let strong = t.inline(InlineKind::Strong, Children::from_parsed([strong_text]));
        let em_text = text(&mut t, "e");
        let em = t.inline(InlineKind::Emph, Children::from_parsed([em_text]));
        let del_text = text(&mut t, "d");
        let del = t.inline(InlineKind::Strike, Children::from_parsed([del_text]));
        let code = t.leaf(InlineKind::Code("x < y".to_string()));
        let soft = t.leaf(InlineKind::SoftBreak);
        let html = t.leaf(InlineKind::Html("<kbd>".to_string()));
        let hard = t.leaf(InlineKind::HardBreak);
        let end = text(&mut t, "z");
        let p = para(
            &mut t,
            vec![a, strong, em, del, code, soft, html, hard, end],
        );
        assert_eq!(
            draw(&t, vec![p], &cx()),
            "<p>a &lt;&amp;&gt; &quot;q&quot;<strong>s</strong><em>e</em><del>d</del>\
             <code>x &lt; y</code><br />\n<kbd><br />\nz</p>\n"
        );
    }

    #[test]
    fn leaf_and_container_blocks() {
        let mut t = Tree::<Plain>::new();
        let h_text = text(&mut t, "Title");
        let h = t.block(
            BlockKind::Heading { level: 2 },
            Content::Inlines(Children::from_parsed([h_text])),
        );
        let hr = t.block(BlockKind::ThematicBreak, Content::Leaf);
        let q_text = text(&mut t, "quoted");
        let q_para = para(&mut t, vec![q_text]);
        let quote = t.block(
            BlockKind::Quote,
            Content::Blocks(Children::from_parsed([q_para])),
        );
        let code = t.block(
            BlockKind::Code {
                info: "rust ignore".to_string(),
                body: "a < b\n".to_string(),
            },
            Content::Leaf,
        );
        let plain_code = t.block(
            BlockKind::Code {
                info: String::new(),
                body: "x\n".to_string(),
            },
            Content::Leaf,
        );
        let mermaid = t.block(
            BlockKind::Code {
                info: "mermaid".to_string(),
                body: "a --> b\n".to_string(),
            },
            Content::Leaf,
        );
        let html = t.block(
            BlockKind::Html("<div>raw</div>\n".to_string()),
            Content::Leaf,
        );
        assert_eq!(
            draw(
                &t,
                vec![h, hr, quote, code, plain_code, mermaid, html],
                &cx()
            ),
            "<h2>Title</h2>\n<hr />\n<blockquote>\n<p>quoted</p>\n</blockquote>\n\
             <pre><code class=\"language-rust\">a &lt; b\n</code></pre>\n\
             <pre><code>x\n</code></pre>\n\
             <pre class=\"mermaid\">a --&gt; b\n</pre>\n<div>raw</div>\n"
        );
    }

    #[test]
    fn lists_and_tasks() {
        let mut t = Tree::<Plain>::new();
        let item = |t: &mut Tree<Plain>, task, s: &str, sub: Option<B<Plain>>| {
            let s = text(t, s);
            let p = para(t, vec![s]);
            t.block(
                BlockKind::ListItem { task },
                Content::Blocks(Children::from_parsed([p].into_iter().chain(sub))),
            )
        };
        let list = |t: &mut Tree<Plain>, ordered, start, tight, items: Vec<B<Plain>>| {
            t.block(
                BlockKind::List {
                    ordered,
                    start,
                    tight,
                },
                Content::Blocks(Children::from_parsed(items)),
            )
        };
        let nested = item(&mut t, None, "nested", None);
        let sub = list(&mut t, false, 1, true, vec![nested]);
        let open = item(&mut t, Some(TaskStatus::Open), "open", Some(sub));
        let done = item(&mut t, Some(TaskStatus::Done), "done", None);
        let doing = item(&mut t, Some(TaskStatus::InProgress), "doing", None);
        let dropped = item(&mut t, Some(TaskStatus::Canceled), "dropped", None);
        let other = item(&mut t, Some(TaskStatus::Other('"')), "other", None);
        let plain = item(&mut t, None, "plain", None);
        let ul = list(
            &mut t,
            false,
            1,
            true,
            vec![open, done, doing, dropped, other, plain],
        );
        let first = item(&mut t, None, "one", None);
        let second = item(&mut t, Some(TaskStatus::InProgress), "two", None);
        // A task item that doesn't start with a paragraph draws the
        // checkbox itself.
        let hr = t.block(BlockKind::ThematicBreak, Content::Leaf);
        let third = t.block(
            BlockKind::ListItem {
                task: Some(TaskStatus::Done),
            },
            Content::Blocks(Children::from_parsed([hr])),
        );
        let loose = list(&mut t, true, 1, false, vec![first, second, third]);
        let fifth = item(&mut t, None, "five", None);
        let ol5 = list(&mut t, true, 5, true, vec![fifth]);
        assert_eq!(
            draw(&t, vec![ul, loose, ol5], &cx()),
            "<ul class=\"contains-task-list\">\n\
             <li class=\"task-list-item\" data-task=\" \">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" /> open\n\
             <ul>\n<li>nested</li>\n</ul>\n</li>\n\
             <li class=\"task-list-item\" data-task=\"x\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" checked=\"\" \
             disabled=\"\" /> done</li>\n\
             <li class=\"task-list-item\" data-task=\"/\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" \
             title=\"In progress\" /> doing</li>\n\
             <li class=\"task-list-item\" data-task=\"-\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" \
             title=\"Canceled\" /> dropped</li>\n\
             <li class=\"task-list-item\" data-task=\"&quot;\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" /> \
             other</li>\n\
             <li>plain</li>\n\
             </ul>\n\
             <ol class=\"contains-task-list\">\n<li><p>one</p>\n</li>\n\
             <li class=\"task-list-item\" data-task=\"/\"><p>\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"\" \
             title=\"In progress\" /> two</p>\n</li>\n\
             <li class=\"task-list-item\" data-task=\"x\">\
             <input type=\"checkbox\" class=\"task-list-item-checkbox\" checked=\"\" \
             disabled=\"\" /> <hr />\n</li>\n</ol>\n\
             <ol start=\"5\">\n<li>five</li>\n</ol>\n"
        );
    }

    #[test]
    fn tables_draw_alignment_and_sections() {
        let mut t = Tree::<Plain>::new();
        let row = |t: &mut Tree<Plain>, header, cells: &[&str]| {
            let cells: Vec<_> = cells
                .iter()
                .map(|c| {
                    let c = text(t, c);
                    t.block(
                        BlockKind::TableCell,
                        Content::Inlines(Children::from_parsed([c])),
                    )
                })
                .collect();
            t.block(
                BlockKind::TableRow { header },
                Content::Blocks(Children::from_parsed(cells)),
            )
        };
        let head = row(&mut t, true, &["a", "b", "c"]);
        let body = row(&mut t, false, &["1", "2", "3"]);
        let table = t.block(
            BlockKind::Table {
                align: vec![Align::None, Align::Center, Align::Right],
            },
            Content::Blocks(Children::from_parsed([head, body])),
        );
        let head_only = row(&mut t, true, &["x"]);
        let lone = t.block(
            BlockKind::Table {
                align: vec![Align::Left],
            },
            Content::Blocks(Children::from_parsed([head_only])),
        );
        assert_eq!(
            draw(&t, vec![table, lone], &cx()),
            "<table>\n<thead>\n<tr>\n<th>a</th>\n<th align=\"center\">b</th>\n\
             <th align=\"right\">c</th>\n</tr>\n</thead>\n\
             <tbody>\n<tr>\n<td>1</td>\n<td align=\"center\">2</td>\n\
             <td align=\"right\">3</td>\n</tr>\n</tbody>\n</table>\n\
             <table>\n<thead>\n<tr>\n<th align=\"left\">x</th>\n</tr>\n</thead>\n</table>\n"
        );
    }

    fn link<P: Host>(t: &mut Tree<P>, dest: &str, title: Option<&str>, label: &str) -> I<P> {
        let label = text(t, label);
        t.inline(
            InlineKind::Link {
                dest: dest.to_string(),
                title: title.map(str::to_string),
            },
            Children::from_parsed([label]),
        )
    }

    fn image<P: Host>(t: &mut Tree<P>, dest: &str, alt: Vec<I<P>>) -> I<P> {
        t.inline(
            InlineKind::Image {
                dest: dest.to_string(),
                title: None,
            },
            Children::from_parsed(alt),
        )
    }

    fn embed<P: Host>(t: &mut Tree<P>, target: &str, alt: Option<&str>) -> I<P> {
        t.leaf(InlineKind::Embed {
            target: target.to_string(),
            alt: alt.map(str::to_string),
            width: None,
            height: None,
        })
    }

    #[test]
    fn links_images_and_embeds_resolve_from_the_page_folder() {
        let mut t = Tree::<Plain>::new();
        let a = link(&mut t, "../x.md", Some("T"), "a");
        let b = link(&mut t, "../../../up.md", None, "b");
        let c = link(&mut t, "https://e/x?a=1&b='2' 3", None, "c");
        let alt_text = text(&mut t, "i ");
        let alt_code = t.leaf(InlineKind::Code("c".to_string()));
        let i = image(&mut t, "p.png", vec![alt_text, alt_code]);
        let o_alt = text(&mut t, "o");
        let o = image(&mut t, "../../../o.png", vec![o_alt]);
        let q = embed(&mut t, "/q.svg", None);
        let r = embed(&mut t, "../../../r.png", Some("R"));
        let p = para(&mut t, vec![a, b, c, i, o, q, r]);
        assert_eq!(
            draw(&t, vec![p], &cx()),
            "<p><a href=\"/b/Projects/x\" title=\"T\">a</a>\
             <a class=\"unresolved\">b</a>\
             <a href=\"https://e/x?a=1&amp;b=&#x27;2&#x27;%203\">c</a>\
             <img src=\"/b/Projects/Alpha/p.png\" alt=\"i c\" />\
             <img alt=\"o\" />\
             <img src=\"/b/q.svg\" alt=\"/q.svg\" />\
             <img alt=\"R\" /></p>\n"
        );
    }

    #[test]
    fn embeds_resolved_in_the_context_draw_as_resolved() {
        let mut t = Tree::<Plain>::new();
        let found = embed(&mut t, "pic.png", None);
        let missing = embed(&mut t, "gone.png", Some("Gone"));
        let mut cx = cx();
        cx.embeds = HashMap::from([
            (
                found.id,
                WikiHref {
                    href: "/b/assets/pic.png".to_string(),
                    resolved: true,
                },
            ),
            (
                missing.id,
                WikiHref {
                    href: "/b/_wiki/link?to=gone.png".to_string(),
                    resolved: false,
                },
            ),
        ]);
        let p = para(&mut t, vec![found, missing]);
        assert_eq!(
            draw(&t, vec![p], &cx),
            "<p><img src=\"/b/assets/pic.png\" alt=\"pic.png\" />\
             <a href=\"/b/_wiki/link?to=gone.png\" class=\"wikilink unresolved\">Gone</a></p>\n"
        );
    }

    #[test]
    fn wikilinks_and_tags_take_ordinals_from_the_context() {
        let mut t = Tree::<Plain>::new();
        let wl = |t: &mut Tree<Plain>, target: &str, label: Option<&str>| {
            t.leaf(InlineKind::WikiLink {
                target: target.to_string(),
                label: label.map(str::to_string),
            })
        };
        let a = wl(&mut t, "A", None);
        let head = wl(&mut t, "#Head", None);
        let missing = wl(&mut t, "Gone#x", Some("the label"));
        let tag = t.leaf(InlineKind::Tag("Status/Open".to_string()));
        let ordinals = HashMap::from([(a.id, 0), (head.id, 1), (missing.id, 2), (tag.id, 0)]);
        let p = para(&mut t, vec![a, head, missing, tag]);
        let mut cx = cx();
        cx.wikilinks = vec![
            Some(WikiHref {
                href: "/b/A".to_string(),
                resolved: true,
            }),
            None,
            Some(WikiHref {
                href: "/b/_wiki/link?to=Gone".to_string(),
                resolved: false,
            }),
        ];
        cx.ordinals = ordinals;
        assert_eq!(
            draw(&t, vec![p], &cx),
            "<p><a href=\"/b/A\" id=\"wl-0\" class=\"wikilink\">A</a>\
             <a href=\"#head\" id=\"wl-1\" class=\"wikilink\">#Head</a>\
             <a href=\"/b/_wiki/link?to=Gone\" id=\"wl-2\" class=\"wikilink unresolved\">the label</a>\
             <a class=\"tag\" id=\"tag-0\" href=\"/b/_wiki/tag/status/open\">#Status/Open</a></p>\n"
        );
    }

    #[test]
    fn headings_draw_their_id_and_an_anchor() {
        let mut t = Tree::<Plain>::new();
        let a_text = text(&mut t, "Café & co");
        let a = t.block(
            BlockKind::Heading { level: 2 },
            Content::Inlines(Children::from_parsed([a_text])),
        );
        let b_text = text(&mut t, "No id");
        let b = t.block(
            BlockKind::Heading { level: 3 },
            Content::Inlines(Children::from_parsed([b_text])),
        );
        let mut cx = cx();
        cx.headings = HashMap::from([(a.id, "café--co".to_string())]);
        assert_eq!(
            draw(&t, vec![a, b], &cx),
            "<h2 id=\"café--co\">Café &amp; co<a class=\"anchor\" href=\"#caf%C3%A9--co\">#</a></h2>\n\
             <h3>No id</h3>\n"
        );
    }

    #[test]
    fn drawing_past_the_depth_limit_is_text() {
        let mut t = Tree::<Plain>::new();
        let mut node = text(&mut t, "<deep>");
        for _ in 0..100_000 {
            node = t.inline(InlineKind::Emph, Children::from_parsed([node]));
        }
        let quote = |t: &mut Tree<Plain>, inner: B<Plain>| {
            t.block(
                BlockKind::Quote,
                Content::Blocks(Children::from_parsed([inner])),
            )
        };
        let mut block = para(&mut t, vec![node]);
        for _ in 0..100_000 {
            block = quote(&mut t, block);
        }
        // The stack a tokio worker thread has.
        let html = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || draw(&t, vec![block], &cx()))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(html.matches("<blockquote>").count(), MAX_DEPTH + 1);
        assert!(!html.contains("<em>"), "{html}");
        assert!(html.contains("&lt;deep&gt;"));
    }

    struct TestHost;

    enum TestBlock {
        Callout(String),
    }

    enum TestInline {
        Ticket(String),
        Marker,
    }

    impl Host for TestHost {
        type Block = TestBlock;
        type Inline = TestInline;
        type Template = DefaultTemplate;
    }

    impl Element<TestHost> for TestBlock {
        fn draw(&self, cx: &DrawCtx<'_, TestHost>) -> Markup {
            match self {
                TestBlock::Callout(kind) => html! { aside class=(kind) { (cx.draw_children()) } },
            }
        }
    }

    impl Element<TestHost> for TestInline {
        fn draw(&self, cx: &DrawCtx<'_, TestHost>) -> Markup {
            match self {
                TestInline::Ticket(id) => {
                    html! { a.ticket href={ (cx.base_path()) "/" (id) } { (id) } }
                }
                TestInline::Marker => html! { span.marker { (cx.draw_children()) } },
            }
        }
    }

    #[test]
    fn host_elements_draw_around_their_children() {
        let mut t = Tree::<TestHost>::new();
        let ticket = t.leaf(InlineKind::Host(TestInline::Ticket("ABC-1".to_string())));
        let bold_text = text(&mut t, "foo");
        let bold = t.inline(InlineKind::Strong, Children::from_parsed([bold_text]));
        let rest = text(&mut t, " bar");
        let marker = t.inline(
            InlineKind::Host(TestInline::Marker),
            Children::from_parsed([bold, rest]),
        );
        let p = para(&mut t, vec![ticket, marker]);
        let callout = t.block(
            BlockKind::Host(TestBlock::Callout("note".to_string())),
            Content::Blocks(Children::from_parsed([p])),
        );
        assert_eq!(
            draw(&t, vec![callout], &cx()),
            "<aside class=\"note\"><p><a class=\"ticket\" href=\"/b/ABC-1\">ABC-1</a>\
             <span class=\"marker\"><strong>foo</strong> bar</span></p>\n</aside>"
        );
    }

    #[test]
    fn raw_html_takes_its_line_on_its_first_open_tag() {
        for (html, want) in [
            (
                "<div class=\"a\">x</div>\n",
                "<div data-line=\"7\" class=\"a\">x</div>\n",
            ),
            ("  <table>\n", "  <table data-line=\"7\">\n"),
            ("<br/>", "<br data-line=\"7\"/>"),
            ("<my-el>", "<my-el data-line=\"7\">"),
            // Not an open tag: left as it is.
            ("<!-- c -->\n", "<!-- c -->\n"),
            ("</div>\n", "</div>\n"),
            ("text <b>x</b>", "text <b>x</b>"),
            ("<a@b>", "<a@b>"),
            ("", ""),
        ] {
            assert_eq!(with_line(html, 7), want, "{html:?}");
        }
    }
}
