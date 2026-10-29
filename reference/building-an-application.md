# Building your own application

agent_wiki is a Rust library (axum) plus a thin binary. To customize it, wrap the library in an application of your own: a basic wrapper is a few lines, and everything the binary doesn't offer is done in code. Build what the user asks for.

The repository's README has the API in more detail, and `docs/design/example.rs` sketches all of it: https://github.com/dgrijalva/agent_wiki

## A basic wrapper

```toml
[dependencies]
agent_wiki = { git = "https://github.com/dgrijalva/agent_wiki" }
anyhow = "1"
axum = "0.8"
tokio = { version = "1", features = ["full"] }
```

```rust
use agent_wiki::Wiki;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let wiki = Wiki::builder("notes")
        .stylesheet("body { max-width: 60rem; }")
        .script_filter(|_| false) // recommended: no script in a note runs
        .build()
        .await?;
    let app = axum::Router::new().merge(wiki.router());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("Serving at http://127.0.0.1:3000/, MCP at http://127.0.0.1:3000/_wiki/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}
```

The router serves the pages, the MCP endpoint at `{base}/_wiki/mcp`, and this reference at `{base}/_wiki/reference`. Mount it under a path with `.base_path("/docs")`; keep the index on disk with `.index_storage(IndexStorage::FilePath(path))`.

## Scripts in notes

Raw HTML in a note passes through. Without `.script_filter`, a `<script>`, an `onerror=` handler or a `javascript:` link in a clipped web page, pasted HTML or a cloned repository's docs runs with the site's origin, where it can read every other page. Block them, as above, unless the user needs scripts in their notes: agent_wiki's own scripts (highlighting, diagrams, live updates) and the scripts a `Template` puts in its `head` keep running.

To allow some, decide per script; each page then allows only what the filter allowed:

```rust
use agent_wiki::{PageKind, ScriptKind};

let wiki = Wiki::builder("notes")
    .script_filter(|script| match script.kind() {
        // Files from one domain, on any page.
        ScriptKind::External(src) => src.starts_with("https://widgets.example.com/"),
        // Code written in the notes under Dashboards/.
        ScriptKind::Inline(_) => matches!(
            script.page(),
            PageKind::Document(doc) if doc.path().starts_with("Dashboards/")
        ),
        // Event handlers (`onclick=`) and `javascript:` links.
        _ => false,
    })
    .build()
    .await?;
```

Under a filter, `eval` and htmx's `hx-on` never run, and a script that an allowed script adds to the page runs only if the page's policy already allows its URL or code.

## What to use for what

| The user wants | Use |
|---|---|
| Their own look | `.stylesheet(css)` layers CSS over the built-in stylesheet; a template can drop the built-in one. |
| Their own navigation or page chrome | A `Template`: `head` and `body` hooks around every page, placing the built-in pieces (`ctx.search_box()`, `ctx.breadcrumbs()`, …) where they like, or replacing a page's body. |
| Their own syntax, e.g. `[[ABC-123]]` as a ticket | A `Host`: its parse hooks claim syntax before the built-ins, and its elements draw it. |
| Pages built from the documents | The data layer: `wiki.root()`, `wiki.lookup(path)`, `wiki.search(…)`, folders' `documents()` and `recent(n)`, documents' `outline()`, `section(anchor)` and `chunk(…)`. |
| Their own pages, in the wiki's look | `wiki.custom_page(&request, title)`, drawn with the library's components (`RecentlyEdited`, `TagCloud`, `Contents`, …). `.live()` refetches it when the application calls `wiki.signal(path)`. |
| Their own records indexed alongside the documents | `#[derive(EntityType)]`, registered with `.entity::<T>()` and pushed by `Host::extract`; queried with `wiki.query::<T>()`. |
| Their own agent tools | A struct implementing `agent_wiki::Tool`, added with `.tools(\|wiki\| wiki.tools().with_route(route(MyTool::new(wiki.clone()))))`; or serve `wiki.tools()` from their own rmcp server. |
| Control over scripts in notes | `.script_filter(…)`, which can allow some by page or source; blocking them all is recommended. See above. |
| Their own auth | The application's own axum middleware, around `wiki.router()`. |
