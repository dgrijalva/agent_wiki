//! The document set as data: folders and documents, loaded from the files
//! and the index. What the built-in pages are drawn from, and what a host
//! builds its own pages and dashboards on.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Result, bail};
use indextree::NodeId;
use maud::Markup;
use serde_json::Value;
use tokio::sync::watch;

use crate::Wiki;
use crate::document::{DocumentTree, Extracted, OutlineHeading, slug};
use crate::draw::{DrawCtx, DrawCx, Element, WikiHref, draw_blocks};
use crate::entity::{Tag, Wikilink};
use crate::index::{FsKind, Resolution, fs_kind};
use crate::names::{LinkTarget, encode_component, folder_url, path_to_url};
use crate::parse::{Host, parse};
use crate::query::filter::eq;
use crate::signals::DocumentChange;
use crate::tree::{BlockKind, Children, InlineKind, KindRef, NodeRef};

/// What is at a path: a folder or a document.
pub enum Entry<P: Host> {
    Folder(Folder<P>),
    Document(Box<Document<P>>),
}

/// A folder of the wiki; the root is the folder at `""`. Its contents come
/// from the index.
pub struct Folder<P: Host> {
    wiki: Wiki<P>,
    path: String,
}

// Derived `Clone` would require `P: Clone`.
impl<P: Host> Clone for Folder<P> {
    fn clone(&self) -> Self {
        Folder {
            wiki: self.wiki.clone(),
            path: self.path.clone(),
        }
    }
}

/// How a search is run.
#[derive(Debug, Clone, Default)]
pub struct SearchOpts {
    /// The most results to return; all of them when `None`.
    pub limit: Option<usize>,
}

impl<P: Host> Folder<P> {
    pub(crate) fn new(wiki: Wiki<P>, path: String) -> Self {
        Folder { wiki, path }
    }

    /// The folder's path from the root, without slashes at either end: `""`
    /// for the root, e.g. `Projects/Alpha`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The last segment of the path; `""` for the root.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }

    /// The folder's URL path under the base, ending in `/`, e.g.
    /// `/Projects/Alpha/`.
    pub fn url(&self) -> String {
        folder_url(&self.path)
    }

    /// Immediate subfolders holding at least one document at any depth,
    /// sorted case-insensitively.
    pub async fn folders(&self) -> Result<Vec<Folder<P>>> {
        let (folders, _) = self.wiki.inner.index.folder_listing(&self.path).await?;
        Ok(folders
            .into_iter()
            .map(|name| Folder::new(self.wiki.clone(), join(&self.path, &name)))
            .collect())
    }

    /// The documents directly in the folder, sorted case-insensitively by
    /// title.
    pub async fn documents(&self) -> Result<Vec<Document<P>>> {
        let (_, pages) = self.wiki.inner.index.folder_listing(&self.path).await?;
        load_all(&self.wiki, pages.into_iter().map(|p| p.path)).await
    }

    /// The `n` most recently edited documents in the folder or below it,
    /// newest first.
    pub async fn recent(&self, n: usize) -> Result<Vec<Document<P>>> {
        let pages = self.wiki.inner.index.recent(&self.path, n).await?;
        load_all(&self.wiki, pages.into_iter().map(|(p, _)| p.path)).await
    }

    /// The `n` tags on the most documents in the folder or below it, in
    /// alphabetical order, each with how many documents carry it.
    pub async fn tags(&self, n: usize) -> Result<Vec<(String, usize)>> {
        self.wiki.inner.index.tag_cloud(&self.path, n).await
    }

    /// Documents in the folder or below it matching `query`, best first:
    /// title matches, then tag matches, then body matches.
    pub async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<Document<P>>> {
        let prefix = if self.path.is_empty() {
            String::new()
        } else {
            format!("{}/", self.path)
        };
        let pages = self.wiki.inner.index.search(query).await?;
        let paths = pages
            .into_iter()
            .map(|p| p.path)
            .filter(|path| path.starts_with(&prefix))
            .take(opts.limit.unwrap_or(usize::MAX));
        load_all(&self.wiki, paths).await
    }

    /// A watch on the folder's whole subtree: its value counts the changes
    /// seen since the watch was made, so it says that something under the
    /// folder changed, not what.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.wiki.inner.signals.watch_folder(&self.path)
    }

    /// Every document in the folder or below it, by path, loaded one at a
    /// time.
    pub async fn walk(&self) -> Result<Walk<P>> {
        let mut paths: Vec<String> = self
            .wiki
            .inner
            .index
            .mtimes_under(&self.path)
            .await?
            .into_keys()
            .collect();
        paths.sort();
        Ok(Walk {
            wiki: self.wiki.clone(),
            paths: paths.into(),
        })
    }
}

/// The documents of [`Folder::walk`].
pub struct Walk<P: Host> {
    wiki: Wiki<P>,
    paths: VecDeque<String>,
}

impl<P: Host> Walk<P> {
    /// The next document; `None` once every one has been loaded. A document
    /// whose file is gone by the time it is reached is skipped.
    pub async fn next(&mut self) -> Option<Result<Document<P>>> {
        while let Some(path) = self.paths.pop_front() {
            match Document::load(&self.wiki, &path).await {
                Ok(Some(doc)) => return Some(Ok(doc)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
        None
    }
}

/// A document, parsed when it is loaded.
pub struct Document<P: Host> {
    wiki: Wiki<P>,
    path: String,
    /// Shared with the document's chunks.
    tree: Arc<DocumentTree<P>>,
    ex: Extracted,
    /// Lowercase, sorted; empty until the document is indexed.
    tags: Vec<String>,
    /// `None` until the document is indexed.
    edited: Option<i64>,
}

impl<P: Host> Document<P> {
    /// Reads and parses the document at `path` (with `.md`), if it is a
    /// file reached without symlinks.
    pub(crate) async fn load(wiki: &Wiki<P>, path: &str) -> Result<Option<Document<P>>> {
        let root = &wiki.inner.root;
        if !matches!(fs_kind(root, path).await, Ok(FsKind::File)) {
            return Ok(None);
        }
        let Ok(source) = tokio::fs::read_to_string(root.join(path)).await else {
            return Ok(None);
        };
        let tree = parse(&*wiki.inner.host, &source);
        let ex = tree.extract();
        let edited = wiki
            .inner
            .index
            .details(&[path.to_string()])
            .await?
            .remove(path)
            .map(|details| details.edited);
        let records = wiki.query::<Tag>().document(path).fetch().await?;
        let mut tags: Vec<String> = records.into_iter().map(|t| t.into_record().tag).collect();
        tags.sort();
        Ok(Some(Document {
            wiki: wiki.clone(),
            path: path.to_string(),
            tree: Arc::new(tree),
            ex,
            tags,
            edited,
        }))
    }

    /// The file's path from the root, e.g. `Notes/Daily.md`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The page's URL path under the base, e.g. `/Notes/Daily`.
    pub fn url(&self) -> String {
        path_to_url(&self.path)
    }

    /// The folder holding the document; `""` for the root.
    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(folder, _)| folder)
    }

    /// The frontmatter title, else the first heading, else the file name.
    pub fn title(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        self.ex.title(name.strip_suffix(".md").unwrap_or(name))
    }

    /// `None` when the document has no frontmatter, or it isn't well formed.
    pub fn frontmatter(&self) -> Option<&Value> {
        self.tree.frontmatter()
    }

    /// Inline and frontmatter tags, lowercase and sorted, from the index.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// The frontmatter description or summary, else the first paragraph's
    /// text, cut short; `""` when there is none.
    pub fn summary(&self) -> String {
        self.ex.summary()
    }

    /// When the document was last edited, in seconds since the epoch, from
    /// the index; `None` until it is indexed.
    pub fn edited(&self) -> Option<i64> {
        self.edited
    }

    /// Every heading, in document order.
    pub fn outline(&self) -> &[OutlineHeading] {
        &self.ex.outline
    }

    /// A watch on the document: its value is how it last changed, `None`
    /// until it does.
    pub fn watch(&self) -> watch::Receiver<Option<DocumentChange>> {
        self.wiki.inner.signals.watch_document(&self.path)
    }

    /// The parsed tree.
    pub fn tree(&self) -> &DocumentTree<P> {
        &self.tree
    }

    /// The document's content as an element, with its wikilinks resolved
    /// against the index (one lookup per link).
    pub async fn body(&self) -> Result<DocumentBody<'_, P>> {
        let mut wikilinks = Vec::with_capacity(self.ex.wikilinks.len());
        for raw in &self.ex.wikilinks {
            wikilinks.push(resolve_href(&self.wiki, raw).await?);
        }
        Ok(DocumentBody {
            doc: self,
            cx: DrawCx {
                base: self.wiki.inner.base.clone(),
                folder: self.folder().to_string(),
                wikilinks,
                ordinals: self.ex.ordinals.clone(),
                headings: self.ex.headings.clone(),
                lines: true,
                page: None,
            },
        })
    }

    /// The top-level heading with id `anchor`, and every block after it up
    /// to the next heading of the same or a higher level. `None` when there
    /// is no such heading, or it is inside a block quote or list item.
    pub fn section(&self, anchor: &str) -> Option<Section<'_, P>> {
        let tree = &self.tree.tree;
        let body = &self.tree.body.ids;
        let start = body
            .iter()
            .position(|id| self.ex.headings.get(id).is_some_and(|a| a == anchor))?;
        let level = heading_level(tree, body[start])?;
        let end = body[start + 1..]
            .iter()
            .position(|&id| heading_level(tree, id).is_some_and(|l| l <= level))
            .map_or(body.len(), |n| start + 1 + n);
        Some(Section {
            blocks: body[start..end]
                .iter()
                .map(|&id| NodeRef::new(tree, id))
                .collect(),
        })
    }

    /// `nodes`, all of this document, as an element to draw on another
    /// page: their wikilinks resolved against the index (one lookup per
    /// link in them), in-page links (`#…`) pointing to this document's
    /// page, and no `data-line`. Fails when a node is of another document.
    pub async fn chunk<'a>(
        &'a self,
        nodes: impl IntoIterator<Item = NodeRef<'a, P>>,
    ) -> Result<Chunk<P>> {
        let tree = &self.tree.tree;
        let mut ids = Vec::new();
        for node in nodes {
            if !std::ptr::eq(node.tree, tree) {
                bail!("a node of another document, in a chunk of {}", self.path);
            }
            ids.push(node.id);
        }
        let mut ordinals = HashMap::new();
        let mut headings = HashMap::new();
        // Only the chunk's wikilinks are resolved; the entries for the rest
        // are never read.
        let mut wikilinks = vec![None; self.ex.wikilinks.len()];
        for node in ids.iter().flat_map(|id| id.descendants(&tree.arena)) {
            if let Some(&n) = self.ex.ordinals.get(&node) {
                ordinals.insert(node, n);
                if matches!(
                    NodeRef::new(tree, node).kind(),
                    KindRef::Inline(InlineKind::WikiLink { .. })
                ) {
                    wikilinks[n] = resolve_href(&self.wiki, &self.ex.wikilinks[n]).await?;
                }
            }
            if let Some(slug) = self.ex.headings.get(&node) {
                headings.insert(node, slug.clone());
            }
        }
        let base = &self.wiki.inner.base;
        Ok(Chunk {
            tree: self.tree.clone(),
            blocks: Children::new(ids),
            cx: DrawCx {
                base: base.clone(),
                folder: self.folder().to_string(),
                wikilinks,
                ordinals,
                headings,
                lines: false,
                page: Some(format!("{base}{}", self.url())),
            },
        })
    }
}

/// A heading's level; `None` for any other node.
fn heading_level<P: Host>(tree: &crate::tree::Tree<P>, id: NodeId) -> Option<u8> {
    match NodeRef::new(tree, id).kind() {
        KindRef::Block(BlockKind::Heading { level }) => Some(*level),
        _ => None,
    }
}

/// A heading and the blocks in its section; from [`Document::section`].
/// Iterates as its [`blocks`](Section::blocks), so it can be passed straight
/// to [`Document::chunk`].
pub struct Section<'a, P: Host> {
    /// The heading first.
    blocks: Vec<NodeRef<'a, P>>,
}

impl<'a, P: Host> Section<'a, P> {
    pub fn heading(&self) -> NodeRef<'a, P> {
        self.blocks[0]
    }

    /// The heading, then every block in its section, subsections included.
    pub fn blocks(&self) -> impl Iterator<Item = NodeRef<'a, P>> + use<'_, 'a, P> {
        self.blocks.iter().copied()
    }
}

impl<'a, P: Host> IntoIterator for Section<'a, P> {
    type Item = NodeRef<'a, P>;
    type IntoIter = std::vec::IntoIter<NodeRef<'a, P>>;

    fn into_iter(self) -> Self::IntoIter {
        self.blocks.into_iter()
    }
}

/// Part of a document, drawn off its own page; from [`Document::chunk`].
/// Owns what it draws from.
pub struct Chunk<P: Host> {
    tree: Arc<DocumentTree<P>>,
    blocks: Children<BlockKind<P>>,
    cx: DrawCx,
}

impl<P: Host> Element<P> for Chunk<P> {
    fn draw(&self, _: &DrawCtx<'_, P>) -> Markup {
        draw_blocks(&self.tree.tree, &self.blocks, &self.cx)
    }
}

/// A document's content, drawn from its tree; from [`Document::body`].
pub struct DocumentBody<'a, P: Host> {
    doc: &'a Document<P>,
    cx: DrawCx,
}

impl<P: Host> Element<P> for DocumentBody<'_, P> {
    fn draw(&self, _: &DrawCtx<'_, P>) -> Markup {
        draw_blocks(&self.doc.tree.tree, &self.doc.tree.body, &self.cx)
    }
}

impl<P: Host> Wiki<P> {
    /// Draws an element outside any page, e.g. for an HTML fragment a host
    /// serves itself.
    pub fn draw(&self, element: &impl Element<P>) -> Markup {
        element.draw(&DrawCtx::detached(&self.inner.base))
    }

    /// The root folder.
    pub fn root(&self) -> Folder<P> {
        Folder::new(self.clone(), String::new())
    }

    /// The folder or document at `path`, relative to the root; a document's
    /// `.md` is optional. `None` when there is neither, or the path has an
    /// empty, hidden or `..` segment, or reaches through a symlink.
    pub async fn lookup(&self, path: &str) -> Result<Option<Entry<P>>> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(Some(Entry::Folder(self.root())));
        }
        if !path
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'))
        {
            return Ok(None);
        }
        let root = &self.inner.root;
        if matches!(fs_kind(root, path).await, Ok(FsKind::Dir)) {
            return Ok(Some(Entry::Folder(Folder::new(
                self.clone(),
                path.to_string(),
            ))));
        }
        let rel = if path.ends_with(".md") {
            path.to_string()
        } else {
            format!("{path}.md")
        };
        Ok(Document::load(self, &rel)
            .await?
            .map(|doc| Entry::Document(Box::new(doc))))
    }

    /// The root folder's [`Folder::search`].
    pub async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<Document<P>>> {
        self.root().search(query, opts).await
    }

    /// Documents carrying `tag` (case-insensitive), sorted by title, each
    /// with the ordinal of its first inline occurrence (`None` when the tag
    /// is only in frontmatter), which the page draws as `#tag-{n}`.
    pub async fn tagged(&self, tag: &str) -> Result<Vec<(Document<P>, Option<i64>)>> {
        let tagged = self
            .query::<Tag>()
            .filter(eq("tag", tag.to_lowercase()))
            .fetch()
            .await?;
        let mut out = Vec::with_capacity(tagged.len());
        for tag in tagged {
            if let Some(doc) = Document::load(self, tag.path()).await? {
                out.push((doc, tag.record().first));
            }
        }
        out.sort_by_cached_key(|(doc, _)| doc.title().to_lowercase());
        Ok(out)
    }

    /// Where the wikilink `raw` (its target, e.g. `Foo` or `Project/Foo#Bar`)
    /// leads: the documents it matches, and the documents linking to it,
    /// each with the ordinal of its first such link (drawn as `#wl-{n}`).
    /// `None` when `raw` isn't a link target.
    pub async fn link_target(&self, raw: &str) -> Result<Option<LinkPage<P>>> {
        let Some(target) = LinkTarget::parse(raw) else {
            return Ok(None);
        };
        let index = &self.inner.index;
        let matches = match index.resolve(&target).await? {
            Resolution::Unique(page) => vec![page],
            Resolution::Ambiguous(pages) => pages,
            Resolution::Missing => Vec::new(),
        };
        let matches = load_all(self, matches.into_iter().map(|p| p.path)).await?;
        // In page order, then document order: a page's first link is first.
        let links = self
            .query::<Wikilink>()
            .filter(eq("key", target.name_key.clone()))
            .fetch()
            .await?;
        let mut references: Vec<(Document<P>, i64)> = Vec::new();
        for link in links {
            if references
                .last()
                .is_some_and(|(doc, _)| doc.path() == link.path())
            {
                continue;
            }
            if let Some(doc) = Document::load(self, link.path()).await? {
                references.push((doc, link.ordinal()));
            }
        }
        references.sort_by_cached_key(|(doc, _)| doc.title().to_lowercase());
        Ok(Some(LinkPage {
            anchor: heading_anchor(&target),
            raw: target.raw,
            matches,
            references,
        }))
    }
}

/// Where a wikilink leads; from [`Wiki::link_target`].
pub struct LinkPage<P: Host> {
    raw: String,
    anchor: String,
    matches: Vec<Document<P>>,
    references: Vec<(Document<P>, i64)>,
}

impl<P: Host> LinkPage<P> {
    /// The link's target as written.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// `#{slug}` of the target's heading, or `""`, for linking to a match.
    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    /// The documents the target matches.
    pub fn matches(&self) -> &[Document<P>] {
        &self.matches
    }

    /// The documents linking to the target, sorted by title, each with the
    /// ordinal of its first such link.
    pub fn references(&self) -> &[(Document<P>, i64)] {
        &self.references
    }
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// The documents at `paths`, in order, skipping any whose file is gone.
async fn load_all<P: Host>(
    wiki: &Wiki<P>,
    paths: impl Iterator<Item = String>,
) -> Result<Vec<Document<P>>> {
    let mut docs = Vec::new();
    for path in paths {
        if let Some(doc) = Document::load(wiki, &path).await? {
            docs.push(doc);
        }
    }
    Ok(docs)
}

pub(crate) async fn resolve_href<P: Host>(wiki: &Wiki<P>, raw: &str) -> Result<Option<WikiHref>> {
    let Some(target) = LinkTarget::parse(raw) else {
        return Ok(None);
    };
    let base = &wiki.inner.base;
    Ok(Some(match wiki.inner.index.resolve(&target).await? {
        Resolution::Unique(page) => WikiHref {
            href: format!("{base}{}{}", page.url, heading_anchor(&target)),
            resolved: true,
        },
        // The fragment rides along, for the disambiguation page's links.
        Resolution::Ambiguous(_) | Resolution::Missing => WikiHref {
            href: link_href(base, raw.trim()),
            resolved: false,
        },
    }))
}

/// `#{slug}` of the target's heading fragment, or `""` for none (or an
/// empty slug).
fn heading_anchor(target: &LinkTarget) -> String {
    match target.fragment.as_deref().map(slug) {
        Some(slug) if !slug.is_empty() => format!("#{slug}"),
        _ => String::new(),
    }
}

fn link_href(base: &str, target: &str) -> String {
    format!("{base}/_wiki/link?to={}", encode_component(target))
}
