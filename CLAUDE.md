# agent_wiki

Host a folder of markdown files as a website. An open-source Rust/axum library plus a thin binary, built to be embedded in other applications that wrap it in their own auth and tooling.

## Start here

1. **Read `docs/process.md`.** Markers (`[OPEN]`, `[PROVISIONAL]`; unmarked means decided) and what "decided" means.
2. **The design doc is `docs/design/agent_wiki.md`.** It is the source of truth for what agent_wiki should do.
3. **Load the `define` skill for any design work.**
4. **Plans and journals are in `docs/plans/`.** The journal is the resume entry point.

## Build & test

```bash
cargo build
cargo test
cargo clippy
cargo run -- <folder>   # serve a folder of markdown
```
