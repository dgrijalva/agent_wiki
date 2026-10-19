# How this repo works

Single-developer project.

## Layout

| Path | Holds |
|---|---|
| `docs/design/agent_wiki.md` | The design doc: the source of truth for what agent_wiki is supposed to be. |
| `docs/plans/` | Plans and journals. A plan is trash once the plan is done. |
| `tools/pending/` | Lists every marker in the docs. |

## Markers

Bracketed, in prose. **Unmarked prose is decided.**

- `[OPEN: …]` — needs a decision; the gap is known.
- `[PROVISIONAL: …]` — a choice an agent made to keep work moving, not yet reviewed. Not authoritative.

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
