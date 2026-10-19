use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use tokio::sync::{broadcast, mpsc};

use crate::index::{FsKind, Index, collect_pages, fs_kind, list_folder};
use crate::names::is_hidden;
use crate::parse::Host;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Watches `root` (canonical) recursively and keeps `index` current,
/// publishing on `changed` the rel of every page reindexed or removed. The
/// returned debouncer stops the watch when dropped, which also ends the
/// task applying the changes and drops its `changed` sender.
pub fn start<P: Host>(
    root: PathBuf,
    index: Index<P>,
    changed: broadcast::Sender<Arc<str>>,
) -> Result<Debouncer<RecommendedWatcher>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(DEBOUNCE, move |batch: DebounceEventResult| {
        let _ = tx.send(batch);
    })?;
    debouncer.watcher().watch(&root, RecursiveMode::Recursive)?;
    tokio::spawn(async move {
        while let Some(batch) = rx.recv().await {
            // Merge every batch already queued, so a flood that arrives while
            // one is being applied collapses into the next one.
            let mut paths = BTreeSet::new();
            let mut add = |batch: DebounceEventResult| match batch {
                Ok(events) => paths.extend(events.into_iter().map(|e| e.path)),
                Err(e) => eprintln!("agent_wiki: watch error: {e}"),
            };
            add(batch);
            while let Ok(more) = rx.try_recv() {
                add(more);
            }
            let mut touched = Touched::new();
            apply_batch(&root, &index, paths, &mut touched).await;
            for rel in touched {
                // An error only means nobody is subscribed.
                let _ = changed.send(rel.into());
            }
        }
    });
    Ok(debouncer)
}

/// Brings the index in line with the disk for every path in one merged batch:
/// grouped by parent folder, each folder checked and listed once.
async fn apply_batch<P: Host>(
    root: &Path,
    index: &Index<P>,
    paths: BTreeSet<PathBuf>,
    touched: &mut Touched,
) {
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut root_changed = false;
    for path in paths {
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };
        if is_hidden(rel_path) {
            continue;
        }
        let Some(rel) = rel_string(rel_path) else {
            continue;
        };
        if rel.is_empty() {
            root_changed = true;
            continue;
        }
        let (folder, name) = rel.rsplit_once('/').unwrap_or(("", &rel));
        groups
            .entry(folder.to_string())
            .or_default()
            .insert(name.to_string());
    }
    if root_changed {
        // A recursive rescan of the root covers every other event.
        rescan(root, index, "", touched).await;
        return;
    }
    // Ancestors sort before their descendants, so a folder rescanned while
    // applying its parent's group is skipped when its own group comes up.
    let mut rescanned: Vec<String> = Vec::new();
    for (folder, names) in groups {
        if covered(&rescanned, &folder) {
            continue;
        }
        let applied = apply_folder(root, index, &folder, &names, &mut rescanned, touched).await;
        if let Err(e) = applied {
            eprintln!("agent_wiki: watch {}: {e:#}", root.join(&folder).display());
        }
    }
}

/// Applies the event `names` in `folder` against one listing of it, then
/// drops indexed pages and subfolders of `folder` that the listing lacks.
async fn apply_folder<P: Host>(
    root: &Path,
    index: &Index<P>,
    folder: &str,
    names: &BTreeSet<String>,
    rescanned: &mut Vec<String>,
    touched: &mut Touched,
) -> Result<()> {
    match fs_kind(root, folder).await {
        Ok(FsKind::Dir) => {}
        Ok(_) => return remove_folder(index, folder, touched).await,
        Err(e) => {
            remove_folder(index, folder, touched).await?;
            return Err(e.into());
        }
    }
    let listing = match list_folder(root, folder).await {
        Ok(listing) => listing,
        Err(e) => {
            remove_folder(index, folder, touched).await?;
            return Err(e.into());
        }
    };
    for name in names {
        let rel = join(folder, name);
        if covered(rescanned, &rel) {
            continue;
        }
        let applied = match listing.get(name) {
            Some(FsKind::File) if name.ends_with(".md") => {
                reindex(root, index, &rel, touched).await
            }
            Some(FsKind::Dir) => {
                rescan(root, index, &rel, touched).await;
                rescanned.push(rel.clone());
                Ok(())
            }
            // Absent, a symlink (not followed), or not a page.
            _ => remove(index, &rel, touched).await,
        };
        if let Err(e) = applied {
            eprintln!("agent_wiki: watch {}: {e:#}", root.join(&rel).display());
        }
    }
    let (subfolders, pages) = index.folder_listing(folder).await?;
    for page in pages {
        let name = page.path.rsplit('/').next().unwrap_or(&page.path);
        if listing.get(name) != Some(&FsKind::File) {
            remove_page(index, &page.path, touched).await?;
        }
    }
    for name in subfolders {
        if listing.get(&name) != Some(&FsKind::Dir) {
            remove_folder(index, &join(folder, &name), touched).await?;
        }
    }
    Ok(())
}

/// Reindexes the pages under `folder` whose `mtime` differs from the stored
/// one and drops indexed pages under it that are gone from disk. If `folder`
/// itself can't be walked, its pages are removed. Errors are logged.
async fn rescan<P: Host>(root: &Path, index: &Index<P>, folder: &str, touched: &mut Touched) {
    let result = async {
        let walked = match collect_pages(root, folder).await {
            Ok(walked) => walked,
            Err(e) => {
                remove_folder(index, folder, touched).await?;
                return Err(e);
            }
        };
        let mut stored = index.mtimes_under(folder).await?;
        for (rel, mtime) in &walked {
            if stored.remove(rel) == Some(*mtime) {
                continue;
            }
            if let Err(e) = reindex(root, index, rel, touched).await {
                eprintln!("agent_wiki: skipping {}: {e:#}", root.join(rel).display());
            }
        }
        for gone in stored.keys() {
            remove_page(index, gone, touched).await?;
        }
        Ok(())
    };
    if let Err(e) = result.await {
        eprintln!("agent_wiki: watch {}: {e:#}", root.join(folder).display());
    }
}

/// Rels of the pages reindexed or removed while applying one merged batch.
type Touched = BTreeSet<String>;

/// `index_file`, noting `rel` as touched whether it was reindexed or (on a
/// failed read) removed.
async fn reindex<P: Host>(
    root: &Path,
    index: &Index<P>,
    rel: &str,
    touched: &mut Touched,
) -> Result<()> {
    touched.insert(rel.to_string());
    index.index_file(root, rel).await
}

/// `remove_path`, noting every page it removed as touched. For an event
/// path, which may have been a page or a folder.
async fn remove<P: Host>(
    index: &Index<P>,
    rel_or_folder: &str,
    touched: &mut Touched,
) -> Result<()> {
    touched.extend(index.remove_path(rel_or_folder).await?);
    Ok(())
}

/// `remove_folder`, noting every page it removed as touched. For a path
/// known to be a folder (an event's parent, a stored subfolder, a folder
/// being rescanned), so a page now at that path is kept.
async fn remove_folder<P: Host>(
    index: &Index<P>,
    folder: &str,
    touched: &mut Touched,
) -> Result<()> {
    touched.extend(index.remove_folder(folder).await?);
    Ok(())
}

/// `remove_page`, noting `rel` as touched if it was indexed. For a path
/// taken from a stored page row, so a folder now at that path keeps its
/// pages.
async fn remove_page<P: Host>(index: &Index<P>, rel: &str, touched: &mut Touched) -> Result<()> {
    if index.remove_page(rel).await? {
        touched.insert(rel.to_string());
    }
    Ok(())
}

/// True when `rel` is, or is under, a folder already rescanned recursively.
fn covered(rescanned: &[String], rel: &str) -> bool {
    rescanned.iter().any(|r| {
        r.is_empty()
            || rel == r
            || rel
                .strip_prefix(r.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// `/`-separated relative path; `None` if any segment is not UTF-8.
fn rel_string(rel: &Path) -> Option<String> {
    let segments = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexStorage;
    use crate::parse::Plain;

    /// One batch holding both the deletion under folder `x.md/` and the new
    /// page `x.md`: the `x.md` group finds a file where its folder was, and
    /// must remove only the folder's pages, not the page just indexed.
    #[tokio::test]
    async fn batch_with_folder_replaced_by_page_keeps_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("x.md")).unwrap();
        std::fs::write(root.join("x.md/y.md"), "# Y\n").unwrap();
        let index = Index::open(&IndexStorage::InMemory, &root, Arc::new(Plain))
            .await
            .unwrap();
        index.scan(&root).await.unwrap();
        std::fs::remove_dir_all(root.join("x.md")).unwrap();
        std::fs::write(root.join("x.md"), "# X\n").unwrap();
        let paths = BTreeSet::from([root.join("x.md"), root.join("x.md/y.md")]);
        let mut touched = Touched::new();
        apply_batch(&root, &index, paths, &mut touched).await;
        let left: Vec<String> = index.mtimes_under("").await.unwrap().into_keys().collect();
        assert_eq!(left, ["x.md"]);
        assert_eq!(touched, Touched::from(["x.md".into(), "x.md/y.md".into()]));
    }
}
