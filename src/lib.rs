mod document;
mod draw;
mod index;
mod links;
mod mcp;
mod names;
mod parse;
mod tabs;
mod template;
mod tree;
mod watch;
mod web;

pub use maud;
pub use nom;
pub use rmcp;

pub use crate::draw::{DrawChildren, DrawCtx, Element};
pub use crate::mcp::{
    OpenFiles, OpenFilesArgs, Overview, OverviewArgs, Search, SearchArgs, Tool, route,
};
pub use crate::parse::{BlockStream, Host, InlineStream, Never, Plain};
pub use crate::template::{BodyCtx, DefaultTemplate, DocumentMeta, HeadCtx, PageKind, Template};
pub use crate::tree::{Align, BlockKind, Children, Content, InlineKind, Parsed};

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use notify::RecommendedWatcher;
use notify_debouncer_mini::Debouncer;

use crate::index::Index;

/// How many changed-page notices the live-update channel buffers per
/// subscriber before it lags.
const CHANGED_CAPACITY: usize = 1024;

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
    root: PathBuf,
    /// Base path without trailing slash; `""` at the root.
    base: String,
    host: Arc<P>,
    template: P::Template,
    index: Index<P>,
    /// Host CSS emitted after the built-in stylesheet on every page.
    stylesheet: Option<String>,
    /// Keeps the index current; dropping it stops the watch.
    _watcher: Debouncer<RecommendedWatcher>,
    /// Becomes `true` once the initial background scan has finished.
    indexed: tokio::sync::watch::Receiver<bool>,
    /// The rel of every page the watcher reindexed or removed, after each
    /// batch. The watcher task holds the other sender, so subscribers'
    /// streams end only once this `Inner` (every `Wiki` and router clone) and
    /// the watcher are both gone. A served SSE connection keeps a router
    /// clone, so it is never ended from this side; see `web::page_events`.
    changed: tokio::sync::broadcast::Sender<Arc<str>>,
    /// The document pages open in browsers, for the `open_files` tool.
    tabs: tabs::Tabs,
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
        }
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
        let root = self.root.canonicalize()?;
        std::fs::read_dir(&root)?;
        let host = Arc::new(self.host);
        let index = Index::open(&self.index_storage, &root, host.clone()).await?;
        let (changed, _) = tokio::sync::broadcast::channel(CHANGED_CAPACITY);
        let watcher = watch::start(root.clone(), index.clone(), changed.clone())?;
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
                root,
                base: normalize_base(&self.base_path),
                host,
                template: self.template,
                index,
                stylesheet: self.stylesheet,
                _watcher: watcher,
                indexed,
                changed,
                tabs: tabs::Tabs::new(),
            }),
        })
    }
}

fn normalize_base(base: &str) -> String {
    let trimmed = base.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}
