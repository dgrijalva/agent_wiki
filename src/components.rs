//! The library's page components and page bodies: elements built from the
//! data layer, which the built-in pages are made of and a host can place
//! on its own pages. Each one is loaded (async) and then drawn.

use anyhow::Result;
use maud::{Markup, html};
use serde_json::Value;

use crate::Wiki;
use crate::data::{Document, Folder, SearchOpts};
use crate::draw::{DrawCtx, Element, tag_href};
use crate::index::search_terms;
use crate::names::folder_url;
use crate::parse::Host;

/// How many documents a folder index's "Recently edited" shows.
const RECENT: usize = 10;
/// How many tags a folder index shows.
const TOP_TAGS: usize = 25;

/// The search form, holding a query.
pub struct SearchBox {
    query: String,
}

impl SearchBox {
    pub fn new(query: impl Into<String>) -> Self {
        SearchBox {
            query: query.into(),
        }
    }
}

impl<P: Host> Element<P> for SearchBox {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        html! {
            form role="search" action={ (cx.base_path()) "/_wiki/search" } method="get" {
                input type="search" name="q" placeholder="Search" value=(self.query);
            }
        }
    }
}

/// Links to the root and each folder down to `folder` (`""` for the root
/// alone).
pub struct Breadcrumbs {
    folder: String,
}

impl Breadcrumbs {
    pub fn new(folder: impl Into<String>) -> Self {
        Breadcrumbs {
            folder: folder.into(),
        }
    }
}

impl<P: Host> Element<P> for Breadcrumbs {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        let mut path = String::new();
        let mut trail = Vec::new();
        for segment in self.folder.split('/').filter(|s| !s.is_empty()) {
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
}

/// Tag links, each to its tag page; nothing for no tags.
pub struct TagLinks {
    tags: Vec<String>,
}

impl TagLinks {
    pub fn new(tags: Vec<String>) -> Self {
        TagLinks { tags }
    }
}

impl<P: Host> Element<P> for TagLinks {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        if self.tags.is_empty() {
            return html! {};
        }
        html! {
            p.tags {
                @for (i, tag) in self.tags.iter().enumerate() {
                    @if i > 0 { " " }
                    a.tag href=(tag_href(cx.base_path(), tag)) { "#" (tag) }
                }
            }
        }
    }
}

/// A button that copies a document's path from the wiki root, for pasting
/// to an agent.
pub struct CopyPath {
    path: String,
}

impl CopyPath {
    pub fn new(path: impl Into<String>) -> Self {
        CopyPath { path: path.into() }
    }
}

impl<P: Host> Element<P> for CopyPath {
    fn draw(&self, _: &DrawCtx<'_, P>) -> Markup {
        html! {
            button.copy-path type="button" data-path=(self.path) title={ "Copy " (self.path) } {
                "Copy path"
            }
        }
    }
}

/// Frontmatter as a collapsed table; nothing unless it is a non-empty
/// mapping.
pub struct FrontmatterTable {
    frontmatter: Option<Value>,
}

impl FrontmatterTable {
    pub fn new(frontmatter: Option<Value>) -> Self {
        FrontmatterTable { frontmatter }
    }
}

impl<P: Host> Element<P> for FrontmatterTable {
    fn draw(&self, _: &DrawCtx<'_, P>) -> Markup {
        let Some(Value::Object(map)) = &self.frontmatter else {
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

/// A document as the lists show it.
struct Listed {
    title: String,
    url: String,
    path: String,
    folder: String,
    edited: Option<i64>,
    summary: String,
    tags: Vec<String>,
}

impl Listed {
    fn of<P: Host>(doc: &Document<P>) -> Listed {
        Listed {
            title: doc.title(),
            url: doc.url(),
            path: doc.path().to_string(),
            folder: doc.folder().to_string(),
            edited: doc.edited(),
            summary: doc.summary(),
            tags: doc.tags().to_vec(),
        }
    }

    fn file(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// A folder's most recently edited documents, one tight line each: title,
/// path relative to the folder, edit date. Nothing when there are none.
pub struct RecentlyEdited {
    folder: String,
    docs: Vec<Listed>,
}

impl RecentlyEdited {
    /// The `n` most recently edited documents in `folder` or below it.
    pub async fn load<P: Host>(folder: &Folder<P>, n: usize) -> Result<Self> {
        let docs = folder.recent(n).await?;
        Ok(RecentlyEdited {
            folder: folder.path().to_string(),
            docs: docs.iter().map(Listed::of).collect(),
        })
    }
}

impl<P: Host> Element<P> for RecentlyEdited {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        html! {
            @if !self.docs.is_empty() {
                section.recent {
                    h2 { "Recently edited" }
                    ul {
                        @for doc in &self.docs {
                            li {
                                a href={ (base) (doc.url) } { (doc.title) }
                                span.file { (relative_path(&self.folder, &doc.path)) }
                                @if let Some(edited) = doc.edited {
                                    time { (utc_date(edited)) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A folder's listing: its subfolders, then its documents with title, file
/// name, edit date, summary and tags.
pub struct Contents {
    folder: String,
    folders: Vec<String>,
    docs: Vec<Listed>,
}

impl Contents {
    pub async fn load<P: Host>(folder: &Folder<P>) -> Result<Self> {
        let folders = folder.folders().await?;
        let docs = folder.documents().await?;
        Ok(Contents {
            folder: folder.path().to_string(),
            folders: folders.iter().map(|f| f.name().to_string()).collect(),
            docs: docs.iter().map(Listed::of).collect(),
        })
    }
}

impl<P: Host> Element<P> for Contents {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        html! {
            section.contents {
                h2 { "Contents" }
                ul.listing {
                    @for name in &self.folders {
                        @let path = if self.folder.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{name}", self.folder)
                        };
                        li.folder { a href={ (base) (folder_url(&path)) } { (name) "/" } }
                    }
                    @for doc in &self.docs {
                        (listing_entry(base, doc))
                    }
                }
            }
        }
    }
}

/// A document in the folder listing: its title, file name and edit date,
/// then its summary and tags when it has them.
fn listing_entry(base: &str, doc: &Listed) -> Markup {
    html! {
        li {
            div.entry-head {
                span {
                    a href={ (base) (doc.url) } { (doc.title) }
                    " " span.file { (doc.file()) }
                }
                @if let Some(edited) = doc.edited {
                    time { (utc_date(edited)) }
                }
            }
            @if !doc.summary.is_empty() {
                p.summary { (doc.summary) }
            }
            @if !doc.tags.is_empty() {
                p.tags {
                    @for tag in &doc.tags {
                        a.tag href=(tag_href(base, tag)) { "#" (tag) } " "
                    }
                }
            }
        }
    }
}

/// A folder's most used tags, each with its document count when above
/// one. Nothing when there are none.
pub struct TagCloud {
    tags: Vec<(String, usize)>,
}

impl TagCloud {
    /// The `n` tags on the most documents in `folder` or below it.
    pub async fn load<P: Host>(folder: &Folder<P>, n: usize) -> Result<Self> {
        Ok(TagCloud {
            tags: folder.tags(n).await?,
        })
    }
}

impl<P: Host> Element<P> for TagCloud {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        html! {
            @if !self.tags.is_empty() {
                section.top-tags {
                    h2 { "Tags" }
                    p {
                        @for (tag, count) in &self.tags {
                            a.tag href=(tag_href(cx.base_path(), tag)) {
                                "#" (tag)
                                @if *count > 1 { " " span.count { "(" (count) ")" } }
                            }
                            " "
                        }
                    }
                }
            }
        }
    }
}

/// A folder index page's content: the title, then recently edited, the
/// contents, and the tags.
pub struct FolderIndexBody {
    title: String,
    recent: RecentlyEdited,
    contents: Contents,
    tags: TagCloud,
}

impl FolderIndexBody {
    pub async fn load<P: Host>(folder: &Folder<P>) -> Result<Self> {
        let title = match folder.name() {
            "" => "Index",
            name => name,
        };
        Ok(FolderIndexBody {
            title: title.to_string(),
            recent: RecentlyEdited::load(folder, RECENT).await?,
            contents: Contents::load(folder).await?,
            tags: TagCloud::load(folder, TOP_TAGS).await?,
        })
    }

    /// The folder's name, or "Index" at the root.
    pub fn title(&self) -> &str {
        &self.title
    }
}

impl<P: Host> Element<P> for FolderIndexBody {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        html! {
            h1 { (self.title) }
            div.index {
                (Element::<P>::draw(&self.recent, cx))
                (Element::<P>::draw(&self.contents, cx))
                (Element::<P>::draw(&self.tags, cx))
            }
        }
    }
}

/// A search page's content: the query, then the results. No results list
/// for a query with no words.
pub struct SearchBody {
    query: String,
    results: Option<Vec<Listed>>,
}

impl SearchBody {
    pub async fn load<P: Host>(wiki: &Wiki<P>, query: &str) -> Result<Self> {
        let results = if search_terms(query).is_empty() {
            None
        } else {
            let docs = wiki.search(query, SearchOpts::default()).await?;
            Some(docs.iter().map(Listed::of).collect())
        };
        Ok(SearchBody {
            query: query.to_string(),
            results,
        })
    }

    /// `Search: {query}`.
    pub fn title(&self) -> String {
        format!("Search: {}", self.query)
    }
}

impl<P: Host> Element<P> for SearchBody {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        html! {
            h1 { (self.title()) }
            @match &self.results {
                Some(results) if results.is_empty() => p { "No results" },
                Some(results) => ul {
                    @for doc in results {
                        li { a href={ (base) (doc.url) } { (doc.title) } (folder_note(doc)) }
                    }
                },
                None => {},
            }
        }
    }
}

/// A tag page's content: the documents carrying the tag, each linked to
/// its first inline use.
pub struct TagBody {
    tag: String,
    docs: Vec<(Listed, String)>,
}

impl TagBody {
    pub async fn load<P: Host>(wiki: &Wiki<P>, tag: &str) -> Result<Self> {
        let tagged = wiki.tagged(tag).await?;
        let docs = tagged
            .iter()
            .map(|(doc, ordinal)| {
                let anchor = ordinal.map(|n| format!("#tag-{n}")).unwrap_or_default();
                (Listed::of(doc), anchor)
            })
            .collect();
        Ok(TagBody {
            tag: tag.to_lowercase(),
            docs,
        })
    }

    /// `#{tag}`, lowercase.
    pub fn title(&self) -> String {
        format!("#{}", self.tag)
    }
}

impl<P: Host> Element<P> for TagBody {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        html! {
            h1 { (self.title()) }
            ul {
                @for (doc, anchor) in &self.docs {
                    li { a href={ (base) (doc.url) (anchor) } { (doc.title) } }
                }
            }
        }
    }
}

/// A disambiguation page's content: the documents a wikilink matches, and
/// the documents linking to it, each linked to its first such link.
pub struct DisambiguationBody {
    raw: String,
    anchor: String,
    matches: Vec<Listed>,
    references: Vec<(Listed, i64)>,
}

impl DisambiguationBody {
    /// `None` when `raw` isn't a link target.
    pub async fn load<P: Host>(wiki: &Wiki<P>, raw: &str) -> Result<Option<Self>> {
        let Some(link) = wiki.link_target(raw).await? else {
            return Ok(None);
        };
        Ok(Some(DisambiguationBody {
            raw: link.raw().to_string(),
            anchor: link.anchor().to_string(),
            matches: link.matches().iter().map(Listed::of).collect(),
            references: link
                .references()
                .iter()
                .map(|(doc, n)| (Listed::of(doc), *n))
                .collect(),
        }))
    }

    /// The link's target as written.
    pub fn title(&self) -> &str {
        &self.raw
    }
}

impl<P: Host> Element<P> for DisambiguationBody {
    fn draw(&self, cx: &DrawCtx<'_, P>) -> Markup {
        let base = cx.base_path();
        html! {
            h1 { (self.raw) }
            @if !self.matches.is_empty() {
                h2 { "Matching pages" }
                ul {
                    @for doc in &self.matches {
                        li {
                            a href={ (base) (doc.url) (self.anchor) } { (doc.title) }
                            (folder_note(doc))
                        }
                    }
                }
            }
            @if !self.references.is_empty() {
                h2 { "Pages that link here" }
                ul {
                    @for (doc, ordinal) in &self.references {
                        li { a href={ (base) (doc.url) "#wl-" (ordinal) } { (doc.title) } }
                    }
                }
            }
        }
    }
}

/// A not-found page's content.
pub struct NotFoundBody;

impl<P: Host> Element<P> for NotFoundBody {
    fn draw(&self, _: &DrawCtx<'_, P>) -> Markup {
        html! { h1 { "Not found" } }
    }
}

/// The document's folder in muted text, or nothing at the root.
fn folder_note(doc: &Listed) -> Markup {
    html! {
        @if !doc.folder.is_empty() {
            " " span.muted { (doc.folder) }
        }
    }
}

/// `path` relative to `folder` (`""` is the root), which holds it.
fn relative_path<'a>(folder: &str, path: &'a str) -> &'a str {
    if folder.is_empty() {
        return path;
    }
    path.strip_prefix(folder)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
}

/// `YYYY-MM-DD` in UTC for `secs` since the epoch.
fn utc_date(secs: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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

#[cfg(test)]
mod tests {
    use super::utc_date;

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_791_225_600), "2026-10-05");
        assert_eq!(utc_date(1_791_158_399), "2026-10-04");
    }
}
