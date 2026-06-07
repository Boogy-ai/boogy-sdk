# boogy-sdk

The framework crate for hand-writing Boogy APIs (and the foundation the codegen pipeline emits against).

A Boogy API is a Wasm component that exports the WIT `http_handler` interface and runs inside the Boogy host. The SDK gives you the building blocks — request routing, response builders, JSON-RPC envelopes, typed row accessors, table builders — and a single macro (`wit_glue!`) that handles all the WIT plumbing so you write idiomatic Rust without ever touching the raw bindings.

> **Looking for the dense LLM-friendly cheat sheet?** [`AGENTS.md`](AGENTS.md) is the canonical "everything you can use inside a Boogy handler" reference. If you're just trying to write a handler, read that first; if conventions in this README and `AGENTS.md` ever drift, `AGENTS.md` wins.

## Table of Contents

- [Two ways to build a Boogy service](#two-ways-to-build-a-boogy-service)
- [Quick start (hand-written)](#quick-start-hand-written)
- [The Api trait](#the-api-trait)
- [The wit_glue! macro](#the-wit_glue-macro)
- [Modules](#modules)
  - [router](#router) — HTTP routing
  - [response](#response) — response builders
  - [json](#json) — serde re-exports
  - [store](#store) — typed row + table builders
  - [rpc](#rpc) — JSON-RPC envelope and errors
- [Recipes](#recipes)
  - [Plain CRUD over a table](#plain-crud-over-a-table)
  - [JSON-RPC dispatcher](#json-rpc-dispatcher)
  - [Iterating raw store::find results](#iterating-raw-storefind-results)
  - [Calling the host's other capabilities](#calling-the-hosts-other-capabilities)
- [Building and deploying](#building-and-deploying)
- [Reference: WIT bindings layout](#reference-wit-bindings-layout)

## Two ways to build a Boogy service

1. **Hand-write it against the SDK** (this guide). You own the source, you understand every line, you can do anything Rust + the WIT capabilities allow. Best for non-trivial APIs, library code, performance-critical paths, or anything you'd want to read back later.
2. **Generate it from a typed spec** using the Boogy codegen service. Describe the API as JSON or YAML and let the codegen service emit the Rust + build the wasm. Best for CRUD-shaped APIs, AI-driven authoring (chat → spec → wasm), and rapid iteration.

Both paths produce wasm components that are interchangeable at deploy time. They share the same SDK surface — anything you learn from one transfers to the other.

## Quick start (hand-written)

A complete Boogy API for a notes service, including CRUD endpoints:

```rust
mod bindings {
    wit_bindgen::generate!({
        world: "service",
        path: "../../boogy-wit/wit",
    });
}

boogy_sdk::wit_glue!(bindings, NotesApi);

use boogy_sdk::Api;

struct NotesApi;

impl Api for NotesApi {
    fn init_tables() {
        create_table_from(
            &Table::new("notes")
                .text("title")
                .text("body")
                .text(DEFAULT_OWNER_COL),
        );
    }

    fn build_router() -> Router {
        Router::new()
            .get("/api/notes", list_notes)               // index — uses find_owned
            .post("/api/notes", create_note)             // create — stamps owner inline
            // Per-resource routes guarded by `owns_resource`: it loads
            // the row, denies-by-existence-mask if missing or
            // not-owned, and stashes the loaded row in `req.ctx` so
            // handlers don't re-fetch.
            .group([auth::owns_resource("notes", DEFAULT_OWNER_COL, "id")], |g| g
                .get("/api/notes/{id}", get_note)
                .delete("/api/notes/{id}", delete_note))
    }
}

#[derive(Serialize)]
struct NotesList { items: Vec<json::Value>, count: usize }

fn list_notes(_req: &mut Req<'_>) -> Result<Json<NotesList>, ApiError> {
    let rows = auth::find_owned("notes", DEFAULT_OWNER_COL)?;
    let items: Vec<_> = rows.iter().map(|r| r.to_json(&["title", "body"])).collect();
    let count = items.len();
    Ok(Json(NotesList { items, count }))
}

#[derive(Deserialize, garde::Validate)]
struct CreateNote {
    #[garde(length(min = 1, max = 200))]
    title: String,
    #[garde(length(max = 100_000))]
    body: String,
}

#[derive(Serialize)]
struct NoteOut { id: String, title: String, body: String }

fn create_note(req: &mut Req<'_>) -> Result<Created<NoteOut>, ApiError> {
    let principal = auth::current_principal().ok_or_else(ApiError::unauthenticated)?;
    let input: CreateNote = validate_body(req.body())?;
    let id = store::insert("notes", &[
        store::Column { name: "title".into(), val: store::Value::Text(input.title.clone()) },
        store::Column { name: "body".into(),  val: store::Value::Text(input.body.clone())  },
        store::Column { name: DEFAULT_OWNER_COL.into(), val: store::Value::Text(principal) },
    ])
    .map_err(ApiError::internal)?;
    Ok(Created(NoteOut { id, title: input.title, body: input.body }))
}

// owns_resource has already loaded the row and stashed it in ctx — read it.
fn get_note(req: &mut Req<'_>) -> Json<json::Value> {
    let row = req.ctx.require::<Row>();
    Json(row.to_json(&["title", "body"]))
}

fn delete_note(req: &mut Req<'_>) -> Result<NoContent, ApiError> {
    let id = req.params.get("id").unwrap_or("");
    let deleted = store::delete("notes", id).map_err(ApiError::internal)?;
    if deleted { Ok(NoContent) } else { Err(ApiError::not_found()) }
}
```

Handlers return one of:
- `Json<T>` / `Created<T>` / `NoContent` / `Redirect` — typed wrappers that
  render to the right status + body. Pure success path.
- `Result<R, ApiError>` where `R` is one of the wrappers above (or
  `Option<wrapper>`). The `?` operator propagates failures from
  `validate_body`, `auth::find_owned`, `store::*`, etc. into the structured
  RFC 7807 error response.

The legacy `response::ok(&body)` / `response::HttpResponse` shape still
compiles (every handler signature implements `IntoResponse` automatically),
but new code should prefer the typed wrappers + `?`.

That's the entire file. Build it with:

```bash
cargo build -p notes-api --target wasm32-wasip2 --release
```

Working code lives in the notes-api example (see the examples directory in the Boogy repository).

## The Api trait

```rust
pub trait Api {
    fn init_tables() {}                            // default: no-op
    fn build_router() -> boogy_sdk::router::Router;
}
```

Both methods are associated functions. `init_tables` runs once per request (idempotent — every `create_table` is `IF NOT EXISTS` under the hood). `build_router` is called per request to dispatch to handlers.

The macro wires both into the `Guest::handle` impl that the WIT layer calls.

## The wit_glue! macro

```rust
boogy_sdk::wit_glue!(bindings, NotesApi);
```

Two arguments: the `bindings` module (whatever you named the `wit_bindgen::generate!` block) and your API struct.

The macro emits, into your crate's namespace:

| Item | Purpose |
|---|---|
| `impl Guest for NotesApi` | The WIT-required handler. Calls `init_tables` then dispatches via `build_router`. |
| `bindings::export!(NotesApi with_types_in bindings)` | Registers your struct as the API export. |
| `create_table_from(&Table)` | Convert the SDK `Table` builder to a WIT `create_table` call. |
| `to_sdk_row(&store::Row) -> Row` | Convert a raw WIT row to the typed SDK `Row`. Used when you walk `store::find` results manually. |
| `get_row(table, id) -> Result<Option<Row>, RpcError>` | Read one row + convert in one call. Errors mapped to `RpcError::internal`. |
| `find_all_rows(table) -> Result<(Vec<Row>, u64), RpcError>` | List + convert all rows. |
| `auth::current_principal() -> Option<String>` | The caller's principal id, or `None` for anonymous requests. |
| `auth::required() -> Guard` | 401-responding `Router::guard(...)`. Use on routes that need authentication but don't load a specific resource (so `auth::owns_resource` doesn't apply) — e.g. a list endpoint paired with `auth::find_owned`. |
| `auth::find_owned(table, owner_col) -> Result<Vec<Row>, RpcError>` | List rows whose `owner_col` matches the current principal. Returns 401-coded `RpcError` when anonymous. |
| `auth::load_owned(table, owner_col, id) -> Result<Option<Row>, RpcError>` | Fetch one row only if owned by the caller. `None` for either "missing" or "not yours" — existence-mask. Use in MCP / JSON-RPC handlers where the resource id arrives in a body, not a path param. |
| `auth::owns_resource(table, owner_col, id_param) -> OwnsResource` | Builder for a `Router::guard(...)` that loads a row by id, gates on ownership (deny-by-existence-mask), and stashes the row in `req.ctx`. Add `.slot("name")` to disambiguate when multiple resources are loaded on one route. |
| `DEFAULT_OWNER_COL: &str = "owner_principal"` | The SDK convention for the owner-column name. Use this constant when declaring tables and calling the auth helpers — keeps multi-tenant ownership uniform across the fleet and makes future tooling (audit / migration / billing) possible without per-service column-name negotiation. |
| `use` statements for `Deserialize`, `Serialize`, `json`, `response`, `Params`, `Req`, `Router`, `Ctx`, `Row`, `Table`, `Val`, `store` | Common imports so handler code reads cleanly without per-file boilerplate. |

The macro also holds the WIT-bindings paths in one place. If WIT regenerates with different paths, this is the only thing to update.

## Spec endpoints

Every deployed service automatically serves `GET …/openapi.json`
(OpenAPI 3.0.3) and — when one or more `Router::rpc(...)` mounts exist
— `GET …/openrpc.json` (OpenRPC 1.3.2). No handler code required.

Opt-in controls:

- `Router::info(title, version, description)` — set the doc identity.
- `Router::undocumented(|g| …)` — register routes without recording
  them in the spec; they still dispatch normally.
- `schemars::JsonSchema` on payload types — required for typed
  extractors and responses to appear in the generated schema. Add
  `schemars = "0.8"` as a direct dep (or `{ workspace = true }`
  in-repo) and derive it alongside `Serialize`/`Deserialize`.
- `Router::mcp(path, handler)` — the canonical MCP mount; records the
  endpoint in `openapi.json` automatically.
- `Router::rpc(path, || Dispatcher::new()…)` — the canonical JSON-RPC
  mount; captures method shapes for `openrpc.json`.

Two-tier visibility: anonymous callers see only unguarded routes;
authenticated callers see everything.

## Modules

### `router`

Declarative request routing built on `matchit`, with standards-compliant method dispatch.

```rust
Router::new()
    .get("/api/users", list_users)
    .post("/api/users", create_user)
    .get("/api/users/{id}", get_user)              // named path param
    .put("/api/users/{id}", update_user)
    .delete("/api/users/{id}", delete_user)
    .get("/files/{*path}", serve_file)             // catch-all path param
    .route_many(&["GET", "POST"], "/sync", sync)   // same handler on multiple methods
```

**Path params:**
- `/{name}` — single-segment named parameter, read with `params.get("name")`.
- `/{*rest}` — catch-all, captures everything after the prefix. Read with `params.get("rest")`.

**Nesting:**

Mount one router under another with `.nest(prefix, sub_router)`. Useful for API versioning, admin scopes, and any non-trivial layout:

```rust
fn build_router() -> Router {
    Router::new()
        .nest("/api/v1", v1_routes())
        .nest("/api/v2", v2_routes())
        .nest("/admin", admin_routes())
        .get("/health", health)            // top-level routes coexist with nests
}

fn v1_routes() -> Router {
    Router::new()
        .get("/users", list_users)
        .post("/users", create_user)
        .get("/users/{id}", get_user)
}
```

Nesting is just path concatenation, so `outer.nest("/a", inner.nest("/b", ...))` produces routes under `/a/b`. Trailing slashes on the prefix are dropped; a sub-router's `/` route maps to the prefix itself (so a sub-router's index handler ends up at the prefix path).

**Guards:**

A guard is a pre-handler check that either lets the request through or short-circuits it with a response. Use them for auth, rate limiting, or any cross-cutting precondition.

```rust
fn require_admin(req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
    if req.header("x-admin-token") == Some("secret") {
        Ok(())
    } else {
        Err(response::bad_request("admin token required"))
    }
}

fn build_router() -> Router {
    Router::new()
        .get("/health", health)                    // not guarded
        .nest("/admin",
            Router::new()
                .group([require_admin], |g| g      // guards every route in the closure
                    .get("/users", admin_list_users)
                    .post("/users", admin_create_user)))
}
```

Guards may also write into `req.ctx` to pass loaded resources, parsed bodies, or cached lookups through to the handler — see the `auth::owns_resource` factory in `wit_glue!`-emitted helpers, which loads a row by id, gates on ownership, and stashes the row in `req.ctx` for the handler to read via `req.ctx.require::<Row>()`.

Semantics:
- `.group([g1, g2, ...], |r| r.get(...).post(...))` applies the guard array to every route registered inside the closure. Routes outside the closure are not affected. Multiple `.group()` calls are independent — each route gets only its own group's guards.
- When a router is nested inside another, the **outer router's guards run first**, then the sub-router's. An outer rejection short-circuits before any inner guard fires.
- HEAD requests that fall back to GET still run the GET route's guards.
- OPTIONS auto-responses don't run guards (they're just metadata about supported methods — important for CORS preflight).
- Guards can't observe or mutate the response. For that, write a wrapper handler manually.

**Handler signature.** Canonical: `fn(&mut Req<'_>) -> Result<R, ApiError>` where `R` implements `IntoResponse` (`Json<T>`, `Created<T>`, `NoContent`, `Redirect`, `Option<T>`, etc.). The `?` operator flows through every fallible step. Always-succeeds handlers can drop the `Result` and return the wrapper directly. The legacy `-> response::HttpResponse` shape still compiles for cases that need full header control.

**Guard signature:** `fn(&mut Req<'_>) -> Result<(), response::HttpResponse>` — short-circuits the request with the `Err` response when the precondition fails.

The `Req` exposes accessors — prefer them over reaching through `req.request.X`:
- `req.body() -> Option<&[u8]>` — request body bytes
- `req.header(name) -> Option<&str>` — case-insensitive header lookup (HTTP convention)
- `req.method() -> &str`, `req.path() -> &str`, `req.query(name) -> Option<&str>`
- `req.params.get("id").unwrap_or("")` or `req.params.require("id")?` — extracted path params
- `req.ctx.require::<T>()`, `req.ctx.get::<T>()` (and `*_at(slot)` variants) — typed extension bag populated by upstream guards
- `req.request` — the raw inbound `boogy_sdk::Request`, kept public for cases that need to hand it off to `mcp::McpServer::handle` / `rpc::Dispatcher::handle`

**Dispatch behaviour:**

| Situation | Result |
|---|---|
| Path matched, method registered | run that handler |
| Path matched, no HEAD handler, GET registered | run GET, strip the response body (RFC 9110 §9.3.2) |
| Path matched, no OPTIONS handler | `204 No Content` with `Allow:` header listing supported methods (incl. HEAD if GET present, OPTIONS itself) |
| Path matched, method not registered | `405 Method Not Allowed` with `Allow:` header |
| No path matched | `404 Not Found` |

Method matching is case-insensitive on the wire (`Request.method` can be lowercase).

### `response`

Two complementary surfaces:

- **Typed `IntoResponse` wrappers** for handlers that just want to return a payload — `Json(t)`, `Created(t)`, `NoContent`, `Redirect::to(url)`, `Option<T>` (`None` → 404), `Result<T, ApiError>` (errors as RFC 7807).
- **Status-typed builders** for the cases where you need full header control:

```rust
response::ok(&body)            // 200, JSON via Serialize
response::created(&body)       // 201, JSON
response::no_content()         // 204
response::redirect(&url)       // 302
response::raw(status, body, content_type)  // any status, raw bytes

// Error builders — every one produces application/problem+json (RFC 7807):
response::bad_request("msg")
response::unauthenticated()
response::forbidden("msg")
response::not_found()
response::conflict("msg")
response::server_error("msg")
```

`body` is anything that implements `serde::Serialize` — typed structs, `serde_json::Value`, `json::json!({...})`, etc.

Errors should normally come from `ApiError::*` constructors and propagate through `?`. The `response::*` error builders are the same shape on the wire — they exist so handlers that produce `HttpResponse` directly stay terse.

### `json`

Re-exports of `serde` and `serde_json` so you don't need them in your `Cargo.toml` separately. `json::Deserialize`, `json::Serialize`, `json::json!`, `json::from_slice`, `json::from_value`, `json::to_value`, etc.

### `store`

SDK-side types for table definition and row reading. The `wit_glue!` macro generates the converters between these and the WIT-side `bindings::boogy::platform::store` types.

```rust
// Table builder — used in init_tables
let table = Table::new("users")
    .text("email")
    .text("name")
    .nullable_text("avatar_url")
    .integer("created_at");
create_table_from(&table);

// Row accessors (after get_row / find_all_rows / to_sdk_row)
let email:   String = row.text("email");
let count:   i64    = row.int("login_count");
let active:  bool   = row.bool("is_active");
let created: f64    = row.real("created_at");
let id:      String = row.id();              // shorthand for row.text("_id")

// Serialize selected fields back to JSON (always includes "id")
let body = row.to_json(&["email", "name", "is_active"]);
```

The pagination helpers `Page<T>` and `CursorPage<T>` (in `boogy_sdk::pagination`) cover offset and cursor-style listings.

### `rpc`

JSON-RPC 2.0 envelope and error types. Use these when you want to expose a single dispatcher endpoint that fans out to multiple methods by name (the [JSON-RPC dispatcher](#json-rpc-dispatcher) recipe below shows the full pattern).

```rust
pub struct Request {                          // envelope from the wire
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
    pub id: Option<serde_json::Value>,
}

pub struct RpcError {                         // typed error
    pub code: i64,
    pub message: String,
}

// Constructors that map to standard JSON-RPC codes:
RpcError::parse_error(msg)        // -32700
RpcError::invalid_request(msg)    // -32600
RpcError::method_not_found(msg)   // -32601
RpcError::invalid_params(msg)     // -32602
RpcError::internal(msg)           // -32603
RpcError::application(code, msg)  // any positive code

// Response builders:
rpc::success_response(id, &result)
rpc::error_response(id, &err)

// Declarative dispatcher (preferred — see the JSON-RPC recipe below):
rpc::Dispatcher::new()
    .method("name", typed_handler)
    .handle(&req)
```

`String` and `&str` auto-convert into `RpcError::internal(...)` via `From` impls, so the `?` operator works inside method handlers that return `Result<T, RpcError>`.

## Recipes

### Plain CRUD over a table

The [Quick start](#quick-start-hand-written) above is exactly this. The pattern:

1. Declare the table in `init_tables` via `Table::new(...)` + `create_table_from`.
2. Wire CRUD routes in `build_router`.
3. Each handler uses `get_row` / `find_all_rows` for reads, `store::insert` / `store::update` / `store::delete` for writes, validates with normal Rust expressions, and returns through a `response::*` builder.

### JSON-RPC dispatcher

For custom logic that doesn't fit the CRUD shape, expose a single dispatcher endpoint that routes by `method` in the request body. The SDK's `Dispatcher` builder hides all the envelope parsing, params decoding, and error mapping — you just register typed handlers by name:

```rust
fn build_router() -> Router {
    Router::new()
        // ... CRUD routes ...
        .post("/api/notes/rpc", rpc_dispatch)
}

fn rpc_dispatch(req: &mut Req<'_>) -> response::HttpResponse {
    boogy_sdk::rpc::Dispatcher::new()
        .method("search_notes", search_notes)
        .method("share_note", share_note)
        .handle(req.request)
}

#[derive(Deserialize)]
struct SearchParams { query: String }

#[derive(Serialize)]
struct SearchResult { items: Vec<serde_json::Value> }

fn search_notes(params: SearchParams) -> Result<SearchResult, boogy_sdk::rpc::RpcError> {
    let (rows, _) = find_all_rows("notes")?;
    let q = params.query.to_lowercase();
    let items: Vec<_> = rows.iter()
        .filter(|r| r.text("title").to_lowercase().contains(&q))
        .map(|r| r.to_json(&["title", "body"]))
        .collect();
    Ok(SearchResult { items })
}
```

Method handlers are typed `fn(P) -> Result<R, RpcError>` where `P: Deserialize` and `R: Serialize`. The dispatcher decodes incoming params into `P` and serialises the returned `R`, mapping each failure mode to the right JSON-RPC error code:

| Failure | JSON-RPC code |
|---|---|
| Missing body | `-32600 invalid_request` |
| Body not parseable as an envelope | `-32700 parse_error` |
| Unknown method | `-32601 method_not_found` |
| Params don't match handler's `P` | `-32602 invalid_params` |
| Handler returned `Err(RpcError)` | passed through as-is |
| Result serialisation failed | `-32603 internal` |

The `?` operator works inside method handlers because `String → RpcError::internal` is a free conversion.

This is exactly the pattern the codegen pipeline emits for spec-declared `methods:`. Hand-rolling it is just as clean.

### Iterating raw store::find results

When you need filters, sorting, or pagination beyond what `find_all_rows` provides, drop to `store::find` directly. Convert each row with `to_sdk_row(...)` to get typed accessors:

```rust
let opts = store::FindOptions {
    filters: vec![
        store::Filter {
            column: "is_active".into(),
            op: store::FilterOp::Eq,
            val: store::Value::Boolean(true),
        },
    ],
    sort: vec![
        store::SortBy { column: "created_at".into(), dir: store::SortDir::Desc },
    ],
    page: Some(store::Page { limit: 50, offset: 0 }),
};
let result = store::find("users", &opts).map_err(rpc::RpcError::internal)?;
let users: Vec<_> = result.rows.iter()
    .map(|r| {
        let row = to_sdk_row(r);
        json::json!({
            "id": row.id(),
            "email": row.text("email"),
            "logins": row.int("login_count"),
        })
    })
    .collect();
```

### Calling the host's other capabilities

The host exposes these capabilities through the WIT bindings (gated by the deployment manifest's `[capabilities]` block):

- `store` — the per-service isolated database (raw SQL + structured CRUD)
- `auth` — caller identity and role checks
- `clock` — wall-clock time and monotonic instants
- `entropy` — secure random bytes
- `logging` — structured logs the host captures

Each is reachable through `bindings::boogy::platform::<capability>::<fn>(...)`. The SDK doesn't (yet) wrap most of these — call them directly from your handlers. See [`crates/boogy-wit/wit`](../boogy-wit/wit) for the full surface.

## Building and deploying

Required workspace setup (already true in the Boogy repo):

```toml
# Cargo.toml of your API crate
[package]
name = "my-api"
version = "0.1.0"
edition = "2021"

[dependencies]
boogy-sdk = { path = "../../boogy-sdk" }    # or a registry version
wit-bindgen   = "0.39"
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"

[lib]
crate-type = ["cdylib"]
```

```bash
# Build the wasm component
cargo build -p my-api --target wasm32-wasip2 --release

# Deploy via the Boogy CLI
boogy deploy crates/examples/my-api/boogy.toml

# Or directly via the host's admin endpoint
curl -X POST http://host:3000/_admin/deploy \
  -F manifest=@crates/examples/my-api/boogy.toml \
  -F wasm=@target/wasm32-wasip2/release/my_api.wasm
```

The manifest declares routing prefix, owner, capabilities, and resource limits. See the Boogy documentation for the full manifest schema.

## Reference: WIT bindings layout

After `wit_bindgen::generate!({ world: "service", path: "..." })` runs, your crate has these paths:

```
bindings::
├── exports::boogy::platform::http_handler::
│   ├── Guest                 (trait — implemented by wit_glue!)
│   ├── HttpRequest           (the WIT request type)
│   └── HttpResponse          (the WIT response type)
└── boogy::platform::
    ├── store::               (database capability — used directly in handlers)
    │   ├── create_table, insert, get, find, update, delete, ...
    │   ├── Column, ColumnDef, ColumnType, Value
    │   ├── FindOptions, FindResult, Filter, FilterOp, SortBy, SortDir, Page
    │   └── Row
    ├── auth::                (caller identity)
    ├── clock::               (time)
    ├── entropy::             (random bytes)
    └── runtime::             (logging)
```

The `wit_glue!` macro hides most of this — the `use store;` it emits puts the WIT store module in scope, so handler code reads `store::insert(...)` directly.
