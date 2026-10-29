# agent_wiki

Host a folder of markdown files as a website. A Rust/axum library plus a thin binary, built to be embedded in other applications that wrap it in their own auth and tooling.

**Agents:** if you were asked to install or set up agent_wiki, read [`reference/onboarding.md`](reference/onboarding.md). The [reference](reference/) is written for you.

## Status

Early prototype. The API will change.

## Features

- **Markdown:** GFM plus Obsidian syntax: wikilinks, `#tags`, frontmatter, and single newlines as line breaks. Code blocks are highlighted and mermaid diagrams drawn in the browser.
- **Wikilinks** match file names loosely: case-insensitive, `-` and `_` match spaces, and a leading date is ignored. A link with no single match goes to a disambiguation page listing every page that references it.
- **Tags** go to a tag page listing every page that uses them.
- **Search** over titles, tags and body text, ranked in that order.
- **Folder index pages** list recently edited pages below them, then the folder's subfolders and pages with each page's summary, edit date and tags, then the top tags below them.
- **Live updates:** an open page updates in place when its file changes on disk, or when the host signals it.
- **Links:** headings are linkable, `[[Page#Heading]]` jumps to the heading, `#:N` links to line N of the page's source, and `{base}/foo/bar.md` reaches the page at `{base}/foo/bar`.
- **Images** and ordinary links resolve relative to the page, or from the root with a leading `/`, and never leave the root folder.
- **Styling:** a built-in stylesheet with dark mode. A host stylesheet can be layered on top or replace it.
- **Scripts in notes** run as written by default. A host's script filter decides which run; blocking them all is the recommended setup. See [Embedding in an axum app](#embedding-in-an-axum-app).
- **Startup is immediate.** The folder is indexed in the background while pages are served.
- **MCP server** for agents: search, document overviews, reading documents by line range or section, links into the web view, and what the user has open and selected. See [MCP](#mcp).
- **A reference for agents**, built in and served at `{base}/_wiki/reference`: using the MCP, onboarding, and building your own application. Its source is [`reference/`](reference/).
- **Layered API** for hosts: the whole wiki as a router, a page template, replaceable page bodies, page components, and the documents and index as data, down to the host's own indexed record types. See [Extending with a host](#extending-with-a-host).

## Quick start

Requirements: a Rust toolchain with `cargo`, and `git`.

```sh
cargo install --git https://github.com/dgrijalva/agent_wiki agent_wiki
agent_wiki <folder> [--port N] [--index <path>] [--reindex]
```

Or from a checkout, `cargo run -- <folder>`.

Serves on `127.0.0.1`, port 3000 by default. Ctrl-C starts a graceful shutdown; a second Ctrl-C quits.

### Index

The index of links, tags and search text is kept in memory by default and rebuilt on every start. `--index <path>` keeps it on disk in the database file `<path>` instead, so it persists across runs. `--reindex` rebuilds it from the files at startup, in the background.

The index is disposable: it holds nothing that isn't derived from the files, and a library update can rebuild it at any time.

## MCP

The wiki serves an MCP server (streamable HTTP, no sessions) at `{base}/_wiki/mcp`; the binary prints its URL at startup. To connect Claude Code:

```sh
claude mcp add --transport http agent_wiki http://127.0.0.1:3000/_wiki/mcp
```

Tools:

- **`search`**: a query and an optional folder. Each result is the document's path, title and tags, then its summary.
- **`overview`**: a document's title, size in lines, summary, frontmatter, outline (each heading with its line number and anchor) and resolved wikilinks, so an agent can read just the section it needs.
- **`read_document`**: a document whole, a line range (`lines`: `12-40`, `L12-L40`, `12-`, `L12-end`), or sections by anchor (`sections`); not both. A folder's path gives its index.
- **`page_url`**: a document, plus an optional line or heading, gives its URL in the web view, to hand the user a link.
- **`open_files`**: the documents open in the web view, most recently active first, with which one has focus and the selection as a line range.

Paths in results are relative to the wiki root; the server's instructions give the root's absolute path once, on connect, and point the agent to the reference at `_wiki/reference`. The reference is left out of search, indexes and listings, except a search whose `folder` is in `_wiki/reference`. Each document page also has a **Copy path** button that copies its root-relative path, for pasting to an agent.

Local use only, for now: `open_files` reports every browser's open pages, and requests are only accepted when addressed to `localhost`, `127.0.0.1` or `::1`.

## Embedding in an axum app

```rust
let wiki = agent_wiki::Wiki::builder(root)
    .base_path("/docs")   // optional; default "/"
    .stylesheet(host_css) // optional; CSS layered on top of the built-in stylesheet
    .scripts(|_| false)   // recommended; no script in a note runs (default: all run)
    .index_storage(agent_wiki::IndexStorage::FilePath(index_file)) // optional; default InMemory
    .build()
    .await?;
let app = app.merge(wiki.router());
```

Every generated URL carries the base path. The wiki's own routes live under `{base}/_wiki/`, including the MCP endpoint.

Raw HTML in a note passes through, so without a script filter a note's `<script>`, event handlers and `javascript:` links run with the site's origin. With one, every page carries a Content-Security-Policy that allows the library's own scripts, the template's `head` scripts, and the scripts the filter allows (by hash or URL); the rest are removed from the page and blocked. The filter gets each script's page and kind (`Inline`, `External`, `Handler`, `Url`), so it can allow, say, files from one domain or inline scripts on some pages.

A host's own routes stay outside the wiki, but can draw their pages through it, with the wiki's template, styles and live updates:

```rust
let recent = RecentlyEdited::load(&projects, 10).await?; // a library component
let html = wiki
    .custom_page(&request, "Projects")
    .live() // refetches whenever the host calls wiki.signal("/projects")
    .draw(|ctx| html! { h1 { "Projects" } (ctx.draw(&recent)) })
    .await;
```

### MCP tools in your own server

The built-in tools are exported, so a host can serve them from its own rmcp server (`agent_wiki::rmcp` is re-exported). `wiki.tools()` returns the default collection, generic over the server type, to edit or merge:

```rust
let mut tools: ToolRouter<MyServer> = wiki.tools();
tools.remove_route("open_files");
tools.add_route(agent_wiki::route(MyTool::new(wiki.clone())));
```

A tool is a plain struct implementing `agent_wiki::Tool` (name, description, argument type, and an async `call` returning text); `route` adapts it to rmcp. The built-in tools can also be called directly, without rmcp, e.g. `Search::new(wiki.clone()).call(args)`.

## Extending with a host

A host type, passed with `.host(host, template)`, customises parsing and presentation:

- **`Host`** claims syntax during parsing. For example, a host can turn `[[ABC-123]]` into a ticket element instead of a wikilink.
- **`Element`** draws the host's elements to HTML.
- **`Element`** is also how everything draws: the library's page components (`SearchBox`, `Breadcrumbs`, `RecentlyEdited`, `Contents`, `TagCloud`, …) and page bodies (`FolderIndexBody`, `SearchBody`, …) are elements, loaded from the data layer and drawn where the host places them.
- **`Template`** provides `head` and `body` hooks that wrap every page, for the host's own navigation and chrome. A custom `body` places the built-in chrome itself: `ctx.search_box()`, `ctx.breadcrumbs()`, `ctx.copy_path()`, `ctx.tags()`, `ctx.frontmatter_table()`. It can also replace a page's body, building its own from `ctx.kind()` and `ctx.wiki()`.

```rust
struct SampleHost;

impl Host for SampleHost {
    type Block = Never;
    type Inline = SampleInline; // e.g. Ticket(String)
    type Template = DefaultTemplate;

    fn parse_inline(&self, s: &mut InlineStream<'_, Self>) -> Option<Parsed<InlineKind<Self>>> {
        // [[ABC-1234]] is a ticket, not a wikilink
        let id = s.try_parse(delimited(tag("[["), ticket_id, tag("]]")))?;
        Some(s.leaf(InlineKind::Host(SampleInline::Ticket(id))))
    }
}
```

### The documents as data

- **Folders and documents:** `wiki.root()`, `wiki.lookup(path)`, `wiki.search(query, SearchOpts { limit })`; a `Folder`'s `documents()`, `recent(n)`, `tags(n)`, `walk()`; a `Document`'s `title()`, `tags()`, `summary()`, `outline()`, its parsed `tree()`, and `body().await` to draw it; `section(anchor)` for a heading and its section, and `chunk(nodes).await` to draw part of it on another page.
- **Watches:** `document.watch()` (modified or removed) and `folder.watch()` (anything in its subtree), as `tokio::sync::watch` receivers.
- **Signals:** `wiki.signal(path)` makes every open instance of the page at that URL path refetch itself, whether it's a wiki page or the host's own.

### The host's own record types

A host can index its own records alongside the documents. `#[derive(EntityType)]` gives a struct its own table; `#[entity(index)]` and `#[entity(fulltext)]` mark fields for a b-tree or full-text index. `Host::extract` pushes records from each parsed document, and the type is registered with `.entity::<T>()`. A changed type is rebuilt from the files automatically.

```rust
#[derive(EntityType)]
struct Mention {
    #[entity(index)]
    who: String,
    #[entity(fulltext)]
    text: String,
}

// Tickets on pages where dave is mentioned, recent first.
let tickets = wiki
    .query::<Ticket>()
    .on_pages_with::<Mention>(filter::eq("who", "dave"))
    .order_by(Sort::Edited, Direction::Desc)
    .limit(20)
    .fetch()
    .await?; // Vec<Entity<Ticket>>: each record with its page's path, title and edit time
```

Filters cover field comparisons (`eq`, `lt`, `is_in`, `between`, `like`, `is_null`, …), `and`/`or`/`not`, and `matches` for full-text fields. The built-in wikilinks and tags are record types too (`Wikilink`, `Tag`), so a query can filter on them.

The full sketch is in [`docs/design/example.rs`](docs/design/example.rs), and the design in [`docs/design/`](docs/design/), starting from [`agent_wiki.md`](docs/design/agent_wiki.md); the data model is in [`data.md`](docs/design/data.md), and the MCP server in [`mcp.md`](docs/design/mcp.md).

## Planned features

- **Semantic search**, likely only with an on-disk index.

## License

MIT. See [LICENSE](LICENSE).
