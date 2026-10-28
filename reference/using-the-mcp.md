# Using the MCP

The wiki is a folder of markdown files. Its MCP server lets you find documents, read the part of one you need, link the user to a page, and see what the user has open in the web view.

## Paths

Every path in results is relative to the wiki root. The server's instructions give the root's absolute path when you connect, so you can also read and edit the files directly. A document's `.md` is optional in arguments.

The reference you are reading lives at `_wiki/reference`. It is built into agent_wiki and has no files on disk, so read it with `read_document`. It is left out of every search unless the search's `folder` is in `_wiki/reference`.

## Finding documents

`search` takes a `query` and an optional `folder`. Results are ranked: title matches, then tag matches, then body matches. Each result is the document's path, title and tags, then its summary.

`read_document` on a folder (`""` for the root) gives its index: its subfolders, then each document with its title, tags and summary.

## Reading a document

1. `overview` gives a document's title, its size in lines, its summary and frontmatter, its outline, and where its wikilinks lead. Each heading in the outline has the line it starts on and its anchor.
2. `read_document` then reads what you need:
   - with just `path`, the whole document;
   - with `lines`, a range: `12-40`, `L12-L40`, `12-` or `L12-end` (to the end);
   - with `sections`, the sections under the anchors you list: each heading and everything up to the next heading of the same or a higher level.

   `lines` and `sections` can't be given together.

## Working with the user

- `open_files` lists the documents the user has open in the web view, most recent first. It marks the one with focus, and gives the user's selection as a line range such as `selected L12-L18`. A selection can be passed to `read_document`'s `lines` as written.
- `page_url` gives a document's URL in the web view, optionally to a line or a heading, so you can hand the user a link.
- Every document page has a **Copy path** button, so the user may paste you a path relative to the wiki root.

## Writing documents

The tools only read. Edit the files on disk; the wiki notices the change and updates any open page.

agent_wiki reads GitHub-flavoured markdown plus Obsidian syntax: `[[wikilinks]]`, `![[image.png]]` image embeds, `#tags`, callouts (`> [!note] Title`), YAML frontmatter, and single newlines as line breaks. A wikilink matches a file name loosely: case doesn't matter, `-` and `_` match spaces, and a leading date is ignored. `[[Page#Heading]]` links to a heading. Frontmatter `title`, `description` or `summary`, and `tags` are used for the page's title, summary and tags.
