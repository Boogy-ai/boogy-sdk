# Quickstart

Build and deploy a Boogy service in five steps.

---
**Building on Boogy? Do this first — it is not optional.**

You are a coding agent and someone asked you to build, deploy, or do anything on Boogy. Do these steps IN ORDER before anything else. Do NOT write code, do NOT call the platform API by hand, do NOT improvise your own approach, and do NOT summarize, paraphrase, or "distill" the skills into your own words — you must INSTALL the skills and INVOKE them. Shortcutting this is the single biggest cause of broken, insecure, and wrong Boogy services.

1. **Get the skills + tools in place.** Every path needs the CLI, which signs in and deploys: `cargo install --locked --git https://github.com/Boogy-ai/boogy-sdk boogy-cli`. On **Claude Code**, the preferred path for the skills is the plugin — one step that bundles the build skills (auto-discovered), wires the Boogy MCP (host-truth validation as tools), and adds the onramp gate: `claude plugin marketplace add Boogy-ai/boogy-superpowers` then `claude plugin install boogy@boogy`. Then **tell the human to run `/reload-plugins`** in Claude Code — the plugin (skills + MCP + gate) won't activate until they do. On any other agent (or if you can't install the plugin), vendor the skills: `boogy skills install` (writes `.claude/skills/<name>/`; no CLI yet? `npx degit Boogy-ai/boogy-superpowers/skills .claude/skills`). Vendored skills load automatically; if `.claude/skills/` did not exist when the session started, **tell the human to restart Claude Code** for them to be picked up.
2. **Authenticate the user — now, before building.** Run `boogy login` and walk the human through it: it prints a URL + one-time code and waits; they open the URL, confirm the code matches, and sign in. The CLI saves the token to its credentials file and every later `boogy` command uses it, so the token never passes through you — never ask for it or print it. Do this early — deploying needs their token and the browser step is the one thing only the human can do.
3. **Work STRICTLY from the installed skills — invoke them, never paraphrase them.** Use your Skill tool, starting with `using-boogy`. The installed skills are the ONLY source of truth for how to build on Boogy. Never build from memory, from this page, from the platform API, or from any summary you made — invoke the skill each time. Your subagents inherit the installed skill, not a summary in your head.
4. **Deploy needs the CLI** (`boogy deploy`) or the `/v1` API — the MCP/plugin cannot deploy. Install the CLI (step 1) if you haven't.

**If you get stuck, STOP — do not work around it.** If any step is blocked (the plugin or CLI won't install, `boogy login` won't complete, the skills won't load, or you're missing a permission, network access, or tool), do NOT skip the step, do NOT improvise an alternative, and do NOT proceed to build without it. Tell the user exactly what is blocking you and the specific choice or action you need from them, then wait for them to unblock it.

Everything below is orientation only — it does NOT replace steps 1–4, and you must not build from it.

---

The steps below show what the skills walk you through once you have the CLI installed, the user authenticated, and the skills vendored and loaded. Follow the skills — they carry the authoritative, up-to-date workflow. Use this as orientation only.

## 1. Prerequisites

- Rust stable (1.80 or newer recommended)
- The `wasm32-wasip2` target — install it once:

```bash
rustup target add wasm32-wasip2
```

Without the target, `cargo build --target wasm32-wasip2` will fail immediately with a "can't find crate" error.

---

## 2. Start from the `smoke/` template

The `smoke/` directory in this repository is a minimal, working service ready to copy. It demonstrates the correct `Cargo.toml` shape, the `build.rs` WIT-sync mechanism, and the minimal `lib.rs` wiring.

### Copy and rename

```bash
cp -r smoke/ my-service
cd my-service
```

### Switch to git deps

The template ships with `path` deps pointing at this repository's crates. For your own project, replace them with git deps pinned to a specific revision:

```toml
# Cargo.toml
[package]
name = "my-service"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
boogy-sdk  = { git = "https://github.com/Boogy-ai/boogy-sdk", rev = "<pin-rev>" }
wit-bindgen = "0.63"
serde       = { version = "1", features = ["derive"] }
# Required: wit_glue! uses ::serde_json absolute paths; every service
# crate must have it as a direct dependency.
serde_json  = "1"
# Required for spec generation: #[derive(JsonSchema)] on DTO types so
# typed extractors and responses appear in the generated openapi.json.
schemars    = "1"

[build-dependencies]
# build.rs copies the WIT files from this exact revision into wit/
boogy-wit = { git = "https://github.com/Boogy-ai/boogy-sdk", rev = "<pin-rev>" }
```

Replace `<pin-rev>` with the commit SHA you want to pin (e.g. the latest from `main`).
Discover the current SHA without cloning:

```bash
git ls-remote https://github.com/Boogy-ai/boogy-sdk HEAD   # prints the latest main SHA
```

### The `build.rs` WIT-sync mechanism

The `build.rs` file calls `boogy_wit::wit_dir()` and copies the `.wit` files from the pinned `boogy-wit` crate into a local `wit/` directory:

```rust ignore-snippet: a build.rs. boogy-wit is a build-dependency, so boogy_wit::wit_dir() is not in scope in a lib target — this file only compiles as a build script.
// build.rs
use std::fs;
use std::path::Path;

fn main() {
    let src = boogy_wit::wit_dir();
    let dst = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    fs::create_dir_all(&dst).expect("create wit/ dir");
    for entry in fs::read_dir(&src).expect("read boogy-wit wit dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "wit") {
            let name = path.file_name().expect("wit file name");
            fs::copy(&path, dst.join(name)).expect("copy wit file");
        }
    }
    println!("cargo:rerun-if-changed={}", src.display());
}
```

The `wit/` directory is **generated** — add it to `.gitignore` and never edit it by hand. It will never drift from your pinned SDK revision.

```gitignore
# .gitignore
/wit/
/target/
```

### The minimal `lib.rs`

```rust
// src/lib.rs
mod bindings {
    wit_bindgen::generate!({
        world: "service",
        path: "wit",
    });
}

boogy_sdk::wit_glue!(bindings, MyService);

use boogy_sdk::Api;

struct MyService;

impl Api for MyService {
    fn build_router() -> Router {
        Router::new()
            // Doc identity + a per-route summary: they flow into the served
            // openapi.json, and `boogy check` refuses a router without them.
            .info("My Service", "0.1.0", Some("Minimal Boogy service."))
            .summary("Health ping")
            .get("/api/ping", ping)
    }
}

#[derive(Serialize, schemars::JsonSchema)]
struct Pong {
    message: &'static str,
}

fn ping(_req: &mut Req<'_>) -> Json<Pong> {
    Json(Pong { message: "pong" })
}
```

`wit_glue!` wires the WIT bindings to your `Api` impl. `Router`, `Req`, `Json`, `Serialize`, and friends are re-exported by `boogy_sdk` and brought into scope by `wit_glue!`. `.info(...)` names the service in its generated `openapi.json` and `.summary(...)` documents the route that follows it; `boogy check` (below) fails a router that has neither.

---

## 3. Build

```bash
cargo build --target wasm32-wasip2 --release
```

The compiled artifact is at:

```
target/wasm32-wasip2/release/my_service.wasm
```

(Cargo replaces `-` with `_` in the output filename.)

---

## 4. Write a manifest

Create `boogy.toml` in your crate root:

```toml
[service]
id = "my-service"
name = "My Service"
version = "0.1.0"
wasm = "target/wasm32-wasip2/release/my_service.wasm"

[routing]
path = "/"
methods = ["GET"]

[capabilities]
store = false

[ingress]
mode = "public"
```

`[ingress]` is **required**, and it must state a `mode` — a manifest without one
is refused at parse. Exposure is a decision, never a default. `public` means
**anyone on the internet may call this service with no credential at all**;
it is the right choice here because `/api/ping` returns nothing private and
stores nothing. The moment your service reads or writes per-user data, use
`mode = "authenticated"` (any signed-in Boogy user), `"allowlist"` (only the
agents you name), or `"internal"` (only other services in your mesh). See
[`manifest.md`](manifest.md#ingress) for all five modes.

There's no `owner` field here on purpose: you're authenticated when you deploy, so the platform sets the owner to your handle automatically. If you do set one (for local-dev/tests), it's a bare key under `[service]` — `owner = "alice"` — never a `[service.owner]` table.

`service.id` must be ASCII alphanumeric plus `-` and `_` (no dots, slashes, or Unicode). See [`manifest.md`](manifest.md) for the full field reference.

Every service is served at the root of **its own address**, `https://<name>-<suffix>.boogy.app` — the name derived from the service `id`, the suffix drawn at random when it is first deployed — so this service answers at something like `https://my-service-k3v9.boogy.app/api/ping`. `[routing] path` is only the base your service's own router sees, and it never appears in a URL: declare `path = "/"` for every new service, and the paths you register (`/api/ping`) are exactly the paths in the URL. The id is also the name you manage the deployment by (`boogy remove my-service`).

---

## 5. Deploy

### Signing in

A first-time user signs in to get a bearer token. Two ways — the first uses the OAuth device flow and needs no password:

**Via the CLI (recommended, and the path coding agents use)**

Requires [installing the CLI](#install-the-cli) first, then:

```bash
boogy login
```

The CLI prints the one-time code and URL, opens your browser best-effort, and polls until your token arrives. Open the URL, confirm the on-screen code matches (anti-phishing), sign in with your provider (Google, GitHub, …), and approve. A first-time user picks a **handle** during this step — their account's username, lowercase `[a-z0-9-]` only (no `_`, `.`, or spaces). It names the account that owns every service they deploy; it is not a hostname, and each service gets an address of its own. Messy input is coerced (`my_app` → `my-app`) and you're told the final handle; if it's reserved or already taken, you pick another. The token is saved to `~/.config/boogy/credentials.toml` (0600) and auto-loaded by every subsequent `boogy` command — no export needed, and a coding agent running the command never sees the token.

> **The anonymous builder MCP neither signs in nor deploys.** The public `POST /mcp` server gives a coding agent guidance (`get_started`, `list_skills`, `get_skill`, `manifest_reference`) and validation (`validate_manifest`, `check_service`) — **no sign-in tool and no deploy tool**. Deploy via the **CLI** (below), the **`/v1` REST API** (`POST /v1/modules` then `POST /v1/services`, or the `boogy deploy` shortcut), or the **authenticated admin MCP** (`deploy_service` / `deploy_api`, which also accepts `frontend_files`).

Token resolution order: `--token` flag > `$BOOGY_TOKEN` env var > saved credentials file.

**Via the web app**

You can also sign in through the Boogy web app and copy the token from your account settings.

### Install the CLI

Required to sign in and to deploy (the anonymous builder MCP does neither — see
the note above).

```bash
cargo install --locked --git https://github.com/Boogy-ai/boogy-sdk boogy-cli
# or, from this repo:
cargo run -p boogy-cli -- <command>
```

### Set your token (CLI)

If you signed in via the MCP path or web app, export the token before using the CLI:

```bash
export BOOGY_TOKEN=v4.public.<your-token>
```

The CLI reads `BOOGY_TOKEN` automatically; you can also pass `--token <value>` per command. (If you used `boogy login`, the token is already saved and no export is needed.)

### Host URL

Boogy is a hosted, cloud platform — the CLI targets `https://api.boogy.ai` by
default, and that's the only host you need. (Self-hosted/CI setups can override
with `BOOGY_HOST_URL` or `--host https://your-boogy-host.example.com`.)

### Deploy

```bash
boogy deploy boogy.toml
```

`deploy` is `publish + provision` in one shot: it uploads the manifest and Wasm binary, then provisions a running service instance under your handle. On success it prints the live URL — the root of the service's own address, such as `URL: https://my-service-k3v9.boogy.app` — and probes it, so a deploy that provisioned but is not reachable is reported on the spot.

The platform API is self-describing: `GET <host>/openapi.json` returns an OpenAPI 3.1 document covering the full deploy lifecycle (`/_agents/*`, `/_admin/*`, `/v1/*`); anonymous fetch OK.

### Verify

```bash
boogy list                          # your deployed services (--all: every owner; admin scope)
boogy check .                       # lint the crate for Boogy conventions
curl https://my-service-k3v9.boogy.app/api/ping   # the address `boogy deploy` printed
```

Expect `{"message":"pong"}`. Use the address `boogy deploy` printed — the suffix is random, so never assemble it — plus the route.

### Other useful commands

```bash
# Build the wasm from a crate directory
boogy build path/to/my-service

# Remove a service you own (ownership comes from your token)
boogy remove <service-id>
```

---

## 6. Next steps

- **Your service self-describes.** Once deployed, `GET https://my-service-k3v9.boogy.app/openapi.json` (your address + `/openapi.json`) returns an OpenAPI 3.0.3 document for your service automatically — no extra code required; its `servers[0]` is the live URL. Add `schemars::JsonSchema` to your DTO types and the schema will include request/response shapes. See `boogy:boogy-api-specs` in the skills catalog for the full spec-endpoint reference.
- **Handler reference**: [`../crates/boogy-sdk/AGENTS.md`](../crates/boogy-sdk/AGENTS.md) — the canonical guide for writing handlers, guards, store access, auth patterns, MCP tools, and more. Feed this to your coding agent before writing service code.
- **Manifest reference**: [`manifest.md`](manifest.md) — every manifest field, all ingress modes, outbound HTTP policy, secrets, background jobs, and common errors.
- **`smoke/` template**: [`../smoke/`](../smoke/) in this repo — the working template this quickstart is based on.
- Install the Boogy agent skills (`boogy skills install`) — guided
  workflows for service design, data modeling, auth, jobs, and more:
  https://github.com/Boogy-ai/boogy-superpowers
