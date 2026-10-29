# agent_wiki — MCP

The MCP server, its tools, and round-tripping between the web UI and the agent. Part of the design; the core is `agent_wiki.md`. Unmarked prose is decided; anything not decided carries `[OPEN: …]` or `[PROVISIONAL: …]` (see `docs/process.md`).

agent_wiki serves an MCP server so agents can work with the wiki. (Dave, 2026-10-20)

## Shape

- **Built on rmcp.**
- **A set of built-in tools.**
- **Hosts enable or disable the built-in tools, and provide their own.**
- **The library exports its tools**, so a host can build whatever collection it wants, using rmcp's tool collections. (Dave, 2026-10-20)
- **The wiki has a `tools` method** that returns the default collection, which the host can edit. (Dave, 2026-10-20)
- **The builder has a `.tools` method** that sets the collection the wiki's own endpoint serves. It takes a closure from the wiki to the collection, since the tools hold the wiki and it doesn't exist until `build`. The closure starts from `wiki.tools()`, so the host adds, removes or replaces tools in one place. (Dave, 2026-10-23)
  - [PROVISIONAL: the mounted server type is made public as `McpServer<P>`, so the closure can name the collection's server type.]
  - [PROVISIONAL: the closure is kept and called each time the endpoint makes its server.] Today the endpoint calls `wiki.tools()` that way. Keeping the built collection in the wiki would be a reference cycle, since the tools hold the wiki.
  - **`.tools` can only be set after `.host()`**, since `.host()` changes the builder's host type. (Dave, 2026-10-23)
  - [PROVISIONAL: it is enforced at runtime: `.tools` set before `.host()` fails `build`, as an invalid entity type does.]

  ```rust
  Wiki::builder(root)
      .tools(|wiki| wiki.tools().with_route(route(MyTool::new(wiki.clone()))))
  ```
- **Each tool holds its own copy of the wiki** (a cheap clone of the handle). Using a tool directly means instantiating its type with a wiki instance, which carries the host type. (Dave, 2026-10-20)
- **A tool that needs something from the host gets it through the `Host` trait.** None do for now. (Dave, 2026-10-20)
- **Tools are plain structs, adapted to rmcp by one generic `route` function.** (Dave, 2026-10-20: "that sketch looks fine.")
  - A tool is a struct holding the wiki, with an async `call(args)`. Using a tool directly is `Search::new(wiki).call(args)`, without rmcp.
  - Tools share a small trait of agent_wiki's own: name, description, argument type, and `call`. rmcp's `AsyncTool` can't serve, since its `invoke` has no `&self`.
  - `route` wraps any tool in rmcp's `ToolRoute::new_dyn`, with a closure that holds the tool, parses the arguments, calls it, and serializes the result. Tool authors don't write closures.
  - `tools` is generic over rmcp's server type `S`, so a host can merge the collection into its own server.
  - [PROVISIONAL: `call` returns `String` (formatted text). The trait gets a structured output only when a tool needs JSON.]
  - [PROVISIONAL: a tool's error is returned to the agent as an error result it can read, not as a protocol error.]

  ```rust
  pub struct Search<P: Host> { wiki: Wiki<P> }

  impl<P: Host> Search<P> {
      pub fn new(wiki: Wiki<P>) -> Self { … }
      pub async fn call(&self, args: SearchArgs) -> Result<SearchResult> { … }
  }

  impl<P: Host> Wiki<P> {
      pub fn tools<S: Send + Sync + 'static>(&self) -> ToolRouter<S> {
          ToolRouter::new()
              .with_route(route(Search::new(self.clone())))
              .with_route(route(Overview::new(self.clone())))
      }
  }
  ```

## Serving

- **The standalone binary hosts it over HTTP.**
- **Hosts have two ways in:** attach the HTTP MCP to their router, or incorporate the exported tools into their own MCP server. (Dave, 2026-10-20)
- **Endpoint: `{base}/_wiki/mcp`**, for now. (Dave, 2026-10-20)
- **Always on**, for now. (Dave, 2026-10-20)
- **No cargo features**, for now. (Dave, 2026-10-20)
- **No MCP sessions until something needs them.** (Dave, 2026-10-20) MCP protocol `2026-07-28` removes sessions, and rmcp serves that version without them.
- [PROVISIONAL: a call is answered with plain JSON rather than an SSE stream when the tool sends nothing before its result (rmcp's `json_response`).]
- [PROVISIONAL: rmcp's default host check is kept: only requests addressed to `localhost`, `127.0.0.1` or `::1` are accepted, against DNS rebinding.]
- [PROVISIONAL: a host that sets the builder's `.allowed_hosts` replaces it: the wiki's own check covers the endpoint, and rmcp's is turned off.] See Host check against DNS rebinding in `agent_wiki.md`.
- [PROVISIONAL: the binary prints the MCP URL next to the site URL at startup.]

## Results

- **Tool results refer to documents by file path, relative to the wiki root**, "since that's how the agent will access them". Page URLs in every result would waste tokens. (Dave, 2026-10-20)
- **The wiki root's absolute path is given to the agent once, in the server's instructions**, not in every file reference. (Dave, 2026-10-20)
  - The binary always includes it.
  - Used as a library, it is included too, for now. The host should eventually decide; that is addressed as part of hosted mode. "Hosted mode for custom applications will require cooperation." (Dave, 2026-10-20)
  [OPEN: deferred — a hosted vs local mode for the built-in binary.]
- **Results are mostly formatted text**, which is smaller and which the agent reads fine; JSON where it's useful. (Dave, 2026-10-20)
- [PROVISIONAL: a tool's `path` argument is relative to the wiki root, with `.md` optional, as in the web routes. A path with an empty or hidden segment (including `..`) is refused, and so is a symlink.]

## Tools

- **Start with basics to get a spike working, then expand the tools.**
- **The spike's tools: search and doc overview.** (Dave, 2026-10-20)

### Search tool

- **Input:** a query string, and an optional folder. "Enough for now." (Dave, 2026-10-20)
- **Output:** whatever is most likely to be useful to the agent without taking up too many tokens. (Dave, 2026-10-20)
  [PROVISIONAL: one block per result, in rank order. Path first, since the agent reads files by path; tags only when there are some; the summary line only when there is one. No URLs.]
  ```
  projects/foo.md — Foo Project  #tag1  #tag2
    First 280 chars of summary…
  ```
  [PROVISIONAL: no limit on the number of results.]
  [PROVISIONAL: an empty result says "No matches."]
- **Results while the index is still filling are fine**, for now. (Dave, 2026-10-20)
  [OPEN: deferred — exposing index state, e.g. "initial state loading" or the last index time. Dave (2026-10-20): "probably useful in many places", e.g. the index page fills in over a few refreshes after a restart. "Not necessary for this, unless it's easy."]

### Doc overview tool

> **Dave (2026-10-20):** "the idea here is to give the agent a way to inspect a file without spending the tokens to read the whole thing… this should make it easy for an agent looking for a specific topic to then go grab a useful chunk by line range."

- **The frontmatter.**
- **The outline: a nested tree of headings, each with its line number and anchor.** (Dave, 2026-10-20)
- **References to other docs.** (Dave, 2026-10-20)
  [PROVISIONAL: references are the page's wikilinks, each as written, resolved to a path, or marked ambiguous (with the match count) or unmatched.] Ordinary markdown links to other `.md` files aren't extracted today.
  [PROVISIONAL: image embeds (`![[image.png]]`) aren't references.]
- **The summary, if available.** (Dave, 2026-10-20)
- **Page stats**, so the agent can decide how to consume the document. (Dave, 2026-10-28) "page size is probably very useful for how the agent decides to consume a file"
  - **Size is given in lines.** (Dave, 2026-10-28)
    [PROVISIONAL: a `lines: N` line after the title.]
- [PROVISIONAL: output, with sections that have nothing in them left out:]
  ```
  path: projects/foo.md
  title: Foo Project
  summary: …
  frontmatter:
    <frontmatter as YAML>
  outline:
    L3    # Foo Project  #foo-project
    L10     ## Status  #status
    L24       ### Risks  #risks
  references:
    [[Bar]] → notes/bar.md
    [[Baz]] → ambiguous (3 matches)
    [[Qux]] → no match
  ```

### Page URL tool

- **Takes a file, and optionally a line or a heading, and returns the page's URL.** (Dave, 2026-10-20)
- [PROVISIONAL: named `page_url`. A line gives a line link (`#:N`); a heading, matched by its text or its anchor, ignoring case, gives the heading's anchor. Giving both is an error, and so is a heading the page doesn't have.]
- [PROVISIONAL: the URL is absolute, built from the MCP request's `Host` header (and `X-Forwarded-Proto`, else `http`) plus the base path, since the agent reached the server there. Called directly, outside an MCP request, it is a path from the site root.]

### Read document tool

- **`read_document` reads any document**, including the reference documents built into the binary, which have no file on disk (see `agent_integration.md`). (Dave, 2026-10-27)
- **It mirrors the web UI, serving both documents and index pages.** (Dave, 2026-10-28)
  [OPEN: deferred — hosts participating, as they do in web rendering. Dave (2026-10-28): "it would be cool if hosts could participate in this".]
- **Arguments: `path`, `lines` (optional), `sections` (optional).** (Dave, 2026-10-28)
  - With neither, it returns the whole document.
  - `lines` returns a line range.
  - `sections` returns those sections, as extracted by section (see Extracting a section in `data.md`). **Sections are named by anchor.** (Dave, 2026-10-28)
  - **`lines` and `sections` are mutually exclusive**; giving both is an error. (Dave, 2026-10-28)
  - **`lines` is a string like `"12-40"`**, matching how `open_files` reports selections. (Dave, 2026-10-28)
    It also accepts the `open_files` form as written (`L12-L18`, `L40-end`), and `"12-"` to the end. (Dave, 2026-10-28)
  - [OPEN: deferred — searching within the document, returning the blocks or sections that contain the match plus where they are in the file. Dave (2026-10-28): "maybe later".]
- **No overview mode.** The doc overview tool already gives the overview, and stays where it is. (Dave, 2026-10-28)
- **Reading an index page returns something like the web view, to start: the file list with some brief preview content.** (Dave, 2026-10-28)
  [PROVISIONAL: `folder: <path>/`, the reference's prompt at its root, then `folders:` (one path each) and `documents:`, each as `search` gives it: path, title and tags, then its summary. A path that is `""`, ends in `/`, or is a folder with no document of the same name reads the index.]
- [PROVISIONAL: a document, a range or a section is returned as its source text, without line numbers. Several sections are joined by a blank line. A range ending past the last line stops there; one starting past it is an error.]
- [PROVISIONAL: a section's lines run from its heading's line to the line before the next outline heading of the same or a higher level.]

### Tool ideas

Offered by Dave, not yet chosen (2026-10-20):

- Follow or expand wikilinks.
- Search within a document, returning the sections that contain the search term. Now a possible later mode of `read_document`.

## Round-tripping between the web UI and the agent

> **Dave (2026-10-20):** "one thing that's important is being able to round-trip between the webui and the agent. ideally, the agent could see what the user has open. maybe even what they have selected?"

- **Important:** the agent can see what the user has open in the web UI.
- **The browser side is part of the feature set**: the state push, the open-files tool, and the copy button. It doesn't have to be in before testing starts. (Dave, 2026-10-20)
- **Only for the local case**, for now. "The hosted version will need a different solution that we can figure out later." (Dave, 2026-10-20)
  [OPEN: deferred — the hosted version. It needs a different design, with some kind of auth, which lets us track per-user state. Dave (2026-10-20): "a problem for another day."]
- [PROVISIONAL: a page counts as open while its live-update stream is connected. The server gives each document page load a tab id, in the stream's URL (`{base}/_wiki/events?url={page url path}&tab={id}`) and on `<body data-tab>`. Other pages' streams, such as a live custom page's, carry no tab. When the stream drops (the tab is closed, or navigates away), the page is no longer open. Ids are `{process start ms}-{counter}`, so a page reconnecting after a restart keeps a distinct id.]
- **Nice to have: a one-click copy-to-pasteboard button** that captures the current page in a way the agent can follow. (Dave, 2026-10-20)
  It copies the page's path relative to the wiki root. (Dave, 2026-10-20)
  [PROVISIONAL: `BodyCtx::copy_path()`, a `button.copy-path`; the default template puts it at the right of the breadcrumbs. A shared click handler in the page skeleton shows "Copied" for a moment.]
- **The client pushes its state to a new endpoint as it changes:** focus and selection, with other fields added over time. Not a live socket. (Dave, 2026-10-20)
  [PROVISIONAL: `POST {base}/_wiki/state` with `{tab, focused, selection: {start, end} | null}`, sent when the stream connects (`htmx:sseOpen`), on `focus`, `blur` and `visibilitychange`, and on `selectionchange` (debounced 250 ms). Unchanged state isn't resent. A push for a tab whose stream isn't connected is ignored. `focused` is `document.hasFocus()`.]
- **The agent can see what the user has selected**, reported as the page's path plus a line range from the blocks' `data-line`. (Dave, 2026-10-20)
  [PROVISIONAL: `start` is the line of the block the selection starts in; `end` is one before the first block after the block it ends in, or none when it ends in the last block (to the end of the file). Only a selection inside the htmx root's numbered blocks counts. No selected text is sent.]
- **A tool returns all the open files, from most to least recent.** Recency comes from the client's pushed state, so it is the most recently focused or interacted with; when that's all there is, when the page was opened. (Dave, 2026-10-20)
  [PROVISIONAL: a tab becomes the most recently active when it connects, gains focus, or changes to a non-empty selection.]
  [PROVISIONAL: named `open_files`, in the default collection, with no arguments. One line per file, deduped by file (the most recent tab's state wins): `path  (focused)  selected L12-L18`, `L40-end` for a selection to the end; "No documents open." when none.]
- [OPEN: deferred — moving live updates from server-sent events to a full websocket, to carry richer data between client and server. Dave (2026-10-20): "a task for another day?"]
