//! The MCP tools, called directly and through the router's endpoint.

use std::fs;
use std::path::Path;

use agent_wiki::{Overview, OverviewArgs, Search, SearchArgs, Tool, Wiki};
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
    assert_eq!(names, ["overview", "search"]);

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
    assert_eq!(
        error["result"]["content"][0]["text"],
        "no document at nope.md"
    );
}
