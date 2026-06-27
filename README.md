# Boogy SDK

Ship whole services on [Boogy](https://boogy.ai) — frontend, API, and data —
from one Rust crate compiled to `wasm32-wasip2`. Each service gets a route
subtree, an isolated **relational, ACID** store, capability-based security,
in-process calls to other services, and REST / JSON-RPC / MCP surfaces — and a
single transaction can span a whole chain of services.

> **Status: early development.** APIs change without notice. Pin a git
> `rev` in your `Cargo.toml`. Published to crates.io once stable.

---

**Building on Boogy with a coding agent? Do this first — it is not optional.**

If you are a coding agent (or you're pointing one at Boogy), follow these steps IN ORDER before anything else. Do NOT write code, do NOT call the platform API by hand, do NOT improvise your own approach, and do NOT summarize, paraphrase, or "distill" the skills into your own words — you must INSTALL the skills and INVOKE them. Shortcutting this is the single biggest cause of broken, insecure, and wrong Boogy services.

1. **Get the skills + tools in place.** On **Claude Code**, the preferred path is: install the plugin — one step that bundles the build skills (auto-discovered), wires the Boogy MCP (sign-in + host-truth validation as tools), and adds the onramp gate: `claude plugin marketplace add Boogy-ai/boogy-superpowers` then `claude plugin install boogy-superpowers`. Then **tell the human to run `/reload-plugins`** in Claude Code — the plugin (skills + MCP + gate) won't activate until they do. On any other agent (or if you can't install the plugin): install the CLI — `cargo install --locked --git https://github.com/Boogy-ai/boogy-sdk boogy-cli` — and vendor the skills: `boogy skills install` (writes `.claude/skills/<name>/`; no CLI either? `npx degit Boogy-ai/boogy-superpowers/skills .claude/skills`). Vendored skills load automatically; if `.claude/skills/` did not exist when the session started, **tell the human to restart Claude Code** for them to be picked up.
2. **Authenticate the user — now, before building.** Run the `login` tool (the plugin wires it) or `boogy login`, and walk the human through it: it prints a URL + one-time code; they open the URL, confirm the code matches, and sign in. Do this early — deploying needs their token and the browser step is the one thing only the human can do.
3. **Work STRICTLY from the installed skills — invoke them, never paraphrase them.** Use your Skill tool, starting with `using-boogy`. The installed skills are the ONLY source of truth for how to build on Boogy. Never build from memory, from this README, from the platform API, or from any summary you made — invoke the skill each time. Your subagents inherit the installed skill, not a summary in your head.
4. **Deploy needs the CLI** (`boogy deploy`) or the `/v1` API — the MCP/plugin cannot deploy. Install the CLI (step 1) if you haven't.

**If you get stuck, STOP — do not work around it.** If any step is blocked (the plugin or CLI won't install, `login` won't complete, the skills won't load, or you're missing a permission, network access, or tool), do NOT skip the step, do NOT improvise an alternative, and do NOT proceed to build without it. Tell the user exactly what is blocking you and the specific choice or action you need from them, then wait for them to unblock it.

Everything below is reference — it does NOT replace steps 1–4.

---

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
`https://api.boogy.ai/mcp` (no account, no install — e.g.
`claude mcp add boogy https://api.boogy.ai/mcp`). It serves guidance
(`get_started`, `list_skills`, `get_skill`, `manifest_reference`), host-truth
validation (`validate_manifest`, `check_service`), and sign-in (`login`,
`login_status`). It does not deploy — after `login`, ship with the CLI
(`boogy deploy`), the `/v1` REST API, or the authenticated admin MCP.

Install the Boogy skills into your project so your agent builds with
expert workflows: `boogy skills install` (vendors
[boogy-superpowers](https://github.com/Boogy-ai/boogy-superpowers) **flat** into
`.claude/skills/<name>/` — one folder per skill, the layout Claude Code
discovers; readable by any agent). Vendored skills load automatically — if
`.claude/skills/` was just created, tell the human to restart Claude Code.
**Preferred for Claude Code: install the plugin** (`claude plugin marketplace add Boogy-ai/boogy-superpowers` then
`claude plugin install boogy-superpowers`) and tell the human to run **`/reload-plugins`** to activate it
mid-session. `crates/boogy-sdk/AGENTS.md` remains the canonical handler-authoring
reference.

## Ready-made services

Browse [boogy-catalog](https://github.com/Boogy-ai/boogy-catalog) for
first-party, provisionable building-block services — send email, take
payments — that you deploy into your own tenant wired to your own API keys.
They double as production-grade examples of the patterns the skills teach.

## License

MIT OR Apache-2.0, at your option.
