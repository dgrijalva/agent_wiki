# Onboarding

How to install agent_wiki and set it up for a user, who has probably asked you to "help me install and set up agent_wiki for my …". Do what you can yourself, and ask where a choice is theirs.

## 1. Find out the use case

What the wiki is for, which documents it serves, and which agent will use it.

## 2. The binary or a custom application

**Recommend starting with the binary.** It needs no decisions to start using, it's enough for a lot of use cases, and it's a good demo for anyone considering building more.

Build a custom application when the user already has a specific application in mind, or is using the binary and wants to customize it. Wrapping agent_wiki is a few lines of Rust, and the code is the configuration: see [Building your own application](building-an-application.md).

## 3. Install

Requires a Rust toolchain and `git`.

```sh
cargo install --git https://github.com/dgrijalva/agent_wiki agent_wiki
```

## 4. Pick or create the folder

The wiki serves one folder of markdown, subfolders included; hidden entries are skipped and symlinks aren't followed. If the folder is a git repository, edit dates come from its history.

```sh
agent_wiki <folder> [--port N] [--index <path>] [--reindex]
```

It serves on `127.0.0.1`, port 3000 by default. The index is in memory and rebuilt at every start unless `--index <path>` keeps it in a database file; `--reindex` rebuilds that file at startup. Let the user choose.

## 5. Connect the MCP

The endpoint is `http://127.0.0.1:<port>/_wiki/mcp`, streamable HTTP, without sessions. It only accepts requests addressed to `localhost`, `127.0.0.1` or `::1`.

## 6. Check it works

The site lists the folder's pages; the MCP's tools answer; a page open in the browser shows up in `open_files`. Then read [Using the MCP](using-the-mcp.md).

## 7. Offer to keep it running

The site and the MCP are only up while the process runs. Offer to keep it running. If the user opts in and has a custom application, make sure they have an easy way to restart it.
