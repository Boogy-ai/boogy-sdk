# Quickstart

Build and deploy a Boogy service in five steps.

> **Coding agents: vendor the skills first.** Run `boogy skills install` (CLI) up
> front — it writes the Boogy agent skills **flat** into `.claude/skills/<name>/`
> (one folder per skill — the only layout Claude Code discovers) where coding
> agents (and any implementer subagents) auto-discover and read them directly.
> Then run `/reload-skills` to register them in-session (no restart). The
> anonymous MCP's `get_skill` is great for ad-hoc lookup by the *driving* agent,
> but its results don't persist into a fresh subagent's context; vendored skills
> are the durable copy your agents actually build from. Re-run `boogy skills
> install` to refresh. (Do this before generating service code.)

---

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
wit-bindgen = "0.46"
serde       = { version = "1", features = ["derive"] }
# Required: wit_glue! uses ::serde_json absolute paths; every service
# crate must have it as a direct dependency.
serde_json  = "1"
# Required for spec generation: #[derive(JsonSchema)] on DTO types so
# typed extractors and responses appear in the generated openapi.json.
schemars    = "0.8"

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

```rust
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
        Router::new().get("/api/ping", ping)
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

`wit_glue!` wires the WIT bindings to your `Api` impl. `Router`, `Req`, `Json`, `Serialize`, and friends are re-exported by `boogy_sdk` and brought into scope by `wit_glue!`.

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
path = "/api/ping"
methods = ["GET"]

[capabilities]
store = false

[ingress]
mode = "public"
```

There's no `owner` field here on purpose: you're authenticated when you deploy, so the platform sets the owner to your handle automatically. If you do set one (for local-dev/tests), it's a bare key under `[service]` — `owner = "alice"` — never a `[service.owner]` table.

`service.id` must be ASCII alphanumeric plus `-` and `_` (no dots, slashes, or Unicode). See [`manifest.md`](manifest.md) for the full field reference.

---

## 5. Deploy

### Signing in

A first-time user signs in to get a bearer token. Three ways — the first two use the same OAuth device flow and need no password:

**Via the MCP server (zero-install — recommended for agent sessions)**

If your coding agent is already connected to Boogy's MCP server, no install needed. The agent:

1. Calls the `login` tool → receives a `user_code`, a `verification_uri_complete`, and a `device_code`.
2. Shows you the URL and code — open the URL in your browser, confirm the on-screen code matches (anti-phishing), sign in with your provider (Google, GitHub, …), and approve. A first-time user picks a handle during this step.
3. Polls the `login_status` tool with the `device_code` until it returns `{status: "complete", token, handle}`.

The returned `token` is your Boogy bearer token. Set it as `BOOGY_TOKEN` in the session (or pass `--token` per command) for any subsequent CLI calls.

> **The anonymous builder MCP does not deploy.** The public `POST /mcp` server gives a coding agent guidance (`get_started`, `list_skills`, `get_skill`, `manifest_reference`), validation (`validate_manifest`, `check_service`), and `login`/`login_status` — but **no deploy tool**. After you have a token, deploy via the **CLI** (below), the **`/v1` REST API** (`POST /v1/modules` then `POST /v1/services`, or the `boogy deploy` shortcut), or the **authenticated admin MCP** (`deploy_service` / `deploy_api`, which also accepts `frontend_files`). The cold-entry `/mcp` is for getting *ready* to deploy, not deploying.

**Via the CLI**

Requires [installing the CLI](#install-the-cli) first, then:

```bash
boogy login
```

The CLI prints the one-time code and URL, opens your browser best-effort, and polls until your token arrives. The token is saved to `~/.config/boogy/credentials.toml` (0600) and auto-loaded by every subsequent `boogy` command — no export needed.

Token resolution order: `--token` flag > `$BOOGY_TOKEN` env var > saved credentials file.

**Via the web app**

You can also sign in through the Boogy web app and copy the token from your account settings.

### Install the CLI

Required to deploy (the anonymous builder MCP can't — see the note above). Even an
agent working through the MCP session needs the CLI or the `/v1` REST API (or the
authenticated admin MCP) to actually deploy.

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

Boogy is a hosted, cloud platform — the CLI targets `https://boogy.ai` by
default, and that's the only host you need. (Self-hosted/CI setups can override
with `BOOGY_HOST_URL` or `--host https://your-boogy-host.example.com`.)

### Deploy

```bash
boogy deploy boogy.toml
```

`deploy` is `publish + provision` in one shot: it uploads the manifest and Wasm binary, then provisions a running service instance for your user ID.

The platform API is self-describing: `GET <host>/openapi.json` returns an OpenAPI 3.1 document covering the full deploy lifecycle (`/_agents/*`, `/_admin/*`, `/v1/*`); anonymous fetch OK.

### Verify

```bash
boogy list                          # list deployed services (requires admin scope)
curl https://boogy.ai/your-user-id/api/ping
```

### Other useful commands

```bash
# Build the wasm from a crate directory
boogy build path/to/my-service

# Remove a service
boogy remove <owner-user-id> <service-id>
```

---

## 6. Next steps

- **Your service self-describes.** Once deployed, `GET /<owner>/<service-id>/openapi.json` returns an OpenAPI 3.0.3 document for your service automatically — no extra code required. Add `schemars::JsonSchema` to your DTO types and the schema will include request/response shapes. See `boogy:boogy-api-specs` in the skills catalog for the full spec-endpoint reference.
- **Handler reference**: [`../crates/boogy-sdk/AGENTS.md`](../crates/boogy-sdk/AGENTS.md) — the canonical guide for writing handlers, guards, store access, auth patterns, MCP tools, and more. Feed this to your coding agent before writing service code.
- **Manifest reference**: [`manifest.md`](manifest.md) — every manifest field, all ingress modes, outbound HTTP policy, secrets, background jobs, and common errors.
- **`smoke/` template**: [`../smoke/`](../smoke/) in this repo — the working template this quickstart is based on.
- Install the Boogy agent skills (`boogy skills install`) — guided
  workflows for service design, data modeling, auth, jobs, and more:
  https://github.com/Boogy-ai/boogy-superpowers
