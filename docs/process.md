# How this repo works

Single-developer project.

## Layout

| Path | Holds |
|---|---|
| `docs/design/` | The design: the source of truth for what agent_wiki is supposed to be. `agent_wiki.md` is the core; `data.md` is the data model layer; `mcp.md` is the MCP server. |
| `docs/plans/` | Plans and journals. A plan is trash once the plan is done. |
| `tools/pending/` | Lists every marker in the docs. |

## Design files

One file per area. The layout is expected to evolve toward (Dave, 2026-10-20):

- core (main concept, high level overview)
- web UI
- MCP
- data model layer
- integration guide

## Markers

Bracketed, in prose. **Unmarked prose is decided.**

- `[OPEN: …]` — needs a decision; the gap is known.
- `[PROVISIONAL: …]` — a choice an agent made to keep work moving, not yet reviewed. Not authoritative.

A marker brackets only the undecided claim, and closes on the line it opens. Anything else stays outside the brackets: a code block or example illustrating the claim follows the closing `]`, and so does context such as how things work today. A `]` after a code fence breaks the fence.

`cargo run -q -p pending -- [path] [--section X]` lists them; `--lint` finds unbracketed text that reads as pending.

## Decided means non-silent-override, not immutable

Decisions change all the time. Implementors must not violate them; they raise the issue instead.

- **Good** — "this constraint forces a messy implementation. Suggest changing x to y. That would allow ___."
- **Bad** — "decisions x and y conflicted. Replaced x with ___."
- **Unacceptable** — overriding silently.

When two decided things conflict, the agent does not pick a winner.

## Agent-made choices

Anything no person decided is either marked `[PROVISIONAL]` where it sits, or kept out of the design doc (in the plan or journal). Never unmarked in the design doc.

## Standing truth

A document states current truth. When something changes, merge it into the doc that owns it; history comes from git. Commits carry the rationale.
