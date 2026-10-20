//! The document pages open in browsers, and the state each one last pushed:
//! what the `open_files` tool reports. A page is open while its live-update
//! stream is connected.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// Every connected page, by tab id.
pub(crate) struct Tabs {
    /// Distinguishes this process's ids from an earlier one's, whose pages
    /// reconnect after a restart.
    epoch: u128,
    next: AtomicU64,
    open: Arc<Mutex<HashMap<String, Tab>>>,
}

struct Tab {
    rel: Arc<str>,
    /// When the page connected, or last pushed state that counts as use.
    active: SystemTime,
    focused: bool,
    selection: Option<Selection>,
    /// How many streams hold this id: a reconnect can overlap the old one.
    streams: usize,
}

/// A selection in the page's content, by source line. `end` is `None` when
/// the selection runs into the page's last block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) struct Selection {
    pub(crate) start: usize,
    pub(crate) end: Option<usize>,
}

/// What a page pushes to `{base}/_wiki/state`.
#[derive(Debug, Deserialize)]
pub(crate) struct PushedState {
    pub(crate) tab: String,
    pub(crate) focused: bool,
    pub(crate) selection: Option<Selection>,
}

/// An open file as the `open_files` tool lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenFile {
    pub(crate) rel: Arc<str>,
    pub(crate) focused: bool,
    pub(crate) selection: Option<Selection>,
}

/// Holds a tab open; dropping it (when the stream ends) closes it.
pub(crate) struct TabGuard {
    id: String,
    open: Arc<Mutex<HashMap<String, Tab>>>,
}

impl Drop for TabGuard {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap();
        if let Some(tab) = open.get_mut(&self.id) {
            tab.streams -= 1;
            if tab.streams == 0 {
                open.remove(&self.id);
            }
        }
    }
}

impl Tabs {
    pub(crate) fn new() -> Tabs {
        Tabs {
            epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
            next: AtomicU64::new(1),
            open: Arc::default(),
        }
    }

    /// A new tab id, for a page being drawn.
    pub(crate) fn new_id(&self) -> String {
        format!(
            "{}-{}",
            self.epoch,
            self.next.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Opens tab `id` on page `rel`, for as long as the guard lives.
    pub(crate) fn connect(&self, id: String, rel: Arc<str>) -> TabGuard {
        let mut open = self.open.lock().unwrap();
        let tab = open.entry(id.clone()).or_insert_with(|| Tab {
            rel: rel.clone(),
            active: SystemTime::now(),
            focused: false,
            selection: None,
            streams: 0,
        });
        tab.rel = rel;
        tab.streams += 1;
        TabGuard {
            id,
            open: self.open.clone(),
        }
    }

    /// Records a connected tab's pushed state; a tab that isn't connected is
    /// ignored. Gaining focus or changing the selection makes it the most
    /// recently active.
    pub(crate) fn push(&self, state: PushedState) {
        let mut open = self.open.lock().unwrap();
        let Some(tab) = open.get_mut(&state.tab) else {
            return;
        };
        let gained_focus = state.focused && !tab.focused;
        let selected = state.selection.is_some() && state.selection != tab.selection;
        if gained_focus || selected {
            tab.active = SystemTime::now();
        }
        tab.focused = state.focused;
        tab.selection = state.selection;
    }

    /// Each open file once, most recently active first, with the state of
    /// its most recently active tab.
    pub(crate) fn open_files(&self) -> Vec<OpenFile> {
        let open = self.open.lock().unwrap();
        let mut tabs: Vec<&Tab> = open.values().collect();
        tabs.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.rel.cmp(&b.rel)));
        let mut files: Vec<OpenFile> = Vec::new();
        for tab in tabs {
            if !files.iter().any(|f| f.rel == tab.rel) {
                files.push(OpenFile {
                    rel: tab.rel.clone(),
                    focused: tab.focused,
                    selection: tab.selection,
                });
            }
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(tabs: &Tabs, tab: &str, focused: bool, selection: Option<Selection>) {
        tabs.push(PushedState {
            tab: tab.into(),
            focused,
            selection,
        });
        // `SystemTime` can tie within a test.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    #[test]
    fn most_recently_active_first() {
        let tabs = Tabs::new();
        let a = tabs.connect("a".into(), "a.md".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let _b = tabs.connect("b".into(), "b.md".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let rels = |tabs: &Tabs| -> Vec<String> {
            tabs.open_files()
                .iter()
                .map(|f| f.rel.to_string())
                .collect()
        };
        assert_eq!(rels(&tabs), ["b.md", "a.md"]);

        push(&tabs, "a", true, None);
        assert_eq!(rels(&tabs), ["a.md", "b.md"]);
        assert!(tabs.open_files()[0].focused);

        let sel = Selection {
            start: 3,
            end: Some(5),
        };
        push(&tabs, "a", false, None);
        push(&tabs, "b", false, Some(sel));
        assert_eq!(rels(&tabs), ["b.md", "a.md"]);
        assert_eq!(tabs.open_files()[0].selection, Some(sel));

        drop(a);
        assert_eq!(rels(&tabs), ["b.md"]);
        // A closed tab's pushes are ignored.
        push(&tabs, "a", true, None);
        assert_eq!(rels(&tabs), ["b.md"]);
    }

    #[test]
    fn one_entry_per_file_and_overlapping_streams() {
        let tabs = Tabs::new();
        let first = tabs.connect("x".into(), "same.md".into());
        let _y = tabs.connect("y".into(), "same.md".into());
        assert_eq!(tabs.open_files().len(), 1);
        // A reconnect overlapping the old stream keeps the tab open.
        let again = tabs.connect("x".into(), "same.md".into());
        drop(first);
        push(&tabs, "x", true, None);
        assert!(tabs.open_files()[0].focused);
        drop(again);
        assert!(!tabs.open_files()[0].focused);
    }
}
