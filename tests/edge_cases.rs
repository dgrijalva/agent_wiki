//! Startup robustness, symlinks, anchor consistency and escaping, each on its
//! own temporary folder.

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

async fn ok_body(app: &Router, uri: &str) -> String {
    let (status, body) = get_path(app, uri).await;
    assert_eq!(status, StatusCode::OK, "{uri}:\n{body}");
    body
}

async fn eventually_contains(app: &Router, uri: &str, needle: &str) {
    let start = Instant::now();
    loop {
        let (status, body) = get_path(app, uri).await;
        if status == StatusCode::OK && body.contains(needle) {
            return;
        }
        if start.elapsed() > DEADLINE {
            panic!("{uri}: timed out waiting for {needle}; last {status}:\n{body}");
        }
        sleep(INTERVAL).await;
    }
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

/// The `#…` anchor of the listed link to `page_url` in a generated page.
fn listed_anchor(listing: &str, page_url: &str) -> String {
    let needle = format!("href=\"{page_url}#");
    let at = listing
        .find(&needle)
        .unwrap_or_else(|| panic!("{needle} not in:\n{listing}"));
    let rest = &listing[at + needle.len()..];
    rest[..rest.find('"').unwrap()].to_string()
}

#[tokio::test]
async fn multibyte_names_at_startup_and_while_running() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Meetings – Q3.md", "# Q3 meetings\n");
    write(dir.path(), "会議メモの記録.md", "# Memo\n");
    write(dir.path(), "a.md", "[[Meetings – Q3]] [[会議メモの記録]]\n");
    let app = open(&dir).await;
    let body = ok_body(&app, "/a").await;
    assert!(
        body.contains("href=\"/Meetings%20%E2%80%93%20Q3\""),
        "{body}"
    );
    assert!(
        body.contains("class=\"wikilink\">会議メモの記録</a>"),
        "{body}"
    );
    ok_body(&app, "/Meetings%20%E2%80%93%20Q3").await;

    // The watcher survives such a name and keeps applying later changes.
    write(dir.path(), "新しい議事録メモ.md", "# New memo\n");
    write(dir.path(), "later.md", "# Later\n");
    eventually_contains(&app, "/", ">New memo</a>").await;
    eventually_contains(&app, "/", ">Later</a>").await;
}

#[tokio::test]
async fn unreadable_files_and_folders_are_skipped_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "good.md", "# Good\n");
    fs::write(dir.path().join("bad.md"), [0xff, 0xfe, 0x00, b'x']).unwrap();
    write(dir.path(), "locked/inner.md", "# Inner\n");
    let locked = dir.path().join("locked");
    set_mode(&locked, 0o000);

    let opened = Wiki::builder(dir.path()).build().await;
    // The scan runs in the background: wait for it, and look at the index,
    // before the folder becomes readable again.
    let root = match &opened {
        Ok(wiki) => {
            wiki.indexed().await;
            Some(get_path(&wiki.router(), "/").await)
        }
        Err(_) => None,
    };
    set_mode(&locked, 0o755);
    let app: Router = opened.unwrap().router();
    let (status, root) = root.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(root.contains(">Good</a>"), "{root}");
    assert!(!root.contains("locked"), "{root}");

    ok_body(&app, "/good").await;
    assert_eq!(get_path(&app, "/bad").await.0, StatusCode::NOT_FOUND);
}

/// Restores a folder's mode when dropped, so the tempdir can be removed
/// even if the test fails first.
struct RestoreMode<'a>(&'a Path);

impl Drop for RestoreMode<'_> {
    fn drop(&mut self) {
        set_mode(self.0, 0o755);
    }
}

#[tokio::test]
async fn unlistable_folder_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n");
    write(dir.path(), "d/x.md", "# InX\n");
    let app = open(&dir).await;
    let root = ok_body(&app, "/").await;
    assert!(root.contains("href=\"/d/\""), "{root}");

    let d = dir.path().join("d");
    let _restore = RestoreMode(&d);
    // Traverse-only: files can still be opened by path, but `d` can't be listed.
    set_mode(&d, 0o311);
    let start = Instant::now();
    loop {
        let root = ok_body(&app, "/").await;
        if !root.contains("href=\"/d/\"") {
            break;
        }
        if start.elapsed() > DEADLINE {
            panic!("/ still lists d/:\n{root}");
        }
        sleep(INTERVAL).await;
    }
}

#[tokio::test]
async fn page_served_before_index_completes() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..3000 {
        write(
            dir.path(),
            &format!("p{n}.md"),
            &format!("# Page {n}\n\nBody #t{n}\n"),
        );
    }
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    let app: Router = wiki.router();
    let body = ok_body(&app, "/p2999").await;
    assert!(body.contains("<title>Page 2999</title>"), "{body}");

    wiki.indexed().await;
    let root = ok_body(&app, "/").await;
    assert!(root.contains("href=\"/p2999\""), "{root}");
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[tokio::test]
async fn symlinks_are_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.md", "# Secret\n");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n");
    let link = |name: &str, target: &Path| {
        std::os::unix::fs::symlink(target, dir.path().join(name)).unwrap();
    };
    link("linked.md", &outside.path().join("secret.md"));
    link("linkdir", outside.path());

    let app = open(&dir).await;
    assert_eq!(get_path(&app, "/linked").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        get_path(&app, "/linkdir/secret").await.0,
        StatusCode::NOT_FOUND
    );

    link("linked2.md", &outside.path().join("secret.md"));
    // A later visible change; once it shows, the symlink event has been applied.
    write(dir.path(), "sentinel.md", "# Sentinel\n");
    eventually_contains(&app, "/", ">Sentinel</a>").await;
    assert_eq!(get_path(&app, "/linked2").await.0, StatusCode::NOT_FOUND);
    let root = ok_body(&app, "/").await;
    assert!(!root.contains("Secret"), "{root}");
}

#[tokio::test]
async fn swapped_in_symlink_is_not_served_before_the_watcher_runs() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "victim.md", "# Secret outside\n");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "victim.md", "# Victim\n");
    write(dir.path(), "d/victim.md", "# Inner victim\n");
    let app = open(&dir).await;
    ok_body(&app, "/victim").await;
    ok_body(&app, "/d/victim").await;

    // Swap a page, and a page's folder, for symlinks out of the root, then
    // request at once: the index still lists both pages.
    fs::remove_file(dir.path().join("victim.md")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("victim.md"),
        dir.path().join("victim.md"),
    )
    .unwrap();
    fs::remove_dir_all(dir.path().join("d")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("d")).unwrap();
    for uri in ["/victim", "/d/victim"] {
        let (status, body) = get_path(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}:\n{body}");
        assert!(!body.contains("Secret outside"), "{uri}:\n{body}");
    }
}

#[tokio::test]
async fn every_indexed_anchor_exists_on_the_page() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "p.md",
        "# P\n\n\
         ![alt #imgtag](i.png) and *stress #emtag* and [link #linktag](x) [[Target]] #plain\n\n\
         | head | other |\n|---|---|\n| #celltag | [[Cell]] |\n",
    );
    let app = open(&dir).await;
    let page = ok_body(&app, "/p").await;

    for tag in ["emtag", "plain", "celltag"] {
        let listing = ok_body(&app, &format!("/_wiki/tag/{tag}")).await;
        let anchor = listed_anchor(&listing, "/p");
        assert!(
            page.contains(&format!("id=\"{anchor}\"")),
            "{tag} → {anchor}"
        );
        assert!(
            page.contains(&format!("id=\"{anchor}\" href=\"/_wiki/tag/{tag}\"")),
            "{tag} → {anchor} points at another tag"
        );
    }
    for target in ["Target", "Cell"] {
        let listing = ok_body(&app, &format!("/_wiki/link?to={target}")).await;
        let anchor = listed_anchor(&listing, "/p");
        let id = format!("id=\"{anchor}\"");
        let at = page
            .find(&id)
            .unwrap_or_else(|| panic!("{target}: no {id}"));
        let text_start = at + page[at..].find('>').unwrap() + 1;
        assert!(
            page[text_start..].starts_with(target),
            "{target} → {anchor}"
        );
    }
    for absent in ["imgtag", "linktag"] {
        let listing = ok_body(&app, &format!("/_wiki/tag/{absent}")).await;
        assert!(!listing.contains("href=\"/p"), "{absent}: {listing}");
        assert!(!page.contains(&format!("/_wiki/tag/{absent}")), "{absent}");
    }
}

#[tokio::test]
async fn generated_pages_escape_titles_targets_and_tags() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\ntitle: \"<script>alert(1)</script>\"\n---\n[[<script>alert(2)</script>]] #t\n",
    );
    let app = open(&dir).await;

    for uri in [
        "/_wiki/tag/t",
        "/_wiki/link?to=%3Cscript%3Ealert(2)%3C%2Fscript%3E",
        "/_wiki/link?to=%3Cscript%3Ealert(3)%3C%2Fscript%3E",
        "/_wiki/tag/%3Cscript%3Ealert(4)%3C%2Fscript%3E",
        "/",
        "/x",
    ] {
        let body = ok_body(&app, uri).await;
        assert!(!body.contains("<script>alert("), "{uri}:\n{body}");
        assert!(body.contains("&lt;script&gt;alert("), "{uri}:\n{body}");
    }
    let listing = ok_body(&app, "/_wiki/link?to=%3Cscript%3Ealert(2)%3C%2Fscript%3E").await;
    assert!(
        listing.contains("<a href=\"/x#wl-0\">&lt;script&gt;alert(1)&lt;/script&gt;</a>"),
        "{listing}"
    );
}

#[tokio::test]
async fn symlinked_image_is_not_served() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.png", "not really a png");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "real.png", "a png");
    let link = |name: &str, target: &Path| {
        std::os::unix::fs::symlink(target, dir.path().join(name)).unwrap();
    };
    link("linked.png", &outside.path().join("secret.png"));
    link("linkdir", outside.path());

    let app = open(&dir).await;
    assert_eq!(get_path(&app, "/real.png").await.0, StatusCode::OK);
    for uri in ["/linked.png", "/linkdir/secret.png"] {
        let (status, body) = get_path(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(!body.contains("not really"), "{uri}");
    }
}

/// Runs `git args` in `dir` with a fixed identity, no signing and no hooks.
fn git(dir: &Path, args: &[&str], date: Option<&str>) {
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

/// Today's date in UTC, `YYYY-MM-DD`.
fn today() -> String {
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `(page url, date)` for each "Recently edited" entry on `/`, in order,
/// with the date the listing shows for it.
async fn recently_edited(app: &Router) -> Vec<(String, String)> {
    let body = ok_body(app, "/").await;
    let heading = "<h2>Recently edited</h2>";
    let start = body.find(heading).expect("recently edited") + heading.len();
    let section = &body[start..];
    let section = &section[..section.find("</ul>").unwrap()];
    let listing = &body[body.find("<h2>Contents</h2>").expect("contents")..];
    section
        .split("<li>")
        .skip(1)
        .map(|item| {
            let href = item.split("href=\"").nth(1).unwrap();
            let href = &href[..href.find('"').unwrap()];
            let entry = listing.split(&format!("href=\"{href}\"")).nth(1).unwrap();
            let date = entry.split("<time>").nth(1).unwrap();
            let date = &date[..date.find('<').unwrap()];
            (href.to_string(), date.to_string())
        })
        .collect()
}

/// `old.md` committed and clean, `changed.md` committed then modified, and
/// `new.md` untracked; the commits are dated 2001-02-03 when `git` is set.
async fn edited_times_app(dir: &TempDir, use_git: bool) -> Router {
    let root = dir.path();
    write(root, "old.md", "# Old\n");
    write(root, "changed.md", "# Changed\n");
    if use_git {
        let date = Some("2001-02-03T00:00:00Z");
        git(root, &["init", "-q"], None);
        git(root, &["add", "old.md", "changed.md"], None);
        git(root, &["commit", "-q", "--no-verify", "-m", "old"], date);
    }
    write(root, "changed.md", "# Changed\n\nNow dirty.\n");
    write(root, "new.md", "# New\n");
    open(dir).await
}

#[tokio::test]
async fn recently_edited_uses_git_time_for_clean_files() {
    let dir = tempfile::tempdir().unwrap();
    let app = edited_times_app(&dir, true).await;
    let entries = recently_edited(&app).await;
    let today = today();
    let mut first_two: Vec<(String, String)> = entries[..2].to_vec();
    first_two.sort();
    assert_eq!(
        first_two,
        [
            ("/changed".to_string(), today.clone()),
            ("/new".to_string(), today.clone()),
        ],
        "{entries:?}"
    );
    assert_eq!(
        entries[2..],
        [("/old".to_string(), "2001-02-03".to_string())],
        "{entries:?}"
    );
}

#[tokio::test]
async fn recently_edited_without_git_uses_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let app = edited_times_app(&dir, false).await;
    let entries = recently_edited(&app).await;
    assert_eq!(entries.len(), 3, "{entries:?}");
    let today = today();
    assert!(
        entries.iter().all(|(_, date)| *date == today),
        "{entries:?}"
    );
}

#[tokio::test]
async fn headings_are_linkable_without_taking_library_ids() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "# Plan\n\n[[b]] #t\n\n## Plan\n\n## wl 0\n\n## Tag 0\n\n## htmx root\n",
    );
    let app = open(&dir).await;
    let body = ok_body(&app, "/a").await;
    for (id, line, text) in [
        ("plan", 1, "Plan"),
        ("plan-1", 5, "Plan"),
        ("wl-0-1", 7, "wl 0"),
        ("tag-0-1", 9, "Tag 0"),
        ("htmx-root-1", 11, "htmx root"),
    ] {
        let heading = format!(
            "data-line=\"{line}\" id=\"{id}\">{text}<a class=\"anchor\" href=\"#{id}\">#</a></h"
        );
        assert!(body.contains(&heading), "{heading}:\n{body}");
    }
    for id in ["htmx-root", "wl-0", "tag-0"] {
        assert_eq!(body.matches(&format!("id=\"{id}\"")).count(), 1, "{id}");
    }

    // A live update re-fetches the page and swaps in its root: the ids are
    // drawn afresh from the new text.
    write(dir.path(), "a.md", "# Intro\n\n# Plan\n\n## Plan\n");
    eventually_contains(&app, "/a", "id=\"intro\"").await;
    let body = ok_body(&app, "/a").await;
    assert!(body.contains("<h1 data-line=\"3\" id=\"plan\">"), "{body}");
    assert!(
        body.contains("<h2 data-line=\"5\" id=\"plan-1\">"),
        "{body}"
    );
    assert!(!body.contains("wl-0-1"), "{body}");
}

#[tokio::test]
async fn wikilink_heading_fragments_jump_to_the_heading() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n\n## Second Part\n");
    write(dir.path(), "x/dup.md", "# Dup\n\n## Sec\n");
    write(dir.path(), "y/dup.md", "# Dup\n\n## Sec\n");
    write(
        dir.path(),
        "b.md",
        "# Local Heading\n\n[[a#Second Part]] [[a#Second Part|label]] [[a#Nope]] \
         [[a]] [[#Local Heading]] [[dup#Sec]] [[Gone#Sec]] [[a#]]\n",
    );
    let app = open(&dir).await;
    let body = ok_body(&app, "/b").await;
    for link in [
        "<a href=\"/a#second-part\" id=\"wl-0\" class=\"wikilink\">a#Second Part</a>",
        "<a href=\"/a#second-part\" id=\"wl-1\" class=\"wikilink\">label</a>",
        // No such heading: still the page, with the slug.
        "<a href=\"/a#nope\" id=\"wl-2\" class=\"wikilink\">",
        "<a href=\"/a\" id=\"wl-3\" class=\"wikilink\">",
        "<a href=\"#local-heading\" id=\"wl-4\" class=\"wikilink\">",
        "<a href=\"/_wiki/link?to=dup%23Sec\" id=\"wl-5\" class=\"wikilink unresolved\">",
        "<a href=\"/_wiki/link?to=Gone%23Sec\" id=\"wl-6\" class=\"wikilink unresolved\">",
        // An empty fragment adds no anchor.
        "<a href=\"/a\" id=\"wl-7\" class=\"wikilink\">",
    ] {
        assert!(body.contains(link), "{link}:\n{body}");
    }

    // The disambiguation page's matches carry the fragment.
    let body = ok_body(&app, "/_wiki/link?to=dup%23Sec").await;
    for link in ["href=\"/x/dup#sec\"", "href=\"/y/dup#sec\""] {
        assert!(body.contains(link), "{link}:\n{body}");
    }
    assert!(body.contains("<h1>dup</h1>"), "{body}");
    let body = ok_body(&app, "/_wiki/link?to=Gone%23Sec").await;
    assert!(body.contains("<h1>Gone</h1>"), "{body}");
}

#[tokio::test]
async fn a_page_is_reached_by_its_file_name_too() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "# A\n\ntext\n");
    write(dir.path(), "sub/My Note.md", "# Note\n");
    write(dir.path(), "x.md.md", "# Double\n");
    write(dir.path(), "pic.png", "png");
    let app = open(&dir).await;
    for (uri, to) in [("/a.md", "/a"), ("/sub/My%20Note.md", "/sub/My%20Note")] {
        let response = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT, "{uri}");
        assert_eq!(response.headers()["location"], to, "{uri}");
    }
    // A page whose own name ends in `.md` keeps its URL; images and missing
    // files are unchanged.
    assert!(ok_body(&app, "/x.md").await.contains(">Double<"));
    assert_eq!(
        get_path(&app, "/pic.png").await,
        (StatusCode::OK, "png".to_string())
    );
    assert_eq!(get_path(&app, "/missing.md").await.0, StatusCode::NOT_FOUND);

    // Under a base path the redirect stays under it.
    let wiki = Wiki::builder(dir.path())
        .base_path("/docs")
        .build()
        .await
        .unwrap();
    let app = wiki.router::<()>();
    let response = app
        .oneshot(Request::get("/docs/a.md").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["location"], "/docs/a");
}

#[tokio::test]
async fn pages_carry_source_lines_for_line_links() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntitle: A\n---\n# A\n\n- one\n- two\n",
    );
    let app = open(&dir).await;
    let body = ok_body(&app, "/a").await;
    for needle in [
        "<h1 data-line=\"4\" id=\"a\">",
        "<li data-line=\"6\">one</li>",
        "<li data-line=\"7\">two</li>",
        // The client script and the default highlight.
        "/^#:(\\d+)$/",
        ".line-target { background: var(--line-target); }",
    ] {
        assert!(body.contains(needle), "{needle}:\n{body}");
    }
}
