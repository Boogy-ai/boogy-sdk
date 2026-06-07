# boogy-wit

WIT interface definitions for the Boogy platform. This crate is the **contract**: every wasm component compiled against the Boogy world imports these interfaces, and the host implements the matching `Host` traits.

The crate's only Rust output is a tiny stub (the `.wit` files are the artifact); but it lives in the workspace so `cargo check --workspace` validates that the WIT package parses, and so other crates depend on it as a path dependency for the bindgen path.

## World

`world service` (in `wit/world.wit`) is what every Boogy API exports / imports:

```wit
world service {
    export http-handler;

    import store;
    import auth;
    import runtime;
    import peer;
    import outbound-http;
}
```

Wasm components export the inbound `http-handler` (so the host can dispatch HTTP requests into them) and import the capabilities they're permitted to use. The manifest's `[capabilities]` block is the *operator-visible* gate; bindgen always emits all imports.

## Interfaces

| File | Purpose |
|---|---|
| `auth.wit` | `current-identity()` — caller's principal + scopes. Read-only; the host populates from request auth state. |
| `http-handler.wit` | `http-request` (method, path, headers, params, body) + `http-response`. The single export every component implements. |
| `outbound-http.wit` | `outbound-request` / `outbound-response` / `fetch-error` + `fetch(request)`. Mediated by the host: per-service allowlist, SSRF firewall, secret injection, rate limit. |
| `peer.wit` | Cross-service in-process dispatch. `peer-request` / `peer-response` / `fetch-error` + `fetch(target, request)`. Target is a `boogy://<owner>/services/<service_id>` URI; the host strips identity-bearing headers on every hop. |
| `runtime.wit` | `now-ms()`, `random-bytes()`, `log()`. Capability-gated time / entropy / logging. |
| `store.wit` | Per-service store surface. `insert` / `update` / `delete` / `find` / `execute` / `query`, plus a `transaction` resource type for atomic multi-row writes. |

## Versioning

Package version: `boogy:platform@0.1.0`. Pre-1.0 — every change is a breaking change. Once the SDK stabilizes, the world will lock and additive changes will move via versioned interface imports.

## Where the implementations live

| Interface | Host impl | SDK wrapper |
|---|---|---|
| `auth` | Host capability (auth.rs) | `boogy_sdk::auth::*` |
| `http-handler` | Host linker (calls into wasm via `Api::handle`) | `wit_glue!` macro |
| `outbound-http` | Host capability (outbound) | `boogy_sdk::http::*` |
| `peer` | Host capability (peer.rs) | `boogy_sdk::peer::*` |
| `runtime` | Host capability (runtime.rs) | (used directly by SDK macros) |
| `store` | Host capability (store.rs) | `boogy_sdk` table builder + `tx()` |
