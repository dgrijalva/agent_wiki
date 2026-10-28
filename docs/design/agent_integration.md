# agent_wiki — Agent integration

How agents learn to use agent_wiki: the reference documents, the MCP tool that serves them, and onboarding. Part of the design; the core is `agent_wiki.md`. Unmarked prose is decided; anything not decided carries `[OPEN: …]` or `[PROVISIONAL: …]` (see `docs/process.md`).

> **Dave (2026-10-27):** "i want to be able to tell people: 'paste this into your agent: help me install and setup <github url> for my <user use case>' and have it get set up and customized for them. i want the whole thing to be self contained in the project. the agent should be able to read the README on the github and do/walk the user through setup. it should figure out if the user needs a custom application or just the binary and set it up for them or walk them through it."

## Shape

- **Pieces:** help for agents using the MCP, a guide to building your own agent_wiki application, and onboarding (Dave, 2026-10-27); and a guide to using the site (Dave, 2026-10-28).
- **Self-contained in the project.** (Dave, 2026-10-27)
- **The entry point is the README on GitHub.** An agent given the repo URL reads it, and sets things up or walks the user through it. (Dave, 2026-10-27)
- **Everything is reference documents.** "maybe we just eat our own dogfood and provide the reference all just as documents, baked in at build time, in a special path" (Dave, 2026-10-27)
  - **The content is ordinary agent_wiki content, in `reference/` at the repo root.** (Dave, 2026-10-27)
  - **Baked in at build time**, "probably via a small macro" (Dave, 2026-10-27).
    [PROVISIONAL: the macro is `embed_folder!`, a function-like proc macro in the derive crate, which reads every `.md` file under the folder with `include_str!`.] Editing a file rebuilds the crate; a newly added file is only picked up when the crate next rebuilds for another reason.
  - **Hosted virtually at `{base}/_wiki/reference`, as if they were documents in the wiki.** (Dave, 2026-10-27)
  - **Reference documents are excluded everywhere except a search within `_wiki/reference`.** (Dave, 2026-10-27/28)
  - **Served as a wiki of their own, with a separate in-memory index**, built at startup, rather than in the wiki's index. (Dave, 2026-10-28)
  - [PROVISIONAL: the reference wiki draws with the `DefaultTemplate` and the host's stylesheet, not the host's template. Its pages have their own search box, which searches only the reference.]
  - [PROVISIONAL: its documents' edited time is when the process started, since the build's time isn't known.]
  - [PROVISIONAL: reference pages don't count as open for `open_files`.]
  - [OPEN: deferred — a host adding its own reference content. Dave (2026-10-27): "great idea. defer until we have the built in stuff working."]
- **No per-agent skill formats.** Writing the reference out as skills for each agent is a rabbit hole, "since there are so many different agents out there. i don't want to figure that out. the agents can on their side." (Dave, 2026-10-27)
- [OPEN: deferred — how the CLI exposes the reference. "build them into probably both the cli and mcp as reference" (Dave, 2026-10-27).] The CLI today is `agent_wiki <folder>` with no subcommands.

## MCP

- **A minimal instruction set on connect.** It tells the agent the reference is available at its path. (Dave, 2026-10-27)
- **The reference's index page is the agent's entry point.** (Dave, 2026-10-27) It is a custom index page: a prompt, in addition to the actual index. (Dave, 2026-10-28)
  [PROVISIONAL: the prompt is `reference/README.md`, drawn above the index. It is not a document of the reference, so GitHub shows the same prompt above the folder's files.]
- [PROVISIONAL: the instructions on connect give the wiki root, say paths are relative to it, and say the reference explains the wiki and its tools, read with `read_document` at `_wiki/reference`.] The usage notes they carried moved to the reference.
- **No separate reference tool.** The agent reads reference documents with the `read_document` tool, like any other document (see `mcp.md`). (Dave, 2026-10-27)

## Reference documents

- **Written for an agent as capable as the one writing them.** "anyone using opus or equivalent will get an agent that knows roughly what you know." The reference doesn't spell out what the agent already knows, such as how to run a service: "the agent on the other side will know what tools available and be way more familiar with the user's environment." (Dave, 2026-10-28)

### Using the MCP

What an agent needs to work with the wiki through the MCP tools (see `mcp.md`).

### Using the site

What the user sees in the web view, so the agent can help the user find the site and answer questions about how to use it. (Dave, 2026-10-28)

### Building your own application

A guide for a programmer, or their agent, building a host application on the library. It maps each need to the layer that serves it: styles, template, host syntax and elements, entity types, custom pages, tools. (Agreed, 2026-10-27)

- **A basic wrapper application is trivial to construct**, so the standalone binary doesn't have to implement things like skinning. (Dave, 2026-10-27)
  > **Dave (2026-10-27):** "the point of this project is to share documents with an agent. agents should be able to just make what the user asks for. code is the new config file."

### Onboarding

The onboarding guide is a reference document. (Dave, 2026-10-27) Before anything is installed, the agent reaches it from the README.

- **The README lists the requirements**; the agent handles the rest. (Dave, 2026-10-27)
  > **Dave (2026-10-27):** "i know the install path is not super accessible right now. i don't care. that's a later thing. the first set of consumers are my team, the next audience will be some programmer friends."
- **The flow** (agreed, 2026-10-27):
  1. Find out the use case.
  2. Decide whether it needs the binary or a custom application.
  3. Install.
  4. Pick or create the folder.
  5. Connect the MCP to the user's agent.
  6. Check it works.
  7. **Help the user access it.** Ideally the agent helps the user find the site and answers questions about how to use it. (Dave, 2026-10-28) [PROVISIONAL: placed after checking it works and before keeping it running.]
  8. **Offer to keep it running.** If the user opts in and has a custom application, make sure they have an easy way to restart it. (Dave, 2026-10-27)
- **For the binary, the startup choice for now is the index on disk or in memory.** (Dave, 2026-10-27)
- **Recommend starting with the binary**, "since they don't have to make any decisions to start using it. the binary is enough for a lot of use cases and it's a great demo people considering building more." (Dave, 2026-10-27)
- **A custom application is for when the user already has a specific application in mind, or is using the binary and wants to customize it.** (Dave, 2026-10-27)
- [OPEN: deferred until hosted mode — use cases that need a hosted wiki, which the MCP doesn't serve yet (see `mcp.md`, hosted version).]

## Later

- **The binary will evolve to have config.** "config with a UI could be a cool feature to offer, especially if a host app could add their own content to it. i have some ideas for that." (Dave, 2026-10-27)
- **A config file for basic settings**, such as ignore paths, the embeddings model, and logging preferences. "these things are cheap to make configurable and they aren't customizing so much as just basic config." Customizing, such as skinning, is done with a wrapper application (see Building your own application). (Dave, 2026-10-27)
