<p align="center">
  <img src="assets/octo.svg" width="96" alt="Octo logo">
</p>

<h1 align="center">Octo</h1>

<p align="center">
  <b>A local context indexer for AI coding agents.</b><br>
  Register your context once, curate it into indexes, and let agents load only what they need over MCP.
</p>

<p align="center">
  <b>English</b> · <a href="README.ko.md">한국어</a>
</p>

<p align="center">
  <img alt="Rust 2024" src="https://img.shields.io/badge/rust-2024_edition-orange">
  <img alt="Platforms" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-blue">
  <img alt="MCP" src="https://img.shields.io/badge/MCP-Streamable_HTTP-5b5bd6">
</p>

---

## Table of contents

- [Why Octo](#why-octo)
- [How it works](#how-it-works)
- [Features](#features)
- [Getting started](#getting-started)
- [Connecting an agent](#connecting-an-agent)
- [MCP tools](#mcp-tools)
- [Link cache](#link-cache)
- [Data and configuration](#data-and-configuration)
- [Security](#security)
- [Architecture](#architecture)
- [Contributing](#contributing)
- [License](#license)

## Why Octo

When you work with AI coding agents such as Claude Code, Cursor, or Codex, every new session starts from zero.
You end up re-explaining the same things: *the payment policy lives in this doc, the source is in that folder, the domain model is in neo4j.*

The usual workarounds each have a cost:

- **Pasting everything into the prompt** fills the context window before any real work starts.
- **Leaving things out** makes the agent guess, and the guesses are often wrong.
- **Copying docs into each repository** creates copies that quietly drift from the original.

Octo takes a different approach, built on three principles:

1. **Table of contents first.** Agents receive a compact index of titles and one-line summaries, then load full content only for the items they need.
2. **Follow the source.** Path contexts point at the original file, folder, or URL instead of storing a copy, so agents always see the current version.
3. **No stale context.** When content cannot be verified as current, Octo says so or refuses to serve it, rather than handing out outdated text.

## How it works

```text
 ┌──────────────── Octo (tray app) ────────────────┐
 │                                                 │
 │  Contexts             Indexes                   │        ┌──────────────┐
 │  ├─ Doc   Payment ──▶ backend ★ (default)       │  MCP   │ Claude Code  │
 │  ├─ Path  src/    ──▶  1. Payment policy        │◀──────▶│ Cursor       │
 │  ├─ Path  https://…    2. Source folder         │  HTTP  │ Codex …      │
 │  └─ Graph domain  ──▶ frontend                  │        └──────────────┘
 │                                                 │
 └──────────────────── ~/.octo ────────────────────┘
```

1. **Register contexts** in the app: write a doc, point at a path or URL, or save a Cypher query.
2. **Curate indexes**: pick the contexts relevant to a project and set their order.
3. **Connect once**: agents call `get_index` to read the table of contents, then `load_context` for the items they need.

## Features

### Three kinds of context

| Kind | What you register | What the agent receives |
|---|---|---|
| **Doc** | Markdown written in the app, or an imported `.md` file | The document body |
| **Path** | A file, a folder, or a web link | File content, a folder listing, or the page text |
| **Graph** | A neo4j connection plus a saved Cypher query | The query result, executed read-only |

- If you leave the one-line summary empty, Octo generates one from the source: the first line of a doc, the page title of a link, or node and relationship counts for a graph.
- When a source can't be read, the table of contents shows it as **Broken**, together with the reason.

### Indexes

- One context can be added to any number of indexes.
- A **default index** is what new sessions start with.
- An agent can switch indexes during a session with `use_index`. The switch applies only to that session.

### A single MCP endpoint

- Octo runs a local [Streamable HTTP](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports) MCP server at `http://127.0.0.1:47614/mcp`.
- For Claude Code, one click registers it at user scope, so it is available in every project.
- For other agents, you paste a JSON snippet into their MCP configuration.

### Link cache

- Pages that need a login, such as claude.ai artifacts, Google Docs, or Confluence, can't be fetched by Octo directly.
- When an agent manages to read one with its own tools, it can report back. Octo then remembers **how** the page was opened and **what** it contained, so later sessions don't have to work it out again. See [Link cache](#link-cache) for details.

### A resident desktop app

- Closing the window leaves Octo running in the system tray, so the MCP server stays available.
- Only one instance runs at a time. Launching Octo again brings the existing window to the front.
- Passwords are stored in the OS keychain: Windows Credential Manager or macOS Keychain.
- The interface is available in English and Korean.

## Getting started

### Prerequisites

| Requirement | Needed for |
|---|---|
| [Rust](https://rustup.rs) (stable, 2024 edition) | Building Octo |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI (`claude`) | *Optional.* One-click MCP registration |
| [neo4j](https://neo4j.com) with the HTTP API enabled | *Optional.* Graph contexts |

### Build and run

```bash
git clone <repository-url>
cd octopuser
cargo run --release
```

The first build takes a few minutes.

> **Window doesn't appear, or you get a graphics error?**
> Fall back to the software renderer:
> `ICED_BACKEND=tiny-skia cargo run --release`.
> On PowerShell, run `$env:ICED_BACKEND = "tiny-skia"` first.

### First steps

The app is a single screen that flows from left to right.

```text
① Contexts ──▶ ② Indexes ──▶ ③ Session
```

1. In **① Contexts**, click **+ Add** and register a doc, a path, or a graph query.
2. In **② Indexes**, create an index and add contexts to it. Click **★ Make default** to make it the default.
3. In **③ Session**, click **Connect in one click** for Claude Code, or copy the JSON for another agent.
4. Open a new agent session. The agent can now call `get_index`.

## Connecting an agent

**Claude Code**: click **Connect in one click** in the app. This is equivalent to running:

```bash
claude mcp add --transport http --scope user octo http://127.0.0.1:47614/mcp
```

**Other MCP clients**: add this to the client's MCP configuration:

```json
{
  "mcpServers": {
    "octo": { "type": "http", "url": "http://127.0.0.1:47614/mcp" }
  }
}
```

**Choosing the index.** A session resolves its index in this order:

1. The index chosen in that session with `use_index`
2. An `?index=<name>` query parameter on the URL. Use this to pin an index per project, for example `http://127.0.0.1:47614/mcp?index=backend`.
3. The default index set in the app

## MCP tools

| Tool | Arguments | Description |
|---|---|---|
| `get_index` | none | Table of contents for the current index: id, kind, title, one-line summary, and status. |
| `load_context` | `id` | Full content of one context. |
| `list_indexes` | none | Available indexes. Marks the current and default ones. |
| `use_index` | `name` | Switches the index for this session only. |
| `report_access` | `id`, `method`, `content?`, `version?` | Records how the agent opened a link Octo couldn't fetch, and optionally what it read. |

Here is an example of what an agent receives from `get_index`:

```text
# Context index: backend

1. [Doc] Payment policy — Refunds are processed within 3 business days
   id: 18d8cd425d413f44
2. [Path] iced ref — iced 첫 앱 만들기
   id: 18d8cdc016e7bedc
   path: https://claude.ai/artifact/…
   status: Cached · submitted by agent (claude-code) · 2h ago · version 1790398596-f070
```

## Link cache

The link cache stores two things separately, because they differ in how long they stay valid.

### 1. Access method: how a link can be opened

- An agent submits a method with `report_access`, for example *"Claude Code: call the Artifact tool with action `read` and this URL."* Octo stores it together with the name of the reporting client, because different clients have different tools.
- Octo looks for a method in this order:
  1. The method reported for this exact URL
  2. The method reported for another URL on the same domain
  3. A built-in rule. Built-in rules cover claude.ai, Google Docs, and Atlassian.
- Access methods don't expire.
- When a link can't be fetched, the error includes the method as a hint, so the next agent can go straight to the approach that worked.

### 2. Fetched result: what the link contained

| Origin | While fresh (< 1 day) | After 1 day |
|---|---|---|
| **Fetched by Octo** | Octo revalidates on every read with `ETag` / `Last-Modified`. A `304` response serves the cached copy and renews it. | Deleted. The page is fetched again. |
| **Submitted by an agent** | Served with its origin attached, for example `Cached · submitted by agent (claude-code) · 2h ago · version …`. The agent is also asked to check the version. | Deleted and **no longer served**. Only the access method remains. |

- **Stale content is never served.** Once a cached result is older than one day, Octo refuses to serve it, even if the source can't be reached.
- **Failed fetches are remembered for 10 minutes.** During that time Octo doesn't retry, so a broken link can't stall `get_index` with repeated timeouts.
- **Cleanup follows your edits.** When you delete a context or change its URL, and no other context uses the old URL, its cached result is discarded. Access methods are kept, because other links may still use them.
- **You can inspect and clear the cache.** Open a link context in the app to see its cache status, and use **Clear cache** or **Clear method**.

## Data and configuration

All data stays on your machine.

- The default location is `~/.octo`.
- Set the `OCTO_HOME` environment variable to use a different location.

```text
~/.octo/
├─ config.json            MCP port, default index, language
├─ contexts/<id>.json     Context definitions (doc bodies in <id>.md)
├─ indexes/<name>.json    Indexes and their order
├─ connections/           neo4j connection settings (passwords excluded)
└─ cache/
   ├─ access.json         Access methods, by URL and by domain
   └─ results/            Fetched link results (removed after 1 day)
```

- **The files are the source of truth.** The app writes every change straight to disk, and the MCP server reads the same files.
- **Changes need no restart.** Edits made in the app are visible to agents immediately.

## Security

- **Local only.** The MCP server binds to `127.0.0.1`.
- **Browser requests are restricted.** Requests that come from a browser must have a local `Origin`. This protects against DNS rebinding.
- **Graph queries are read-only.** Cypher runs in read-only transactions (`access-mode: READ`).
- **Secrets stay in the OS keychain.** neo4j passwords are kept out of the data directory. Octo writes them to a file there only if no keychain is available.
- **Agent-submitted content is treated as untrusted.** It is always labelled with its origin, expires after one day, and can be deleted from the app at any time.

## Architecture

```text
src/
├─ main.rs         Entry point: single-instance check → store → MCP server → GUI daemon
├─ mcp.rs          Streamable HTTP MCP server (/mcp) and session state
├─ core/           Context domain, shared by the UI and MCP
│  ├─ store.rs     File-backed store: contexts, indexes, connections, config
│  ├─ content.rs   Content loading and table-of-contents generation
│  ├─ web.rs       Link fetching, HTML-to-text, conditional requests
│  ├─ cache.rs     Link access-method and fetched-result caches
│  ├─ graph.rs     neo4j HTTP transaction API
│  └─ secret.rs    OS keychain password storage
├─ daemon/         Resident behaviour: single instance, tray, Claude Code registration
├─ ui/             iced GUI: state (app.rs), screens (view/), design (theme, widgets)
└─ i18n.rs         English / Korean strings
```

Octo is built with [iced](https://github.com/iced-rs/iced) 0.14 for the GUI, [tiny_http](https://github.com/tiny-http/tiny-http) for the MCP server, [ureq](https://github.com/algesten/ureq) for HTTP requests, [tray-icon](https://github.com/tauri-apps/tray-icon) for the system tray, and [keyring](https://github.com/hwchen/keyring-rs) for password storage.

## Contributing

Issues and pull requests are welcome.

```bash
cargo test     # unit tests
cargo clippy   # lints
```

A few conventions keep the codebase consistent:

- **Keep `core/` UI-agnostic.** Both the GUI and the MCP server call the same functions, so behaviour stays identical.
- **Write every user-facing string in both languages.** Use `t("…", "…")` for fixed strings and `tr!("…", "…", args)` for formatted ones.
- **Make error messages actionable.** They are shown to users and agents as-is, so each one should say what went wrong and what to do next.

## License

To be decided. Until a license file is added, all rights are reserved by the authors.
