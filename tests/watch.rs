use std::fs;
use std::path::Path;
use std::time::Duration;

use agent_wiki::Wiki;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const DEADLINE: Duration = Duration::from_secs(5);
const INTERVAL: Duration = Duration::from_millis(50);

async fn get_path(app: &Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// Polls `uri` until `check(status, body)` holds, panicking after the deadline.
async fn eventually(app: &Router, uri: &str, what: &str, check: impl Fn(StatusCode, &str) -> bool) {
    eventually_within(DEADLINE, app, uri, what, check).await;
}

async fn eventually_within(
    deadline: Duration,
    app: &Router,
    uri: &str,
    what: &str,
    check: impl Fn(StatusCode, &str) -> bool,
) {
    let start = Instant::now();
    loop {
        let (status, body) = get_path(app, uri).await;
        if check(status, &body) {
            return;
        }
        if start.elapsed() > deadline {
            panic!("{uri}: timed out waiting for {what}; last {status}:\n{body}");
        }
        sleep(INTERVAL).await;
    }
}

async fn eventually_contains(app: &Router, uri: &str, needle: &str) {
    eventually(app, uri, needle, |s, b| {
        s == StatusCode::OK && b.contains(needle)
    })
    .await;
}

async fn eventually_status(app: &Router, uri: &str, want: StatusCode) {
    eventually(app, uri, want.as_str(), |s, _| s == want).await;
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

async fn open(dir: &TempDir) -> Router {
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    wiki.router()
}

#[tokio::test]
async fn create_resolves_link() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "[[B]] #t\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/a").await;
    assert!(body.contains(r#"href="/_wiki/link?to=B""#), "{body}");
    write(dir.path(), "B.md", "bee\n");
    eventually_contains(&app, "/a", r#"href="/B""#).await;
}

#[tokio::test]
async fn modify_updates_title() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "B.md", "bee\n");
    let app = open(&dir).await;
    write(dir.path(), "B.md", "# Bee\n\nbee\n");
    eventually_contains(&app, "/", ">Bee</a>").await;
}

#[tokio::test]
async fn delete_unresolves_link() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "[[B]] #t\n");
    write(dir.path(), "B.md", "bee\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/a").await;
    assert!(body.contains(r#"href="/B""#), "{body}");
    fs::remove_file(dir.path().join("B.md")).unwrap();
    eventually_contains(&app, "/a", r#"href="/_wiki/link?to=B""#).await;
    eventually_status(&app, "/B", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn folder_rename_moves_pages() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "a\n");
    let app = open(&dir).await;
    write(dir.path(), "sub/C.md", "see\n");
    eventually_status(&app, "/sub/C", StatusCode::OK).await;
    fs::rename(dir.path().join("sub"), dir.path().join("moved")).unwrap();
    eventually_status(&app, "/moved/C", StatusCode::OK).await;
    eventually_status(&app, "/sub/C", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn page_replaced_by_same_named_folder_keeps_the_folders_pages() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "x.md", "# X\n\n#xtag\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/_wiki/tag/xtag").await;
    assert!(body.contains(r#"href="/x#tag-0""#), "{body}");
    fs::remove_file(dir.path().join("x.md")).unwrap();
    write(dir.path(), "x.md/y.md", "# Y\n\n#ytag\n");
    eventually_contains(&app, "/_wiki/tag/ytag", "x.md/y").await;
    eventually(&app, "/_wiki/tag/xtag", "no tagged pages", |s, b| {
        s == StatusCode::OK && !b.contains(r#"href="/x"#)
    })
    .await;
    // Once every event is applied, the folder's page is still indexed.
    sleep(Duration::from_millis(1000)).await;
    let (_, body) = get_path(&app, "/_wiki/tag/ytag").await;
    assert!(body.contains("x.md/y"), "{body}");
    let (_, body) = get_path(&app, "/").await;
    assert!(body.contains("x.md"), "{body}");
}

#[tokio::test]
async fn folder_replaced_by_same_named_page_keeps_the_page() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "x.md/y.md", "# Y\n\n#ytag\n");
    write(dir.path(), "o.md", "# O\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/_wiki/tag/ytag").await;
    assert!(body.contains("x.md/y"), "{body}");
    fs::remove_dir_all(dir.path().join("x.md")).unwrap();
    write(dir.path(), "x.md", "# X\n\n#xtag\n");
    eventually_contains(&app, "/_wiki/tag/xtag", r#"href="/x#tag-0""#).await;
    eventually(&app, "/_wiki/tag/ytag", "no tagged pages", |s, b| {
        s == StatusCode::OK && !b.contains("x.md/y")
    })
    .await;
    // Once every event is applied, the page is still indexed.
    sleep(Duration::from_millis(1000)).await;
    let (_, body) = get_path(&app, "/_wiki/tag/xtag").await;
    assert!(body.contains(r#"href="/x#tag-0""#), "{body}");
    let (_, body) = get_path(&app, "/").await;
    assert!(body.contains(r#"href="/x""#), "{body}");
    assert!(!body.contains("x.md/y"), "{body}");
}

#[tokio::test]
async fn tag_removal_empties_tag_page() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "[[B]] #t\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/_wiki/tag/t").await;
    assert!(body.contains(r#"href="/a#tag-0""#), "{body}");
    write(dir.path(), "a.md", "[[B]]\n");
    eventually(&app, "/_wiki/tag/t", "no tagged pages", |s, b| {
        s == StatusCode::OK && !b.contains(r#"href="/a"#)
    })
    .await;
}

#[tokio::test]
async fn hidden_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "[[x]]\n");
    let app = open(&dir).await;
    write(dir.path(), ".obsidian/x.md", "# Hidden\n");
    // A visible change after the hidden one; once it shows, the hidden write
    // has been through the watcher too.
    write(dir.path(), "sentinel.md", "# Sentinel\n");
    eventually_contains(&app, "/", ">Sentinel</a>").await;
    let (_, body) = get_path(&app, "/a").await;
    assert!(body.contains(r#"href="/_wiki/link?to=x""#), "{body}");
    let (_, root) = get_path(&app, "/").await;
    assert!(!root.contains(".obsidian"), "{root}");
    assert!(!root.contains("Hidden"), "{root}");
}

/// True when `dir`'s filesystem ignores case in names.
fn case_insensitive(dir: &Path) -> bool {
    fs::write(dir.join("case-probe"), "").unwrap();
    let insensitive = dir.join("CASE-PROBE").exists();
    fs::remove_file(dir.join("case-probe")).unwrap();
    insensitive
}

#[tokio::test]
async fn case_only_renames_leave_no_duplicate_or_ghost() {
    let dir = tempfile::tempdir().unwrap();
    if !case_insensitive(dir.path()) {
        eprintln!("skipping: filesystem is case-sensitive");
        return;
    }
    write(dir.path(), "a.md", "[[bar]]\n");
    write(dir.path(), "bar.md", "# Bar\n");
    write(dir.path(), "sub/C.md", "# See\n");
    let app = open(&dir).await;

    fs::rename(dir.path().join("bar.md"), dir.path().join("Bar.md")).unwrap();
    fs::rename(dir.path().join("sub"), dir.path().join("Sub")).unwrap();
    eventually(&app, "/", "one Bar and one Sub", |s, b| {
        s == StatusCode::OK
            && b.contains(r#"href="/Bar""#)
            && !b.contains(r#"href="/bar""#)
            && b.contains(r#"href="/Sub/""#)
            && !b.contains(r#"href="/sub/""#)
    })
    .await;
    eventually_contains(&app, "/a", r#"href="/Bar""#).await;
    eventually_status(&app, "/Sub/C", StatusCode::OK).await;

    fs::remove_file(dir.path().join("Bar.md")).unwrap();
    fs::remove_dir_all(dir.path().join("Sub")).unwrap();
    eventually(&app, "/", "no Bar and no Sub", |s, b| {
        s == StatusCode::OK && !b.contains(">Bar</a>") && !b.contains(">Sub/</a>")
    })
    .await;
    for uri in ["/bar", "/Bar", "/sub/C", "/Sub/C"] {
        eventually_status(&app, uri, StatusCode::NOT_FOUND).await;
    }
}

#[tokio::test]
async fn flood_in_one_folder_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..500 {
        write(dir.path(), &format!("big/p{n}.md"), &format!("# P{n}\n"));
    }
    let app = open(&dir).await;
    for n in 0..500 {
        write(
            dir.path(),
            &format!("big/p{n}.md"),
            &format!("# P{n}\n #flood"),
        );
    }
    eventually_within(
        Duration::from_secs(20),
        &app,
        "/_wiki/tag/flood",
        "500 flood pages",
        |s, b| s == StatusCode::OK && b.matches("<li>").count() == 500,
    )
    .await;
}

#[tokio::test]
async fn empty_folder_has_an_index() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n");
    let app = open(&dir).await;
    fs::create_dir(dir.path().join("e")).unwrap();
    let (status, body) = get_path(&app, "/e/").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = app
        .clone()
        .oneshot(Request::get("/e").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(response.headers()["location"], "/e/");
}

#[tokio::test]
async fn edits_become_searchable() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n\nNothing yet.\n");
    let app = open(&dir).await;
    let (_, body) = get_path(&app, "/_wiki/search?q=quokka").await;
    assert!(body.contains("No results"), "{body}");
    write(dir.path(), "a.md", "# A\n\nA quokka appears.\n");
    eventually_contains(&app, "/_wiki/search?q=quokka", "<a href=\"/a\">A</a>").await;
}

#[tokio::test]
async fn edit_moves_page_to_top_of_recent() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n");
    write(dir.path(), "b.md", "# B\n");
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(dir.path().join("a.md"))
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    let app = open(&dir).await;
    let first_recent = |b: &str| {
        let at = b.find("<h2>Recently edited</h2>")?;
        let rest = &b[at..];
        let href = rest.split("href=\"").nth(1)?;
        Some(href[..href.find('"')?].to_string())
    };
    let (_, body) = get_path(&app, "/").await;
    assert_eq!(first_recent(&body).as_deref(), Some("/b"), "{body}");
    write(dir.path(), "a.md", "# A\n\nEdited.\n");
    eventually(&app, "/", "a first under Recently edited", |s, b| {
        s == StatusCode::OK && first_recent(b).as_deref() == Some("/a")
    })
    .await;
}

/// Sends `GET uri` and returns the response, checking it is an SSE stream.
async fn subscribe(app: &Router, uri: &str) -> Body {
    let response = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/event-stream",
        "{uri}"
    );
    response.into_body()
}

/// The next data frame of `body` as text; `None` when the stream has ended.
async fn next_text(body: &mut Body) -> Option<String> {
    loop {
        let frame = body.frame().await?.unwrap();
        if let Ok(data) = frame.into_data() {
            return Some(String::from_utf8(data.to_vec()).unwrap());
        }
    }
}

#[tokio::test]
async fn sse_announces_changes_to_that_page() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "a.md", "# A\n");
    write(dir.path(), "b.md", "# B\n");
    let app = open(&dir).await;
    let mut body = subscribe(&app, "/_wiki/events/a").await;

    write(dir.path(), "b.md", "# B changed\n");
    // Let the watcher apply `b` on its own before `a` changes.
    sleep(Duration::from_millis(500)).await;
    write(dir.path(), "a.md", "# A changed\n");

    let mut seen = Vec::new();
    let first = tokio::time::timeout(DEADLINE, async {
        loop {
            let text = next_text(&mut body).await.expect("stream ended");
            if text.contains("event: changed") {
                return text;
            }
            seen.push(text);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no `changed` event; frames before: {seen:?}"));
    assert!(first.contains("data: a.md"), "{first}");
    assert!(!seen.iter().any(|t| t.contains("data: b.md")), "{seen:?}");
}

/// With `oneshot`, the streams' only owners of the `Wiki` are the router and
/// the `Wiki` itself, so dropping both ends them. This is not how a served
/// connection behaves: under `axum::serve` each connection keeps its own
/// router clone, so a connected client's stream does not end on the server
/// side, and a host's graceful shutdown waits for that client.
#[tokio::test]
async fn sse_streams_end_when_every_wiki_and_router_clone_is_dropped() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "a.md", "# A\n");
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    let app: Router = wiki.router();
    let mut a = subscribe(&app, "/_wiki/events/a").await;
    let mut missing = subscribe(&app, "/_wiki/events/not/yet").await;
    drop(app);
    drop(wiki);
    for body in [&mut a, &mut missing] {
        let ended =
            tokio::time::timeout(DEADLINE, async { while next_text(body).await.is_some() {} })
                .await;
        assert!(
            ended.is_ok(),
            "SSE stream still open after the wiki and its router were dropped"
        );
    }
}
