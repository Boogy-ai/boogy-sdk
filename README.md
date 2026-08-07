# Boogy SDK

Build services for [Boogy](https://boogy.ai) — Rust compiled to
`wasm32-wasip2`, deployed to a shared runtime with isolated transactional
storage, capability-based security, cross-service calls, and a built-in
MCP surface for LLM clients.

> **Status: early development.** APIs change without notice. Pin a git
> `rev` in your `Cargo.toml`. Published to crates.io once stable.

## What's here

| Crate | Purpose |
|-------|---------|
| `crates/boogy-sdk` | The SDK: `Router`, typed handlers, `wit_glue!`, store helpers, auth guards, `McpServer` |
| `crates/boogy-sdk-macros` | `#[derive(Model)]` and friends |
| `crates/boogy-wit` | WIT interface definitions (the `service` world) |
| `crates/boogy-auth-core` | API-key format primitives used by `api_keys::guard` |
| `crates/boogy-cli` | `boogy` CLI — build and deploy services |
| `smoke/` | Minimal consumer service — copy this to start a project |

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

Install the Boogy skills into your project so your agent builds with
expert workflows: `boogy skills install` (vendors
[boogy-superpowers](https://github.com/Boogy-ai/boogy-superpowers) into
`.claude/skills/boogy/` — auto-discovered by Claude Code, readable by any
agent). `crates/boogy-sdk/AGENTS.md` remains the canonical handler-authoring
reference.

## Ready-made services

Browse [boogy-catalog](https://github.com/Boogy-ai/boogy-catalog) for
first-party, provisionable building-block services — send email, take
payments — that you deploy into your own tenant wired to your own API keys.
They double as production-grade examples of the patterns the skills teach.

## License

MIT OR Apache-2.0, at your option.
