# agent_wiki — Design

The standing definition of agent_wiki. Unmarked prose is decided; anything not decided carries `[OPEN: …]` or `[PROVISIONAL: …]` (see `docs/process.md`). Decisions quote Dave where his words carry the decision.

## Pitch

> **Dave (2026-10-19):** "I have expressed an idea to want to be able to host a folder of markdown files as a website. I want to be able to use it in multiple contexts. Let's build it as an open source project. We can build it using rust/axum so it can be dropped in anywhere. This way, it can be hosted inside other applications and wrapped in things like auth/tooling/etc."

## Shape

- **What it does:** hosts a folder of markdown files as a website.
- **Open source**, standalone project. Single developer. Name: **agent_wiki**.
- **License: MIT.** (Dave, 2026-10-19)
- **Rust / axum.**
- **Embeddable:** it can be dropped into other applications and hosted inside them, where the host wraps it in things like auth and tooling. Auth etc. belong to the host, not to this project.
- **Library + thin binary.** The library is what hosts embed; the binary is a thin standalone wrapper.
- **Multiple contexts** — more than one host application.
- **First target: the standalone binary.**
  > **Dave (2026-10-19):** "If we just target the standalone binary, we can run it in either project for testing."

- **Library surface:** something that can be dropped into an axum router, with a defined root path. (Dave, 2026-10-19)
  > **Dave:** "So something like [a host application] could host the whole thing under `/docs` and also host other, application specific content."
- **One crate.** (Dave, 2026-10-19)

### Construction — a builder

> **Dave (2026-10-19):** "In the constructor for the thing, like we use a builder so that you can pass in other things, whatever else we want to be able to add dynamically… be able to configure the system when we create it."

- The wiki is constructed with a builder.
- One thing it takes: a base stylesheet (see Styling).
- A trait with hooks for behaviours is a possible later direction — "I don't know what those would be yet, so we don't have to go down that road."

- Builder surface for now: **base path and stylesheet.** "We'll just add to it as we add to it." (Dave, 2026-10-19)

### Extensibility — simple default, completely replaceable

> **Dave (2026-10-19):** "Make 'configuration' minimal and actually rely on callbacks or an object passed in that has a trait on it… Rather than have like three modes for path resolution, you have the default one if nothing is specified, and then if something is passed in, it would be like a function that can resolve image paths, and then the application can totally implement its own logic… a simple default implementation and then completely replaceable logic. No multi-mode selection kind of stuff."

- Each customisable behaviour has one simple default.
- A host replaces it wholesale by passing in a callback or trait object.
- No modes or option switches for choosing between built-in behaviours.
- "I don't want to over design here. Let's just keep it simple for now." Hooks are added when a real integration needs them.
[OPEN: whether rendering is usable without axum.]

## Host API

> **Dave (2026-10-19):** "[A host application] is already hosting these like dashboard pages which have special live logic… we want to be able to directly integrate the agent wiki into that application… a sort of hosted view that contains live data, like anywhere we reference a linear ticket it expands it and shows status and stuff… custom widgets, but also other pages, like custom product dashboards… that application will wrap this one."

Two places where host behaviour has to be configurable:

1. **API layers.** The data agent_wiki holds is usable programmatically, not only as a web view.
2. **Content hooks.** A host detects content elements unique to it and presents them specially.

### API layers

> **Dave (2026-10-19):** "The simplest, most naive version is create the thing, start it, or create the thing hosted inside of an axum application… One layer is like here's how we serve the web view. But actually a layer below that is treating the document set as essentially a database, or as the data source, and the ability to view it and interact with it as structured data programmatically."

- **Web view:** create and start it, or mount it in a host axum application (see Shape).
- **Below the web view, the document set as a data source:** the documents, the search engine, the outline extracted from the parsed markdown, viewable and queryable as structured data.
- **Host-specific views are built on top, not inside.** Dave's example: a project folder has an entry-point document full of Linear references; a live dashboard of that project's tickets and their status "doesn't belong in the agent wiki, which is general purpose, but we should be able to build on top of it by being able to extract the parsed contents and query for things."

- **The wiki doesn't host a host's other pages.** (Dave, 2026-10-19) "The whole wiki can be hosted inside another application, so we don't need the wiki to be able to host other pages. We can just host them directly."

### The data layer

**Generated on demand from the file.** (Dave, 2026-10-19)

> **Dave:** "If I wanted to have an API that could say give me the structured form of this thing, or give me the outline… we should be able to generate them from the raw file in… two milliseconds at most… for search purposes we want to [store it], but if we're just trying to make it easy for other pieces of software to interact with the data, we just have to describe how we want to present it in an API, and then we can do that on demand directly from the files."

- The API passes the file through the same parsing behaviour as the web view and returns the result as structured data. Net new: today the parse is thrown away.
- What the index stores is driven by search. Dave expects some or all frontmatter, and headings or the outline, to be stored "eventually".

**What a document is.** (Dave, 2026-10-19)

- **A metadata structure:** a fixed set of properties you can just read.
- **A document content structure:** an AST or some other structured view. From it you can extract all the parsed elements, including host-specific ones (e.g. Linear ticket links) as their own application-specific types. "There's probably some generic stuff that gets involved here."
- **Accessors for other kinds of data**, each with its own type, e.g. "give me the outline for this document" returns a tree.
- **Expose things as a use case needs them.** "I'd sort of bias towards exposing things as we need them for a use case rather than doing a bunch proactively and then discovering it's wrong."

**How a host queries.** (Dave, 2026-10-19) "All the APIs are like give me documents."

- **The path of any document**, so a host can link to it.
- **Search.**
- **Documents under a folder**, which is "kind of a version of search", e.g. "search for all the files under projects with this keyword".
- **Iterating the documents and the file tree.** The naive case is that the host walks the content and filters it itself; "we can build in optimizations where we need them."
- **The most recently updated documents in a tree**, e.g. the first 20.
- **No raw SQL access.**

**Watching for changes.** Since agent_wiki already watches the files and processes what changed, it can offer hosts a rich API for this: an async channel to subscribe to updates, for specific files or trees. "We don't have to solve that now, but it's a good call out for something that we should build support for." (Dave, 2026-10-19)

[OPEN: the shape of the watch API.]

### Content hooks

> **Dave (2026-10-19):** "The ability to detect content elements that are unique to that application and present them specially. Which is tricky because that kind of happens inside of a document view… we are going to want to be able to pass in some object that can interact with the content as it's being parsed and change how it's presented."

- The host passes in an object that interacts with content as it is parsed and changes how it is presented.
- **Detection changes what a thing parses as.** Dave's example: in a host's content, `[[ABC-1234]]` wikilinks are Linear tickets by convention. "This wiki link, by a regular expression match, is definitely a ticket, not a normal wiki link, so instead of parsing it as one type of thing we're gonna parse it as a different type of thing that's application specific."
- **Presentation:** the ticket renders as a rich inline HTML element showing the ticket ID and its status. Clicking it goes to the ticket, not to the disambiguation page.
- The component library for such elements can belong to the host, not to agent_wiki.
- **Detection also changes how the thing is treated in the data layer**, not just how it is presented. (Dave, 2026-10-19)

### Parsing and extraction — designed first

> **Dave (2026-10-19):** "I think we work from the inside out… the place where I want to start is in designing the parsing and extraction layer… in the beginning it's just like how do we produce the data, the data type, right? And then from there, the API kind of falls out easily if we have the data type and this trait."

- **Order:** the parsing and extraction layer and its data type come first. The query API and document data types are built on top of them.
- **The markdown file is treated like a DOM.** The frontmatter forms the header structure, and the rest is the body. The body breaks down into a tree of element nodes, "probably HTML-ish, although they don't have to be those ones specifically."
- **agent_wiki builds its own tree.** "It gives us control." (Dave, 2026-10-19) The web view and the data layer use this same tree. The page is rendered from it, not from another parser's tree.
- **The tree is both an AST and a DOM.** "Our node tree is sort of pulling double duty as both an AST and a DOM." (Dave, 2026-10-19)

#### Our own parser, with the host first

> **Dave (2026-10-19):** "What if we used a parser like nom and allowed the host to provide their own components that fit in specific slots."

Chosen over limiting custom inserts to a fixed set of markdown-like shapes. (Dave, 2026-10-19: "yes, the parser.")

- **The design is the sketch in `docs/design/example.rs`.** (Dave, 2026-10-19: "I think we have a design.")
- **The host participates in parsing through a trait, as a generic parameter.** A built-in implementation that does nothing is the builder's default. When a host supplies its own, the builder builds the wiki with the host's type.
- **Host matchers run first, then the built-in ones.** The exception is escapes and code spans, which are matched before the host. (Dave, 2026-10-19: "those choices seem fine.")
- **Nesting goes from library to host to library**, as in `[OPEN: **foo** text with inline _formatting]`.
- **Two phases.** "Markdown is basically blocks and inline sections." (Dave, 2026-10-19)
  - **Block phase:** any approach works. Identify the next block, offer it to the host, and fall back to the built-in block types.
  - **A host block can interrupt a paragraph.** (Dave, 2026-10-19)
  - **Inline sections:** parsed nom-style, with the host first, then the built-ins.
- **Built-in syntax is the library's own matchers.** A host overrides one by matching first; e.g. a host's `[[ABC-1234]]` is a ticket, not a wikilink.
- **A claimed node is no longer what it was.** Once a host claims a `[[ABC-1234]]` wikilink as a ticket, it isn't a wikilink. It leaves the wikilink index and no longer produces disambiguation pages. (Dave, 2026-10-19)
- **The parse hook is synchronous and does no I/O.** Pages are parsed on every request, and nothing slow belongs in the page-load path. Live data (e.g. ticket status) does not come from the parse hook. (Dave, 2026-10-19)
- **Parse hooks are pure.** (Dave, 2026-10-19) Any other work is done afterwards, by walking the graph once it's built: "We can do any post-processing work by walking the graph after it's created." Rendering hooks may also need to be pure and synchronous; that hasn't been settled.
- **The API must not commit to nesting.** (Dave, 2026-10-19) "The problem with the API returning `Foo(Bar(Vec<Baz>))` is that you can't change this to use daggy without also having a separate representation that doesn't nest one in the other." So a node's kind holds no children, and children are an opaque library type (see the sketch).
- **Two host element types: `P::Block` and `P::Inline`.** Public types are parameterized by the host type `P` (see the `Host` trait below), e.g. `BlockKind<P>` with `Host(P::Block)` and `InlineKind<P>` with `Host(P::Inline)`. (Dave, 2026-10-19)
- **Storage: an arena.** "There are good, mature off the shelf solutions to this, like daggy." (Dave, 2026-10-19)
- **No source ranges on nodes.** "We don't need source ranges anymore." (Dave, 2026-10-19)
  - Blocks do record the line they start on, for line links (see Markdown support → Line links). That is one line number per block, not a range. (Dave, 2026-10-19)
- **Frontmatter is a `serde_json::Value`.** (Dave, 2026-10-19) Frontmatter that isn't well formed isn't parsed. (Dave, 2026-10-19: "probably just don't parse frontmatter if it isn't well formed?")
- **Raw HTML blocks end at their closing tag**, not at the next blank line as in CommonMark. (Dave, 2026-10-19: "closing tag seems right?") This fixes pages with no blank lines, such as some Notion exports.
  - A consequence: markdown inside a raw HTML block (e.g. `<details>`) stays raw and isn't searchable.
  [OPEN: deferred — searchable text inside raw HTML, e.g. by parsing the HTML to find its plain text, through the same "every node returns its text" path. Dave (2026-10-19): "Probably not important right now."]
- **Host blocks that close with the same marker can't nest through `blocks_until`** (`inlines_until` can). "If we don't need it right now, then we'll worry about it later. Keep track of it though." (Dave, 2026-10-19)
  [OPEN: deferred — nesting host blocks that share a closer.]
- **How faithful to the spec:** "It just needs to work for our use case. Using an existing test suite is a valuable shortcut, but I don't care about obscure cases until they bite." (Dave, 2026-10-19)

#### Editing the tree

- **Full-graph editing.** agent_wiki builds the whole DOM, and the host can edit it by graph editing. (Dave, 2026-10-19)
  > **Dave:** "It's probably easier to write code that peeks at things an event based system wouldn't be able to see. We can make it easy to walk the graph. DOM manipulation is well understood."
- **Editing borrows existing DOM APIs.** "APIs for editing DOMs probably don't need reinvention." (Dave, 2026-10-19)
- **Round-tripping is interesting.** Converting the tree back to markdown would make document editing possible in the API. A round trip serializes the tree; splicing edits into the source text is ruled out ("a nightmare to implement"). (Dave, 2026-10-19) This bears on the existing `[OPEN: read-only, or any editing.]`.
- **Not needed for the first host integration**, nor is serializing back to markdown. (Dave, 2026-10-19)

[OPEN: persisting or indexing host-produced data, or not. Deferred: "in the beginning it's just like how do we produce the data."]
- **Changing a host's parse rules doesn't trigger a reindex.** A persisted index is rebuilt only when agent_wiki's own index version changes. For now, a host that changes its rules deletes the index file and starts again. (Dave, 2026-10-19)
  [OPEN: deferred — an API for the host to trigger reindexing.]

### Page wrapper — hooks into the draw cycle

> **Dave (2026-10-19):** "You would want to be able to provide like a wrapper, essentially a template… all pages render inside of this thing. That way you could provide custom heading or custom navigation… [A host] might want its own navigation row where, before the page is rendered, it's handed the document and it's able to say, oh, this is the root document for a project, so I'm gonna draw custom breadcrumbs… the wiki can say, okay, this is the page that's being loaded, here's the rendered body of the document, and then the host can draw the wrapper around it."

- A host can provide a wrapper that all pages render inside, for its own header or navigation.
- A host-supplied renderer gets callbacks or hooks at different layers of the draw cycle. One example: it is handed the document before the page is drawn and decides the breadcrumbs.

- **A custom template, with a widget toolkit for live widgets.** (Dave, 2026-10-19) For example, the host draws a Linear ticket element and loads its data from an endpoint in the host app.

The hooks are under "What the page gets from the library" below.

#### What the page gets from the library

> **Dave (2026-10-19):** "Let's start by making an inventory of all the things that must/should/could come from the library… the goal is to make something that's easy for the application to extend without breaking any of these."

Dave's first take:

- **Page title:** provided by the library, "but ideally overridable by application."
- **Basic theme:** provided by the library, "replaceable OR additive with application styles."
- **htmx:** the `<script>`, plus a required root node.
- **Library-specific behaviour code**, like the live-update watcher. "Probably more in the future for things like making search more interactive, etc."

Not yet addressed. Deferred until after the first prototype (Dave, 2026-10-19: "capture and defer the undecided parts"):

[OPEN: whether `suppress_default_styles()` also drops the code-highlighting stylesheets, and whether a host can theme mermaid.]
[OPEN: whether a host can replace (not just wrap) the bodies of pages that aren't documents: folder index, search, tag, disambiguation.]
[OPEN: whether a host can set attributes on `<html>` and `<body>`, e.g. `lang`, or a theme class.]
[OPEN: how host scripts re-run after a live-update swap. A host listening for `htmx:afterSwap` itself is the default.]
[OPEN: assets a host element needs, loaded only on pages where the element appears. Until then the host includes its scripts on every page through `head()`.]

**The library owns the page skeleton, and the host fills it.** (Dave, 2026-10-19)

- **A separate `Template` trait.** (Dave, 2026-10-19)
- **The template is an associated type** on the host's trait, `P::Template`, next to `P::Block` and `P::Inline`. The wiki keeps one generic parameter and dispatches statically. **The host's trait is renamed `Host`**, since it now covers more than parsing. (Dave, 2026-10-19)
- **The hooks are async.** (Dave, 2026-10-19)
- **`head(ctx) -> html`.** `ctx` has data about the page, plus methods that alter the default behaviour, e.g. `set_title(…)` and `suppress_default_styles()`. These methods are only on the head's `ctx`, not the body's. (Dave, 2026-10-19)
- **`body(ctx, content) -> html`.** The host embeds the passed-in content where it wants it. `content` is a closure, `content(ctx)`, so it renders lazily. (Dave, 2026-10-19)
- **The whole output of `body()` is wrapped in the htmx root node**, so the host's chrome is refreshed by live updates too. (Dave, 2026-10-19)
- **The default layout is today's layout**, inside the htmx root. (Dave, 2026-10-19) Dave's earlier sketch, `<body><div id="htmx-root"><div>{search box and breadcrumbs}</div>{body content}</div></body>`, left out the tag links and frontmatter table by accident.
- **The built-in chrome elements:** the search box, breadcrumbs, the page's tag links, and the frontmatter table. (Dave, 2026-10-19)
- **A host that provides its own `body` places the built-in elements itself**, such as the search box and breadcrumbs. `ctx` exposes them for that, e.g. `ctx.breadcrumbs()`. "The complexity is interleaving all the stuff." (Dave, 2026-10-19)
- **`ctx` says what kind of page it is:** a document (with its metadata), a folder index, search, tag, disambiguation, or not found. (Dave, 2026-10-19)
- **`ctx` gives access to the request**, e.g. for the host's auth. (Dave, 2026-10-19)
- **`ctx` exposes the base path**, so a host's chrome can link into the wiki without hard-coding it. "It's just plumbing." (Dave, 2026-10-19)

#### Drawing

- **Rendering is part of the element trait.** Host-provided element types turn themselves into HTML. (Dave, 2026-10-19)
- **Drawing is the parsing shape in reverse.** (Dave, 2026-10-19) Roughly `fn draw(&self, children) -> String`: an element renders itself to HTML and calls `draw` on its children.
- **`Element::draw` gets the base path**, so elements can generate relative paths. (Dave, 2026-10-19)

- **Server-side HTML: maud**, "as long as it doesn't make the API super complicated." (Dave, 2026-10-19)
- **In the browser: htmx, and the host is free to add its own.** For example, a host could use Alpine to build its Linear widget. (Dave, 2026-10-19)

### First host integration

> **Dave (2026-10-19):** "I want to get to where this is working in a host application."

- **Goal:** today's web UI functionality, rendered inside the host's wrapper (the draw hooks), plus host elements. (Dave, 2026-10-19)
  - **Host elements:** live Linear tickets, and Obsidian tasks parsed into a widget, "just presentation, for now".
  - **The task widget is a list item containing a task.** (Dave, 2026-10-19) This replaces an earlier choice that the host claims the whole task list item at block level, `- [ ]` marker included.
    - It doesn't capture task hierarchy. "[The host] already has its own mechanisms for extracting tasks. This is a presentation layer thing."
    - Dave's idea: treat todos as inline items. "The catch is that the `-` is part of the syntax. A task is a list item containing exactly one task. Could we have the parser produce a list item containing a task?"
    - **The task is inline**, inside the library's list item. "Inline is probably fine, since the block is the list item." (Dave, 2026-10-19)
    - **To the user, task syntax is one thing:** `- [ ] desc plus emoji metadata fields until end of line`. The parser splits it into two concerns, a list item and a task, "but we want the effect to be the same." (Dave, 2026-10-19) So a `[ ]` is a task only at the start of a list item, and the task runs to the end of the line.
    - **In lists that mix ordinary items and tasks**, it "would be nice if it worked 'as expected'." Something simpler is fine for this pass, "just to get something working." (Dave, 2026-10-19)
    - **Statuses:** Obsidian configs can add custom statuses, such as `[/]` in progress, `[-]` canceled, `[?]` unknown. For now, use parse-obsidian and let it miss these. Custom status support probably belongs in parse-obsidian later, since it's a feature of the Tasks plugin. (Dave, 2026-10-19)
    - **Task text is searchable.** "We definitely want to be able to search." (Dave, 2026-10-19)
    - **Host content reaches search through the extracted text, not the raw markdown.** Host elements can contribute text (e.g. an `Element` method that returns nothing by default), in addition to the text of their children. (Dave, 2026-10-19)
  - **Linear IDs:** a general pattern like `[A-Z]{3,4}-[0-9]+`. (Dave, 2026-10-19: "I don't know if we'll even find other examples.")
  - **A ticket that isn't in the host's Linear cache is loaded on demand.** Possibly keep a list of recently accessed tickets cached. (Dave, 2026-10-19)
  - **Tickets on the page being viewed should stay current.** Today a ticket loaded on demand never refreshes, which "is not great because if I'm looking at a page and working from a page, I want the tickets to update." (Dave, 2026-10-19)
    [OPEN: how. Dave's idea (2026-10-19): the server knows which page the user is looking at, because it just served it and the page holds a live-update connection. So it could keep only the tickets that page references up to date. As the user browses, the set of tracked tickets follows them.]
  - **A failed ticket fetch isn't remembered**, which is fine as long as nothing can retry automatically in an endless loop. (Dave, 2026-10-19)
    [OPEN: how a ticket element shows a failed fetch.]
  - **Linear tickets are written `[[ABC-N]]`.** (Dave, 2026-10-19: "could we use the `[[ABC-N]]` format…?")
- **Mounted at `/docs`** in the host, which depends on agent_wiki by path. (Dave, 2026-10-19)
- **Not in it:** the exposed data layer (deferred), editing the tree, and serializing back to markdown. (Dave, 2026-10-19)

## Markdown support

> **Dave (2026-10-19):** "There's not really any conflict between obsidian and github flavors, is there? Couldn't we just support both?"

Support both GFM and Obsidian syntax. Features Dave named:

- Formatted code blocks
- Mermaid diagrams
- Wikilinks
- Obsidian-style tasks — **nice to have**

- **Headings are linkable.** (Dave, 2026-10-19)
- **A wikilink with a heading fragment (`[[Page#Heading]]`) jumps to that heading.** (Dave, 2026-10-19)

### Line links

> **Dave (2026-10-19):** "could we come up with a pattern that lets us link to a line? `/docs/foo/bar.md#:123`" … "sounds great. go ahead and make the change in agent wiki."

- **`#:N` links to line N of the page's source file.** It can't collide with a heading id, since slugs never start with `:`. (Mechanism proposed by Claude and approved by Dave, 2026-10-19.)
- **Each block records the line it starts on**: 1-based, in the original file, counting frontmatter lines. It is drawn as a `data-line="N"` attribute on block elements: paragraphs, headings, lists, list items, block quotes, code blocks, tables, table rows, thematic breaks, and HTML blocks.
- **A small client script handles `#:N`**, on load and on `hashchange`: it finds the last element in the page content whose `data-line` is at most N, scrolls it into view, and highlights it.
- **Precision is per block.** A line in the middle of a paragraph lands on the paragraph.
- [PROVISIONAL: an HTML block, and a host block, get `data-line` on the first open tag of their markup, when it starts with one (not text, a comment or a closing tag). Nothing wraps them, so layout and host markup are unchanged. Blocks a host builds inside its claim with `node` have no line; the library's blocks inside a host's `blocks_until` do.]
- [PROVISIONAL: the highlight is the `line-target` class, with a default background in the built-in stylesheet (`--line-target`). A host restyles `.line-target`, or drops it with the default styles.]
- [PROVISIONAL: the script skips elements that aren't laid out (e.g. a `<style>` HTML block). After a live-update swap it marks the line again without scrolling, so a page being edited doesn't jump.]

[OPEN: the rest of the feature list — callouts, math, embeds (`![[…]]`), block links, highlights, `%%comments%%`, footnotes, backlinks, graph, table of contents. Offered, not yet chosen.]
[OPEN: whether parsing builds on `parse-obsidian`.]

### Line breaks — Obsidian style

> **Dave (2026-10-19):** "Obsidian style. More space is generally better. LLM generated markdown always risks being a 'wall of text', so anything to give it more breathing room helps readability."

A single newline renders as a line break.

### Raw HTML — allowed

Raw HTML in markdown passes through. (Dave, 2026-10-19)

### Code highlighting and mermaid — in the browser

> **Dave (2026-10-19):** "In the browser. We can import those tools without even embedding them in the project, right?"

Client-side, loaded from a CDN rather than vendored into the project.

[OPEN: which highlighter; whether a host can point the asset URLs elsewhere (e.g. offline use).]

### Page titles

Frontmatter title, if present; otherwise the first heading; otherwise the filename. (Dave, 2026-10-19)

### Frontmatter

> **Dave (2026-10-19):** "Some of the fields, we may use directly in the template. We can show it as a table but have it collapsed by default."

- Shown as a table, collapsed by default.
- Some fields feed the page template directly.

[OPEN: which fields the template uses.]

## Wikilinks

> **Dave (2026-10-19):** "Obsidian style. As for what they match, I want to do something a little bit different. We probably often won't have files that exactly match. If a wikilink exactly matches a filename, great. If it doesn't they should go to a disambiguation page that shows every file that references the same link."

**Syntax:** Obsidian, `[[target|label]]`.

### Matching

A wikilink target matches a file's name when they are equal under these rules (Dave, 2026-10-19):

- **Case-insensitive.**
- **`-` and `_` match whitespace.**
- **A leading date is stripped:** `2021-01-01-foo-example.md` matches `[[Foo Example]]`.
- **Paths are only for disambiguating.** `[[Project/Foo]]` == `[[Foo]]` unless it's disambiguating.

[OPEN: the date formats that are stripped — only `YYYY-MM-DD-`, or others too.]

### Where a link goes

- **Exactly one matching file** → that file.
- **Multiple matching files** → a disambiguation page. A path in the link narrows the candidates.
- **No matching file** → a disambiguation page listing every file that references the same link.

[OPEN: on a multiple-match disambiguation page, whether it lists the matching files, the referencing files, or both.]

### Disambiguation page

> **Dave (2026-10-19):** "Start basic. We'll iterate."

- Lists all the pages that have that wikilink.
- Shows page titles.
- Links each to its page.
- Links directly to the first wikilink reference on that page, if that's possible.



## Tags

Wikilinks and tags appear in different parts of the UI, but behave similarly. **Tags always go to a tag disambiguation page; they never match to files.** (Dave, 2026-10-19)

[OPEN: tag syntax recognised — inline `#tag`, frontmatter `tags:`, nested `#a/b`.]

## Index

> **Dave (2026-10-19):** "We're already talking about using an in-process db to do indexing/search. We can add a table specifically for indexing wikilinks (and tags)."

- Lives in this project.
- **libSQL** (Turso's SQLite fork, the `libsql` crate). Switched from SurrealDB — Dave: "let's switch to libsql" — after a benchmark showed SurrealDB's memory growing on every reindex (in memory and on disk) while libSQL's stayed flat. The newer `turso` Rust rewrite was considered and set aside for now ("we can bench turso for the time being").
- Indexes wikilinks and tags.
- **Kept current by watching files**, using a Rust library over the OS-provided file-watch mechanisms.

### Index storage

> **Dave (2026-10-19):** "Thus far, we've kept the index in memory. That's the right choice for the standalone version. For [larger hosts], it makes way more sense to store the index on disk. Here's the first case where we're going to do some pluggable behavior."

- The host picks the storage and passes it to the builder.

- **The underlying database is not swappable.** > **Dave (2026-10-19):** "I'm not particularly interested in making the underlying db swappable. That's a big interface to maintain and the whole thing is supposed to be opaque to the user (aside from supplying a file path)."

- **An enum: `InMemory` or `FilePath(PathBuf)`.** Default: `InMemory`. (Dave, 2026-10-19)
- An `Other(handle)` variant for a host-connected database handle was discussed and **left out for now**, "to simplify."

- **Standalone binary:** `--index <path>` selects `FilePath`; when not provided, in-memory. (Dave, 2026-10-19)

[OPEN: the enum's name.]

### Semantic index

> **Dave (2026-10-19):** "I don't want to send all my content to openai for embedding, so we'll need to use a local embeddings library/model… fastembed I think. Other users may prefer to use openai or otherwise. For the in-memory index, it probably doesn't make sense to generate embeddings at all… we also probably need to store which embedding model was used for each record."

- Some hosts need **local** embeddings (fastembed); other users may want OpenAI or others.
- Store **which embedding model** produced each record.

- **Deferred.** "Let's do the embeddings later. Let's just get the persisted index working and look at the memory situation." (Dave, 2026-10-19)
- When built: **the embedder is separate** from index storage configuration. (Dave, 2026-10-19)
- **No embeddings for an in-memory index.** (Dave, 2026-10-19)

## URLs

A page's URL is its file path relative to the root of the hosted site, without `.md`, sanitized as necessary. (Dave, 2026-10-19)

- **The file name reaches the page too.** `{base}/foo/bar.md` goes to the same page as `{base}/foo/bar`. (Dave, 2026-10-19: "we already have urls for every page. the agent might write either form: path or filename. could we support both? we already have the code that turns one into the other since we use it in the web routing") It reuses the conversion that `.md` links on a page already go through.
- [PROVISIONAL: `{base}/foo/bar.md` redirects (307) to `{base}/foo/bar`, rather than serving the page there. That keeps one URL per page, which the live-update wiring and the page's own links already use, and browsers keep the `#fragment` (e.g. `#:123`) across a redirect. Temporary, not permanent, so browsers don't cache the choice. A file whose own name ends in `.md` (`x.md.md`) is still served at `/x.md`. Images and other files are unchanged.]

## Navigation

Folder index pages, for now. "This will probably evolve." (Dave, 2026-10-19)

### Index page — make it more useful

> **Dave (2026-10-19):** "Maybe we can do some stuff to make it nicer… a list of the most recently edited documents at the top… Especially since we have a database where we've indexed all the contents, like we could do most recently edited documents, tag clouds… basically make it more useful."

- **Recently edited documents**, at the top. Edit time: if a file is dirty, its local timestamp; if it's committed, its git timestamp. Kept in the index so it's cheap to read, written when updates are applied. (Dave, 2026-10-19) Exactness doesn't matter: "whichever one you have is kind of good enough." 
- It doesn't need to replace host dashboards: "when this gets wrapped in [a host application], there's other dashboards and stuff, so we don't have to rebuild those here."

- **On every folder index page**, not just the root — "it's just a single implementation, like every index page behaves the same." (Dave, 2026-10-19)

> **Dave (2026-10-20):** "i'm fine with recently edited then the main listing, though they should be presented differently and clearly separated."

- **Order: recently edited, then the main listing, then tags.** (Dave, 2026-10-20)
- **Recently edited: one tight line per page, with disambiguation data:** the title, then the relative path and filename, then the edit date. "it's also hard distinguish between similarly named files in different folders. and it kind of dominates the space." (Dave, 2026-10-20)
  [PROVISIONAL: the path is relative to the folder being shown; the date is `YYYY-MM-DD`, at the right of the line.]
- **The main listing has a header**, and is clearly separated from recently edited. (Dave, 2026-10-20)
  [PROVISIONAL: the header reads "Contents"; subfolders come first, as plain links, then the pages.]
- **The main listing shows expanded content for each page:** title, file name, description/summary/first body text, edit date, tags. (Dave, 2026-10-20) The file name because "when the title or first heading is not the same as the filename, it can be confusing."
  [PROVISIONAL: the summary is frontmatter `description`, else frontmatter `summary`, else the text of the first paragraph, cut to 280 characters. It is stored in the index.]
- **Tags at the bottom: the top 25, each with its count when it is greater than one,** e.g. `#foo (3)`. No sizing by weight: "it looks messy." (Dave, 2026-10-20)

[OPEN: what else goes on the index page.]

### Search

> **Dave (2026-10-19):** "We have an index, but we haven't added search. We should just add a search UI… a bar at the top of the screen where you can type a search and hit enter and then a search page with results."

- A search bar at the top of the screen.
- Enter → a search results page.

- Searches **titles, tags, and body text**. (Dave, 2026-10-19)
- A semantic index comes later: "we'll save that for after we start persisting the index to disk."

- **Ranking: title matches, then tag matches, then body matches.** (Dave, 2026-10-19)

## Styling

> **Dave (2026-10-19):** "Because this is meant to be embedded in multiple places, we probably want to let the host application control the styling. The stuff you built looks fine… as a built-in stylesheet, it's okay."

- The host controls styling; it can pass a base stylesheet through the builder.
- **The built-in theme is Rosé Pine.** (Dave, 2026-10-20)
  [PROVISIONAL: Rosé Pine Dawn in light mode and Rosé Pine (main) in dark mode, with the matching highlight.js themes.]
- **Code is set in a monospace font**; "just pick a monospace font for now." (Dave, 2026-10-20)

- A host's styles **add to or replace** the built-in stylesheet. (Dave, 2026-10-19) A host replaces it through `suppress_default_styles()` (see Host API → Page wrapper).
- **Dark mode:** yes. (Dave, 2026-10-19) Light and dark follow the system automatically. (Dave, 2026-10-20)

## Live updates

> **Dave (2026-10-19):** "It would be useful if we could have pages be able to live update. Imagine I have [a note] open and I'm talking to the agent and it's being modified. If we're watching the files, we should be able to know that in real time. And then we just need a way for the client to be notified, a push or whatever."

An open page updates when its file changes on disk.

- **Push via server-sent events.** (Dave, 2026-10-19: "sounds simpler. I'm fine with that.")
- **Update in place** is preferred; "we can do a reload if needed."

- **htmx** for this first iteration. (Dave, 2026-10-19) "We might want some local toolkit for building widgets… but I think htmx is okay for this first iteration."

- **Standalone binary shutdown:** the first interrupt starts a graceful shutdown and prints that it is waiting for connections to close; a second interrupt kills it. (Dave, 2026-10-20) An open page's live-update stream doesn't end on its own, so with a page open the first interrupt waits until the second.
  [OPEN: the library ending its live-update streams on shutdown, so graceful shutdown completes with pages open, for the binary and for hosts.]

## MCP

agent_wiki serves an MCP server so agents can work with the wiki. (Dave, 2026-10-20)

- **Built on rmcp.**
- **A set of built-in tools.**
- **Hosts enable or disable the built-in tools, and provide their own.**
- **The library exports its tools**, so a host can build whatever collection it wants, using rmcp's tool collections. (Dave, 2026-10-20)
- **The wiki has a `tools` method** that returns the default collection, which the host can edit. (Dave, 2026-10-20)
- **Each tool holds its own copy of the wiki** (a cheap clone of the handle). Using a tool directly means instantiating its type with a wiki instance, which carries the host type. (Dave, 2026-10-20)
- **A tool that needs something from the host gets it through the `Host` trait.** None do for now. (Dave, 2026-10-20)
- **Tools are plain structs, adapted to rmcp by one generic `route` function.** (Dave, 2026-10-20: "that sketch looks fine.")
  - A tool is a struct holding the wiki, with an async `call(args)`. Using a tool directly is `Search::new(wiki).call(args)`, without rmcp.
  - Tools share a small trait of agent_wiki's own: name, description, argument type, and `call`. rmcp's `AsyncTool` can't serve, since its `invoke` has no `&self`.
  - `route` wraps any tool in rmcp's `ToolRoute::new_dyn`, with a closure that holds the tool, parses the arguments, calls it, and serializes the result. Tool authors don't write closures.
  - `tools` is generic over rmcp's server type `S`, so a host can merge the collection into its own server.

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
- **The standalone binary hosts it over HTTP.**
- **Hosts have two ways in:** attach the HTTP MCP to their router, or incorporate the exported tools into their own MCP server. (Dave, 2026-10-20)
- **Endpoint: `{base}/_wiki/mcp`**, for now. (Dave, 2026-10-20)
- **Always on**, for now. (Dave, 2026-10-20)
- **No cargo features**, for now. (Dave, 2026-10-20)
- **Tool results refer to documents by file path, relative to the wiki root**, "since that's how the agent will access them". Page URLs in every result would waste tokens. (Dave, 2026-10-20)
  [OPEN: Dave's proposal — a tool that converts `file:line` to a URL.]
- **Results are mostly formatted text**, which is smaller and which the agent reads fine; JSON where it's useful. (Dave, 2026-10-20)
- **No MCP sessions until something needs them.** (Dave, 2026-10-20)
- **Start with basics to get a spike working, then expand the tools.**
- **The spike's tools: search and doc overview.** (Dave, 2026-10-20)

#### Search tool

- **Input:** a query string, and an optional folder. "Enough for now." (Dave, 2026-10-20)
- **Output:** whatever is most likely to be useful to the agent without taking up too many tokens. (Dave, 2026-10-20)
- **Results while the index is still filling are fine**, for now. (Dave, 2026-10-20)
  [OPEN: deferred — exposing index state, e.g. "initial state loading" or the last index time. Dave (2026-10-20): "probably useful in many places", e.g. the index page fills in over a few refreshes after a restart. "Not necessary for this, unless it's easy."]

#### Doc overview tool

> **Dave (2026-10-20):** "the idea here is to give the agent a way to inspect a file without spending the tokens to read the whole thing… this should make it easy for an agent looking for a specific topic to then go grab a useful chunk by line range."

- **The frontmatter.**
- **The outline: a nested tree of headings, each with its line number and anchor.** (Dave, 2026-10-20)
- **References to other docs.** (Dave, 2026-10-20)
- **The summary, if available.** (Dave, 2026-10-20)

### Round-tripping between the web UI and the agent

> **Dave (2026-10-20):** "one thing that's important is being able to round-trip between the webui and the agent. ideally, the agent could see what the user has open. maybe even what they have selected?"

- **Important:** the agent can see what the user has open in the web UI.
- **The browser side is part of the feature set**: the state push, the open-files tool, and the copy button. It doesn't have to be in before testing starts. (Dave, 2026-10-20)
- **Only for the local case**, for now. "The hosted version will need a different solution that we can figure out later." (Dave, 2026-10-20)
  [OPEN: deferred — the hosted version.]
- **Nice to have: a one-click copy-to-pasteboard button** that captures the current page in a way the agent can follow. (Dave, 2026-10-20)
  It copies the page's path relative to the wiki root. (Dave, 2026-10-20)
- **The client pushes its state to a new endpoint as it changes:** focus and selection, with other fields added over time. Not a live socket. (Dave, 2026-10-20)
- **The agent can see what the user has selected**, reported as the page's path plus a line range from the blocks' `data-line`. (Dave, 2026-10-20)
- **A tool returns all the open files, from most to least recent.** Recency comes from the client's pushed state, so it is the most recently focused or interacted with; when that's all there is, when the page was opened. (Dave, 2026-10-20)
- [OPEN: deferred — moving live updates from server-sent events to a full websocket, to carry richer data between client and server. Dave (2026-10-20): "a task for another day?"]

### Tool ideas

Offered by Dave, not yet chosen (2026-10-20):

- Follow or expand wikilinks.
- Search within a document, returning the sections that contain the search term.

## Performance — nothing slow in the page-load path

> **Dave (2026-10-19):** "Ideally we do all of the processing and indexing in the background so that there isn't something in the hot path for page load that makes it really slow."

- **Debounce filesystem events so a flood of changes produces one scan.** (Dave, 2026-10-19)
- **The server is available as soon as possible** and the index keeps filling in behind it. (Dave, 2026-10-19)

## Images and ordinary links

Images and ordinary markdown links to other files should work. (Dave, 2026-10-19: "the images and links, we probably do want to make those work.")

- **Only images** are served, for now. (Dave, 2026-10-19)
- Ordinary links follow normal link rules: `/other.md` is absolute (from the site root), `../other.md` is relative. (Dave, 2026-10-19: "probably? maybe follow normal link rules?")

- **Image embeds use the same rules:** relative by default, absolute with a leading `/`. (Dave, 2026-10-19)
- Other resolution behaviour (e.g. a host's dedicated images folder) comes later through a host-replaceable resolver (see Extensibility), "when we go to do integrations."
- **Resolution never leaves the root folder.** No `../` escape out of the hosted content. (Dave, 2026-10-19)

## Not now

Explicitly deferred — not "never", just not now (Dave, 2026-10-19):

- **Following symlinks.** Symlinks are not followed. "It adds complexity that we don't need to address right now."
- **Accent normalisation in names.** "If there's a way to just normalize the accent out… that would probably be good but… not an issue I want to spend time on."

Accepted as-is (Dave, 2026-10-19): titles from the first heading of any level (titles are a content fix, not an implementation fix); internal error text in 500 responses; library dependencies; a user `_wiki` folder being shadowed; escaped `\#word` becoming a tag.

## Open, not yet discussed

[OPEN: rendering timing — on request vs. cached.]
[OPEN: read-only, or any editing.]

## Origins (pointers, not decisions)

- `parse-obsidian` (Dave's crate, comrak-based) already parses Obsidian markdown.
