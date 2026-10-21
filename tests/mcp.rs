//! The MCP tools, called directly and through the router's endpoint.

use std::fs;
use std::path::Path;

use agent_wiki::{
    OpenFiles, OpenFilesArgs, Overview, OverviewArgs, PageUrl, PageUrlArgs, Search, SearchArgs,
    Tool, Wiki,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

async fn open(dir: &TempDir) -> Wiki {
    let wiki = Wiki::builder(dir.path()).build().await.unwrap();
    wiki.indexed().await;
    wiki
}

fn folder() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "projects/alpha.md",
        "---\ntitle: Alpha Project\nowner: dave\ntags: [plan]\n---\n\
         # Alpha\n\nThe alpha rocket.\n\n## Status\n\nFine. #active\n\n\
         ### Risks\n\nSee [[Beta]] and [[Notes]] and [[Nowhere]] and [[Beta]].\n\n\
         # Appendix\n",
    );
    write(root, "projects/beta.md", "# Beta\n\nAnother rocket.\n");
    write(root, "notes/notes.md", "# Notes one\n");
    write(root, "other/notes.md", "# Notes two\n");
    write(root, "rocket.md", "# Rocket\n\nAbout rockets.\n");
    dir
}

#[tokio::test]
async fn search_lists_path_title_tags_and_summary() {
    let dir = folder();
    let wiki = open(&dir).await;
    let out = Search::new(wiki)
        .call(SearchArgs {
            query: "rocket".into(),
            folder: None,
        })
        .await
        .unwrap();
    // The title match first, then body matches.
    assert!(
        out.starts_with("rocket.md — Rocket\n  About rockets.\n"),
        "{out}"
    );
    assert!(
        out.contains("projects/alpha.md — Alpha Project  #active  #plan\n  The alpha rocket.\n"),
        "{out}"
    );
    assert!(out.contains("projects/beta.md — Beta\n"), "{out}");
}

#[tokio::test]
async fn search_in_a_folder() {
    let dir = folder();
    let wiki = open(&dir).await;
    let search = Search::new(wiki);
    let out = search
        .call(SearchArgs {
            query: "rocket".into(),
            folder: Some("/projects/".into()),
        })
        .await
        .unwrap();
    assert!(!out.contains("rocket.md — Rocket"), "{out}");
    assert!(out.contains("projects/alpha.md"), "{out}");
    let out = search
        .call(SearchArgs {
            query: "rocket".into(),
            folder: Some("proj".into()),
        })
        .await
        .unwrap();
    assert_eq!(out, "No matches.");
}

#[tokio::test]
async fn overview_of_a_document() {
    let dir = folder();
    let wiki = open(&dir).await;
    let out = Overview::new(wiki)
        .call(OverviewArgs {
            path: "projects/alpha".into(),
        })
        .await
        .unwrap();
    let want = "\
path: projects/alpha.md
title: Alpha Project
summary: The alpha rocket.
frontmatter:
  title: Alpha Project
  owner: dave
  tags:
  - plan
outline:
  L6    # Alpha  #alpha
  L10     ## Status  #status
  L14       ### Risks  #risks
  L18   # Appendix  #appendix
references:
  [[Beta]] → projects/beta.md
  [[Notes]] → ambiguous (2 matches)
  [[Nowhere]] → no match
";
    assert_eq!(out, want);
}

#[tokio::test]
async fn overview_errors() {
    let dir = folder();
    let wiki = open(&dir).await;
    let overview = Overview::new(wiki);
    let missing = overview
        .call(OverviewArgs {
            path: "projects/gamma.md".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.to_string(), "no document at projects/gamma.md");
    let escape = overview
        .call(OverviewArgs {
            path: "../secret".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(escape.to_string(), "not a document path: ../secret");
}

async fn rpc(app: &axum::Router, body: Value) -> Value {
    let request = Request::post("/_wiki/mcp")
        .header("host", "127.0.0.1")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK, "{text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

#[tokio::test]
async fn over_http() {
    let dir = folder();
    let app = open(&dir).await.router::<()>();
    let init = rpc(
        &app,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }}),
    )
    .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "agent_wiki", "{init}");
    let root = dir.path().canonicalize().unwrap();
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(&format!("The wiki root is `{}`", root.display())),
        "{instructions}"
    );

    let list = rpc(
        &app,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["open_files", "overview", "page_url", "search"]);

    let call = rpc(
        &app,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "search", "arguments": {"query": "rocket", "folder": "projects"}
        }}),
    )
    .await;
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{call}"));
    assert!(text.contains("projects/alpha.md — Alpha Project"), "{text}");
    assert_eq!(call["result"]["isError"], false, "{call}");

    let error = rpc(
        &app,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "overview", "arguments": {"path": "nope"}
        }}),
    )
    .await;
    assert_eq!(error["result"]["isError"], true, "{error}");

    // Over HTTP, a page URL is absolute, from the request's host.
    let url = rpc(
        &app,
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
            "name": "page_url", "arguments": {"path": "projects/alpha.md", "line": 10}
        }}),
    )
    .await;
    assert_eq!(
        url["result"]["content"][0]["text"], "http://127.0.0.1/projects/alpha#:10",
        "{url}"
    );
    assert_eq!(
        error["result"]["content"][0]["text"],
        "no document at nope.md"
    );
}

async fn push(app: &axum::Router, state: Value) {
    let request = Request::post("/_wiki/state")
        .header("content-type", "application/json")
        .body(Body::from(state.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

/// Opens a page's live-update stream as tab `tab`; the page is open while
/// the returned response (and its body) is kept.
async fn connect(app: &axum::Router, url: &str, tab: &str) -> axum::response::Response {
    let request = Request::get(format!("/_wiki/events{url}?tab={tab}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
}

#[tokio::test]
async fn open_files_follow_the_pages_streams_and_pushed_state() {
    let dir = folder();
    let wiki = open(&dir).await;
    let app = wiki.router::<()>();
    let tool = OpenFiles::new(wiki);
    let open_files = || tool.call(OpenFilesArgs {});

    assert_eq!(open_files().await.unwrap(), "No documents open.");

    let alpha = connect(&app, "/projects/alpha", "1-1").await;
    let beta = connect(&app, "/projects/beta", "1-2").await;
    assert_eq!(
        open_files().await.unwrap(),
        "projects/beta.md\nprojects/alpha.md\n"
    );

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    push(
        &app,
        json!({"tab": "1-1", "focused": true, "selection": {"start": 10, "end": 13}}),
    )
    .await;
    assert_eq!(
        open_files().await.unwrap(),
        "projects/alpha.md  (focused)  selected L10-L13\nprojects/beta.md\n"
    );

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    push(
        &app,
        json!({"tab": "1-2", "focused": false, "selection": {"start": 3, "end": null}}),
    )
    .await;
    assert_eq!(
        open_files().await.unwrap(),
        "projects/beta.md  selected L3-end\nprojects/alpha.md  (focused)  selected L10-L13\n"
    );

    // Closing a page's stream closes it.
    drop(beta);
    assert_eq!(
        open_files().await.unwrap(),
        "projects/alpha.md  (focused)  selected L10-L13\n"
    );
    drop(alpha);
    assert_eq!(open_files().await.unwrap(), "No documents open.");
}

#[tokio::test]
async fn a_document_page_carries_its_tab_and_copy_button() {
    let dir = folder();
    let app = open(&dir).await.router::<()>();
    let request = Request::get("/projects/alpha").body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        body.contains("sse-connect=\"/_wiki/events/projects/alpha?tab="),
        "{body}"
    );
    assert!(body.contains("data-base=\"\""), "{body}");
    assert!(
        body.contains(
            "<button class=\"copy-path\" type=\"button\" data-path=\"projects/alpha.md\""
        ),
        "{body}"
    );
    assert!(body.contains("/_wiki/state"), "{body}");
}

#[tokio::test]
async fn page_urls() {
    let dir = folder();
    let wiki = Wiki::builder(dir.path())
        .base_path("/docs")
        .build()
        .await
        .unwrap();
    let tool = PageUrl::new(wiki);
    let url = |path: &str, line: Option<usize>, heading: Option<&str>| {
        tool.call(PageUrlArgs {
            path: path.into(),
            line,
            heading: heading.map(Into::into),
        })
    };
    // Called directly, outside an MCP request: a path from the site root.
    assert_eq!(
        url("projects/alpha", None, None).await.unwrap(),
        "/docs/projects/alpha"
    );
    assert_eq!(
        url("projects/alpha.md", Some(14), None).await.unwrap(),
        "/docs/projects/alpha#:14"
    );
    for heading in ["Status", "## status", "status"] {
        assert_eq!(
            url("projects/alpha", None, Some(heading)).await.unwrap(),
            "/docs/projects/alpha#status",
            "{heading}"
        );
    }
    let err = |r: anyhow::Result<String>| r.unwrap_err().to_string();
    assert_eq!(
        err(url("projects/alpha", Some(3), Some("Status")).await),
        "give a line or a heading, not both"
    );
    assert_eq!(
        err(url("projects/alpha", None, Some("Nope")).await),
        "no heading \"Nope\" in projects/alpha.md"
    );
    assert_eq!(
        err(url("projects/alpha", Some(0), None).await),
        "lines start at 1"
    );
    assert_eq!(err(url("gone", None, None).await), "no document at gone.md");
}
