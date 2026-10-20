//! The page template: the library owns the page skeleton, and the host's
//! template fills its `<head>` and the htmx root in its `<body>`.

use std::future::Future;
use std::marker::PhantomData;

use axum::http::request::Parts;
use maud::{Markup, html};
use serde_json::Value;

use crate::draw::tag_href;
use crate::names::folder_url;
use crate::parse::Host;

/// A host's page template, the host's `P::Template`. Impls write `async fn`.
pub trait Template<P: Host>: Send + Sync + 'static {
    /// Markup added after the library's `<head>` contents. `ctx` can
    /// `set_title(…)` and `suppress_default_styles()`.
    fn head(&self, ctx: &mut HeadCtx<'_, P>) -> impl Future<Output = Markup> + Send;

    /// The contents of the htmx root. Must embed `content(ctx)`. A template
    /// that doesn't use the default layout places the built-in chrome itself
    /// (`ctx.search_box()`, `ctx.breadcrumbs()`, `ctx.copy_path()`,
    /// `ctx.tags()`, `ctx.frontmatter_table()`).
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
    FolderIndex,
    /// `query` as typed, which the search box shows.
    Search {
        query: String,
    },
    Tag,
    Disambiguation,
    NotFound,
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
    kind: &'a PageKind,
    request: &'a Parts,
    /// Base path without trailing slash (`""` at root).
    base: &'a str,
    /// The folder the breadcrumbs lead down to (`""` for the root alone), or
    /// `None` for no breadcrumbs.
    crumbs: Option<&'a str>,
    host: PhantomData<fn() -> P>,
}

impl<'a, P: Host> BodyCtx<'a, P> {
    pub(crate) fn new(
        kind: &'a PageKind,
        request: &'a Parts,
        base: &'a str,
        crumbs: Option<&'a str>,
    ) -> Self {
        BodyCtx {
            kind,
            request,
            base,
            crumbs,
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

    /// The search form, holding the query on a search page.
    pub fn search_box(&self) -> Markup {
        let query = match self.kind {
            PageKind::Search { query } => query.as_str(),
            _ => "",
        };
        html! {
            form role="search" action={ (self.base) "/_wiki/search" } method="get" {
                input type="search" name="q" placeholder="Search" value=(query);
            }
        }
    }

    /// Links to the root and each folder down to the page's; empty when the
    /// page has none.
    pub fn breadcrumbs(&self) -> Markup {
        let Some(folder) = self.crumbs else {
            return html! {};
        };
        let base = self.base;
        let mut path = String::new();
        let mut trail = Vec::new();
        for segment in folder.split('/').filter(|s| !s.is_empty()) {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(segment);
            trail.push((format!("{base}{}", folder_url(&path)), segment));
        }
        html! {
            nav {
                a href={ (base) "/" } { "Index" }
                @for (href, name) in &trail {
                    " / " a href=(href) { (name) }
                }
            }
        }
    }

    /// The document's tag links; empty on other pages and untagged documents.
    pub fn tags(&self) -> Markup {
        let tags = match self.kind {
            PageKind::Document(meta) => meta.tags(),
            _ => &[],
        };
        if tags.is_empty() {
            return html! {};
        }
        html! {
            p.tags {
                @for (i, tag) in tags.iter().enumerate() {
                    @if i > 0 { " " }
                    a.tag href=(tag_href(self.base, tag)) { "#" (tag) }
                }
            }
        }
    }

    /// A button that copies the document's path from the wiki root, for
    /// pasting to an agent; empty on other pages.
    pub fn copy_path(&self) -> Markup {
        let PageKind::Document(meta) = self.kind else {
            return html! {};
        };
        html! {
            button.copy-path type="button" data-path=(meta.path()) title={ "Copy " (meta.path()) } {
                "Copy path"
            }
        }
    }

    /// The document's frontmatter as a collapsed table; empty when there is
    /// none.
    pub fn frontmatter_table(&self) -> Markup {
        let PageKind::Document(meta) = self.kind else {
            return html! {};
        };
        let Some(Value::Object(map)) = meta.frontmatter() else {
            return html! {};
        };
        html! {
            details {
                summary { "Properties" }
                table {
                    @for (key, value) in map {
                        tr { th { (key) } td { (value_html(value)) } }
                    }
                }
            }
        }
    }
}

/// A scalar as text, a list of scalars joined with commas, anything else as
/// YAML in `<code>`.
fn value_html(value: &Value) -> Markup {
    if let Some(text) = scalar_text(value) {
        return html! { (text) };
    }
    if let Value::Array(items) = value
        && let Some(texts) = items.iter().map(scalar_text).collect::<Option<Vec<_>>>()
    {
        return html! { (texts.join(", ")) };
    }
    let yaml = serde_norway::to_string(value)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    html! { code { (yaml) } }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(_) | Value::Object(_) => None,
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
        let ctx = BodyCtx::<Plain>::new(&kind, &request, "/b", Some("Projects/Alpha"));
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
        let ctx = BodyCtx::<Plain>::new(&kind, &request, "", None);
        let html = DefaultTemplate.body(&ctx, &content).await.into_string();
        assert_eq!(
            html,
            "<form role=\"search\" action=\"/_wiki/search\" method=\"get\">\
             <input type=\"search\" name=\"q\" placeholder=\"Search\" value=\"q&quot;\"></form>\
             <main><p>body</p></main>"
        );
    }
}
