# agent_wiki

Host a folder of markdown files as a website. An open-source Rust/axum library plus a thin binary, built to be embedded in other applications that wrap it in their own auth and tooling.

## Start here

1. **Read `docs/process.md`.** Markers (`[OPEN]`, `[PROVISIONAL]`; unmarked means decided) and what "decided" means.
2. **The design is in `docs/design/`.** It is the source of truth for what agent_wiki should do. `agent_wiki.md` is the core; `data.md` is the data model layer; `mcp.md` is the MCP server; `agent_integration.md` is the reference documents and onboarding for agents.
3. **Load the `define` skill for any design work.**
4. **Plans and journals are in `docs/plans/`.** The journal is the resume entry point.

## Build & test

```bash
cargo build
cargo test
cargo clippy
cargo run -- <folder>   # serve a folder of markdown
```
