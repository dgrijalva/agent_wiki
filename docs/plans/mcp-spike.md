# Plan: MCP spike

Design: `docs/design/agent_wiki.md` → MCP. This plan holds the choices the design leaves to implementation; each is `[PROVISIONAL]` until Dave reviews it.

Branch: `dg/mcp-spike`. Journal: `docs/plans/mcp-spike-journal.md`.

## Scope

In: the search and doc overview tools, the tool trait and `route` adapter, `Wiki::tools::<S>()`, and the HTTP endpoint at `{base}/_wiki/mcp`, served by the binary.

Out, but part of the feature (design → Round-tripping): the client state push, the open-files tool, the copy button. Not needed before testing starts.

## Steps

### 1. Dependency

- `rmcp = "3.6"` with `transport-streamable-http-server`; `schemars` for argument schemas.

### 2. The tool trait and adapter

- A crate-level trait for tools: name, description, argument type (`Deserialize + JsonSchema`), and `async fn call(&self, args) -> Result<String>`.
- `route<S, T>(tool: T) -> ToolRoute<S>`: `ToolRoute::new_dyn` with a closure that holds the tool, parses the arguments, calls it, and returns the text as the result. A tool error becomes an error result the agent can read, not a protocol error.
- `Wiki::tools::<S>()`: the default collection, `Search` and `Overview`.
- [PROVISIONAL: `call` returns `String` (formatted text) for both spike tools. The trait gets a structured output only when a tool needs JSON.]

### 3. Search tool

- Args: `query: String`, `folder: Option<String>` (relative to the wiki root).
- Uses `Index::search`, with the folder applied as a path-prefix filter.
- [PROVISIONAL: output, one block per result in rank order:
  ```
  projects/foo.md — Foo Project  #tag1 #tag2
    First 280 chars of summary…
  ```
  Path first, since the agent reads files by path; tags only when there are some; the summary line only when there is one. No URLs.]
- [PROVISIONAL: no result limit in the spike. Ask Dave if real results come out too long.]
- [PROVISIONAL: an empty result says "No matches." rather than returning nothing.]

### 4. Doc overview tool

- Args: `path: String`, relative to the wiki root. Accepts with or without `.md`, the same as the web routes.
- Reads and parses the file with the host's parser, the same path the index uses, and extracts once.
- New: an outline walk. Each heading's level, its line from `tree.lines`, and its anchor from `Extracted::headings`, nested by level.
- References: each wikilink as written, resolved through `Index::resolve` to a path, or marked ambiguous (with the candidate count) or unmatched.
- [PROVISIONAL: output:
  ```
  path: projects/foo.md
  title: Foo Project
  summary: …
  frontmatter:
    <frontmatter as YAML>
  outline:
    L3  # Foo Project  #foo-project
    L10   ## Status  #status
    L24     ### Risks  #risks
  references:
    [[Bar]] → notes/bar.md
    [[Baz]] → ambiguous (3 matches)
    [[Qux]] → no match
  ```
  Sections with nothing in them are left out.]
- [PROVISIONAL: references cover wikilinks only. Ordinary markdown links to other `.md` files aren't extracted today; adding them is a follow-up if Dave wants them.]

### 5. HTTP endpoint

- `StreamableHttpService` with `NeverSessionManager` and `legacy_session_mode: false` (design: no sessions until needed), mounted with `route_service` at `{base}/_wiki/mcp` inside `web::router`, always on.
- [PROVISIONAL: `json_response: true`, so a call is answered with plain JSON rather than an SSE stream when the tool sends nothing before its result.]
- [PROVISIONAL: rmcp's default host check is kept: only `localhost`, `127.0.0.1` and `::1` are accepted, against DNS rebinding. A host serving the wiki on its own domain would be refused until this is configurable.]
- The server handler is a small struct holding `Wiki<P>` and `ToolRouter<Self>` from `wiki.tools()`.
- The binary prints the MCP URL next to the site URL at startup.

### 6. Browser side

Design → Round-tripping: the state push, the open-files tool, the copy button. Local only.

- [PROVISIONAL: **tab identity.** The server gives each document page load a tab id and puts it in the live-update stream's URL (`…/_wiki/events/{page}?tab={id}`) and on `<body data-tab>`. A page counts as open while that stream is connected; when it drops (tab closed, navigated away), the file is no longer open. Ids are `{process start ms}-{counter}`, so a page reconnecting after a restart keeps a distinct id.]
- [PROVISIONAL: **state push.** `POST {base}/_wiki/state` with `{tab, focused, selection: {start, end} | null}`, sent when the stream connects (`htmx:sseOpen`), on `focus`/`blur`/`visibilitychange`, and on `selectionchange` (debounced 250 ms); unchanged state isn't resent. A push for a tab whose stream isn't connected is ignored. `focused` is `document.hasFocus()`.]
- [PROVISIONAL: **selection lines.** `start` is the `data-line` of the block the selection starts in; `end` is one before the first block after the block it ends in, or `null` when it ends in the last block (to end of file). Only selections inside the htmx root's numbered blocks count. No selected text is sent.]
- [PROVISIONAL: **recency.** A tab becomes most recently active when it connects, gains focus, or changes to a non-empty selection.]
- [PROVISIONAL: **`open_files` tool**, in the default collection, no arguments. One line per file, most recent first, deduped by file (the most recent tab's state wins): `path  (focused)  selected L12-L18`, `L40-end` for a selection to the end; "No documents open." when none.]
- [PROVISIONAL: **copy button.** `BodyCtx::copy_path()`, a `button.copy-path` copying the document's root-relative path; the default template puts it at the right of the breadcrumbs. A shared click handler in the page skeleton shows "Copied" for a moment.]

### 7. Tests

- Each tool called directly against a temp folder: search with and without a folder; overview's outline nesting, lines, anchors, references, and missing file.
- One HTTP test through the router: initialize, `tools/list`, `tools/call search`.
- Manual: point Claude Code at the running binary and use both tools on a real folder.
