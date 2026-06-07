# boogy-wit: WIT interface reference

Catalog of Boogy's WebAssembly Interface Types (WIT) — the capability-based surface that wasm components import to access platform services.

**Most developers should not read this directly.** The SDK wrapper (`crates/boogy-sdk`) provides typed Rust helpers for every capability. This document is for agents and code-generators that need to understand the WIT shape, and for operators verifying that the manifest-declared capability grants match the interfaces.

## Import model

Every Boogy service imports the `boogy:platform` package and targets a world (entry point) from `crates/boogy-wit/wit/world.wit`:

- **`service`** (default): HTTP-only. Wasm handler exports `http-handler` and imports all platform capabilities. Appropriate for the vast majority of services.
- **`service-with-jobs`**: Superset of `service`. Wasm exports both `http-handler` and `job-handler` (background-job processor). Target this if your manifest declares `[background_jobs.handlers.*]` blocks.

Standard invocation in the wasm crate's `lib.rs` or `main.rs`:

```rust
mod bindings {
    wit_bindgen::generate!({ world: "service", path: "../../boogy-wit/wit" });
}
```

For job-handler components:

```rust
mod bindings {
    wit_bindgen::generate!({ world: "service-with-jobs", path: "../../boogy-wit/wit" });
}
```

The generated bindings are opaque to user code — the SDK's `wit_glue!` macro rewrites them into typed, ergonomic Rust stubs. Never hand-write WIT bind calls; use the SDK helpers instead.

## Capability interfaces

### auth

**Gated by**: Always present. No manifest gate.

Returns the caller's resolved identity and scope grants. The identity record contains `principal` (human/agent/workload), optional delegated `actor` (for on-behalf-of flows), and `scopes` (permission claims from the token).

| Function | Signature | Notes |
|---|---|---|
| `current-identity()` | `option<identity>` | `none` → anonymous/unauthenticated. |
| `has-scope(scope)` | `bool` | Convenience to check scope membership. |

**SDK wrapper**: `auth::current_principal()` → `Option<String>`, `auth::required()` → guard, `auth::owns_resource(table, owner_col, id_param)` → guard with resource loading. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md) for the full auth API.

### store

**Gated by**: `[capabilities] store = true` (default: true).

Per-service isolated store. Supports schema DDL (create/drop tables and indexes), structured CRUD (insert, find, update, delete with filters), and transactions. Type system: `value` enum (null, text, integer, real, blob, boolean). Filtering via `filter` records with `filter-op` enum (eq, neq, gt, gte, lt, lte, like, not-like, is-null, is-not-null, in). Sorting, pagination, and result rows are structured.

**Engine**: the platform provides a single built-in per-service store engine — there is no engine selection. The `[store]` manifest section is reserved for future store config but carries no `engine` knob.

**Transactions**: three free funcs — `begin-transaction`, `commit-transaction`, `rollback-transaction` (each `func() -> result<_, store-error>`), with *ambient* semantics. While a transaction is open every store op runs inside it (no per-op handle); peer calls enroll the callee into the same tx. `commit-transaction` is owner-only (a participant enrolled via a peer call cannot commit), and a poisoned tx — one where any participant's write failed — refuses to commit and rolls back. `rollback-transaction` (or simply dropping the request) discards it. Outbound-http and background-jobs are denied inside a transaction. Transactions are always available — the built-in engine is fully transactional.

**SDK wrapper**: `Store`, `Query`, `Transaction` (typed builder pattern). **Never call raw WIT bindings**; use SDK methods (see [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md) Store section).

### auth (capabilities)

**Gated by**: Always present.

Credential and identity primitives. Provided by the SDK's `auth::*` namespace and the implicit identity context (no direct WIT calls needed).

### runtime

**Gated by**: `[capabilities] clock = true` and `entropy = true` (defaults: true).

Three utility functions:

| Function | Returns | Notes |
|---|---|---|
| `now-millis()` | `u64` | Unix time in milliseconds. Monotonic across retries. |
| `random-bytes(len)` | `list<u8>` | Cryptographically sound random bytes. |
| `log(level, message)` | `()` | Host logger. `level` is "debug", "info", "warn", "error". |

**SDK wrapper**: `runtime::now()` → `SystemTime`, `runtime::random_bytes(len)`, `runtime::log(level, msg)`.

### peer

**Gated by**: `[capabilities] peer = true` (default: false).

Cross-service HTTP-style fetch. Target is another deployed workload. Call is in-process (no network roundtrip); the host mediates it and enforces the target's ingress policy. Caller's workload identity is set by the host and cannot be forged. Errors distinguish target-not-found, denied (ingress policy), timeout, recursion depth limit (cross-service cycles), and capability denial.

| Function | Signature | Notes |
|---|---|---|
| `fetch(target, request)` | `result<peer-response, fetch-error>` | `target` is `boogy://<owner>/services/<service_id>`. `request.path` is relative to target's base path. |

**SDK wrapper**: `peer::fetch(target, method, path, headers, body)`. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md) for helper methods and a worked example.

### outbound-http

**Gated by**: `[capabilities] outbound_http = true` (default: false).

Outbound HTTP to arbitrary URLs. Request and response shapes are identical to `peer` but errors are distinct (URL validation, DNS, connection refused, SSRF firewall, rate limit, secret injection, etc.). Manifest gates include egress allowlist (`[outbound.allowed_hosts]`), SSRF firewall (`[outbound.allow_loopback]`), request/response size caps, timeout, and secret injection (`[secrets]`). Secrets are never visible to wasm — the host injects them as headers just before transmission.

| Function | Signature | Notes |
|---|---|---|
| `fetch(request)` | `result<outbound-response, fetch-error>` | Full URL (scheme + host + path) required. HTTP/HTTPS only. |

**SDK wrapper**: `http::fetch(method, url, headers, body)` + builder pattern. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md).

### vector

**Gated by**: `[capabilities] vector = true` (default: false).

Vector similarity search over embedded rows. Supports collection creation (specify dimension count and distance metric), insertion/update/deletion of vectors, and search (k-nearest neighbors with optional filtering). Metrics: cosine, Euclidean, dot-product. HNSW index under the hood.

| Function | Signature | Notes |
|---|---|---|
| `create-collection(table, name, options)` | `result<_, string>` | Specify dimensions, metric, and optional HNSW params (m, ef_construction). |
| `drop-collection(table, name)` | `result<_, string>` | |
| `insert(table, collection, rowid, vector)` | `result<_, string>` | Associate a vector with a row. |
| `insert-batch(table, collection, entries)` | `result<_, string>` | Batch insert (more efficient). |
| `update(table, collection, rowid, vector)` | `result<_, string>` | Replace an existing vector. |
| `delete(table, collection, rowid)` | `result<_, string>` | Remove a vector. |
| `search(table, collection, query, options)` | `result<list<vector-result>, string>` | k-NN search. Options: k, ef_search (HNSW param), optional `store::filter` for post-filtering. Returns rowid + distance. |
| `unlock-collection(table, name, key)` | `result<_, string>` | (Advanced) Unlock a read-locked collection. Used internally by the SDK. |

**SDK wrapper**: `vector::Collection`, `vector::search()`. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md).

### background-jobs (caller)

**Gated by**: `[capabilities] background_jobs = true` (default: true).

Enqueue, cancel, and query background jobs from inside HTTP handlers (or from inside other job handlers). All operations are pinned to the caller's workload identity — there's no way to enqueue a job for a different API. Handler names must match manifest declarations in `[background_jobs.handlers.<name>]`.

| Function | Signature | Notes |
|---|---|---|
| `enqueue(spec)` | `result<string, enqueue-error>` | `spec` includes handler name, opaque payload, optional not-before timestamp, optional max-attempts override, optional idempotency key. Returns job UUID. |
| `cancel(job-id)` | `result<cancel-outcome, cancel-error>` | Pending → cancelled immediately; running → cancellation request sent. |
| `status(job-id)` | `result<job-status-info, cancel-error>` | Current state: pending, running, succeeded, failed, dead-letter, or cancelled. |

**SDK wrapper**: `jobs::enqueue(handler, payload, opts)`, `jobs::cancel()`, `jobs::status()`. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md).

### job-handler (callee)

**Gated by**: No import gate (host exports to you, not vice versa). Implement only if your manifest has `[background_jobs.handlers.*]` blocks and you target `world: "service-with-jobs"`.

Wasm components that process background jobs export this interface. The worker (`boogy-jobworker` binary) calls `handle-job` once per claimed job, after replaying the job's original identity. Handler receives a job context (job ID, handler name, attempt count, not-before timestamp) and opaque payload, and returns either a result (success), a retry error (soft-fail, increments attempts), or a terminal error (straight to dead-letter).

| Function | Signature | Notes |
|---|---|---|
| `handle-job(ctx, payload)` | `result<list<u8>, handler-error>` | `ctx` includes job ID (stable across retries, for idempotency), handler name, attempt count, not-before. Return success with optional response bytes, or `retry` / `terminal` error variant. Wasm traps are treated as retryable with error kind `handler_trap`. |

**SDK wrapper**: `Job::init()` to register a handler; the worker calls your exported `handle-job` automatically. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md).

## http-handler (export only)

**Wasm exports this interface. Host does not.**

Entry point for HTTP requests. The host dispatches inbound HTTP to your `handle(req)` function. Request has method, path, headers, query/path params, and body. Response is status, headers, and body.

| Function | Signature |
|---|---|
| `handle(req)` | `http-response` |

**SDK wrapper**: `Router` and handler function signatures. Never call WIT bindings directly; use the SDK's `#[handler]` and route-builder patterns. See [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md).

## Versioning

WIT interfaces are append-only. Adding a field, function, or enum variant is a forward-compatible deployment — old components continue to run without recompilation. Removing or renaming anything is breaking (requires recompile and redeploy of all components). Changes are coordinated with the host and documented in the commit message and `CHANGELOG`.

## Further reading

- [`crates/boogy-sdk/AGENTS.md`](../boogy-sdk/AGENTS.md) — SDK type wrappers, handler boilerplate, authentication, guards, MCP/JSON-RPC, API keys.
