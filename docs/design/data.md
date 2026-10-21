# agent_wiki — Data

The data model layer: the document set as data, the index, and host entity types. Part of the design; the core is `agent_wiki.md`. Unmarked prose is decided; anything not decided carries `[OPEN: …]` or `[PROVISIONAL: …]` (see `docs/process.md`).

## The data layer

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
- **No raw SQL access, for now.** (Dave, 2026-10-19; reasoning 2026-10-21) The spirit: the db schema shouldn't become an API surface to maintain, and hosts shouldn't have to write raw SQL to accomplish most tasks. But "this is an engine to let devs build arbitrary applications that are roughly shaped like 'a pile of markdown files and an agent', so we don't need to be limiting", and "in the spirit of progressively more control for more complexity (api layering), allowing applications to directly use the index db for their own purposes seems reasonable". The index is purely disposable: "I don't want vibe coded application features to creep in and then have users get mad when a lib update blows away their data."
  [OPEN: deferred — direct access to the index db, as a lower layer.]

**Folders and documents.** (Dave, 2026-10-21)

- **The wiki is a tree of folders and documents.** The root is a **Folder**; it contains documents and folders. A folder renders as an index page.
- **The API starts small:** `root()`, `lookup(path) -> enum { Folder, Document }`, a walker, and `search(query, opts) -> Vec<Document>`.
- **`search` takes a `limit`.** Pagination is deferred.
- **A Document parses its file when it is loaded.** "It seems like parsing is fast enough that we can just parse files on load and worry about that later." (Dave, 2026-10-21)
- **Data is generated on demand where it can be.** Expensive data is behind accessor functions, so it can be optimized under the hood. Data that should always be live can be optimized later, e.g. materialized from the db. "I'm also skeptical this is even a concern unless there are thousands of results."
- "There are probably some other things worth exposing to the API like the backlinks data, the outline."
- [PROVISIONAL: `Wiki::root()`; `Wiki::lookup(path)`, where a document's `.md` is optional, and which is `None` when there is nothing at the path, when a segment is empty, hidden or `..`, or when the path reaches through a symlink; `Wiki::search(query, SearchOpts { limit })`, the root folder's search. A `Folder` has `path()`, `name()`, `url()`, `folders()`, `documents()`, `recent(n)`, `tags(n)` (the tag cloud), `search(query, opts)` and `walk()`, which loads every document below it, one at a time, by path.]
- [PROVISIONAL: a `Document` has `path()`, `url()`, `folder()`, `title()`, `frontmatter()`, `tags()`, `summary()`, `edited()`, `outline()` and `tree()`, the read-only parsed tree (`DocumentTree`, walked with `NodeRef` and `KindRef`). Its edit time and tags come from the index. Resolving its wikilinks for drawing (one index lookup per link) is behind `body().await`, which returns the element that draws its content.]
- [PROVISIONAL: `Wiki::tagged(tag)` and `Wiki::link_target(raw)` (the documents a wikilink matches, and those linking to it), so the tag and disambiguation page bodies are built from public data too.]

**Watching for changes.** Since agent_wiki already watches the files and processes what changed, it can offer hosts a rich API for this. "We don't have to solve that now, but it's a good call out for something that we should build support for." (Dave, 2026-10-19)

- **Folders and documents can be watched.** (Dave, 2026-10-21) The indexer already knows when they change.
- **A watch answers two questions:** "has this changed since I looked?" and "wait for a change". A watcher doesn't need every signal, so it can't fall behind. "A watcher might be a better fit" than a broadcast channel (Dave, 2026-10-21), i.e. `tokio::sync::watch`.
- **A document's watch says whether it was modified or removed.** (Dave, 2026-10-21)
- **A folder's watch covers its whole subtree**, and its value is a change counter: it says that something under the folder changed, not what. (Dave, 2026-10-21)
- [PROVISIONAL: `Document::watch()` returns a `tokio::sync::watch::Receiver<Option<DocumentChange>>` (`Modified` or `Removed`, `None` until a change); `Folder::watch()` a `Receiver<u64>`.]

## Host entity types in the index

> **Dave (2026-10-21):** "how to let the host application integrate their data types into the other parts of the system. for example, pre-index all the linear ticket references in each file and include that as a section on an index page. or build a dashboard that shows all your `@mentions` by recency… it would be cool if we could make it easy to participate… could we do something like, a derive macro that let's you mark up how your type gets indexed. each entity type gets its own table. the table schema is derived from the code. if the code changes, the table gets blown away and rebuilt. we could mark fields for index types (full text, semantic, etc)."

- **A derive defines an entity type.** Whether it reuses a host element struct or is a new struct is up to the host. (Dave, 2026-10-21)
- **Each entity type gets its own table**, with its schema derived from the code.
- **Records are collected by extraction:** a single method on `Host` that produces records from a document. (Dave, 2026-10-21) Parse hooks stay pure; extraction is the post-processing walk of the built tree.
- **Entity types are registered with a registration method**, separately from extraction. (Dave, 2026-10-21)
- **Every row gets a unique id and its source info.** (Dave, 2026-10-21)
  [PROVISIONAL: the source info is the page's id plus the record's ordinal in the document. Nothing is denormalized; page fields such as `folder` and `edited` come from a join.]
- **Recency is per document:** a record is as recent as its page's edit time. (Dave, 2026-10-21: "document level granularity is probably fine.")
- **Fields are marked with index kinds** (Dave, 2026-10-21):
  - **Exact and b-tree.**
  - **Full text.** One FTS table per entity type is fine. Full-text search over host records stands on its own, e.g. Obsidian-style tasks searched apart from documents. Host elements' text already reaches the document's full-text index through `Element::text`.
  - **Semantic: deferred**, but clearly wanted when semantic search is built. "Would be super cool to be able to just annotate a column and have it work. Easy with a default embedder. Probably possible to specify in attribute."
  [OPEN: deferred — mixed search across documents and entity types.]
- **Built-in indexed data (wikilinks, tags) uses the same mechanism where feasible.** (Dave, 2026-10-21)
- [PROVISIONAL: the derive is `#[derive(EntityType)]`, implementing the `EntityType` trait; both were named `Entity` until the query result took that name. On the struct, `#[entity(name = "…", version = N)]`: the name defaults to the struct's name in snake case, the version to 0. On a field, `#[entity(index)]` and `#[entity(fulltext)]`. Field types: `String`, integers, `f32`, `f64`, `bool`, and `Option` of those.]
- [PROVISIONAL: the registration method is `WikiBuilder::entity::<T>()`. The extraction method is `Host::extract(&self, path, doc, out)`, which pushes records with `out.push(record)`; one of a type that isn't registered is logged and dropped. `build` fails for an invalid name, a field named `id`, `page` or `ordinal`, a full-text field that isn't text, a type registered twice, or a built-in type's name.]
- [PROVISIONAL: a type's tables: `entity_{name}`, with `id`, `page` (the page's id; a record is deleted with its page), `ordinal`, then the fields; a b-tree index per `index` field; and an external-content FTS5 table over its `fulltext` fields.]
- [PROVISIONAL: the built-in types are `Wikilink { key }` and `Tag { tag, first }`, always registered, and usable in a host's queries. There is a `Wikilink` record for every wikilink the page counts, so its ordinal is the link's `wl-{n}` on the page; one with no target has the key `""`. There is a `Tag` record per distinct lowercase tag; `first` is the ordinal of its first inline use, the page's `tag-{n}`, or `None` when the tag is only in the frontmatter.]

### Querying entity types

**Record queries come first; the rest is deferred.** (Dave, 2026-10-21) A record query returns `Vec<Entity<E>>`: "the outer type holds all the stuff we'd get through joins, like the document reference, dates, etc. `E` would be the host's entity type." The fixed result shape keeps it simple, and covers the most common use case.

- **`Entity<E>` carries** the record's id and ordinal, and its page's path, URL, title, folder and edit time.
- **Scopes:** `under(folder)`, and one document.
- **Filtering joins:** records whose page has a matching record of another type, e.g. "find me all the tickets on pages where I'm mentioned".
- **Ordering:** by the page's edit time, its path, document order, or a field.
- **Limit and offset**; no more pagination than that. "Index based just needs limit and offset. Cursor based is just limit and a query param."
- **Filters are our own functions**, e.g. `eq("key", v)`, taking field names as strings. (Dave, 2026-10-21) Built on sea-query internally, which stays out of the public API. "Query building is almost always immediately more complex than you expect. If we can get a lot for free with a dep, we should use it."
- **The filters are every field-against-value condition sea-query has**, combined with `and`, `or` and `not`, plus full-text `matches` on full-text fields. (Dave, 2026-10-21) That is `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `is_in`, `is_not_in`, `between`, `not_between`, `like`, `not_like`, `glob`, `is_null` and `is_not_null`. Not included: expressions (arithmetic, casts, functions), custom SQL fragments, and comparing two fields.
- [PROVISIONAL: `wiki.query::<E>()`, then `filter(f)` (several must all match), `under(folder)`, `document(path)`, `on_pages_with::<T>(f)`, `order_by(Sort, Direction)` (`Sort::{Edited, Path, Ordinal, Field(name)}`; several apply in order), `limit(n)`, `offset(n)`, and `fetch().await`. Without an ordering, results are by page path, then ordinal; ties go by record id. The filter functions are in `agent_wiki::filter`; a string passed as a value is always a value, never a field. An unknown field, `matches` on a field that isn't full-text, or a type that isn't registered, is an error from `fetch`.]
- [PROVISIONAL: `Entity<E>` has `id()`, `ordinal()`, `path()`, `url()`, `title()`, `folder()`, `edited()`, `record()` and `into_record()`; no `document()`, since `wiki.lookup(entity.path())` loads the page.]
- **Typed field names are deferred.** The derive could generate typed constants per field, accepted where a string is; being optional, they can be added later without breaking anything. (Dave, 2026-10-21)
- **The built-in wikilinks and tags become entity types**, and their reads that fit record queries use them: a tag's pages, a link's referring pages, a document's tags. The tag cloud and search's tag matches stay internal SQL. (Dave, 2026-10-21)
- **Aggregates need a different API**, since they return differently structured data. Deferred, with full query access. "It's probably also worth offering full query access, especially for custom types. The challenge here is really that it makes the db structure part of the public API, which is limiting." (Dave, 2026-10-21)
  [OPEN: deferred — aggregates, queries returning several types, full query access, and whether hosts share the db handle.]

## Reindexing

> **Dave (2026-10-21):** "'user deletes index file' is not a permanently tenable position. that was basically a defer. i think we do need to figure out a way to trigger a full or partial reindex. it's way simpler than building migrations."

- **Schemas are versioned.** When a schema's version changes, its tables are blown away and reindexed.
- **The version is a hash of the schema plus a version number.** The hash is computed mechanically from the `CREATE TABLE` string or equivalent; the version number covers changes the schema doesn't show, such as a changed extraction rule. (Dave, 2026-10-21: "hash plus version.")
- **A reindex API**, and a **`--reindex` flag** on the binary. (Dave, 2026-10-21)
- [PROVISIONAL: the built-in schema's version, stored as `PRAGMA user_version`, is an FNV-1a hash of the schema and `INDEX_VERSION`. Each entity type's version is a hash of its `CREATE` statements and its `version`, stored per type. A type whose version changed gets new tables, and the next scan extracts it from every file, including files it otherwise skips. A type no longer registered loses its tables.]
- [PROVISIONAL: `Wiki::reindex()` drops everything and rescans. `--reindex` runs it in the background at startup, so the server comes up at once.]

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
