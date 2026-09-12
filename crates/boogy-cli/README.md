# boogy-cli

Operator CLI for Boogy. Build wasm components, deploy / list / delete APIs, manage api-keys, run development workflows against a local or remote host.

## Install

```bash
cargo install --path crates/boogy-cli
# or
cargo run -p boogy-cli -- <command>
```

Default host URL is `http://localhost:3000`. Override per-command with `--host <url>` or persist via the env var `BOOGY_HOST_URL`.

## Common workflows

```bash
# Build a wasm service from its crate.
boogy build path/to/my-api

# Deploy a freshly-built component.
boogy deploy path/to/my-api/boogy.toml

# List deployed services (also unauthenticated; useful as a smoke test).
boogy list

# Tear down a deployment.
boogy delete <api-id>

# Manage api-keys (sk_*) issued by a deployed service.
boogy keys list <api-id>
boogy keys create <api-id> --label "stripe-integration"
boogy keys revoke <api-id> <key-id>
```

## Auth

Most commands require an authenticated bearer token. Set:

```bash
export BOOGY_TOKEN=v4.public.<...>
```

Login flows live in the host's `/_agents/*` surface; the CLI doesn't currently bundle a login command — paste a token from the web app or a manual `curl /_agents/login` instead.

## Manifest

Every deployed service has a `boogy.toml` next to its wasm crate. The manifest schema covers routing, capabilities, ingress modes (including delegation), outbound HTTP (`[outbound]` + `[secrets]`), and resource limits — see the Boogy documentation for the authoritative field reference.

## Frontends

A deployment can ship a **web frontend** alongside (or instead of) its wasm. When the manifest has a `[frontend]` section, `boogy deploy` / `boogy publish` tarball the `[frontend].root` source directory and upload it with the publish — you ship the **source** (`.ts`/`.js`/`.html`/`.css`/assets), and the platform transpiles TypeScript to JavaScript at deploy time, so **no JS build step runs in the CLI**.

```toml
[frontend]
root       = "web"          # source dir to tarball + upload
api_prefix = "/api"         # requests under it → wasm (omit → static frontend only)
build      = "ts"           # "ts" (platform transpiles) | "none" (pre-built JS)
```

A manifest with a `[frontend]` section and no `service.wasm` is a static (frontend-only) deployment; with both, it's full-stack. A manifest with neither a wasm nor a `[frontend]` is rejected — there is nothing to deploy.

## Codegen

Spec → wasm scaffolding lives in the Boogy codegen service, not this CLI. The CLI invokes that service for `boogy scaffold` (when configured); pure local builds use `cargo build --target wasm32-wasip2`.

## Tests

```bash
cargo test -p boogy-cli
```
