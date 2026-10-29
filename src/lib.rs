mod components;
mod data;
mod document;
mod draw;
mod entity;
mod index;
mod links;
mod mcp;
mod names;
mod parse;
mod query;
mod reference;
mod root;
mod signals;
mod tabs;
mod template;
mod tree;
mod watch;
mod web;

// The derive's generated code names `::agent_wiki`, here too.
extern crate self as agent_wiki;

pub use agent_wiki_derive::EntityType;
pub use maud;
pub use nom;
pub use rmcp;

pub use crate::components::{
    Breadcrumbs, Contents, CopyPath, DisambiguationBody, FolderIndexBody, FrontmatterTable,
    NotFoundBody, RecentlyEdited, SearchBody, SearchBox, TableOfContents, TagBody, TagCloud,
    TagLinks,
};
pub use crate::data::{
    Chunk, Document, DocumentBody, Entry, Folder, LinkPage, SearchOpts, Section, Walk,
};
pub use crate::document::{DocumentTree, OutlineHeading};
pub use crate::draw::{DrawChildren, DrawCtx, Element};
pub use crate::entity::{
    DecodeError, EntityField, EntityType, Field, FieldKind, FieldValue, Records, Tag, Wikilink,
};
pub use crate::mcp::{
    McpServer, OpenFiles, OpenFilesArgs, Overview, OverviewArgs, PageUrl, PageUrlArgs,
    ReadDocument, ReadDocumentArgs, Search, SearchArgs, Tool, route,
};
pub use crate::parse::{BlockStream, Host, InlineStream, Never, Plain};
pub use crate::query::{Direction, Entity, Filter, Query, Sort, filter};
pub use crate::signals::DocumentChange;
pub use crate::template::{BodyCtx, DefaultTemplate, DocumentMeta, HeadCtx, PageKind, Template};
pub use crate::tree::{
    Align, BlockKind, Children, Content, Fold, InlineKind, KindRef, NodeRef, Parsed,
};
pub use crate::web::CustomPage;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use notify::RecommendedWatcher;
use notify_debouncer_mini::Debouncer;

use crate::index::Index;
use crate::root::Root;

/// Where a [`Wiki`] keeps its index; see [`WikiBuilder::index_storage`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IndexStorage {
    /// A fresh index in memory, rebuilt by every start.
    #[default]
    InMemory,
    /// An index kept on disk in this SQLite database file, which is created
    /// if missing, along with any missing parent folders. A relative path is
    /// relative to the working directory. Only one `Wiki` (in any process)
    /// can have a given file open at a time.
    FilePath(PathBuf),
}

/// A folder of markdown served as a website, parsed with host `P`'s hooks
/// and drawn through its template.
pub struct Wiki<P: Host = Plain> {
    inner: Arc<Inner<P>>,
}

// Derived `Clone` would require `P: Clone`.
impl<P: Host> Clone for Wiki<P> {
    fn clone(&self) -> Self {
        Wiki {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<P: Host> {
    root: Root,
    /// Base path without trailing slash; `""` at the root.
    base: String,
    host: Arc<P>,
    template: P::Template,
    index: Index<P>,
    /// Host CSS emitted after the built-in stylesheet on every page.
    stylesheet: Option<String>,
    /// Keeps the index current; dropping it stops the watch. `None` for
    /// embedded files, which never change.
    _watcher: Option<Debouncer<RecommendedWatcher>>,
    /// Becomes `true` once the initial background scan has finished.
    indexed: tokio::sync::watch::Receiver<bool>,
    /// Watches on documents and folders, and the open pages' channels.
    signals: Arc<signals::Signals>,
    /// The document pages open in browsers, for the `open_files` tool.
    tabs: tabs::Tabs,
    /// Makes the MCP endpoint's tools; holds no wiki, so no cycle.
    tools: mcp::ToolsFn<P>,
    /// The reference documents, served at `{base}/_wiki/reference` as a wiki
    /// of their own; `None` for the reference itself.
    reference: Option<Wiki>,
    /// Markdown drawn above the root's index page; the reference's own.
    prompt: Option<&'static str>,
}

impl Wiki {
    /// Starts building a wiki that serves the folder at `root`, with no host
    /// syntax and the [`DefaultTemplate`]; see [`WikiBuilder::host`].
    pub fn builder(root: impl Into<PathBuf>) -> WikiBuilder {
        WikiBuilder {
            root: root.into(),
            base_path: String::from("/"),
            stylesheet: None,
            index_storage: IndexStorage::default(),
            host: Plain,
            template: DefaultTemplate,
            entities: Vec::new(),
            tools: None,
            tools_before_host: false,
        }
    }
}

impl<P: Host> Wiki<P> {
    /// Routes already mounted under the base path; merge into a host router
    /// with `app.merge(wiki.router())`.
    pub fn router<S: Clone + Send + Sync + 'static>(&self) -> Router<S> {
        web::router(self.clone())
    }

    /// Resolves once the initial background scan has finished; at once if it
    /// already has. Pages are served before then, but links, tags and
    /// listings may be incomplete.
    /// Signals every open instance of the page at URL path `path` (e.g.
    /// `/docs/Projects/Alpha`, or a host's own `/dashboard`) to refetch
    /// itself; nothing if none is open. Only the path is the key.
    pub fn signal(&self, path: &str) {
        self.inner.signals.signal(path);
    }

    /// Rebuilds the index from scratch: drops every stored row and table,
    /// then rescans every file. Resolves once the rescan has finished;
    /// pages are served meanwhile, from an index that is filling.
    pub async fn reindex(&self) -> anyhow::Result<()> {
        let index = &self.inner.index;
        match &self.inner.root {
            Root::Disk(root) => {
                index.reset(root).await?;
                index.scan(root).await
            }
            Root::Embedded(files) => {
                index.reset(std::path::Path::new(reference::PATH)).await?;
                index.index_texts(files, reference::edited()).await
            }
        }
    }

    pub async fn indexed(&self) {
        let mut indexed = self.inner.indexed.clone();
        // An error means the scan task is gone, which only happens after it
        // has sent `true` or panicked; either way there is nothing to wait for.
        let _ = indexed.wait_for(|done| *done).await;
    }
}

/// Configures a [`Wiki`]; created by [`Wiki::builder`].
pub struct WikiBuilder<P: Host = Plain> {
    root: PathBuf,
    base_path: String,
    stylesheet: Option<String>,
    index_storage: IndexStorage,
    host: P,
    template: P::Template,
    /// Registered entity types; an invalid one fails `build`.
    entities: Vec<anyhow::Result<entity::EntitySpec>>,
    tools: Option<mcp::ToolsFn<P>>,
    /// `.tools` was set before `.host()`, which fails `build`.
    tools_before_host: bool,
}

impl<P: Host> WikiBuilder<P> {
    /// The host whose hooks parse every page, and the template pages are
    /// drawn through.
    pub fn host<Q: Host>(self, host: Q, template: Q::Template) -> WikiBuilder<Q> {
        WikiBuilder {
            root: self.root,
            base_path: self.base_path,
            stylesheet: self.stylesheet,
            index_storage: self.index_storage,
            host,
            template,
            entities: self.entities,
            tools: None,
            tools_before_host: self.tools_before_host || self.tools.is_some(),
        }
    }

    /// Registers an entity type: the index keeps a table of its records,
    /// which the host's [`Host::extract`] pushes. Registering one twice, or
    /// two with the same name, fails `build`.
    pub fn entity<T: EntityType>(mut self) -> Self {
        self.entities.push(entity::EntitySpec::of::<T>());
        self
    }

    /// The tools the MCP endpoint serves, made from the wiki: start from
    /// [`Wiki::tools`] and add, remove or replace routes. Set it after
    /// [`host`](Self::host); one set before it fails `build`.
    pub fn tools(
        mut self,
        tools: impl Fn(&Wiki<P>) -> rmcp::handler::server::router::tool::ToolRouter<McpServer<P>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.tools = Some(Arc::new(tools));
        self
    }

    /// Where the router is mounted: `"/"` (the default) for the root, e.g. `"/docs"` otherwise.
    pub fn base_path(mut self, base: &str) -> Self {
        self.base_path = base.to_string();
        self
    }

    /// CSS text layered on top of the built-in stylesheet on every page.
    pub fn stylesheet(mut self, css: impl Into<String>) -> Self {
        self.stylesheet = Some(css.into());
        self
    }

    /// Where the index is kept: [`IndexStorage::InMemory`] (the default) or
    /// on disk with [`IndexStorage::FilePath`].
    pub fn index_storage(mut self, storage: IndexStorage) -> Self {
        self.index_storage = storage;
        self
    }

    /// Checks the folder can be listed, opens the index, starts watching the
    /// folder, and starts the initial scan in the background; see
    /// [`Wiki::indexed`]. Fails if a `FilePath` index can't be opened:
    /// when another `Wiki` or process has it open, or when the file is not
    /// a SQLite database or is one that agent_wiki didn't create.
    pub async fn build(self) -> anyhow::Result<Wiki<P>> {
        if self.tools_before_host {
            anyhow::bail!("`.tools` was set before `.host()`; set it after");
        }
        let root = self.root.canonicalize()?;
        std::fs::read_dir(&root)?;
        let host = Arc::new(self.host);
        let entities = self
            .entities
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()?;
        for (i, spec) in entities.iter().enumerate() {
            if entity::builtin_entities()
                .iter()
                .any(|b| b.name == spec.name)
            {
                anyhow::bail!("entity name {} is the library's own", spec.name);
            }
            if entities[..i].iter().any(|other| other.name == spec.name) {
                anyhow::bail!("entity {} is registered twice", spec.name);
            }
        }
        let index = Index::open_with(&self.index_storage, &root, host.clone(), entities).await?;
        let base = normalize_base(&self.base_path);
        let signals = Arc::new(signals::Signals::new(base.clone()));
        let watcher = watch::start(root.clone(), index.clone(), signals.clone())?;
        let reference = reference::build(&base, self.stylesheet.clone()).await?;
        let (done, indexed) = tokio::sync::watch::channel(false);
        tokio::spawn({
            let (root, index) = (root.clone(), index.clone());
            async move {
                if let Err(e) = index.scan(&root).await {
                    eprintln!("agent_wiki: scan {}: {e:#}", root.display());
                }
                let _ = done.send(true);
            }
        });
        Ok(Wiki {
            inner: Arc::new(Inner {
                root: Root::Disk(root),
                base,
                host,
                template: self.template,
                index,
                stylesheet: self.stylesheet,
                _watcher: Some(watcher),
                indexed,
                signals,
                tabs: tabs::Tabs::new(),
                tools: self
                    .tools
                    .unwrap_or_else(|| Arc::new(|wiki: &Wiki<P>| wiki.tools())),
                reference: Some(reference),
                prompt: None,
            }),
        })
    }
}

pub(crate) fn normalize_base(base: &str) -> String {
    let trimmed = base.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}
