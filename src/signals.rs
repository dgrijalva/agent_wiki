//! Change notification: watches on documents and folders, and the channels
//! of the pages open in browsers, keyed by URL path. The file watcher
//! reports each batch's changes here once the index has them.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

use tokio::sync::watch;
use tokio_stream::Stream;
use tokio_stream::wrappers::WatchStream;

use crate::names::{folder_url, path_to_url};

/// How a watched document last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentChange {
    Modified,
    Removed,
}

/// Every live watch and open page, by path.
pub(crate) struct Signals {
    /// Base path without trailing slash (`""` at root).
    base: String,
    /// Document watches by rel (with `.md`).
    documents: Mutex<HashMap<String, watch::Sender<Option<DocumentChange>>>>,
    /// Folder watches by folder path (`""` is the root).
    folders: Mutex<HashMap<String, watch::Sender<u64>>>,
    /// Open pages by URL path. Each page's [`PageSignals`] holds it weakly,
    /// so dropping the `Signals` drops the senders and ends the streams.
    pages: Arc<Mutex<HashMap<String, watch::Sender<()>>>>,
}

impl Signals {
    pub(crate) fn new(base: String) -> Signals {
        Signals {
            base,
            documents: Mutex::default(),
            folders: Mutex::default(),
            pages: Arc::default(),
        }
    }

    pub(crate) fn watch_document(&self, rel: &str) -> watch::Receiver<Option<DocumentChange>> {
        let mut documents = self.documents.lock().unwrap();
        let sender = documents
            .entry(rel.to_string())
            .or_insert_with(|| watch::Sender::new(None));
        sender.subscribe()
    }

    pub(crate) fn watch_folder(&self, folder: &str) -> watch::Receiver<u64> {
        let mut folders = self.folders.lock().unwrap();
        let sender = folders
            .entry(folder.to_string())
            .or_insert_with(|| watch::Sender::new(0));
        sender.subscribe()
    }

    /// One watcher batch's changes, by rel: tells each document's watch,
    /// each folder watch above it (the root included) once, each open page
    /// of the document, and each open index page of a folder above it once.
    /// Watches nobody holds any more are dropped.
    pub(crate) fn changed(&self, changes: &[(String, DocumentChange)]) {
        {
            let mut documents = self.documents.lock().unwrap();
            documents.retain(|_, sender| sender.receiver_count() > 0);
            for (rel, change) in changes {
                if let Some(sender) = documents.get(rel) {
                    sender.send_replace(Some(*change));
                }
            }
        }
        let mut touched: Vec<&str> = changes.iter().flat_map(|(rel, _)| ancestors(rel)).collect();
        touched.sort_unstable();
        touched.dedup();
        {
            let mut folders = self.folders.lock().unwrap();
            folders.retain(|_, sender| sender.receiver_count() > 0);
            for folder in &touched {
                if let Some(sender) = folders.get(*folder) {
                    sender.send_modify(|n| *n += 1);
                }
            }
        }
        for (rel, _) in changes {
            self.signal(&format!("{}{}", self.base, path_to_url(rel)));
        }
        for folder in touched {
            self.signal(&format!("{}{}", self.base, folder_url(folder)));
        }
    }

    /// Signals every open instance of the page at URL path `path`; nothing
    /// if none is open.
    pub(crate) fn signal(&self, path: &str) {
        if let Some(sender) = self.pages.lock().unwrap().get(path) {
            sender.send_replace(());
        }
    }

    /// Registers an open page at URL path `path`: a stream of its signals,
    /// until it is dropped.
    pub(crate) fn open_page(&self, path: &str) -> PageSignals {
        let mut pages = self.pages.lock().unwrap();
        let sender = pages
            .entry(path.to_string())
            .or_insert_with(|| watch::Sender::new(()));
        PageSignals {
            stream: Some(WatchStream::from_changes(sender.subscribe())),
            path: path.to_string(),
            pages: Arc::downgrade(&self.pages),
        }
    }
}

/// An open page's signals: one item per signal, with signals that arrive
/// before the item is taken collapsed into one. Dropping the last one for a
/// URL removes the URL's channel.
pub(crate) struct PageSignals {
    /// `Some` until dropped.
    stream: Option<WatchStream<()>>,
    path: String,
    pages: Weak<Mutex<HashMap<String, watch::Sender<()>>>>,
}

impl Stream for PageSignals {
    type Item = ();

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<()>> {
        match &mut self.stream {
            Some(stream) => Pin::new(stream).poll_next(cx),
            None => Poll::Ready(None),
        }
    }
}

impl Drop for PageSignals {
    fn drop(&mut self) {
        let Some(pages) = self.pages.upgrade() else {
            return;
        };
        let mut pages = pages.lock().unwrap();
        self.stream = None;
        if pages
            .get(&self.path)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            pages.remove(&self.path);
        }
    }
}

/// The folders holding `rel`, innermost first, ending with the root `""`.
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(rel);
    std::iter::from_fn(move || {
        let (folder, _) = rest?.rsplit_once('/').unwrap_or(("", ""));
        rest = (!folder.is_empty()).then_some(folder);
        Some(folder)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[test]
    fn ancestors_end_at_the_root() {
        assert_eq!(ancestors("a/b/c.md").collect::<Vec<_>>(), ["a/b", "a", ""]);
        assert_eq!(ancestors("c.md").collect::<Vec<_>>(), [""]);
    }

    #[tokio::test]
    async fn watches_and_pages() {
        let signals = Signals::new("/docs".to_string());
        let mut doc = signals.watch_document("a/b.md");
        let mut folder = signals.watch_folder("a");
        let mut root = signals.watch_folder("");
        let other = signals.watch_folder("z");
        let mut page = signals.open_page("/docs/a/b");
        let mut second = signals.open_page("/docs/a/b");
        let mut index = signals.open_page("/docs/a/");
        let mut root_index = signals.open_page("/docs/");
        let mut other_index = signals.open_page("/docs/z/");

        signals.changed(&[
            ("a/b.md".to_string(), DocumentChange::Modified),
            ("a/c.md".to_string(), DocumentChange::Removed),
        ]);
        assert!(doc.has_changed().unwrap());
        assert_eq!(*doc.borrow_and_update(), Some(DocumentChange::Modified));
        // Two changes in one batch bump a folder once.
        assert_eq!(*folder.borrow_and_update(), 1);
        assert_eq!(*root.borrow_and_update(), 1);
        assert!(!other.has_changed().unwrap());
        assert_eq!(page.next().await, Some(()));
        assert_eq!(second.next().await, Some(()));
        // So do the index pages of every folder above them.
        assert_eq!(index.next().await, Some(()));
        assert_eq!(root_index.next().await, Some(()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), other_index.next())
                .await
                .is_err()
        );

        // A host's signal reaches every open instance; signals not yet
        // taken collapse into one.
        signals.signal("/docs/a/b");
        signals.signal("/docs/a/b");
        assert_eq!(page.next().await, Some(()));
        drop(page);
        signals.signal("/docs/a/b");
        assert_eq!(second.next().await, Some(()));
        drop(second);
        drop((index, root_index, other_index));
        assert!(signals.pages.lock().unwrap().is_empty());
    }
}
