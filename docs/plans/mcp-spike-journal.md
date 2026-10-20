# Journal: MCP spike

Plan: `docs/plans/mcp-spike.md`. Design: `docs/design/agent_wiki.md` → MCP.

## 2026-10-20

- Design session with Dave: MCP section added to the design doc (rmcp, exported tools, `tools()` default collection, tool structs adapted by `route`, `{base}/_wiki/mcp`, search and overview for the spike, round-tripping for the local case).
- rmcp checked at 3.6.0 (latest on crates.io): `ToolRouter<S>`, `ToolRoute::new_dyn`, stateless `AsyncTool`, `StreamableHttpService` as described in the plan. MCP protocol `2026-07-28` removes sessions (SEP-2567); rmcp serves that version statelessly.
- Plan written. Dave: "build as written. leave provisional".
- Worktree `dg/mcp-spike` created with the `worktree` CLI; the design doc edits and plan files moved over from the main checkout.
- Built steps 1–6. New: `src/mcp.rs` (`Tool` trait, `route`, `Wiki::tools`, `Search`, `Overview`, the endpoint), `Extracted::outline`, `Index::details`. `rmcp` is re-exported, like `maud` and `nom`. `INDEX_VERSION` not bumped: the outline isn't stored.
- The sketch compiled against rmcp 3.6.0 as designed: `ToolRoute::new_dyn` with a closure holding an `Arc` of the tool, generic over `S` with no bound.
- Tests: `tests/mcp.rs` (5) pass; full suite and clippy clean.
- Manual check with curl against the binary on `docs/`: both tools answer. A search sent right after startup returned "No matches." while the index was filling (accepted by the design).
- Not done: the manual test from a real agent (Claude Code), and the browser side (state push, open-files tool, copy button).
