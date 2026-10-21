//! The page template: the library owns the page skeleton, and the host's
//! template fills its `<head>` and the htmx root in its `<body>`.

use std::future::Future;
use std::marker::PhantomData;

use axum::http::request::Parts;
use maud::{Markup, html};
use serde_json::Value;

use crate::Wiki;
use crate::components::{Breadcrumbs, CopyPath, FrontmatterTable, SearchBox, TagLinks};
use crate::draw::{DrawCtx, Element};
use crate::parse::Host;

/// A host's page template, the host's `P::Template`. Impls write `async fn`.
pub trait Template<P: Host>: Send + Sync + 'static {
    /// Markup added after the library's `<head>` contents. `ctx` can
    /// `set_title(…)` and `suppress_default_styles()`.
    fn head(&self, ctx: &mut HeadCtx<'_, P>) -> impl Future<Output = Markup> + Send;

    /// The contents of the htmx root, normally embedding `content(ctx)`, the
    /// page's body. A template can draw its own body instead, from
    /// `ctx.kind()` and the data in `ctx.wiki()`, out of the library's
    /// components or its own. A template that doesn't use the default
    /// layout places the built-in chrome itself (`ctx.search_box()`,
    /// `ctx.breadcrumbs()`, `ctx.copy_path()`, `ctx.tags()`,
    /// `ctx.frontmatter_table()`).
    fn body(
        &self,
        ctx: &BodyCtx<'_, P>,
        content: &(dyn Fn(&BodyCtx<'_, P>) -> Markup + Sync),
    ) -> impl Future<Output = Markup> + Send;
}

/// What kind of page is being drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum PageKind {
    Document(DocumentMeta),
    /// The folder's path from the root; `""` for the root.
    FolderIndex {
        path: String,
    },
    /// `query` as typed, which the search box shows.
    Search {
        query: String,
    },
    /// The tag, lowercase.
    Tag {
        tag: String,
    },
    /// The wikilink target as written.
    Disambiguation {
        target: String,
    },
    NotFound,
    /// A host's own page, from [`Wiki::custom_page`](crate::Wiki::custom_page).
    Custom,
}

/// A document page's metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentMeta {
    title: String,
    path: String,
    url: String,
    frontmatter: Option<Value>,
    tags: Vec<String>,
}

impl DocumentMeta {
    pub(crate) fn new(
        title: String,
        path: String,
        url: String,
        frontmatter: Option<Value>,
        tags: Vec<String>,
    ) -> Self {
        DocumentMeta {
            title,
            path,
            url,
            frontmatter,
            tags,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The file's path from the root, e.g. `Notes/Daily.md`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The page's URL path under the base, e.g. `/Notes/Daily`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// `None` when the document has no frontmatter, or it isn't well formed.
    pub fn frontmatter(&self) -> Option<&Value> {
        self.frontmatter.as_ref()
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// What the head hook sees, and the changes it can make to the default head.
pub struct HeadCtx<'a, P: Host> {
    kind: &'a PageKind,
    request: &'a Parts,
    base: &'a str,
    title: String,
    suppress_default_styles: bool,
    host: PhantomData<fn() -> P>,
}

impl<'a, P: Host> HeadCtx<'a, P> {
    pub(crate) fn new(
        kind: &'a PageKind,
        request: &'a Parts,
        base: &'a str,
        title: String,
    ) -> Self {
        HeadCtx {
            kind,
            request,
            base,
            title,
            suppress_default_styles: false,
            host: PhantomData,
        }
    }

    pub fn kind(&self) -> &PageKind {
        self.kind
    }

    pub fn request(&self) -> &Parts {
        self.request
    }

    /// Where the wiki is mounted, without a trailing slash: `""` at the
    /// root, e.g. `"/docs"` otherwise.
    pub fn base_path(&self) -> &str {
        self.base
    }

    /// The page's `<title>`: the library's, until `set_title`.
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// Leaves out the library's built-in stylesheet. The highlighting
    /// stylesheets stay.
    pub fn suppress_default_styles(&mut self) {
        self.suppress_default_styles = true;
    }

    pub(crate) fn default_styles_suppressed(&self) -> bool {
        self.suppress_default_styles
    }
}

/// What the body hook sees, with the built-in chrome to place.
pub struct BodyCtx<'a, P: Host> {
    wiki: &'a Wiki<P>,
    kind: &'a PageKind,
    request: &'a Parts,
    /// Base path without trailing slash (`""` at root).
    base: &'a str,
    /// The folder the breadcrumbs lead down to (`""` for the root alone), or
    /// `None` for no breadcrumbs.
    crumbs: Option<&'a str>,
}

impl<'a, P: Host> BodyCtx<'a, P> {
    pub(crate) fn new(
        wiki: &'a Wiki<P>,
        kind: &'a PageKind,
        request: &'a Parts,
        crumbs: Option<&'a str>,
    ) -> Self {
        BodyCtx {
            wiki,
            kind,
            request,
            base: &wiki.inner.base,
            crumbs,
        }
    }

    /// The wiki, for the data a template draws its own page body from.
    pub fn wiki(&self) -> &Wiki<P> {
        self.wiki
    }

    pub fn kind(&self) -> &PageKind {
        self.kind
    }

    pub fn request(&self) -> &Parts {
        self.request
    }

    /// Where the wiki is mounted, without a trailing slash: `""` at the
    /// root, e.g. `"/docs"` otherwise.
    pub fn base_path(&self) -> &str {
        self.base
    }

    /// Draws an element that isn't in a tree, such as a page component.
    pub fn draw(&self, element: &impl Element<P>) -> Markup {
        element.draw(&DrawCtx::detached(self.base))
    }

    /// The search form, holding the query on a search page.
    pub fn search_box(&self) -> Markup {
        let query = match self.kind {
            PageKind::Search { query } => query.as_str(),
            _ => "",
        };
        self.draw(&SearchBox::new(query))
    }

    /// Links to the root and each folder down to the page's; empty when the
    /// page has none.
    pub fn breadcrumbs(&self) -> Markup {
        match self.crumbs {
            Some(folder) => self.draw(&Breadcrumbs::new(folder)),
            None => html! {},
        }
    }

    /// The document's tag links; empty on other pages and untagged documents.
    pub fn tags(&self) -> Markup {
        match self.kind {
            PageKind::Document(meta) => self.draw(&TagLinks::new(meta.tags().to_vec())),
            _ => html! {},
        }
    }

    /// A button that copies the document's path from the wiki root, for
    /// pasting to an agent; empty on other pages.
    pub fn copy_path(&self) -> Markup {
        match self.kind {
            PageKind::Document(meta) => self.draw(&CopyPath::new(meta.path())),
            _ => html! {},
        }
    }

    /// The document's frontmatter as a collapsed table; empty when there is
    /// none.
    pub fn frontmatter_table(&self) -> Markup {
        match self.kind {
            PageKind::Document(meta) => {
                self.draw(&FrontmatterTable::new(meta.frontmatter().cloned()))
            }
            _ => html! {},
        }
    }
}

/// Today's page: the search box, then a header with the breadcrumbs and tag
/// links, then the frontmatter table and the content.
pub struct DefaultTemplate;

impl<P: Host> Template<P> for DefaultTemplate {
    async fn head(&self, _ctx: &mut HeadCtx<'_, P>) -> Markup {
        html! {}
    }

    async fn body(
        &self,
        ctx: &BodyCtx<'_, P>,
        content: &(dyn Fn(&BodyCtx<'_, P>) -> Markup + Sync),
    ) -> Markup {
        let crumbs = ctx.breadcrumbs();
        let copy = ctx.copy_path();
        let tags = ctx.tags();
        html! {
            (ctx.search_box())
            @if !crumbs.0.is_empty() || !copy.0.is_empty() || !tags.0.is_empty() {
                header {
                    @if copy.0.is_empty() {
                        (crumbs)
                    } @else {
                        div.crumbs-row { (crumbs) (copy) }
                    }
                    (tags)
                }
            }
            main {
                (ctx.frontmatter_table())
                (content(ctx))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Plain;

    #[tokio::test]
    async fn default_template_lays_out_todays_page() {
        let (request, ()) = axum::http::Request::new(()).into_parts();
        let frontmatter = serde_json::json!({ "title": "T", "tags": ["a", "b"], "n": { "x": 1 } });
        let kind = PageKind::Document(DocumentMeta::new(
            "T".to_string(),
            "Projects/Alpha/T.md".to_string(),
            "/Projects/Alpha/T".to_string(),
            Some(frontmatter),
            vec!["a".to_string(), "b".to_string()],
        ));
        let dir = tempfile::tempdir().unwrap();
        let wiki = crate::Wiki::builder(dir.path())
            .base_path("/b")
            .build()
            .await
            .unwrap();
        let ctx = BodyCtx::<Plain>::new(&wiki, &kind, &request, Some("Projects/Alpha"));
        let content = |_: &BodyCtx<'_, Plain>| html! { p { "body" } };
        let html = DefaultTemplate.body(&ctx, &content).await.into_string();
        assert_eq!(
            html,
            "<form role=\"search\" action=\"/b/_wiki/search\" method=\"get\">\
             <input type=\"search\" name=\"q\" placeholder=\"Search\" value=\"\"></form>\
             <header><div class=\"crumbs-row\"><nav><a href=\"/b/\">Index</a> / \
             <a href=\"/b/Projects/\">Projects</a> / <a href=\"/b/Projects/Alpha/\">Alpha</a></nav>\
             <button class=\"copy-path\" type=\"button\" data-path=\"Projects/Alpha/T.md\" \
             title=\"Copy Projects/Alpha/T.md\">Copy path</button></div>\
             <p class=\"tags\"><a class=\"tag\" href=\"/b/_wiki/tag/a\">#a</a> \
             <a class=\"tag\" href=\"/b/_wiki/tag/b\">#b</a></p></header>\
             <main><details><summary>Properties</summary><table>\
             <tr><th>title</th><td>T</td></tr><tr><th>tags</th><td>a, b</td></tr>\
             <tr><th>n</th><td><code>x: 1</code></td></tr></table></details>\
             <p>body</p></main>"
        );

        // A search page's box holds its query.
        let kind = PageKind::Search {
            query: "q\"".to_string(),
        };
        let root = crate::Wiki::builder(dir.path()).build().await.unwrap();
        let ctx = BodyCtx::<Plain>::new(&root, &kind, &request, None);
        let html = DefaultTemplate.body(&ctx, &content).await.into_string();
        assert_eq!(
            html,
            "<form role=\"search\" action=\"/_wiki/search\" method=\"get\">\
             <input type=\"search\" name=\"q\" placeholder=\"Search\" value=\"q&quot;\"></form>\
             <main><p>body</p></main>"
        );
    }
}
