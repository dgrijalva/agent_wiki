//! The reference documents: `reference/` at the repo root, baked into the
//! build and served at `{base}/_wiki/reference` as a wiki of their own, with
//! its own in-memory index. Nothing else in the wiki sees them.

use std::sync::Arc;

use anyhow::Result;

use crate::index::Index;
use crate::root::Root;
use crate::{DefaultTemplate, IndexStorage, Inner, Plain, Wiki, mcp, signals, tabs};

/// Where the reference is, relative to the wiki root.
pub(crate) const PATH: &str = "_wiki/reference";

/// The file whose text heads the reference's index page. It is not a
/// document.
const PROMPT: &str = "README.md";

const FILES: &[(&str, &str)] = agent_wiki_derive::embed_folder!("reference");

/// The reference as a wiki mounted under the wiki at `base`, with the wiki's
/// stylesheet, integrity filter and scripts filter. Indexed before it is
/// returned.
pub(crate) async fn build(
    base: &str,
    stylesheet: Option<String>,
    integrity: crate::IntegrityFn,
    scripts: Option<crate::ScriptsFn>,
) -> Result<Wiki> {
    let prompt = FILES
        .iter()
        .find(|(path, _)| *path == PROMPT)
        .map(|(_, text)| *text);
    let files: Vec<(&str, &str)> = FILES
        .iter()
        .copied()
        .filter(|(path, _)| *path != PROMPT)
        .filter(|(path, _)| path.split('/').all(|seg| !seg.starts_with('.')))
        .collect();
    let host = Arc::new(Plain);
    let index = Index::open_with(
        &IndexStorage::InMemory,
        std::path::Path::new(PATH),
        host.clone(),
        Vec::new(),
    )
    .await?;
    index.index_texts(&files, edited()).await?;
    let base = crate::normalize_base(&format!("{base}/{PATH}"));
    let (_, indexed) = tokio::sync::watch::channel(true);
    Ok(Wiki {
        inner: Arc::new(Inner {
            root: Root::Embedded(files),
            signals: Arc::new(signals::Signals::new(base.clone())),
            base,
            host,
            template: DefaultTemplate,
            index,
            stylesheet,
            integrity,
            scripts,
            _watcher: None,
            indexed,
            tabs: tabs::Tabs::new(),
            tools: Arc::new(|wiki: &Wiki| wiki.tools()) as mcp::ToolsFn<Plain>,
            reference: None,
            prompt,
            allowed_hosts: None,
        }),
    })
}

/// When the reference documents count as edited: the time the process
/// started using them, in seconds since the epoch, since the build's own
/// time isn't known.
pub(crate) fn edited() -> i64 {
    static STARTED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

/// The rest of `path` (relative to the wiki root) inside the reference, if
/// it is in it: `""` for the reference's own folder.
pub(crate) fn within(path: &str) -> Option<&str> {
    let path = path.trim().trim_start_matches('/');
    let rest = path.strip_prefix(PATH)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix('/')
}
