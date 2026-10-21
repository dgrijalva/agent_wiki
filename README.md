# agent_wiki

Host a folder of markdown files as a website. A Rust/axum library plus a thin binary, built to be embedded in other applications that wrap it in their own auth and tooling.

## Status

Early prototype. The API will change.

## Features

- **Markdown:** GFM plus Obsidian syntax: wikilinks, `#tags`, frontmatter, and single newlines as line breaks. Code blocks are highlighted and mermaid diagrams drawn in the browser.
- **Wikilinks** match file names loosely: case-insensitive, `-` and `_` match spaces, and a leading date is ignored. A link with no single match goes to a disambiguation page listing every page that references it.
- **Tags** go to a tag page listing every page that uses them.
- **Search** over titles, tags and body text, ranked in that order.
- **Folder index pages** list recently edited pages below them, then the folder's subfolders and pages with each page's summary, edit date and tags, then the top tags below them.
- **Live updates:** an open page updates in place when its file changes on disk.
- **Links:** headings are linkable, `[[Page#Heading]]` jumps to the heading, `#:N` links to line N of the page's source, and `{base}/foo/bar.md` reaches the page at `{base}/foo/bar`.
- **Images** and ordinary links resolve relative to the page, or from the root with a leading `/`, and never leave the root folder.
- **Styling:** a built-in stylesheet with dark mode. A host stylesheet can be layered on top or replace it.
- **Startup is immediate.** The folder is indexed in the background while pages are served.

## Quick start

```sh
cargo run -- <folder> [--port N] [--index <path>]
```

Serves on `127.0.0.1`, port 3000 by default. Ctrl-C starts a graceful shutdown; a second Ctrl-C quits.

### Index

The index of links, tags and search text is kept in memory by default and rebuilt on every start. `--index <path>` keeps it on disk in the database file `<path>` instead, so it persists across runs.

## Embedding in an axum app

```rust
let wiki = agent_wiki::Wiki::builder(root)
    .base_path("/docs")   // optional; default "/"
    .stylesheet(host_css) // optional; CSS layered on top of the built-in stylesheet
    .index_storage(agent_wiki::IndexStorage::FilePath(index_file)) // optional; default InMemory
    .build()
    .await?;
let app = app.merge(wiki.router());
```

Every generated URL carries the base path. The wiki's own routes live under `{base}/_wiki/`.

## Extending with a host

A host type, passed with `.host(host, template)`, customises parsing and presentation:

- **`Host`** claims syntax during parsing. For example, a host can turn `[[ABC-123]]` into a ticket element instead of a wikilink.
- **`Element`** draws the host's elements to HTML.
- **`Template`** provides `head` and `body` hooks that wrap every page, for the host's own navigation and chrome.

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

The full sketch is in [`docs/design/example.rs`](docs/design/example.rs), and the design in [`docs/design/`](docs/design/), starting from [`agent_wiki.md`](docs/design/agent_wiki.md).

## Planned features

- **Semantic search**, likely only with an on-disk index.

## License

MIT. See [LICENSE](LICENSE).
