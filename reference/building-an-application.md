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
use agent_wiki::{AllowedHosts, Wiki};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let wiki = Wiki::builder("notes")
        .stylesheet("body { max-width: 60rem; }")
        .allowed_hosts(AllowedHosts::loopback())
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

## The `Host` check

`.allowed_hosts(…)` makes the wiki's routes refuse, with `403`, any request whose `Host` header isn't on the list. It guards against DNS rebinding: a web page the user opens in their own browser, on a domain its owner controls, re-resolves that domain to `127.0.0.1`, so the browser sends the page's next requests to the wiki on the user's machine. The browser treats the wiki's responses as coming from the attacker's domain, so the page's script can read the rendered notes and send them anywhere. Those requests still carry `Host: <the attacker's domain>`, which the check refuses.

| Serving the wiki | Use |
|---|---|
| On the user's machine (`127.0.0.1`) | `.allowed_hosts(AllowedHosts::loopback())`: `localhost`, `127.0.0.1` and `[::1]`, on any port. The stock binary uses it. |
| On its own names | `.allowed_hosts(AllowedHosts::new(["wiki.example.com", "intranet:8080"]))`: a name allows any port, `name:port` only that port. |
| Behind something else that checks the host | `.allowed_hosts(AllowedHosts::any())`, which also turns off the MCP endpoint's check. |

The MCP endpoint is checked against the same list. Without `.allowed_hosts`, the web pages answer any `Host`, and only the MCP endpoint checks, accepting `localhost`, `127.0.0.1` and `::1`. A request with no `Host` (and, over HTTP/2, no `:authority`) is refused. The check covers only the wiki's own routes, not the application's routes merged beside them.

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
| Their own auth | The application's own axum middleware, around `wiki.router()`. |
| The wiki on its own domain | `.allowed_hosts(AllowedHosts::new([…]))`, for the web pages and the MCP endpoint alike; see The `Host` check above. |
