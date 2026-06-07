# Quickstart

Build and deploy a Boogy service in five steps.

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

[build-dependencies]
# build.rs copies the WIT files from this exact revision into wit/
boogy-wit = { git = "https://github.com/Boogy-ai/boogy-sdk", rev = "<pin-rev>" }
```

Replace `<pin-rev>` with the commit SHA you want to pin (e.g. the latest from `main`).

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

#[derive(Serialize)]
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

[service.owner]
user_id = "your-user-id"

[routing]
path = "/api/ping"
methods = ["GET"]

[capabilities]
store = false

[ingress]
mode = "public"
```

`service.id` and `service.owner.user_id` must be ASCII alphanumeric plus `-` and `_` (no dots, slashes, or Unicode). See [`manifest.md`](manifest.md) for the full field reference.

---

## 5. Deploy

### Install the CLI

```bash
cargo install --locked --git https://github.com/Boogy-ai/boogy-sdk boogy-cli
# or, from this repo:
cargo run -p boogy-cli -- <command>
```

### Set your token

Most commands require a bearer token:

```bash
export BOOGY_TOKEN=v4.public.<your-token>
```

Obtain a token by logging in via the Boogy web app or `curl /_agents/login`. The CLI reads `BOOGY_TOKEN` automatically; you can also pass `--token <value>` per command.

### Default host URL

The CLI targets `http://localhost:3000` by default. Override it with:

```bash
export BOOGY_HOST_URL=https://your-boogy-host.example.com
# or per-command:
boogy deploy boogy.toml --host https://your-boogy-host.example.com
```

### Deploy

```bash
boogy deploy boogy.toml
```

`deploy` is `publish + provision` in one shot: it uploads the manifest and Wasm binary, then provisions a running service instance for your user ID.

### Verify

```bash
boogy list                          # list deployed services (requires admin scope)
curl http://localhost:3000/your-user-id/api/ping
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

- **Handler reference**: [`../crates/boogy-sdk/AGENTS.md`](../crates/boogy-sdk/AGENTS.md) — the canonical guide for writing handlers, guards, store access, auth patterns, MCP tools, and more. Feed this to your coding agent before writing service code.
- **Manifest reference**: [`manifest.md`](manifest.md) — every manifest field, all ingress modes, outbound HTTP policy, secrets, background jobs, and common errors.
- **`smoke/` template**: [`../smoke/`](../smoke/) in this repo — the working template this quickstart is based on.
- Install the Boogy agent skills (`boogy skills install`) — guided
  workflows for service design, data modeling, auth, jobs, and more:
  https://github.com/Boogy-ai/boogy-superpowers
