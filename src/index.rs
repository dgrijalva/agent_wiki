use std::collections::{BTreeSet, HashMap};
use std::fs::Metadata;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use libsql::{Builder, Connection, Row, Rows, TransactionBehavior, params};
use tokio::sync::Mutex;

use crate::IndexStorage;
use crate::names::{FileKeys, LinkTarget, norm, path_to_url};
use crate::parse::{Host, parse};

/// The version of what an index stores. Bump it by hand whenever the schema
/// changes, or anything a stored row holds or means: tables, columns, the
/// FTS5 tokenizer (`unicode61 remove_diacritics 2`), key normalisation,
/// title rules, or what `Document::extract` collects. An index file with a
/// different stored version (`PRAGMA user_version`) is rebuilt.
pub const INDEX_VERSION: i64 = 3;

/// `PRAGMA application_id` of an agent_wiki index file: ASCII `awik`. A
/// SQLite file with another id that holds any schema object is refused.
pub const AGENT_WIKI_APP_ID: i32 = 0x6177_696B;

/// Row meanings: `path` is rel with `.md`; `folder` is `""` at the root;
/// `mtime` is the file's no-follow modification time in ns; `edited` is
/// when the page was last edited, in s (the latest commit time for a clean
/// tracked file when the scan wrote the row, the file's `mtime` otherwise;
/// the scan leaves a row whose `mtime` matches the file alone, so it keeps
/// this across restarts); `summary` is the page's summary, `''` for none.
/// A `wikilink` row is one counted wikilink, keyed by its ordinal; a `tag`
/// row is one distinct lowercase tag of a page, holding its first inline
/// ordinal, or NULL when it is only in frontmatter. `page_fts` indexes `page`'s title and body as external
/// content, kept current by the triggers.
const SCHEMA: &str = "
CREATE TABLE meta (root TEXT NOT NULL);
CREATE TABLE page (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    url TEXT NOT NULL,
    folder TEXT NOT NULL,
    title TEXT NOT NULL,
    key_full TEXT NOT NULL,
    key_stripped TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    body TEXT NOT NULL,
    edited INTEGER NOT NULL,
    summary TEXT NOT NULL
);
CREATE INDEX page_key_full ON page(key_full);
CREATE INDEX page_key_stripped ON page(key_stripped);
CREATE INDEX page_folder ON page(folder);
CREATE INDEX page_recent ON page(edited DESC, path);
CREATE TABLE wikilink (source TEXT NOT NULL, ordinal INTEGER NOT NULL, key TEXT NOT NULL,
    PRIMARY KEY (source, ordinal)) WITHOUT ROWID;
CREATE INDEX wikilink_key ON wikilink(key);
CREATE TABLE tag (source TEXT NOT NULL, tag TEXT NOT NULL, ordinal INTEGER,
    PRIMARY KEY (source, tag)) WITHOUT ROWID;
CREATE INDEX tag_tag ON tag(tag);
CREATE VIRTUAL TABLE page_fts USING fts5(title, body, content='page', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER page_ai AFTER INSERT ON page BEGIN
  INSERT INTO page_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
CREATE TRIGGER page_ad AFTER DELETE ON page BEGIN
  INSERT INTO page_fts(page_fts, rowid, title, body) VALUES ('delete', old.id, old.title, old.body);
END;
CREATE TRIGGER page_au AFTER UPDATE ON page BEGIN
  INSERT INTO page_fts(page_fts, rowid, title, body) VALUES ('delete', old.id, old.title, old.body);
  INSERT INTO page_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
";

/// The columns read into a [`PageRef`], in its field order.
const PAGE_FIELDS: &str = "path, url, folder, title";

/// Removes one page and its rows by exact path.
const REMOVE_PAGE: [&str; 3] = [
    "DELETE FROM page WHERE path = ?1 RETURNING path",
    "DELETE FROM wikilink WHERE source = ?1",
    "DELETE FROM tag WHERE source = ?1",
];

/// Removes the page at `?1` and every page in the range `[?2, ?3)` (a
/// folder's subtree, see [`subtree`]), with their rows.
const REMOVE_PATH: [&str; 3] = [
    "DELETE FROM page WHERE path = ?1 OR (path >= ?2 AND path < ?3) RETURNING path",
    "DELETE FROM wikilink WHERE source = ?1 OR (source >= ?2 AND source < ?3)",
    "DELETE FROM tag WHERE source = ?1 OR (source >= ?2 AND source < ?3)",
];

/// Removes every page in the range `[?1, ?2)` (a folder's subtree, see
/// [`subtree`]), with their rows; never a page at the folder's own path.
const REMOVE_FOLDER: [&str; 3] = [
    "DELETE FROM page WHERE path >= ?1 AND path < ?2 RETURNING path",
    "DELETE FROM wikilink WHERE source >= ?1 AND source < ?2",
    "DELETE FROM tag WHERE source >= ?1 AND source < ?2",
];

/// `mtimes_under` for a folder: paths in `[?1, ?2)`.
const MTIMES_IN: &str = "SELECT path, mtime FROM page WHERE path >= ?1 AND path < ?2";

/// `folder_listing`'s pages directly in folder `?1`.
const LISTING_PAGES: &str = "SELECT path, url, folder, title FROM page WHERE folder = ?1";

/// `listing_details`' pages directly in folder `?1`.
const LISTING_DETAILS: &str = "SELECT path, summary, edited FROM page WHERE folder = ?1";

/// `listing_details`' tags of pages directly in folder `?1`, sorted.
const LISTING_TAGS: &str = "SELECT t.source, t.tag FROM tag t JOIN page p ON p.path = t.source
     WHERE p.folder = ?1 ORDER BY t.source, t.tag";

/// `folder_listing`'s deeper folders of a non-root folder `?3`, whose
/// subtree is `[?1, ?2)`.
const LISTING_DEEPER_IN: &str = "SELECT DISTINCT folder FROM page
     WHERE path >= ?1 AND path < ?2 AND folder != ?3";

/// `recent` in a folder's subtree `[?1, ?2)`, limited to `?3`.
const RECENT_IN: &str = "SELECT path, url, folder, title, edited FROM page
     WHERE path >= ?1 AND path < ?2 ORDER BY edited DESC, path LIMIT ?3";

/// `tag_cloud` in a folder's subtree `[?1, ?2)`.
const CLOUD_IN: &str = "SELECT tag, count(*) FROM tag WHERE source >= ?1 AND source < ?2
     GROUP BY tag";

/// A full-text tier: pages matching the FTS5 query `?1`, best first.
const BY_FTS: &str = "SELECT p.path, p.url, p.folder, p.title
     FROM page_fts JOIN page p ON p.id = page_fts.rowid
     WHERE page_fts MATCH ?1 ORDER BY bm25(page_fts), p.path";

/// The half-open range `[folder/, folder0)` of paths under `folder`: `'0'`
/// is the byte after `'/'`, so under the BINARY collation exactly the
/// strings starting `folder/` fall in it.
fn subtree(folder: &str) -> (String, String) {
    (format!("{folder}/"), format!("{folder}0"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRef {
    pub path: String,
    pub url: String,
    pub folder: String,
    pub title: String,
}

impl PageRef {
    /// A row whose first columns are [`PAGE_FIELDS`].
    fn from_row(row: &Row) -> Result<PageRef> {
        Ok(PageRef {
            path: row.get(0)?,
            url: row.get(1)?,
            folder: row.get(2)?,
            title: row.get(3)?,
        })
    }
}

/// A page's entry in the folder listing, beyond its [`PageRef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDetails {
    /// `""` when the page has none.
    pub summary: String,
    /// Seconds since the epoch; see [`SCHEMA`].
    pub edited: i64,
    /// Lowercase, sorted.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Unique(PageRef),
    Ambiguous(Vec<PageRef>),
    Missing,
}

#[derive(Debug, Clone)]
struct PageRow {
    path: String,
    url: String,
    folder: String,
    title: String,
    key_full: String,
    key_stripped: String,
    /// No-follow modification time, nanoseconds since the epoch.
    mtime: i64,
    /// Plain body text for full-text search.
    body: String,
    /// When the page was last edited, seconds since the epoch; see [`SCHEMA`].
    edited: i64,
    summary: String,
}

#[derive(Debug, Clone)]
struct WikilinkRow {
    key: String,
    ordinal: i64,
}

#[derive(Debug, Clone)]
struct TagRow {
    tag: String,
    ordinal: Option<i64>,
}

/// Every row of `rows`, mapped by `f`. Drains `rows`, so its statement is
/// finished before the caller releases the connection.
async fn collect<T>(mut rows: Rows, f: impl Fn(&Row) -> Result<T>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(f(&row)?);
    }
    Ok(out)
}

/// The first column of the first row of `sql`, if there is a row.
async fn first_i64(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> Result<Option<i64>> {
    let rows = conn.query(sql, params).await?;
    Ok(collect(rows, |row| Ok(row.get::<i64>(0)?)).await?.pop())
}

/// The first column of every row of `sql`, as text.
async fn texts(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    collect(conn.query(sql, ()).await?, |row| Ok(row.get(0)?)).await
}

/// One connection, shared by every clone and serialised by the mutex: each
/// method holds the lock for all its statements (one transaction for a
/// write) and drains its rows before releasing it. Pages are parsed with
/// `host`'s hooks.
pub struct Index<P: Host> {
    conn: Arc<Mutex<Connection>>,
    host: Arc<P>,
}

// Derived `Clone` would require `P: Clone`.
impl<P: Host> Clone for Index<P> {
    fn clone(&self) -> Self {
        Index {
            conn: self.conn.clone(),
            host: self.host.clone(),
        }
    }
}

impl<P: Host> Index<P> {
    /// Opens the index for `root` (canonical) in `storage`. `InMemory` is a
    /// private, always new database. `FilePath` names the database file,
    /// always as a filesystem path (made absolute against the working
    /// directory, so SQLite never reads it as a URI or `:memory:`):
    /// missing parent folders are created, the file is opened with
    /// `locking_mode=EXCLUSIVE`, and a file that is not a SQLite database,
    /// or is one with another application id and any schema object, is
    /// refused untouched before anything is written. Then
    /// `journal_mode=WAL` and `synchronous=NORMAL` are set and the file is
    /// locked by `BEGIN IMMEDIATE`, so a second opener (in any process)
    /// fails. In that transaction the foreign check is repeated, and a
    /// stored index is kept only if it holds [`INDEX_VERSION`] and `root`;
    /// otherwise it is rebuilt. `FilePath` errors are prefixed
    /// `opening index at {path}`.
    pub async fn open(storage: &IndexStorage, root: &Path, host: Arc<P>) -> Result<Index<P>> {
        let root = root.to_string_lossy();
        match storage {
            IndexStorage::InMemory => {
                let conn = Builder::new_local(":memory:").build().await?.connect()?;
                Index::prepare(conn, &root, host).await
            }
            IndexStorage::FilePath(path) => async {
                // Absolute, so SQLite always gets a filesystem path: a name
                // starting `file:` would be read as a URI (with options such
                // as `nolock` or `mode=memory`), and `:memory:` as an
                // in-memory database. Under the working directory they are
                // ordinary file names, and the folders created below are the
                // ones the opened file is in.
                let path = std::path::absolute(path)?;
                let Some(file) = path.to_str() else {
                    bail!("the path is not valid UTF-8");
                };
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let conn = Builder::new_local(file).build().await?.connect()?;
                // `locking_mode` only changes this connection, so it is set
                // first: it keeps the WAL index in heap memory (no `-shm`)
                // even for the check's read. `journal_mode = WAL` is stored
                // in the file, so it waits until the file is known to be
                // ours or empty. Both return a row, so they are queries.
                let locking = conn.query("PRAGMA locking_mode = EXCLUSIVE", ()).await?;
                collect(locking, |_| Ok(())).await?;
                refuse_foreign(&conn).await?;
                let journal = conn.query("PRAGMA journal_mode = WAL", ()).await?;
                collect(journal, |_| Ok(())).await?;
                conn.execute("PRAGMA synchronous = NORMAL", ()).await?;
                Index::prepare(conn, &root, host).await
            }
            .await
            .with_context(|| format!("opening index at {}", path.display())),
        }
    }

    /// Takes the write lock, refuses a foreign database, and keeps the
    /// stored rows or rebuilds, all in one transaction.
    async fn prepare(conn: Connection, root: &str, host: Arc<P>) -> Result<Index<P>> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let ours = refuse_foreign(&tx).await?;
        let version = first_i64(&tx, "PRAGMA user_version", ()).await?;
        let current = ours
            && version == Some(INDEX_VERSION)
            && stored_root(&tx).await? == Some(root.to_string());
        if !current {
            rebuild(&tx, root).await?;
        }
        tx.commit().await?;
        Ok(Index {
            conn: Arc::new(Mutex::new(conn)),
            host,
        })
    }

    /// Reconciles the index with every `.md` file under `root`, skipping
    /// hidden entries and symlinks: a file whose `mtime` equals the stored
    /// one is not re-read (and keeps its stored edited time), every other
    /// file is (re)indexed, and stored pages the walk did not find are
    /// removed by exact path. An empty index is thus filled from scratch.
    /// Edited times of the files written come from git for clean tracked
    /// files, from the file's `mtime` otherwise. A file or subfolder that
    /// can't be read is logged to stderr and skipped; only an unreadable
    /// `root` is an error.
    ///
    /// The scan runs alongside the watcher, so it never replaces a row
    /// whose stored `mtime` is newer than the file it read, and a file
    /// modified after the scan started is dated by its `mtime`, not git.
    pub async fn scan(&self, root: &Path) -> Result<()> {
        let started = mtime_of(std::time::SystemTime::now());
        let committed = git_edited_times(root).await;
        let mut stored = self.mtimes_under("").await?;
        for (rel, mtime) in collect_pages(root, "").await? {
            if stored.remove(&rel) == Some(mtime) {
                continue;
            }
            let scan = Scan {
                committed: committed.get(&rel).copied(),
                started,
            };
            if let Err(e) = self.write_page(root, &rel, Some(scan)).await {
                eprintln!("agent_wiki: skipping {}: {e:#}", root.join(&rel).display());
            }
        }
        for gone in stored.keys() {
            self.remove_page(gone).await?;
        }
        Ok(())
    }

    /// Reads, parses and (re)indexes one page; `rel` is `/`-separated with
    /// `.md`. The stored `mtime` is taken from the opened file, so it matches
    /// the content read, and the edited time is that `mtime`. A file that no
    /// longer exists is removed from the index; one that can't be read is
    /// removed too, and the read error returned.
    pub async fn index_file(&self, root: &Path, rel: &str) -> Result<()> {
        self.write_page(root, rel, None).await
    }

    /// `index_file`, or with `scan` the initial scan's write of it: one
    /// transaction, which with `scan` writes nothing if the stored row is
    /// newer than the file read.
    async fn write_page(&self, root: &Path, rel: &str, scan: Option<Scan>) -> Result<()> {
        let path = root.join(rel);
        let read = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::File::open(path)?;
            let mtime = mtime_nanos(&file.metadata()?);
            let mut source = String::new();
            file.read_to_string(&mut source)?;
            Ok::<_, std::io::Error>((source, mtime))
        })
        .await?;
        let (source, mtime) = match read {
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                self.remove_page(rel).await?;
                return Ok(());
            }
            Err(e) => {
                self.remove_page(rel).await?;
                return Err(e.into());
            }
        };
        let edited = match scan {
            Some(Scan {
                committed: Some(committed),
                started,
            }) if mtime <= started => committed,
            _ => mtime.div_euclid(1_000_000_000),
        };
        let (page, links, tags) = rows_for(&*self.host, rel, &source, mtime, edited);
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await?;
        if scan.is_some() {
            let stored = first_i64(&tx, "SELECT mtime FROM page WHERE path = ?1", [rel]).await?;
            if stored.is_some_and(|stored| stored > page.mtime) {
                return Ok(());
            }
        }
        tx.execute(
            "INSERT INTO page (path, url, folder, title, key_full, key_stripped, mtime, body, edited,
                 summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(path) DO UPDATE SET url = excluded.url, folder = excluded.folder,
                 title = excluded.title, key_full = excluded.key_full,
                 key_stripped = excluded.key_stripped, mtime = excluded.mtime,
                 body = excluded.body, edited = excluded.edited, summary = excluded.summary",
            params![
                page.path.as_str(),
                page.url.as_str(),
                page.folder.as_str(),
                page.title.as_str(),
                page.key_full.as_str(),
                page.key_stripped.as_str(),
                page.mtime,
                page.body.as_str(),
                page.edited,
                page.summary.as_str(),
            ],
        )
        .await?;
        tx.execute("DELETE FROM wikilink WHERE source = ?1", [rel])
            .await?;
        for link in &links {
            tx.execute(
                "INSERT INTO wikilink (source, ordinal, key) VALUES (?1, ?2, ?3)",
                params![rel, link.ordinal, link.key.as_str()],
            )
            .await?;
        }
        tx.execute("DELETE FROM tag WHERE source = ?1", [rel])
            .await?;
        for tag in &tags {
            tx.execute(
                "INSERT INTO tag (source, tag, ordinal) VALUES (?1, ?2, ?3)",
                params![rel, tag.tag.as_str(), tag.ordinal],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Removes exactly the page at `rel`, if indexed, with its rows; true if
    /// there was one.
    pub async fn remove_page(&self, rel: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await?;
        let [page, links, tags] = REMOVE_PAGE;
        let removed = collect(tx.query(page, [rel]).await?, |_| Ok(())).await?;
        tx.execute(links, [rel]).await?;
        tx.execute(tags, [rel]).await?;
        tx.commit().await?;
        Ok(!removed.is_empty())
    }

    /// Removes the page at `rel_or_folder` and every page under it as a folder
    /// prefix; returns the paths of the pages removed.
    pub async fn remove_path(&self, rel_or_folder: &str) -> Result<Vec<String>> {
        let (low, high) = subtree(rel_or_folder.trim_end_matches('/'));
        let params = [rel_or_folder, low.as_str(), high.as_str()];
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await?;
        let [pages, links, tags] = REMOVE_PATH;
        let removed = collect(tx.query(pages, params).await?, |row| Ok(row.get(0)?)).await?;
        tx.execute(links, params).await?;
        tx.execute(tags, params).await?;
        tx.commit().await?;
        Ok(removed)
    }

    /// Removes every page under `folder` (its `[folder/, folder0)` subtree),
    /// but not a page at `folder` itself; returns the paths of the pages
    /// removed. For a path known to be a folder.
    pub async fn remove_folder(&self, folder: &str) -> Result<Vec<String>> {
        let (low, high) = subtree(folder.trim_end_matches('/'));
        let params = [low.as_str(), high.as_str()];
        let conn = self.conn.lock().await;
        let tx = conn.transaction().await?;
        let [pages, links, tags] = REMOVE_FOLDER;
        let removed = collect(tx.query(pages, params).await?, |row| Ok(row.get(0)?)).await?;
        tx.execute(links, params).await?;
        tx.execute(tags, params).await?;
        tx.commit().await?;
        Ok(removed)
    }

    /// Stored `mtime` of each indexed page in `folder` or below it (`""` is
    /// the root), keyed by path.
    pub async fn mtimes_under(&self, folder: &str) -> Result<HashMap<String, i64>> {
        let conn = self.conn.lock().await;
        let rows = if folder.is_empty() {
            conn.query("SELECT path, mtime FROM page", ()).await?
        } else {
            let (low, high) = subtree(folder);
            conn.query(MTIMES_IN, [low, high]).await?
        };
        let rows = collect(rows, |row| Ok((row.get(0)?, row.get(1)?))).await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn resolve(&self, target: &LinkTarget) -> Result<Resolution> {
        let mut matches = {
            let conn = self.conn.lock().await;
            let rows = conn
                .query(
                    &format!(
                        "SELECT {PAGE_FIELDS} FROM page WHERE key_full = ?1 OR key_stripped = ?1"
                    ),
                    [target.name_key.as_str()],
                )
                .await?;
            collect(rows, PageRef::from_row).await?
        };
        matches.sort_by(|a, b| a.path.cmp(&b.path));
        if matches.len() > 1 && !target.qualifier.is_empty() {
            let narrowed: Vec<PageRef> = matches
                .iter()
                .filter(|p| folder_ends_with(&p.folder, &target.qualifier))
                .cloned()
                .collect();
            if !narrowed.is_empty() {
                matches = narrowed;
            }
        }
        Ok(match matches.len() {
            0 => Resolution::Missing,
            1 => Resolution::Unique(matches.remove(0)),
            _ => Resolution::Ambiguous(matches),
        })
    }

    /// Pages containing a wikilink with this link key, with the ordinal of
    /// the first one, ordered by page path.
    pub async fn references(&self, link_key: &str) -> Result<Vec<(PageRef, i64)>> {
        let conn = self.conn.lock().await;
        let rows = conn
            .query(
                "SELECT p.path, p.url, p.folder, p.title, min(w.ordinal)
                 FROM wikilink w JOIN page p ON p.path = w.source
                 WHERE w.key = ?1 GROUP BY p.path ORDER BY p.path",
                [link_key],
            )
            .await?;
        collect(rows, |row| Ok((PageRef::from_row(row)?, row.get(4)?))).await
    }

    /// Pages carrying `tag`, with the ordinal of the first inline occurrence
    /// (`None` when the tag is only in frontmatter), ordered by page path.
    pub async fn tagged(&self, tag: &str) -> Result<Vec<(PageRef, Option<i64>)>> {
        let conn = self.conn.lock().await;
        let rows = conn
            .query(
                "SELECT p.path, p.url, p.folder, p.title, t.ordinal
                 FROM tag t JOIN page p ON p.path = t.source
                 WHERE t.tag = ?1 ORDER BY p.path",
                [tag.to_lowercase()],
            )
            .await?;
        collect(rows, |row| Ok((PageRef::from_row(row)?, row.get(4)?))).await
    }

    /// Pages matching `query`, each once, in its highest tier: title matches
    /// (by score), then pages tagged with a query term or the whole query
    /// (by title), then body matches (by score). Terms are whole words,
    /// ANDed; a query with no letters or digits matches nothing. A repeated
    /// term is queried once: ANDing it again matches the same pages, and
    /// FTS5's cost grows faster than the number of phrases.
    pub async fn search(&self, query: &str) -> Result<Vec<PageRef>> {
        let mut distinct = BTreeSet::new();
        let terms: Vec<String> = search_terms(query)
            .into_iter()
            .filter(|t| distinct.insert(t.clone()))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let phrase = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut tags = terms.clone();
        tags.push(
            query
                .trim()
                .to_lowercase()
                .trim_start_matches('#')
                .to_string(),
        );
        let placeholders = vec!["?"; tags.len()].join(", ");
        let (by_title, mut tag_tier, by_body) = {
            let conn = self.conn.lock().await;
            let rows = conn.query(BY_FTS, [format!("title : ({phrase})")]).await?;
            let by_title = collect(rows, PageRef::from_row).await?;
            let rows = conn
                .query(
                    &format!(
                        "SELECT {PAGE_FIELDS} FROM page WHERE path IN
                             (SELECT source FROM tag WHERE tag IN ({placeholders}))"
                    ),
                    tags,
                )
                .await?;
            let tagged = collect(rows, PageRef::from_row).await?;
            let rows = conn.query(BY_FTS, [format!("body : ({phrase})")]).await?;
            (by_title, tagged, collect(rows, PageRef::from_row).await?)
        };
        tag_tier.sort_by_cached_key(|p| (p.title.to_lowercase(), p.path.clone()));

        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for page in by_title.into_iter().chain(tag_tier).chain(by_body) {
            if seen.insert(page.path.clone()) {
                out.push(page);
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    pub async fn page(&self, rel: &str) -> Result<Option<PageRef>> {
        let conn = self.conn.lock().await;
        let rows = conn
            .query(
                &format!("SELECT {PAGE_FIELDS} FROM page WHERE path = ?1"),
                [rel],
            )
            .await?;
        Ok(collect(rows, PageRef::from_row).await?.pop())
    }

    /// Immediate subfolders containing at least one page (at any depth) and
    /// the pages directly in `folder`, each sorted case-insensitively by
    /// display name. `folder` is `""` for the root.
    pub async fn folder_listing(&self, folder: &str) -> Result<(Vec<String>, Vec<PageRef>)> {
        let prefix = folder_prefix(folder);
        let (mut pages, deeper) = {
            let conn = self.conn.lock().await;
            let rows = conn.query(LISTING_PAGES, [folder]).await?;
            let pages = collect(rows, PageRef::from_row).await?;
            let rows = if folder.is_empty() {
                conn.query("SELECT DISTINCT folder FROM page WHERE folder != ''", ())
                    .await?
            } else {
                let (low, high) = subtree(folder);
                conn.query(LISTING_DEEPER_IN, [low.as_str(), high.as_str(), folder])
                    .await?
            };
            let deeper: Vec<String> = collect(rows, |row| Ok(row.get(0)?)).await?;
            (pages, deeper)
        };
        let folders = subfolders(&prefix, deeper.into_iter());
        pages.sort_by_cached_key(|p| (p.title.to_lowercase(), p.path.clone()));
        Ok((folders, pages))
    }

    /// What the folder listing shows of each page directly in `folder`,
    /// keyed by path.
    pub async fn listing_details(&self, folder: &str) -> Result<HashMap<String, PageDetails>> {
        let conn = self.conn.lock().await;
        let rows = conn.query(LISTING_DETAILS, [folder]).await?;
        let mut details: HashMap<String, PageDetails> = collect(rows, |row| {
            let details = PageDetails {
                summary: row.get(1)?,
                edited: row.get(2)?,
                tags: Vec::new(),
            };
            Ok((row.get(0)?, details))
        })
        .await?
        .into_iter()
        .collect();
        let rows = conn.query(LISTING_TAGS, [folder]).await?;
        for (path, tag) in collect(rows, |row| Ok((row.get::<String>(0)?, row.get(1)?))).await? {
            if let Some(page) = details.get_mut(&path) {
                page.tags.push(tag);
            }
        }
        Ok(details)
    }

    /// The `n` most recently edited pages in `folder` or below it (`""` is
    /// the root), newest first, ties by path, each with its edited time in
    /// seconds since the epoch.
    pub async fn recent(&self, folder: &str, n: usize) -> Result<Vec<(PageRef, i64)>> {
        // The root walks the `page_recent` index; a folder looks up its
        // subtree and sorts that.
        let n = i64::try_from(n).unwrap_or(i64::MAX);
        let conn = self.conn.lock().await;
        let rows = if folder.is_empty() {
            conn.query(
                &format!(
                    "SELECT {PAGE_FIELDS}, edited FROM page
                     ORDER BY edited DESC, path LIMIT ?1"
                ),
                [n],
            )
            .await?
        } else {
            let (low, high) = subtree(folder);
            conn.query(RECENT_IN, params![low, high, n]).await?
        };
        collect(rows, |row| Ok((PageRef::from_row(row)?, row.get(4)?))).await
    }

    /// The `n` heaviest tags on pages in `folder` or below it (`""` is the
    /// root), ties alphabetical, returned in alphabetical order. A tag's
    /// weight is the number of distinct pages carrying it.
    pub async fn tag_cloud(&self, folder: &str, n: usize) -> Result<Vec<(String, usize)>> {
        // A page has one `tag` row per distinct tag, so rows count pages.
        let rows = {
            let conn = self.conn.lock().await;
            let rows = if folder.is_empty() {
                conn.query("SELECT tag, count(*) FROM tag GROUP BY tag", ())
                    .await?
            } else {
                let (low, high) = subtree(folder);
                conn.query(CLOUD_IN, [low, high]).await?
            };
            collect(rows, |row| Ok((row.get(0)?, row.get::<i64>(1)?))).await?
        };
        let mut heaviest: Vec<(String, usize)> = rows
            .into_iter()
            .map(|(tag, pages)| (tag, pages as usize))
            .collect();
        heaviest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        heaviest.truncate(n);
        heaviest.sort();
        Ok(heaviest)
    }

    /// The page's tags (inline and frontmatter), lowercased, deduped and sorted.
    pub async fn page_tags(&self, rel: &str) -> Result<Vec<String>> {
        let mut tags: Vec<String> = {
            let conn = self.conn.lock().await;
            let rows = conn
                .query("SELECT tag FROM tag WHERE source = ?1", [rel])
                .await?;
            collect(rows, |row| Ok(row.get(0)?)).await?
        };
        tags.sort();
        tags.dedup();
        Ok(tags)
    }

    /// What the folder listing shows of each page in `paths` that is
    /// indexed, keyed by path, with each page's tags deduped and sorted.
    pub async fn details(&self, paths: &[String]) -> Result<HashMap<String, PageDetails>> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = vec!["?"; paths.len()].join(", ");
        let conn = self.conn.lock().await;
        let rows = conn
            .query(
                &format!("SELECT path, summary, edited FROM page WHERE path IN ({placeholders})"),
                paths.to_vec(),
            )
            .await?;
        let mut details: HashMap<String, PageDetails> = collect(rows, |row| {
            let details = PageDetails {
                summary: row.get(1)?,
                edited: row.get(2)?,
                tags: Vec::new(),
            };
            Ok((row.get(0)?, details))
        })
        .await?
        .into_iter()
        .collect();
        let rows = conn
            .query(
                &format!("SELECT source, tag FROM tag WHERE source IN ({placeholders})"),
                paths.to_vec(),
            )
            .await?;
        for (path, tag) in collect(rows, |row| Ok((row.get::<String>(0)?, row.get(1)?))).await? {
            if let Some(page) = details.get_mut(&path) {
                page.tags.push(tag);
            }
        }
        for page in details.values_mut() {
            page.tags.sort();
            page.tags.dedup();
        }
        Ok(details)
    }
}

/// Fails with `not an agent_wiki index` if the database has another
/// application id and holds any schema object; otherwise returns whether
/// the id is agent_wiki's. Only reads.
async fn refuse_foreign(conn: &Connection) -> Result<bool> {
    let app_id = first_i64(conn, "PRAGMA application_id", ()).await?;
    let objects = first_i64(conn, "SELECT count(*) FROM sqlite_schema", ()).await?;
    let ours = app_id == Some(i64::from(AGENT_WIKI_APP_ID));
    if !ours && objects != Some(0) {
        bail!("not an agent_wiki index");
    }
    Ok(ours)
}

/// The root stored in `meta`, if the table and its row exist.
async fn stored_root(conn: &Connection) -> Result<Option<String>> {
    let tables = first_i64(
        conn,
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'meta'",
        (),
    )
    .await?;
    if tables != Some(1) {
        return Ok(None);
    }
    Ok(texts(conn, "SELECT root FROM meta").await?.pop())
}

/// Drops every table and view in the open transaction's database, then
/// creates [`SCHEMA`], stores `root` and sets both header pragmas.
async fn rebuild(conn: &Connection, root: &str) -> Result<()> {
    let quoted = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    // Dropping a virtual table drops its shadow tables, so they go first.
    let virtual_tables = texts(
        conn,
        "SELECT name FROM sqlite_schema
         WHERE type = 'table' AND sql LIKE 'CREATE VIRTUAL TABLE%'",
    )
    .await?;
    for name in virtual_tables {
        conn.execute(&format!("DROP TABLE {}", quoted(&name)), ())
            .await?;
    }
    // Dropping a table drops its indexes and triggers.
    let tables = texts(
        conn,
        "SELECT name FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
    )
    .await?;
    for name in tables {
        conn.execute(&format!("DROP TABLE {}", quoted(&name)), ())
            .await?;
    }
    let views = texts(conn, "SELECT name FROM sqlite_schema WHERE type = 'view'").await?;
    for name in views {
        conn.execute(&format!("DROP VIEW {}", quoted(&name)), ())
            .await?;
    }
    conn.execute_batch(SCHEMA).await?;
    conn.execute("INSERT INTO meta (root) VALUES (?1)", [root])
        .await?;
    conn.execute(&format!("PRAGMA user_version = {INDEX_VERSION}"), ())
        .await?;
    conn.execute(&format!("PRAGMA application_id = {AGENT_WIKI_APP_ID}"), ())
        .await?;
    Ok(())
}

/// The query as lowercased words of letters and digits; every other
/// character separates words.
pub fn search_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn rows_for<P: Host>(
    host: &P,
    rel: &str,
    source: &str,
    mtime: i64,
    edited: i64,
) -> (PageRow, Vec<WikilinkRow>, Vec<TagRow>) {
    let ex = parse(host, source).extract();
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let keys = FileKeys::from_rel(rel);
    let folder = rel.rsplit_once('/').map(|(f, _)| f).unwrap_or("");
    let page = PageRow {
        path: rel.to_string(),
        url: path_to_url(rel),
        folder: folder.to_string(),
        title: ex.title(stem),
        key_full: keys.full,
        key_stripped: keys.stripped,
        mtime,
        body: ex.body_text.clone(),
        edited,
        summary: ex.summary(),
    };
    let links = ex
        .wikilinks
        .iter()
        .enumerate()
        .filter_map(|(n, raw)| {
            let target = LinkTarget::parse(raw)?;
            Some(WikilinkRow {
                key: target.name_key,
                ordinal: n as i64,
            })
        })
        .collect();
    // One row per distinct tag, carrying its first inline ordinal (`None`
    // when it is only in frontmatter).
    let inline = ex
        .inline_tags
        .iter()
        .enumerate()
        .map(|(n, tag)| (tag.clone(), Some(n as i64)));
    let frontmatter = ex.frontmatter_tags.iter().map(|t| (t.to_lowercase(), None));
    let mut seen = BTreeSet::new();
    let tags = inline
        .chain(frontmatter)
        .filter(|(tag, _)| seen.insert(tag.clone()))
        .map(|(tag, ordinal)| TagRow { tag, ordinal })
        .collect();
    (page, links, tags)
}

/// The initial scan's write of a page.
#[derive(Debug, Clone, Copy)]
struct Scan {
    /// The page's latest commit time, if it was tracked and clean.
    committed: Option<i64>,
    /// When the scan started, nanoseconds since the epoch.
    started: i64,
}

/// `folder/`, or `""` for the root: a prefix matching paths under `folder`.
fn folder_prefix(folder: &str) -> String {
    if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    }
}

/// Latest commit time (seconds since the epoch) of each `.md` file under
/// `root` that git tracks and that has no uncommitted change, keyed by rel
/// path. Empty when `root` is not in a git work tree, git is unavailable, or
/// any git command fails, so every page then uses its `mtime`.
async fn git_edited_times(root: &Path) -> HashMap<String, i64> {
    let Some(prefix) = git(root, &["rev-parse", "--show-prefix"]).await else {
        return HashMap::new();
    };
    let prefix = String::from_utf8_lossy(&prefix)
        .trim_end_matches('\n')
        .to_string();
    let (log, status) = tokio::join!(
        git(
            root,
            &[
                "log",
                "--format=%x01%ct",
                "--name-only",
                "--relative",
                "-z",
                "--",
                "."
            ],
        ),
        git(
            root,
            &[
                "status",
                "--porcelain",
                "-z",
                "--untracked-files=all",
                "--no-renames",
                "--",
                "."
            ],
        ),
    );
    let (Some(log), Some(status)) = (log, status) else {
        return HashMap::new();
    };
    let mut times = commit_times(&log);
    for dirty in dirty_paths(&status, &prefix) {
        times.remove(&dirty);
    }
    times
}

/// Runs `git args` in `dir`; its stdout if it exits successfully.
async fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The latest commit time of each `.md` path in `git log -z
/// --format=%x01%ct --name-only` output: records `\x01<time>\0` followed by
/// `\n<path>\0<path>\0…`, newest first. Non-UTF-8 paths are skipped.
fn commit_times(log: &[u8]) -> HashMap<String, i64> {
    let mut times = HashMap::new();
    let mut current = None;
    for field in log.split(|&b| b == 0) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        if let Some(time) = field.strip_prefix(b"\x01") {
            current = std::str::from_utf8(time).ok().and_then(|t| t.parse().ok());
            continue;
        }
        let (Some(time), Ok(path)) = (current, std::str::from_utf8(field)) else {
            continue;
        };
        if path.ends_with(".md") {
            times.entry(path.to_string()).or_insert(time);
        }
    }
    times
}

/// The paths in `git status --porcelain -z --no-renames` output (`XY
/// <path>\0` entries, relative to the repository top level), with the
/// root's `prefix` from `git rev-parse --show-prefix` removed.
fn dirty_paths(status: &[u8], prefix: &str) -> Vec<String> {
    status
        .split(|&b| b == 0)
        .filter_map(|entry| std::str::from_utf8(entry.get(3..)?).ok())
        .filter_map(|path| path.strip_prefix(prefix))
        .map(str::to_string)
        .collect()
}

/// Distinct first segments of `folders` below `prefix`, sorted
/// case-insensitively with ties broken by the exact name.
fn subfolders(prefix: &str, folders: impl Iterator<Item = String>) -> Vec<String> {
    let names: BTreeSet<String> = folders
        .filter_map(|folder| {
            let rest = folder.strip_prefix(prefix)?;
            rest.split('/').next().map(str::to_string)
        })
        .collect();
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
    names
}

fn folder_ends_with(folder: &str, qualifier: &[String]) -> bool {
    let segments: Vec<String> = folder
        .split('/')
        .filter(|s| !s.is_empty())
        .map(norm)
        .collect();
    segments.ends_with(qualifier)
}

/// Collects the rel paths of `.md` files under `rel_dir` (`""` is the root),
/// each with its no-follow `mtime`, skipping hidden entries, symlinks and
/// non-UTF-8 names. Entries and subfolders that can't be read are logged to
/// stderr and skipped; only failing to read `rel_dir` itself is an error.
/// Walks on the blocking thread pool.
pub async fn collect_pages(root: &Path, rel_dir: &str) -> Result<Vec<(String, i64)>> {
    let (root, rel_dir) = (root.to_path_buf(), rel_dir.to_string());
    let walked = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        walk(&root, &rel_dir, &mut out).map(|()| out)
    });
    Ok(walked.await??)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Dir,
    File,
    Symlink,
    Missing,
    /// Neither a regular file, a folder nor a symlink (a socket, a FIFO…).
    Other,
}

impl FsKind {
    fn of(kind: std::fs::FileType) -> FsKind {
        if kind.is_symlink() {
            FsKind::Symlink
        } else if kind.is_dir() {
            FsKind::Dir
        } else if kind.is_file() {
            FsKind::File
        } else {
            FsKind::Other
        }
    }
}

/// What is at `root/rel` (`/`-separated; `""` is the root itself), without
/// following symlinks: one `symlink_metadata` per component, no listing. A
/// symlink at `rel` or at any folder between it and `root` gives `Symlink`.
/// Names match as the filesystem matches them, so on a case-insensitive
/// filesystem a differently-cased path finds the entry.
pub async fn fs_kind(root: &Path, rel: &str) -> std::io::Result<FsKind> {
    let (root, rel) = (root.to_path_buf(), rel.to_string());
    tokio::task::spawn_blocking(move || {
        let mut path = root;
        let mut kind = FsKind::Dir;
        for segment in rel.split('/').filter(|s| !s.is_empty()) {
            path.push(segment);
            kind = match std::fs::symlink_metadata(&path) {
                Ok(meta) => FsKind::of(meta.file_type()),
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    return Ok(FsKind::Missing);
                }
                Err(e) => return Err(e),
            };
            if kind == FsKind::Symlink {
                return Ok(kind);
            }
        }
        Ok(kind)
    })
    .await?
}

/// The entries of `root/rel_dir` by exact name, with no-follow kinds,
/// skipping hidden and non-UTF-8 names. One `read_dir`; an entry that can't
/// be read is logged and skipped.
pub async fn list_folder(root: &Path, rel_dir: &str) -> std::io::Result<HashMap<String, FsKind>> {
    let dir = root.join(rel_dir);
    tokio::task::spawn_blocking(move || {
        let mut out = HashMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    eprintln!("agent_wiki: skipping an entry in {}: {e}", dir.display());
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) => {
                    out.insert(name, FsKind::of(kind));
                }
                Err(e) => eprintln!("agent_wiki: skipping {}: {e}", entry.path().display()),
            }
        }
        Ok(out)
    })
    .await?
}

fn mtime_nanos(meta: &Metadata) -> i64 {
    meta.modified().map(mtime_of).unwrap_or(0)
}

/// Nanoseconds since the epoch; 0 before it.
fn mtime_of(time: std::time::SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn walk(root: &Path, rel_dir: &str, out: &mut Vec<(String, i64)>) -> std::io::Result<()> {
    let dir = root.join(rel_dir);
    for entry in std::fs::read_dir(&dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("agent_wiki: skipping an entry in {}: {e}", dir.display());
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let rel = if rel_dir.is_empty() {
            name.clone()
        } else {
            format!("{rel_dir}/{name}")
        };
        // `file_type` and `metadata` do not follow symlinks, so they are
        // neither dirs nor files here.
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(e) => {
                eprintln!("agent_wiki: skipping {}: {e}", entry.path().display());
                continue;
            }
        };
        if kind.is_dir() {
            if let Err(e) = walk(root, &rel, out) {
                eprintln!("agent_wiki: skipping {}: {e}", entry.path().display());
            }
        } else if kind.is_file() && name.ends_with(".md") {
            match entry.metadata() {
                Ok(meta) => out.push((rel, mtime_nanos(&meta))),
                Err(e) => eprintln!("agent_wiki: skipping {}: {e}", entry.path().display()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nom::bytes::complete::{tag, take_while1};
    use nom::sequence::delimited;

    use crate::draw::{DrawChildren, DrawCtx, Element};
    use crate::parse::{InlineStream, Never, Plain};
    use crate::template::DefaultTemplate;
    use crate::tree::{InlineKind, Parsed};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site")
    }

    async fn memory_index(root: &Path) -> Index<Plain> {
        Index::open(&IndexStorage::InMemory, root, Arc::new(Plain))
            .await
            .unwrap()
    }

    async fn fixture_index() -> Index<Plain> {
        let index = memory_index(&fixture()).await;
        index.scan(&fixture()).await.unwrap();
        index
    }

    async fn resolve(index: &Index<Plain>, raw: &str) -> Resolution {
        index
            .resolve(&LinkTarget::parse(raw).unwrap())
            .await
            .unwrap()
    }

    fn unique_path(r: Resolution) -> String {
        match r {
            Resolution::Unique(p) => p.path,
            other => panic!("expected unique, got {other:?}"),
        }
    }

    fn ambiguous_paths(r: Resolution) -> Vec<String> {
        match r {
            Resolution::Ambiguous(ps) => ps.into_iter().map(|p| p.path).collect(),
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolutions_match_fixture_table() {
        let index = fixture_index().await;
        for raw in ["Foo Example", "FOO_example"] {
            assert_eq!(
                unique_path(resolve(&index, raw).await),
                "2021-01-01-foo-example.md"
            );
        }
        assert_eq!(
            ambiguous_paths(resolve(&index, "Shared Name").await),
            [
                "Projects/Alpha/Shared Name.md",
                "Projects/Beta/shared_name.md"
            ]
        );
        assert_eq!(
            unique_path(resolve(&index, "Projects/Alpha/Shared Name").await),
            "Projects/Alpha/Shared Name.md"
        );
        assert_eq!(
            unique_path(resolve(&index, "Beta/Shared Name").await),
            "Projects/Beta/shared_name.md"
        );
        assert_eq!(
            ambiguous_paths(resolve(&index, "Daily").await),
            ["Notes/2024-03-05 Daily.md", "Notes/2024-03-06 Daily.md"]
        );
        assert_eq!(
            unique_path(resolve(&index, "2024-03-05 Daily").await),
            "Notes/2024-03-05 Daily.md"
        );
        assert_eq!(resolve(&index, "Missing Page").await, Resolution::Missing);
        assert_eq!(unique_path(resolve(&index, "home").await), "Home.md");
    }

    #[tokio::test]
    async fn titles_match_fixture_table() {
        let index = fixture_index().await;
        for (rel, title) in [
            ("Home.md", "Welcome Home"),
            ("2021-01-01-foo-example.md", "Foo, Dated"),
            ("Projects/Alpha/Shared Name.md", "Shared Name"),
            ("Projects/Beta/shared_name.md", "Beta Shared"),
            ("Projects/Beta/Deep/Nested Note.md", "Nested Note"),
            ("Notes/2024-03-05 Daily.md", "2024-03-05 Daily"),
            ("Notes/2024-03-06 Daily.md", "2024-03-06 Daily"),
        ] {
            let page = index.page(rel).await.unwrap().unwrap();
            assert_eq!(page.title, title, "{rel}");
        }
        let alpha = index
            .page("Projects/Alpha/Shared Name.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alpha.url, "/Projects/Alpha/Shared%20Name");
        assert_eq!(alpha.folder, "Projects/Alpha");
    }

    #[tokio::test]
    async fn references_to_missing_page() {
        let index = fixture_index().await;
        let refs: Vec<(String, i64)> = index
            .references("missing page")
            .await
            .unwrap()
            .into_iter()
            .map(|(p, n)| (p.path, n))
            .collect();
        assert_eq!(
            refs,
            [
                ("Home.md".to_string(), 8),
                ("Projects/Alpha/Shared Name.md".to_string(), 0),
                ("Projects/Beta/Deep/Nested Note.md".to_string(), 0),
            ]
        );
    }

    #[tokio::test]
    async fn tagged_pages() {
        let index = fixture_index().await;
        let status: Vec<(String, Option<i64>)> = index
            .tagged("status")
            .await
            .unwrap()
            .into_iter()
            .map(|(p, n)| (p.path, n))
            .collect();
        assert_eq!(
            status,
            [
                ("Home.md".to_string(), Some(1)),
                ("Projects/Alpha/Shared Name.md".to_string(), Some(0)),
            ]
        );
        let meta: Vec<(String, Option<i64>)> = index
            .tagged("meta")
            .await
            .unwrap()
            .into_iter()
            .map(|(p, n)| (p.path, n))
            .collect();
        assert_eq!(meta, [("Home.md".to_string(), None)]);
        assert_eq!(
            index.page_tags("Home.md").await.unwrap(),
            ["meta", "project/alpha", "status"]
        );
    }

    #[tokio::test]
    async fn hidden_and_non_markdown_are_not_pages() {
        let index = fixture_index().await;
        assert_eq!(index.page(".hidden/Secret.md").await.unwrap(), None);
        assert_eq!(index.page("Notes/ignored.txt").await.unwrap(), None);
    }

    #[tokio::test]
    async fn listing_details_hold_summary_and_tags() {
        let index = fixture_index().await;
        let details = index.listing_details("Notes").await.unwrap();
        assert_eq!(details.len(), 5);
        let tagged = &details["Notes/Weather Tagged.md"];
        assert_eq!(tagged.summary, "Filed under #zephyr only.");
        assert_eq!(tagged.tags, ["zephyr"]);
        assert!(tagged.edited > 0);
        assert_eq!(
            details["Notes/2024-03-05 Daily.md"].summary,
            "Notes for the fifth."
        );
        assert!(details["Notes/2024-03-05 Daily.md"].tags.is_empty());
        let home = &index.listing_details("").await.unwrap()["Home.md"];
        assert_eq!(home.tags, ["meta", "project/alpha", "status"]);
    }

    #[tokio::test]
    async fn root_folder_listing() {
        let index = fixture_index().await;
        let (folders, pages) = index.folder_listing("").await.unwrap();
        assert_eq!(folders, ["Notes", "Projects"]);
        let paths: Vec<String> = pages.into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["2021-01-01-foo-example.md", "Home.md"]);

        let (folders, pages) = index.folder_listing("Projects/Beta").await.unwrap();
        assert_eq!(folders, ["Deep"]);
        let paths: Vec<String> = pages.into_iter().map(|p| p.path).collect();
        assert_eq!(paths, ["Projects/Beta/shared_name.md"]);
    }

    #[tokio::test]
    async fn remove_folder_then_resolve() {
        let index = fixture_index().await;
        let mut removed = index.remove_path("Projects/Beta").await.unwrap();
        removed.sort();
        assert_eq!(
            removed,
            [
                "Projects/Beta/Deep/Nested Note.md",
                "Projects/Beta/shared_name.md"
            ]
        );
        assert!(index.remove_path("Projects/Beta").await.unwrap().is_empty());
        assert_eq!(
            unique_path(resolve(&index, "Shared Name").await),
            "Projects/Alpha/Shared Name.md"
        );
        assert_eq!(
            index
                .page("Projects/Beta/Deep/Nested Note.md")
                .await
                .unwrap(),
            None
        );
        let refs = index.references("missing page").await.unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[tokio::test]
    async fn folder_removal_keeps_prefix_sharing_siblings() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let removed = ["a/x.md", "a/b/y.md", "a/ü/z.md"];
        let kept = [
            "a.md", "a b/x.md", "a.b/x.md", "a-b/x.md", "a0/x.md", "aé/x.md", "a%/x.md", "a_/x.md",
        ];
        for rel in removed.iter().chain(&kept) {
            put(root, rel, "# Page\n\n[[Elsewhere]] #t\n");
        }
        let index = memory_index(root).await;
        index.scan(root).await.unwrap();
        let mut gone = index.remove_path("a").await.unwrap();
        gone.sort();
        let mut expected = removed.map(String::from).to_vec();
        expected.sort();
        assert_eq!(gone, expected);
        let mut left: Vec<String> = index.mtimes_under("").await.unwrap().into_keys().collect();
        left.sort();
        let mut expected = kept.map(String::from).to_vec();
        expected.sort();
        assert_eq!(left, expected);
        assert_eq!(
            index.references("elsewhere").await.unwrap().len(),
            kept.len()
        );
        assert_eq!(index.tagged("t").await.unwrap().len(), kept.len());
    }

    #[test]
    fn subfolders_dedup_and_order_are_consistent() {
        let rows = ["x/A", "x/a", "x/A/deep", "x/a", "x/b", "x/A"].map(str::to_string);
        assert_eq!(subfolders("x/", rows.into_iter()), ["A", "a", "b"]);
    }

    #[tokio::test]
    async fn listing_ties_break_by_path() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["c.md", "a.md", "b.md"] {
            std::fs::write(dir.path().join(name), "# Same\n").unwrap();
        }
        let index = memory_index(dir.path()).await;
        index.scan(dir.path()).await.unwrap();
        for _ in 0..5 {
            let (_, pages) = index.folder_listing("").await.unwrap();
            let paths: Vec<String> = pages.into_iter().map(|p| p.path).collect();
            assert_eq!(paths, ["a.md", "b.md", "c.md"]);
            index.index_file(dir.path(), "a.md").await.unwrap();
        }
    }

    #[tokio::test]
    async fn index_file_on_vanished_file_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.md"), "[[X]] #t\n").unwrap();
        let index = memory_index(dir.path()).await;
        index.scan(dir.path()).await.unwrap();
        assert!(index.page("gone.md").await.unwrap().is_some());
        std::fs::remove_file(dir.path().join("gone.md")).unwrap();
        index.index_file(dir.path(), "gone.md").await.unwrap();
        assert_eq!(index.page("gone.md").await.unwrap(), None);
        assert!(index.references("x").await.unwrap().is_empty());
        assert!(index.tagged("t").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn index_file_on_unreadable_file_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.md"), "[[X]] #t\n").unwrap();
        let index = memory_index(dir.path()).await;
        index.scan(dir.path()).await.unwrap();
        assert!(index.page("bad.md").await.unwrap().is_some());
        std::fs::write(dir.path().join("bad.md"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(index.index_file(dir.path(), "bad.md").await.is_err());
        assert_eq!(index.page("bad.md").await.unwrap(), None);
        assert!(index.references("x").await.unwrap().is_empty());
        assert!(index.tagged("t").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reindex_replaces_rows() {
        let index = fixture_index().await;
        index.index_file(&fixture(), "Home.md").await.unwrap();
        let refs = index.references("missing page").await.unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(index.tagged("meta").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fs_kind_checks_each_component_without_following_symlinks() {
        let root = fixture();
        for (rel, want) in [
            ("", FsKind::Dir),
            ("Projects/Beta", FsKind::Dir),
            ("Home.md", FsKind::File),
            ("Nope.md", FsKind::Missing),
            ("Home.md/x.md", FsKind::Missing),
        ] {
            assert_eq!(fs_kind(&root, rel).await.unwrap(), want, "{rel}");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "a").unwrap();
        std::os::unix::fs::symlink(root.join("Home.md"), dir.path().join("l.md")).unwrap();
        std::os::unix::fs::symlink(root.join("Projects"), dir.path().join("ld")).unwrap();
        for (rel, want) in [
            ("a.md", FsKind::File),
            ("l.md", FsKind::Symlink),
            ("ld/Beta/shared_name.md", FsKind::Symlink),
        ] {
            assert_eq!(fs_kind(dir.path(), rel).await.unwrap(), want, "{rel}");
        }
    }

    #[tokio::test]
    async fn mtimes_match_the_walk() {
        let root = fixture();
        let index = fixture_index().await;
        let walked: HashMap<String, i64> = collect_pages(&root, "Projects")
            .await
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(index.mtimes_under("Projects").await.unwrap(), walked);
        assert!(walked.values().all(|&m| m > 0));
        assert_eq!(index.mtimes_under("").await.unwrap().len(), 11);
    }

    async fn search(index: &Index<Plain>, query: &str) -> Vec<String> {
        index
            .search(query)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.path)
            .collect()
    }

    #[tokio::test]
    async fn search_ranks_title_then_tag_then_body() {
        let index = fixture_index().await;
        let zephyr = [
            "Notes/Zephyr Notes.md",
            "Notes/Weather Tagged.md",
            "Notes/Body Only.md",
        ];
        assert_eq!(search(&index, "zephyr").await, zephyr);
        assert_eq!(search(&index, "ZEPHYR").await, zephyr);
        assert_eq!(
            search(&index, "gusts zephyr").await,
            ["Notes/Weather Tagged.md", "Notes/Body Only.md"]
        );
        assert!(search(&index, "!!!").await.is_empty());
        assert!(search(&index, "   ").await.is_empty());
        let alpha = search(&index, "project/alpha").await;
        assert_eq!(
            alpha.first().map(String::as_str),
            Some("Home.md"),
            "{alpha:?}"
        );
    }

    #[tokio::test]
    async fn repeated_terms_are_queried_once() {
        let index = fixture_index().await;
        for query in ["zephyr", "gusts zephyr"] {
            let repeated = format!("{query} ").repeat(20_000);
            let started = std::time::Instant::now();
            let found = search(&index, &repeated).await;
            let elapsed = started.elapsed();
            assert_eq!(found, search(&index, query).await, "{query}");
            assert!(!found.is_empty(), "{query}");
            assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
        }
        assert_eq!(
            search(&index, "Zephyr zephyr ZEPHYR").await,
            search(&index, "zephyr").await
        );
    }

    #[tokio::test]
    async fn fts5_syntax_in_queries_is_plain_text() {
        let index = fixture_index().await;
        for query in [
            "\"",
            "\"\"",
            "NEAR(a b)",
            "title:x",
            "title:zephyr",
            "*",
            "a*",
            "^x",
            "(",
            "'",
            "zephyr OR NOT gusts",
        ] {
            if let Err(e) = index.search(query).await {
                panic!("{query}: {e:#}");
            }
        }
        assert_eq!(
            search(&index, "title:zephyr").await,
            search(&index, "title zephyr").await
        );
    }

    #[test]
    fn git_output_parsing() {
        let log = b"\x011791225600\0\na.md\0b \xc3\xa9.md\0x.txt\0\x01981158400\0\na.md\0old.md\0";
        let times = commit_times(log);
        assert_eq!(times.len(), 3, "{times:?}");
        assert_eq!(times["a.md"], 1_791_225_600);
        assert_eq!(times["b é.md"], 1_791_225_600);
        assert_eq!(times["old.md"], 981_158_400);
        let status = b" M sub/a.md\0?? sub/n.md\0?? other.md\0";
        assert_eq!(dirty_paths(status, "sub/"), ["a.md", "n.md"]);
        assert_eq!(
            dirty_paths(status, ""),
            ["sub/a.md", "sub/n.md", "other.md"]
        );
    }

    #[tokio::test]
    async fn scan_does_not_replace_a_newer_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "# Newer\n").unwrap();
        let index = memory_index(dir.path()).await;
        // The watcher indexes the edit first…
        index.index_file(dir.path(), "a.md").await.unwrap();
        // …then the scan reads a file older than that row (as if it had
        // read it before the edit).
        std::fs::write(&path, "# Older\n").unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        index.scan(dir.path()).await.unwrap();
        assert_eq!(index.page("a.md").await.unwrap().unwrap().title, "Newer");
        // Not the scan: the watcher's own write always lands.
        index.index_file(dir.path(), "a.md").await.unwrap();
        assert_eq!(index.page("a.md").await.unwrap().unwrap().title, "Older");
    }

    #[tokio::test]
    async fn scan_dates_files_changed_since_it_started_by_mtime() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
        let index = memory_index(dir.path()).await;
        let now = mtime_of(std::time::SystemTime::now());
        let hour = 3600 * 1_000_000_000;
        let edited = |index: Index<Plain>| async move { index.recent("", 1).await.unwrap()[0].1 };
        let scan = |started| Scan {
            committed: Some(1_000),
            started,
        };
        index
            .write_page(dir.path(), "a.md", Some(scan(now + hour)))
            .await
            .unwrap();
        assert_eq!(edited(index.clone()).await, 1_000);
        index
            .write_page(dir.path(), "a.md", Some(scan(now - hour)))
            .await
            .unwrap();
        assert!(edited(index.clone()).await > now / 1_000_000_000 - 60);
    }

    #[tokio::test]
    async fn reindex_drops_rows_the_page_no_longer_has() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "[[X]] [[Y]] #t #u #t\n").unwrap();
        let index = memory_index(dir.path()).await;
        index.index_file(dir.path(), "a.md").await.unwrap();
        assert_eq!(index.page_tags("a.md").await.unwrap(), ["t", "u"]);
        assert_eq!(
            index.tag_cloud("", 50).await.unwrap(),
            [("t".into(), 1), ("u".into(), 1)]
        );
        std::fs::write(&path, "[[Y]] #u\n").unwrap();
        index.index_file(dir.path(), "a.md").await.unwrap();
        assert_eq!(index.page_tags("a.md").await.unwrap(), ["u"]);
        assert!(index.references("x").await.unwrap().is_empty());
        let y: Vec<i64> = index
            .references("y")
            .await
            .unwrap()
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(y, [0]);
        assert!(index.tagged("t").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reindex_clears_the_ordinal_of_a_reused_tag_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        let index = memory_index(dir.path()).await;
        let tagged = async |tag: &str| -> Vec<(String, Option<i64>)> {
            let rows = index.tagged(tag).await.unwrap();
            rows.into_iter().map(|(p, n)| (p.path, n)).collect()
        };
        let page = |n| vec![("p.md".to_string(), n)];

        // Inline `#a`, frontmatter `b` swapped to inline `#b`, frontmatter `a`.
        std::fs::write(&path, "---\ntags: [b]\n---\n#a\n").unwrap();
        index.index_file(dir.path(), "p.md").await.unwrap();
        assert_eq!(tagged("a").await, page(Some(0)));
        assert_eq!(tagged("b").await, page(None));
        std::fs::write(&path, "---\ntags: [a]\n---\n#b\n").unwrap();
        index.index_file(dir.path(), "p.md").await.unwrap();
        assert_eq!(tagged("a").await, page(None));
        assert_eq!(tagged("b").await, page(Some(0)));

        // Row `1:p.md` holds `a` with ordinal 1, then `a` again but only in
        // frontmatter: the reused row's ordinal must be cleared.
        std::fs::write(&path, "---\ntags: [b]\n---\n#c #a\n").unwrap();
        index.index_file(dir.path(), "p.md").await.unwrap();
        assert_eq!(tagged("a").await, page(Some(1)));
        std::fs::write(&path, "---\ntags: [a]\n---\n#b\n").unwrap();
        index.index_file(dir.path(), "p.md").await.unwrap();
        assert_eq!(tagged("a").await, page(None));
        assert_eq!(tagged("b").await, page(Some(0)));
        assert!(tagged("c").await.is_empty());
        assert_eq!(index.page_tags("p.md").await.unwrap(), ["a", "b"]);
    }

    /// Runs `sql` on the index's connection.
    async fn execute(index: &Index<Plain>, sql: &str) {
        index.conn.lock().await.execute(sql, ()).await.unwrap();
    }

    /// The first column of the first row of `sql`, as an integer.
    async fn query_i64(index: &Index<Plain>, sql: &str) -> Option<i64> {
        let conn = index.conn.lock().await;
        first_i64(&conn, sql, ()).await.unwrap()
    }

    async fn row_count(index: &Index<Plain>, table: &str) -> i64 {
        query_i64(index, &format!("SELECT count(*) FROM {table}"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn special_character_paths_index_reindex_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let folder = "a⟩b/Ünïcödé 日本";
        let rel = format!("{folder}/⟨x⟩:y `q` 'r' \"s\".md");
        std::fs::create_dir_all(dir.path().join(folder)).unwrap();
        let path = dir.path().join(&rel);
        let index = memory_index(dir.path()).await;
        let counts = async || {
            [
                row_count(&index, "page").await,
                row_count(&index, "wikilink").await,
                row_count(&index, "tag").await,
            ]
        };

        std::fs::write(&path, "---\ntags: [fm]\n---\n[[X]] [[Y]] #t #u\n").unwrap();
        index.index_file(dir.path(), &rel).await.unwrap();
        assert_eq!(index.page(&rel).await.unwrap().unwrap().path, rel);
        assert_eq!(index.page_tags(&rel).await.unwrap(), ["fm", "t", "u"]);
        assert_eq!(index.references("x").await.unwrap()[0].0.path, rel);
        assert_eq!(counts().await, [1, 2, 3]);

        // Same row ids, changed contents; then fewer rows.
        std::fs::write(&path, "---\ntags: [u]\n---\n[[Y]] [[Z]] #v #fm\n").unwrap();
        index.index_file(dir.path(), &rel).await.unwrap();
        assert_eq!(counts().await, [1, 2, 3]);
        assert_eq!(index.page_tags(&rel).await.unwrap(), ["fm", "u", "v"]);
        assert!(index.references("x").await.unwrap().is_empty());
        let z = index.references("z").await.unwrap();
        assert_eq!((z[0].0.path.as_str(), z[0].1), (rel.as_str(), 1));
        assert_eq!(index.tagged("fm").await.unwrap()[0].1, Some(1));
        assert_eq!(index.tagged("u").await.unwrap()[0].1, None);
        assert!(index.tagged("t").await.unwrap().is_empty());
        std::fs::write(&path, "[[W]] #w\n").unwrap();
        index.index_file(dir.path(), &rel).await.unwrap();
        assert_eq!(counts().await, [1, 1, 1]);
        assert_eq!(index.page_tags(&rel).await.unwrap(), ["w"]);

        let removed = index.remove_path("a⟩b").await.unwrap();
        assert_eq!(removed, std::slice::from_ref(&rel));
        assert_eq!(counts().await, [0, 0, 0]);
        assert_eq!(index.page(&rel).await.unwrap(), None);
    }

    #[tokio::test]
    async fn recent_and_tag_cloud_are_scoped_to_the_folder() {
        let index = fixture_index().await;
        let recent = index.recent("Projects", 10).await.unwrap();
        assert_eq!(recent.len(), 4, "{recent:?}");
        assert!(recent.iter().all(|(p, _)| p.path.starts_with("Projects/")));
        assert!(
            recent
                .windows(2)
                .all(|w| (-w[0].1, &w[0].0.path) <= (-w[1].1, &w[1].0.path))
        );
        assert_eq!(index.recent("", 3).await.unwrap().len(), 3);
        let cloud = index.tag_cloud("", 50).await.unwrap();
        assert!(cloud.contains(&("status".to_string(), 2)), "{cloud:?}");
        assert!(cloud.contains(&("meta".to_string(), 1)), "{cloud:?}");
        assert!(cloud.windows(2).all(|w| w[0].0 < w[1].0), "{cloud:?}");
        let projects = index.tag_cloud("Projects", 50).await.unwrap();
        assert!(
            projects.contains(&("status".to_string(), 1)),
            "{projects:?}"
        );
        assert!(!projects.iter().any(|(t, _)| t == "meta"), "{projects:?}");
        let top = index.tag_cloud("", 1).await.unwrap();
        assert_eq!(top, [("status".to_string(), 2)]);
    }

    /// Opens a `FilePath` index, retrying for up to 5 s while it is still
    /// locked by another handle.
    async fn reopen(storage: &IndexStorage, root: &Path) -> Index<Plain> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match Index::open(storage, root, Arc::new(Plain)).await {
                Ok(index) => return index,
                Err(e)
                    if format!("{e:#}").contains("lock")
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => panic!("{e:#}"),
            }
        }
    }

    async fn stored_meta(index: &Index<Plain>) -> (Option<i64>, Option<String>) {
        let version = query_i64(index, "PRAGMA user_version").await;
        let conn = index.conn.lock().await;
        let root = texts(&conn, "SELECT root FROM meta").await.unwrap().pop();
        (version, root)
    }

    /// Sets the stored version to one this build doesn't write.
    async fn bump_version(index: &Index<Plain>) {
        execute(
            index,
            &format!("PRAGMA user_version = {}", INDEX_VERSION + 1),
        )
        .await;
    }

    /// A `FilePath` index over `root` in a fresh tempdir, scanned then dropped.
    async fn scanned_and_closed(root: &Path) -> (tempfile::TempDir, IndexStorage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = IndexStorage::FilePath(dir.path().join("idx"));
        let index = Index::open(&storage, root, Arc::new(Plain)).await.unwrap();
        index.scan(root).await.unwrap();
        assert!(row_count(&index, "page").await > 0);
        (dir, storage)
    }

    #[tokio::test]
    async fn persisted_rows_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let storage = IndexStorage::FilePath(dir.path().join("idx"));
        let root = fixture();
        let snapshot = async |index: &Index<Plain>| {
            let mut resolved = Vec::new();
            for raw in ["Home", "Foo Example", "Shared Name", "Missing Page"] {
                resolved.push(resolve(index, raw).await);
            }
            (
                resolved,
                index.tagged("status").await.unwrap(),
                index.search("zephyr").await.unwrap(),
                index.folder_listing("").await.unwrap(),
            )
        };
        let index = Index::open(&storage, &root, Arc::new(Plain)).await.unwrap();
        index.scan(&root).await.unwrap();
        let before = snapshot(&index).await;
        assert!(!before.1.is_empty() && !before.2.is_empty() && !before.3.1.is_empty());
        drop(index);
        let index = reopen(&storage, &root).await;
        assert_eq!(snapshot(&index).await, before);
    }

    #[tokio::test]
    async fn version_mismatch_rebuilds() {
        let (_dir, storage) = scanned_and_closed(&fixture()).await;
        let index = reopen(&storage, &fixture()).await;
        bump_version(&index).await;
        drop(index);
        let index = reopen(&storage, &fixture()).await;
        assert_eq!(row_count(&index, "page").await, 0);
        assert_eq!(stored_meta(&index).await.0, Some(INDEX_VERSION));
    }

    #[tokio::test]
    async fn version_1_index_rebuilds() {
        let (_dir, storage) = scanned_and_closed(&fixture()).await;
        let index = reopen(&storage, &fixture()).await;
        execute(&index, "PRAGMA user_version = 1").await;
        drop(index);
        let index = reopen(&storage, &fixture()).await;
        assert_eq!(row_count(&index, "page").await, 0);
        assert_eq!(stored_meta(&index).await.0, Some(INDEX_VERSION));
    }

    /// Claims `[[ABC-N]]` as a ticket, whose text is its ID.
    struct TicketHost;

    struct Ticket(String);

    impl Element<TicketHost> for Ticket {
        fn draw(&self, _: &DrawCtx<'_>, _: DrawChildren<'_, TicketHost>) -> maud::Markup {
            maud::html! { (self.0) }
        }

        fn text(&self) -> Option<String> {
            Some(self.0.clone())
        }
    }

    impl Host for TicketHost {
        type Block = Never;
        type Inline = Ticket;
        type Template = DefaultTemplate;

        fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
            let id = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-';
            let id = s.try_parse(delimited(tag("[["), take_while1(id), tag("]]")))?;
            Some(s.leaf(InlineKind::Host(Ticket(id.to_string()))))
        }
    }

    #[tokio::test]
    async fn a_claimed_wikilink_leaves_the_wikilink_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.md"), "See [[ABC-1]] and [[Other]].\n").unwrap();
        let key = LinkTarget::parse("ABC-1").unwrap().name_key;
        let plain = memory_index(&root).await;
        plain.scan(&root).await.unwrap();
        let refs = plain.references(&key).await.unwrap();
        assert_eq!(refs.len(), 1);

        let index = Index::open(&IndexStorage::InMemory, &root, Arc::new(TicketHost))
            .await
            .unwrap();
        index.scan(&root).await.unwrap();
        assert!(index.references(&key).await.unwrap().is_empty());
        // The ticket is not a wikilink, so `[[Other]]` is ordinal 0.
        let refs = index.references("other").await.unwrap();
        assert_eq!(refs.into_iter().map(|(_, n)| n).collect::<Vec<_>>(), [0]);
        // Its `text()` is in the body, so it is still found by search.
        let found = index.search("ABC-1").await.unwrap();
        assert_eq!(
            found.into_iter().map(|p| p.path).collect::<Vec<_>>(),
            ["a.md"]
        );
    }

    #[tokio::test]
    async fn missing_version_record_rebuilds() {
        let (_dir, storage) = scanned_and_closed(&fixture()).await;
        let index = reopen(&storage, &fixture()).await;
        assert!(row_count(&index, "page").await > 0);
        execute(&index, "DELETE FROM meta").await;
        drop(index);
        let index = reopen(&storage, &fixture()).await;
        assert_eq!(row_count(&index, "page").await, 0);
    }

    #[tokio::test]
    async fn other_root_rebuilds() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(a.path().join("a.md"), "# A\n").unwrap();
        let (_dir, storage) = scanned_and_closed(a.path()).await;
        let index = reopen(&storage, b.path()).await;
        assert_eq!(row_count(&index, "page").await, 0);
        let b_root = b.path().to_string_lossy().into_owned();
        assert_eq!(
            stored_meta(&index).await,
            (Some(INDEX_VERSION), Some(b_root))
        );
    }

    #[tokio::test]
    async fn invalid_index_paths_fail() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let text = dir.path().join("Home.md");
        let bytes = "# Home\n\nMarkdown, not a database.\n".repeat(50);
        std::fs::write(&text, &bytes).unwrap();
        for path in [folder, text.clone()] {
            let storage = IndexStorage::FilePath(path.clone());
            let Err(e) = Index::open(&storage, &fixture(), Arc::new(Plain)).await else {
                panic!("opened {}", path.display());
            };
            let message = format!("{e:#}");
            assert!(message.contains(&path.display().to_string()), "{message}");
        }
        assert_eq!(std::fs::read_to_string(&text).unwrap(), bytes);
    }

    /// Writes `rel` under `root`, creating its folder.
    fn put(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A `FilePath` index over `root` in a fresh tempdir, scanned and open.
    async fn persisted(root: &Path) -> (tempfile::TempDir, IndexStorage, Index<Plain>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = IndexStorage::FilePath(dir.path().join("idx"));
        let index = Index::open(&storage, root, Arc::new(Plain)).await.unwrap();
        index.scan(root).await.unwrap();
        (dir, storage, index)
    }

    /// A tree with links and tags, in the root and in a subfolder.
    fn reconcile_tree(root: &Path) {
        put(root, "a.md", "# A\n\n[[B]] [[C]] #shared zephyr\n");
        put(root, "b.md", "# B\n\n[[A]] #shared\n");
        put(root, "c.md", "# C\n\n[[X]] #ctag #shared\n");
        put(root, "d/x.md", "# X\n\n[[C]] [[Y]] #dtag zephyr\n");
        put(root, "d/y.md", "---\ntags: [dtag]\n---\n# Y\n\n[[A]]\n");
    }

    type Snapshot = (
        Vec<Resolution>,
        Vec<Vec<(PageRef, Option<i64>)>>,
        Vec<Vec<String>>,
        Vec<(Vec<String>, Vec<PageRef>)>,
        [i64; 3],
    );

    /// What a reader sees of `index`: resolutions, tag pages, searches,
    /// listings and the raw row counts.
    async fn snapshot(index: &Index<Plain>) -> Snapshot {
        let mut resolved = Vec::new();
        for raw in ["A", "B", "C", "X", "Y", "E", "G"] {
            resolved.push(resolve(index, raw).await);
        }
        let mut tagged = Vec::new();
        for tag in ["shared", "ctag", "dtag", "etag"] {
            tagged.push(index.tagged(tag).await.unwrap());
        }
        let mut searched = Vec::new();
        for query in ["zephyr", "a", "x"] {
            searched.push(search(index, query).await);
        }
        let mut listings = Vec::new();
        for folder in ["", "d", "f"] {
            listings.push(index.folder_listing(folder).await.unwrap());
        }
        let counts = [
            row_count(index, "page").await,
            row_count(index, "wikilink").await,
            row_count(index, "tag").await,
        ];
        (resolved, tagged, searched, listings, counts)
    }

    async fn fresh_snapshot(root: &Path) -> Snapshot {
        let index = memory_index(root).await;
        index.scan(root).await.unwrap();
        snapshot(&index).await
    }

    #[tokio::test]
    async fn unchanged_files_are_not_reread() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        put(root, "a.md", "# A\n");
        put(root, "b.md", "# B\n");
        let (_dir, storage, index) = persisted(root).await;
        execute(
            &index,
            "UPDATE page SET title = 'sentinel' WHERE path = 'a.md'",
        )
        .await;
        drop(index);
        let b = root.join("b.md");
        let modified = std::fs::metadata(&b).unwrap().modified().unwrap();
        std::fs::write(&b, "# B two\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_secs(1))
            .unwrap();
        let index = reopen(&storage, root).await;
        index.scan(root).await.unwrap();
        assert_eq!(index.page("a.md").await.unwrap().unwrap().title, "sentinel");
        assert_eq!(index.page("b.md").await.unwrap().unwrap().title, "B two");
    }

    #[tokio::test]
    async fn files_deleted_while_closed_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        reconcile_tree(root);
        let (_dir, storage, index) = persisted(root).await;
        assert_eq!(index.tagged("dtag").await.unwrap().len(), 2);
        drop(index);
        std::fs::remove_file(root.join("c.md")).unwrap();
        std::fs::remove_dir_all(root.join("d")).unwrap();
        let index = reopen(&storage, root).await;
        index.scan(root).await.unwrap();
        let (folders, pages) = index.folder_listing("").await.unwrap();
        assert!(folders.is_empty(), "{folders:?}");
        let paths: Vec<&str> = pages.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md"]);
        assert!(index.folder_listing("d").await.unwrap().1.is_empty());
        for raw in ["C", "X", "Y"] {
            assert_eq!(resolve(&index, raw).await, Resolution::Missing, "{raw}");
        }
        assert!(index.tagged("ctag").await.unwrap().is_empty());
        assert!(index.tagged("dtag").await.unwrap().is_empty());
        let fresh = fresh_snapshot(root).await;
        assert_eq!(fresh.4, [2, 3, 2]);
        assert_eq!(snapshot(&index).await, fresh);
    }

    #[tokio::test]
    async fn files_added_while_closed_are_indexed() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        reconcile_tree(root);
        let (_dir, storage, index) = persisted(root).await;
        drop(index);
        put(root, "e.md", "# E\n\n[[G]] #etag\n");
        put(root, "f/g.md", "# G\n\n[[E]] #etag\n");
        let index = reopen(&storage, root).await;
        index.scan(root).await.unwrap();
        assert_eq!(unique_path(resolve(&index, "E").await), "e.md");
        assert_eq!(unique_path(resolve(&index, "G").await), "f/g.md");
        assert_eq!(index.tagged("etag").await.unwrap().len(), 2);
        assert_eq!(index.folder_listing("f").await.unwrap().1.len(), 1);
        assert_eq!(snapshot(&index).await, fresh_snapshot(root).await);
    }

    #[tokio::test]
    async fn rebuild_then_scan_matches_fresh() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        reconcile_tree(root);
        let (_dir, storage, index) = persisted(root).await;
        bump_version(&index).await;
        drop(index);
        let index = reopen(&storage, root).await;
        assert_eq!(row_count(&index, "page").await, 0);
        index.scan(root).await.unwrap();
        let fresh = fresh_snapshot(root).await;
        assert_eq!(fresh.4, [5, 7, 6]);
        assert_eq!(snapshot(&index).await, fresh);
    }

    /// Runs `git args` in `dir` with a fixed identity, no signing and no
    /// hooks, dated `date` when given.
    fn git_at(dir: &Path, args: &[&str], date: Option<&str>) {
        let mut command = std::process::Command::new("git");
        command
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir);
        if let Some(date) = date {
            command
                .env("GIT_COMMITTER_DATE", date)
                .env("GIT_AUTHOR_DATE", date);
        }
        let status = command.status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn edited_time_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        put(root, "old.md", "# Old\n");
        put(root, "changed.md", "# Changed\n");
        git_at(root, &["init", "-q"], None);
        git_at(root, &["add", "old.md", "changed.md"], None);
        let commit = ["commit", "-q", "--no-verify", "-m", "old"];
        git_at(root, &commit, Some("2001-02-03T00:00:00Z"));
        let edited = async |index: &Index<Plain>| -> BTreeMap<String, i64> {
            let recent = index.recent("", 10).await.unwrap();
            recent.into_iter().map(|(p, t)| (p.path, t)).collect()
        };
        let feb_2001 = 981_158_400;
        let (_dir, storage, index) = persisted(root).await;
        assert_eq!(
            edited(&index).await,
            BTreeMap::from([("changed.md".into(), feb_2001), ("old.md".into(), feb_2001)])
        );
        drop(index);
        put(root, "changed.md", "# Changed\n\nNow dirty.\n");
        // Re-date the commit without touching the files: a re-read of
        // `old.md` would now take 2005, a skipped one keeps 2001.
        let amend = ["commit", "-q", "--amend", "--no-edit", "--no-verify"];
        git_at(root, &amend, Some("2005-01-01T00:00:00Z"));
        let index = reopen(&storage, root).await;
        index.scan(root).await.unwrap();
        let after = edited(&index).await;
        assert_eq!(after["old.md"], feb_2001);
        let now = mtime_of(std::time::SystemTime::now()) / 1_000_000_000;
        assert!((now - 60..=now).contains(&after["changed.md"]), "{after:?}");
    }

    /// Opens the SQLite file at `path` directly, without agent_wiki.
    async fn raw_connection(path: &Path) -> Connection {
        let db = Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        db.connect().unwrap()
    }

    #[tokio::test]
    async fn foreign_sqlite_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        {
            let conn = raw_connection(&path).await;
            conn.execute_batch("CREATE TABLE other (x); INSERT INTO other VALUES (1);")
                .await
                .unwrap();
            let id = first_i64(&conn, "PRAGMA application_id", ()).await.unwrap();
            assert_eq!(id, Some(0));
        }
        let before = std::fs::read(&path).unwrap();
        // Header bytes 18–19 are the write/read format versions: 1 in
        // rollback-journal mode, 2 in WAL mode.
        assert_eq!(before[18..20], [1, 1]);
        let storage = IndexStorage::FilePath(path.clone());
        let Err(e) = Index::open(&storage, &fixture(), Arc::new(Plain)).await else {
            panic!("opened {}", path.display());
        };
        let message = format!("{e:#}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("not an agent_wiki index"), "{message}");
        assert!(std::fs::read(&path).unwrap() == before, "the file changed");
        let siblings = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(siblings, 1, "a journal file was left beside it");
        let conn = raw_connection(&path).await;
        let rows = first_i64(&conn, "SELECT count(*) FROM other", ()).await;
        assert_eq!(rows.unwrap(), Some(1));
    }

    /// The raw connection closes cleanly, so no `-wal` is pending and the
    /// refused file stays byte-identical. Only a file with a pending `-wal`
    /// can be checkpointed by the refusal (L-E6); that case isn't covered.
    #[tokio::test]
    async fn foreign_wal_mode_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        {
            let conn = raw_connection(&path).await;
            let journal = conn.query("PRAGMA journal_mode = WAL", ()).await.unwrap();
            let mode = collect(journal, |row| Ok(row.get::<String>(0)?)).await;
            assert_eq!(mode.unwrap(), ["wal"]);
            conn.execute_batch("CREATE TABLE other (x); INSERT INTO other VALUES (1);")
                .await
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        // Header bytes 18–19 are 2 in WAL mode.
        assert_eq!(before[18..20], [2, 2]);
        let wal = dir.path().join("other.db-wal");
        assert!(!wal.exists(), "a -wal is pending");
        let storage = IndexStorage::FilePath(path.clone());
        let Err(e) = Index::open(&storage, &fixture(), Arc::new(Plain)).await else {
            panic!("opened {}", path.display());
        };
        let message = format!("{e:#}");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("not an agent_wiki index"), "{message}");
        assert!(std::fs::read(&path).unwrap() == before, "the file changed");
        assert!(!wal.exists(), "a -wal was left beside it");
        let conn = raw_connection(&path).await;
        let rows = first_i64(&conn, "SELECT count(*) FROM other", ()).await;
        assert_eq!(rows.unwrap(), Some(1));
        let id = first_i64(&conn, "PRAGMA application_id", ()).await.unwrap();
        assert_eq!(id, Some(0));
    }

    #[tokio::test]
    async fn missing_parent_folders_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/idx.db");
        let storage = IndexStorage::FilePath(path.clone());
        let _index = Index::open(&storage, &fixture(), Arc::new(Plain))
            .await
            .unwrap();
        assert!(path.is_file());
    }

    #[tokio::test]
    async fn lock_is_released_when_the_index_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let storage = IndexStorage::FilePath(dir.path().join("idx.db"));
        let first = Index::open(&storage, &fixture(), Arc::new(Plain))
            .await
            .unwrap();
        let Err(e) = Index::open(&storage, &fixture(), Arc::new(Plain)).await else {
            panic!("a second opener succeeded");
        };
        assert!(format!("{e:#}").contains("lock"), "{e:#}");
        drop(first);
        Index::open(&storage, &fixture(), Arc::new(Plain))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rebuild_drops_objects_of_another_version() {
        let dir = tempfile::tempdir().unwrap();
        let storage = IndexStorage::FilePath(dir.path().join("idx.db"));
        let root = fixture();
        let index = Index::open(&storage, &root, Arc::new(Plain)).await.unwrap();
        execute(&index, "CREATE TABLE stale (x)").await;
        bump_version(&index).await;
        drop(index);
        let index = reopen(&storage, &root).await;
        let stale = "SELECT count(*) FROM sqlite_schema WHERE name = 'stale'";
        assert_eq!(query_i64(&index, stale).await, Some(0));
        index.scan(&root).await.unwrap();
        assert!(!search(&index, "zephyr").await.is_empty());
        assert_eq!(
            query_i64(&index, "PRAGMA user_version").await,
            Some(INDEX_VERSION)
        );
        assert_eq!(
            query_i64(&index, "PRAGMA application_id").await,
            Some(i64::from(AGENT_WIKI_APP_ID))
        );
    }

    #[tokio::test]
    async fn removals_use_indexes() {
        let index = fixture_index().await;
        let (low, high) = subtree("Projects");
        let range = [low.as_str(), high.as_str()];
        let path_and_range = ["Projects", low.as_str(), high.as_str()];
        let mut statements: Vec<(&str, Vec<&str>)> = Vec::new();
        statements.extend(REMOVE_PAGE.map(|sql| (sql, vec!["Home.md"])));
        statements.extend(REMOVE_PATH.map(|sql| (sql, path_and_range.to_vec())));
        statements.extend(REMOVE_FOLDER.map(|sql| (sql, range.to_vec())));
        statements.push((MTIMES_IN, range.to_vec()));
        statements.push((LISTING_PAGES, vec!["Projects"]));
        statements.push((LISTING_DETAILS, vec!["Projects"]));
        statements.push((LISTING_TAGS, vec!["Projects"]));
        statements.push((LISTING_DEEPER_IN, path_and_range.to_vec()));
        statements.push((RECENT_IN, vec![low.as_str(), high.as_str(), "10"]));
        statements.push((CLOUD_IN, range.to_vec()));
        let conn = index.conn.lock().await;
        for (sql, params) in statements {
            let rows = conn
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
                .await
                .unwrap();
            let details: Vec<String> = collect(rows, |row| Ok(row.get(3)?)).await.unwrap();
            assert!(!details.is_empty(), "{sql}");
            assert!(
                details.iter().all(|d| !d.starts_with("SCAN")),
                "{sql}: {details:?}"
            );
        }
    }

    #[tokio::test]
    async fn reconciliation_removes_exactly() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        put(root, "x.md", "# X\n");
        let (_dir, storage, index) = persisted(root).await;
        assert!(index.page("x.md").await.unwrap().is_some());
        drop(index);
        std::fs::remove_file(root.join("x.md")).unwrap();
        put(root, "x.md/y.md", "# Y\n");
        let index = reopen(&storage, root).await;
        index.scan(root).await.unwrap();
        assert_eq!(index.page("x.md").await.unwrap(), None);
        assert!(index.page("x.md/y.md").await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_reads_and_writes() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().to_path_buf();
        reconcile_tree(&root);
        let index = memory_index(&root).await;
        index.scan(&root).await.unwrap();
        let pages = ["a.md", "b.md", "c.md", "d/x.md", "d/y.md"];
        let mut tasks = tokio::task::JoinSet::new();
        for t in 0..8 {
            let (writer, root) = (index.clone(), root.clone());
            tasks.spawn(async move {
                for n in 0..50 {
                    writer
                        .index_file(&root, pages[(t + n) % pages.len()])
                        .await?;
                }
                anyhow::Ok(())
            });
            let reader = index.clone();
            tasks.spawn(async move {
                for _ in 0..50 {
                    reader.search("zephyr").await?;
                    reader.folder_listing("").await?;
                    reader.tag_cloud("", 50).await?;
                }
                anyhow::Ok(())
            });
        }
        while let Some(done) = tasks.join_next().await {
            if let Err(e) = done.unwrap() {
                panic!("{e:#}");
            }
        }
        assert_eq!(snapshot(&index).await, fresh_snapshot(&root).await);
    }

    #[tokio::test]
    async fn tokenizer_folds_diacritics_only() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        put(root, "p.md", "Café straße abc123\n");
        let index = memory_index(root).await;
        index.scan(root).await.unwrap();
        for query in ["cafe", "café", "straße", "abc123"] {
            assert_eq!(search(&index, query).await, ["p.md"], "{query}");
        }
        for query in ["strasse", "abc", "123"] {
            assert!(search(&index, query).await.is_empty(), "{query}");
        }
    }
}
