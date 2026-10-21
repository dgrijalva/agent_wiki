use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use agent_wiki::{IndexStorage, Wiki};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn ok_body(app: &Router, uri: &str) -> String {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Builds a `Wiki` over `root` on `storage`, retrying for up to 5 s while
/// the index is still locked by a just-dropped `Wiki`: the lock is released
/// when the last `Index` clone is dropped, which for a `Wiki` waits until
/// its watcher and scan tasks have ended; then waits for its initial scan.
async fn open(root: &Path, storage: &IndexStorage) -> Wiki {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Wiki::builder(root)
            .index_storage(storage.clone())
            .build()
            .await
        {
            Ok(wiki) => {
                wiki.indexed().await;
                return wiki;
            }
            Err(e) if format!("{e:#}").contains("lock") && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("{e:#}"),
        }
    }
}

#[tokio::test]
async fn second_wiki_on_the_same_index_fails() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.md"), "# A\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("idx");
    let storage = IndexStorage::FilePath(path.clone());
    let first = Wiki::builder(root.path())
        .index_storage(storage.clone())
        .build()
        .await
        .unwrap();
    let Err(e) = Wiki::builder(root.path())
        .index_storage(storage)
        .build()
        .await
    else {
        panic!("a second Wiki opened {}", path.display());
    };
    let message = format!("{e:#}");
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("lock"), "{message}");
    drop(first);
}

#[tokio::test]
async fn stale_rows_are_corrected_by_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("home.md"), "# Home\n").unwrap();
    fs::write(root.path().join("old.md"), "# Old\n\n#oldtag\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = IndexStorage::FilePath(dir.path().join("idx"));
    let first = open(root.path(), &storage).await;
    let app = first.router();
    assert!(
        ok_body(&app, "/_wiki/tag/oldtag")
            .await
            .contains("href=\"/old")
    );
    assert!(ok_body(&app, "/").await.contains("href=\"/old"));
    drop(app);
    drop(first);
    fs::remove_file(root.path().join("old.md")).unwrap();
    let second = open(root.path(), &storage).await;
    let app = second.router();
    let tag = ok_body(&app, "/_wiki/tag/oldtag").await;
    assert!(!tag.contains("<li>"), "{tag}");
    let index = ok_body(&app, "/").await;
    assert!(index.contains("href=\"/home"), "{index}");
    assert!(!index.contains("href=\"/old"), "{index}");
}

#[tokio::test]
async fn reindex_rebuilds_from_the_files() {
    let site = tempfile::tempdir().unwrap();
    fs::write(site.path().join("a.md"), "# Alpha\n\nZephyr.\n").unwrap();
    let store = tempfile::tempdir().unwrap();
    let storage = IndexStorage::FilePath(store.path().join("index.db"));
    let wiki = open(site.path(), &storage).await;
    wiki.reindex().await.unwrap();
    let found = wiki
        .search("zephyr", agent_wiki::SearchOpts::default())
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path(), "a.md");
    let body = ok_body(&wiki.router(), "/").await;
    assert!(body.contains(">Alpha</a>"), "{body}");
}
