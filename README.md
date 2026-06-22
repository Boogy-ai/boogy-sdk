# Boogy SDK

Ship whole services on [Boogy](https://boogy.ai) — frontend, API, and data —
from one Rust crate compiled to `wasm32-wasip2`. Each service gets a route
subtree, an isolated **relational, ACID** store, capability-based security,
in-process calls to other services, and REST / JSON-RPC / MCP surfaces — and a
single transaction can span a whole chain of services.

> **Status: early development.** APIs change without notice. Pin a git
> `rev` in your `Cargo.toml`. Published to crates.io once stable.

## How it fits together

**[`ARCHITECTURE.md`](ARCHITECTURE.md)** is the whole picture — the runtime, the
capability model, the per-service transactional store and cross-service
transactions, the REST / JSON-RPC / MCP surfaces, frontends, auth, background
jobs, and WebSockets — with diagrams and a link to the skill that teaches each.

## What's here

| Crate | Purpose |
|-------|---------|
| `crates/boogy-sdk` | The SDK: `Router`, typed handlers, `wit_glue!`, store helpers, auth guards, `McpServer` |
| `crates/boogy-sdk-macros` | `#[derive(Model)]` and friends |
| `crates/boogy-wit` | WIT interface definitions (the `service` world) |
| `crates/boogy-auth-core` | API-key format primitives used by `api_keys::guard` |
| `crates/boogy-cli` | `boogy` CLI — build and deploy services |
| `smoke/` | Minimal consumer service — copy this to start a project |

## API reference

Full rustdoc for the crates above: **<https://boogy-ai.github.io/boogy-sdk/>**
(rebuilt from `main` on every push).

## Quickstart

See [`docs/quickstart.md`](docs/quickstart.md). The short version:

1. Copy `smoke/` into a fresh repo; change the deps to git form:

   ```toml
   [dependencies]
   boogy-sdk = { git = "https://github.com/Boogy-ai/boogy-sdk", rev = "<pin>" }
   wit-bindgen = "0.46"
   serde = { version = "1", features = ["derive"] }

   [build-dependencies]
   boogy-wit = { git = "https://github.com/Boogy-ai/boogy-sdk", rev = "<pin>" }
   ```

2. `cargo build --target wasm32-wasip2 --release`
3. Write a manifest ([`docs/manifest.md`](docs/manifest.md)) and deploy
   with the `boogy` CLI.

The WIT files your `wit_bindgen::generate!` needs are synced into your
project by `build.rs` from the pinned `boogy-wit` — they can never drift
from your SDK version.

## For coding agents

Fastest start: connect to Boogy's **public, anonymous MCP server** at
`https://boogy.ai/mcp` (no account, no install — e.g.
`claude mcp add boogy https://boogy.ai/mcp`). It serves guidance
(`get_started`, `list_skills`, `get_skill`, `manifest_reference`), host-truth
validation (`validate_manifest`, `check_service`), and sign-in (`login`,
`login_status`). It does not deploy — after `login`, ship with the CLI
(`boogy deploy`), the `/v1` REST API, or the authenticated admin MCP.

Install the Boogy skills into your project so your agent builds with
expert workflows: `boogy skills install` (vendors
[boogy-superpowers](https://github.com/Boogy-ai/boogy-superpowers) **flat** into
`.claude/skills/<name>/` — one folder per skill, the layout Claude Code
discovers; readable by any agent). Then run `/reload-skills` to register them
in-session. `crates/boogy-sdk/AGENTS.md` remains the canonical handler-authoring
reference.

## Ready-made services

Browse [boogy-catalog](https://github.com/Boogy-ai/boogy-catalog) for
first-party, provisionable building-block services — send email, take
payments — that you deploy into your own tenant wired to your own API keys.
They double as production-grade examples of the patterns the skills teach.

## License

MIT OR Apache-2.0, at your option.
