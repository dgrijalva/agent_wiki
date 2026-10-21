# Plan: API layers

Design: `docs/design/agent_wiki.md` → Host API (Layers for drawing pages, The data layer, Host entity types, Reindexing) and Live updates. This plan holds the choices the design leaves to implementation; each is `[PROVISIONAL]` until Dave reviews it.

Branch: `dg/api-layers`. Journal: `docs/plans/api-layers-journal.md`.

## Scope

In: every design item from the 2026-10-21 session except what the design defers (querying entity types, mixed search, semantic indexing, the query-string part of the URL key, custom pages in `open_files`).

## Steps

### 1. `Element::draw(&self, cx)`

- `draw(&self, cx: &DrawCtx<'_, P>) -> Markup`; `cx.draw_children()` returns `DrawChildren` (still lazy, still `Render`). `DrawCtx` holds the node being drawn, or none.
- [PROVISIONAL: `BodyCtx::draw(&impl Element<P>) -> Markup` draws an element that isn't in a tree (no children), with the page's base path. That is how a template or custom page places page-level elements.]

### 2. Data layer: Folder and Document

- `Wiki::root() -> Folder<P>`, `Wiki::lookup(path) -> Result<Option<Entry<P>>>`, `enum Entry<P> { Folder(Folder<P>), Document(Document<P>) }`.
- `Document` parses its file when loaded. [PROVISIONAL: accessors: `path()`, `url()`, `title()`, `frontmatter()`, `tags()`, `summary()`, `edited()`, `outline()`. `edited` and `tags` come from the index, the rest from the parse.]
- [PROVISIONAL: resolving wikilinks for drawing is the expensive part (one index query per link), so it is behind an accessor: `doc.body().await -> DocumentBody`, which is the `Element` that draws the document's content. A listing that loads many documents doesn't pay for it.]
- [PROVISIONAL: `Folder`: `path()`, `name()`, `folders()`, `documents()`, `recent(n)`, `tags(n)` (the tag cloud), `search(query, SearchOpts { limit })`, `walk()`. `Wiki::search` is `root().search`. `walk()` is a stream of every document in the subtree, loaded one at a time.]
- [PROVISIONAL, added while building: `Wiki::tagged(tag)` and `Wiki::link_target(raw)`, so the tag and disambiguation bodies are built from public data too; `Wiki::draw(&element)` draws an element outside any page. `Entry::Document` is boxed.]
- Read-only tree access for host extraction (design sketch: `NodeRef` with `kind`, `children`, `parent`, `next_sibling`), plus `Document::nodes()` in document order.

### 3. Watches

- `Document::watch()` and `Folder::watch()` return `tokio::sync::watch` receivers. A document's value is its last change (`Modified` / `Removed`); a folder's is a counter bumped for any change in its subtree.
- [PROVISIONAL: the wiki keeps a sender per watched path, made on first `watch()`. After each watcher batch it sends to each touched document's sender and to the senders of all its ancestor folders. Senders with no receivers are dropped then.]

### 4. Live updates by URL, and custom pages

- A registry of `URL path → watch::Sender<()>`. A live-update stream connecting subscribes (creating the sender if needed); the entry is removed when its last stream disconnects.
- `Wiki::signal(path)` signals every open instance of that page. The watcher signals `{base}{url}` of each page it reindexes or removes. The broadcast channel goes away.
- [PROVISIONAL: the stream is `{base}/_wiki/events?url={path}&tab={id}`. A document page's stream also registers its tab for `open_files`, as today; a custom page's doesn't (the design leaves that open).]
- Custom page entrypoint: [PROVISIONAL: `wiki.custom_page(&request, title, live).draw(|ctx| …).await -> Markup`, drawn through the host's template like any page, `PageKind::Custom`; empty unless the closure adds content. With `live`, the page's stream is keyed by `request.uri.path()`.]

### 5. Components and page bodies

- Built-in elements: `SearchBox`, `Breadcrumbs`, `TagLinks`, `CopyPath`, `FrontmatterTable`, `RecentlyEdited`, `Contents`, `TagCloud`, `DocumentBody`. The `BodyCtx` chrome methods draw them.
- Page bodies (folder index, search, tag, disambiguation, not found) are built only from those elements and the data layer.
- [PROVISIONAL: a host replaces a page body in its template's `body`: `ctx.kind()` carries what the page is (e.g. `FolderIndex { path }`), `ctx.wiki()` gives the data layer, and the template draws its own content instead of calling `content(ctx)`. No new hook.]

### 6. Reindexing

- [PROVISIONAL: the built-in schema's version is a hash of `SCHEMA` plus `INDEX_VERSION`, stored in `meta`. FNV-1a, so it is stable across Rust versions.]
- `Wiki::reindex()`: rebuild the tables and rescan. `--reindex` on the binary calls it at startup.

### 7. Host entity types

- New crate `agent_wiki_derive` (workspace member), re-exported: `#[derive(Entity)]`.
- [PROVISIONAL: attributes: `#[entity(name = "…", version = N)]` on the struct; `#[entity(index)]` (b-tree, which also serves exact lookups) and `#[entity(fulltext)]` on fields. Field types: `String`, integers, `f64`, `bool`, and `Option` of those.]
- `WikiBuilder::entity::<T>()` registers a type. `Host::extract(&self, doc, out)` (default: nothing) pushes records; a record of an unregistered type is an error, logged.
- [PROVISIONAL: table `entity_{name}`: `id INTEGER PRIMARY KEY, page INTEGER NOT NULL, ordinal INTEGER NOT NULL`, then the fields; a b-tree index per `index` field; an external-content FTS5 table over the `fulltext` fields. Rows are replaced with their page's.]
- Each type's version is a hash of its `CREATE` statements plus its `version`, stored per type. On open, a type whose stored version differs has its tables dropped and recreated, and the startup scan re-extracts that type from every file, including files it otherwise skips.
- [PROVISIONAL: the built-in `wikilink` and `tag` tables stay as they are for now. Moving them onto this mechanism means rewriting the queries that read them, and the query side is deferred.]

## Not in this branch

Querying entity types; mixed search; semantic indexing; the query string in the URL key; custom pages in `open_files`; backlinks as a `Document` accessor (no use case yet).

## Record queries (added 2026-10-21)

Design: `docs/design/agent_wiki.md` → Host entity types → Querying entity types.

### 8. Record queries

- Dependency: `sea-query = { version = "1.0.2", default-features = false, features = ["backend-sqlite", "thread-safe"] }`, private.
- [PROVISIONAL: `wiki.query::<E>()` returns a `Query<P, E>`: `filter(f)` (repeated calls AND), `under(folder)`, `document(path)`, `on_pages_with::<T>(f)`, `order_by(Sort, Direction)` (repeated calls append; `Sort::{Edited, Path, Ordinal, Field(name)}`), `limit(n)`, `offset(n)`, then `fetch().await -> Result<Vec<Entity<E>>>`. With no `order_by`: page path, then ordinal.]
- [PROVISIONAL: filters are free functions in `agent_wiki::filter` (`eq(field, value)` … `matches(field, query)`), combined with `Filter::and`/`or`/`not`. A `Filter` is our own tree, compiled to sea-query against the queried type's table when the query runs, so one filter works for `on_pages_with` too. An unknown field, `matches` on a field that isn't full-text, or querying an unregistered type, is an error from `fetch`. Values are `impl Into<FieldValue>`.]
- [PROVISIONAL: `Entity<E>` has getters `id()`, `ordinal()`, `path()`, `url()`, `title()`, `folder()`, `edited()`, `record()`, and `into_record()`. No `document()`: `wiki.lookup(entity.path())` loads it.]
- The derive also generates decoding a row into `E` (`Entity::from_values`, `EntityField::from_value`).

### 9. Built-ins on the mechanism

- [PROVISIONAL: public entity types `Wikilink { key }` and `Tag { tag, first: Option<i64> }`, always registered first; hosts can use them in `on_pages_with`. A `Wikilink` record is pushed for every counted wikilink, so its record ordinal is the drawn `wl-{n}` (key `""` for one with no target). A `Tag` record is one per distinct tag on the page; `first` is the first inline occurrence's ordinal, drawn as `tag-{n}`, or `None` when it is only in frontmatter.]
- The `wikilink` and `tag` tables and their remove statements go; records cascade with their page.
- `Wiki::tagged`, `Wiki::link_target`'s references, and a document's tags use record queries. The tag cloud, search's tag tier, and `Index::details` stay internal SQL, now against `entity_tag`.
