# Using the site

What the user sees in the wiki's web view, so you can help them find it and answer their questions. The binary serves it at `http://127.0.0.1:<port>/` (3000 by default); an application may mount it under a base path. `page_url` gives the URL of any page.

## Pages

A document's URL is its path without `.md`: `notes/foo.md` is at `/notes/foo`. The file name works too: `/notes/foo.md` redirects there. Light and dark follow the system.

A document page has, above its content:

- **Breadcrumbs** up through its folders to the root.
- **Copy path**, which copies the document's path relative to the wiki root, for pasting to an agent.
- Its **tags**, and its **frontmatter** as a table, collapsed.
- A **table of contents**, collapsed, listing headings down to `####`.

Code blocks are highlighted, and `mermaid` blocks are drawn as diagrams.

Task list items (`- [ ] …`) show a checkbox: `[x]` checked, `[/]` in progress as a partly filled box, `[-]` canceled as muted and struck through, checkbox included. Any other character in the brackets shows as open.

## Folder index pages

Every folder, the root included, has an index page:

1. **Recently edited** documents in and under the folder, one line each, with the path and the edit date. Edit dates come from git for committed files, else the file's modification time.
2. **Contents:** the subfolders, then each document with its title, file name, summary, edit date and tags. The summary is frontmatter `description` or `summary`, else the first paragraph.
3. **Tags:** the top tags in and under the folder, with counts.

## Search

The search box at the top of every page searches titles, tags and body text, and ranks title matches first, then tag matches, then body matches.

## Links

- A `[[wikilink]]` matches a file name loosely: case doesn't matter, `-` and `_` match spaces, and a leading date is ignored. With exactly one match it goes to that page. With several, or none, it goes to a page listing the matches and the pages that use the same link. A folder in the link (`[[projects/Foo]]`) narrows the matches. `[[Page#Heading]]` goes to a heading.
- A `#tag` goes to the tag's page, listing every document with it.
- Every heading is linkable: hover it for its `#` link.
- `#:N` on a page's URL links to line N of its source, and highlights the block holding it.
- Ordinary links and images resolve relative to the page, or from the wiki root with a leading `/`, and never leave the root.
- `![[image.png]]` shows an image. When no file is at that path, it finds the image by file name anywhere in the wiki, matched as a wikilink is. `![[image.png|300]]` or `![[image.png|300x200]]` sets its size. One that matches no image, or several, shows as an unresolved link.

## Live updates

An open page updates in place when its file changes on disk, so the user can watch a document while you edit it. An open folder index page updates when any document in that folder, or in a folder below it, changes. An application can also make its own pages refresh.

## What the agent can see

While a page is open, the wiki tells the MCP which pages are open, which one has focus, and what the user has selected, as a line range: `open_files`. So the user can point at something on the page and ask you about it.
