# boogy-sdk: handler-authoring reference

Canonical reference for everything available inside a Boogy service
handler or guard. **This is the doc to read first when writing or
generating a Boogy service.** The SDK README has the long-form
narrative; this file is the dense, scannable cheat sheet that the
codegen LLM and human authors both work from.

If you find a pattern in this codebase that contradicts what's in this
file, treat the file as authoritative and fix the codebase.

## File shape

A Boogy service is a single Rust crate. The boilerplate is tiny — the
`wit_glue!` macro emits everything needed for the WIT host bridge, so
user code stays focused on tables, routes, and handlers.

```rust
mod bindings {
    wit_bindgen::generate!({ world: "service", path: "../../boogy-wit/wit" });
}
boogy_sdk::wit_glue!(bindings, MyApi);

// Optional — adds /_keys management endpoints + a sk_* bearer guard.
boogy_sdk::api_keys_glue!(bindings);

use boogy_sdk::Api;

struct MyApi;

impl Api for MyApi {
    fn init_tables() {
        create_table_from(&Table::new("things").text("name").text(DEFAULT_OWNER_COL));
    }

    fn build_router() -> Router {
        Router::new()
            .get("/api/things", list_things)
            .post("/api/things", create_thing)
            .group([auth::owns_resource("things", DEFAULT_OWNER_COL, "id")], |g| g
                .get("/api/things/{id}", get_thing)
                .delete("/api/things/{id}", delete_thing))
    }
}
```

### Two WIT worlds

Two worlds available in `wit_bindgen::generate!`:

- `world: "service"` — REST/JSON-RPC/MCP only. The most common case.
- `world: "service-with-jobs"` — adds the `job-handler` export. **If you use this world, you MUST also `impl bindings::exports::boogy::platform::job_handler::Guest for MyApi`** with a `handle_job(ctx, payload)` body, or the crate won't compile. Even a `fn handle_job(ctx, _) { Err(HandlerError::Terminal(format!("unknown handler: {}", ctx.handler))) }` stub is enough until you wire the real dispatch. See the tokenfeed example in the Boogy repository for the canonical pattern (uses scheduled handlers with `#[background_jobs.handlers.*]`).

### Cargo.toml conventions

Example crates use `[lib] crate-type = ["cdylib"]` — wasm component output only. **Do not add `rlib`** to a wasm-target lib: `wit_bindgen::generate!` emits wasm-component-only symbols (e.g. `boogy:platform/http-handler@0.1.0#handle`) that don't host-link, and `cargo test --workspace` fails at link time when cargo tries to build the rlib variant for the host triple.

**Convention for host-testable pure logic:** extract it into a sibling `*-core` crate that has no `wit_bindgen` / `boogy-sdk` deps. The wasm crate depends on the core crate; tests live on the core crate where `cargo test --workspace` runs them cleanly.

```
crates/examples/myapp/         # cdylib only; depends on myapp-core
  src/lib.rs                   # Api impl, handlers, routes
  src/posts.rs                 # imports `use myapp_core::extract::...`

crates/examples/myapp-core/    # rlib only (default); pure Rust
  Cargo.toml                   # serde + your domain deps; NO boogy-sdk
  src/lib.rs                   # pub mod extract; pub mod scoring; ...
  src/extract.rs               # #[cfg(test)] mod tests { ... } — runs on host
```

Matches the platform's own `boogy-jobs-core` (pure logic) / `boogy-jobworker` (binary) split. The tokenfeed example in the Boogy repository demonstrates this pattern.

## Handler / guard signature

```rust
// Canonical: Result-typed, ? flows through every fallible step.
fn handler(req: &mut Req<'_>) -> Result<R, ApiError>
where R: IntoResponse;

// Always-succeeds form (no ? needed):
fn handler(req: &mut Req<'_>) -> R
where R: IntoResponse;

// Guard:
fn guard(req: &mut Req<'_>) -> Result<(), response::HttpResponse>;
```

`R: IntoResponse` is satisfied by `Json<T>` (200), `Created<T>` (201),
`NoContent` (204), `Redirect` (302), `Option<T>` (None → 404),
`HttpResponse` (identity), and `()` (204). Custom types can `impl
IntoResponse` to control their own wire shape.

Both handler shapes take `&mut Req<'_>` — a single per-request bundle.

The legacy `-> response::HttpResponse` signature still compiles —
`HttpResponse: IntoResponse` is identity — and is the right choice
for handlers that need full control over headers (custom status,
streaming, content-type negotiation). New code should default to the
Result-typed shape: it's shorter, the error wire format is consistent
(RFC 7807), and `?` removes the boilerplate.

### Req accessors

Always prefer accessor methods over reaching through `req.request.X`:

| Accessor | Returns | Notes |
|---|---|---|
| `req.method()` | `&str` | HTTP method as received. Compare with `eq_ignore_ascii_case`. |
| `req.path()` | `&str` | Full request path. |
| `req.body()` | `Option<&[u8]>` | Request body bytes. `None` for body-less methods or empty bodies. |
| `req.header(name)` | `Option<&str>` | Case-insensitive header lookup (HTTP convention). |
| `req.query(name)` | `Option<&str>` | Case-sensitive query-param lookup. |
| `req.params.get("id")` | `Option<&str>` | Path-param lookup. |
| `req.params.parse::<T>("id")` | `Result<T, ApiError>` | Typed path-param decode via `FromStr` (`i64`, `Uuid`, etc.). Missing/invalid → 400. |
| `req.ctx.require::<T>()` | `&T` | Read a value an upstream guard stashed. Panics if missing. |
| `req.ctx.get::<T>()` | `Option<&T>` | Same but optional. |
| `req.ctx.require_at::<T>(slot)` | `&T` | Same with named slot — for multi-resource routes. |
| `req.request` | `&Request` | Raw fallback. Used only to hand off to `mcp::McpServer::handle` / `rpc::Dispatcher::handle`. |

## Authentication & authorization

Everything auth-related lives under `auth::*` — this is the canonical
namespace. Do not reach into `bindings::boogy::platform::auth`
directly.

| Item | Purpose |
|---|---|
| `auth::current_principal()` → `Option<String>` | Caller's principal id. `None` for anonymous requests. |
| `auth::required()` → `Guard` | 401 guard for routes that require any authenticated caller (no specific resource). Pair with `auth::find_owned` or business logic that reads identity. |
| `auth::owns_resource(table, owner_col, id_param)` → `OwnsResource` (impl `IntoGuard`) | Builder for a `Router::guard(...)`. Loads the row identified by `req.params[id_param]`, denies-by-existence-mask (404) if the row is missing **or** isn't owned by `current_principal()`, and stashes the loaded row in `req.ctx`. Add `.slot("name")` for multi-resource routes. |
| `auth::find_owned(table, owner_col)` → `Result<Vec<Row>, RpcError>` | Principal-scoped row list for index endpoints. Returns 401-coded `RpcError` when anonymous. |
| `auth::load_owned(table, owner_col, id)` → `Result<Option<Row>, RpcError>` | Single-row load with ownership check. `Ok(None)` for both "missing" and "not yours". Use in MCP / JSON-RPC handlers (where the resource id arrives in a body, not a path param). |
| `DEFAULT_OWNER_COL: &str` | Convention column name (`"owner_principal"`). Use this constant in tables and auth helper calls. Do not invent alternatives like `"owner_id"` / `"created_by"`. |

> **Principal opacity:** `current_principal()` returns an opaque `String`. Today that string is the caller's global agent id; once the end-user app-session tier lands, it will be a per-app **pairwise pseudonym** (`pw_…`) for callers reaching the service through the browser-side `@boogy/web` SDK. The change is transparent to handler code — **do not parse the principal, do not assume it's a UUID, do not strip prefixes**. Use it only as an opaque key in your `owner_principal` columns and as the input to `auth::*` helpers. Code that follows this rule keeps working unchanged across the cutover.

**Two auth patterns you'll write most often:**

REST resource access — guard does the work, handler just reads:
```rust
.group([auth::owns_resource("notes", DEFAULT_OWNER_COL, "id")], |g| g
    .get("/api/notes/{id}", get_note))

fn get_note(req: &mut Req<'_>) -> Json<json::Value> {
    let row = req.ctx.require::<Row>();
    Json(row.to_json(&["title", "body"]))
}
```

REST list endpoint — `auth::find_owned` carries the auth check;
its `RpcError` converts to `ApiError` via `?`:
```rust
#[derive(Serialize)]
struct NotesList { items: Vec<json::Value>, count: usize }

fn list_notes(_req: &mut Req<'_>) -> Result<Json<NotesList>, ApiError> {
    let rows = auth::find_owned("notes", DEFAULT_OWNER_COL)?;
    let items: Vec<_> = rows.iter().map(|r| r.to_json(&["title", "body"])).collect();
    let count = items.len();
    Ok(Json(NotesList { items, count }))
}
```

## Validation & structured errors

For request body parsing, prefer `validate_body::<T>(req.body())` over
hand-rolled `json::from_slice` + ad-hoc field checks. It combines JSON
parsing and `garde`-driven validation in one call, returning a
structured `ApiError` on any failure (missing body / bad JSON / failed
validation). The error converts cleanly to `HttpResponse` via `.into()`.

Add `garde = { workspace = true }` to your crate's `Cargo.toml` (it's
a direct dep because the derive macro emits absolute `::garde::*`
paths).

```rust
#[derive(Deserialize, garde::Validate)]
struct CreateNote {
    #[garde(length(min = 1, max = 200))]
    title: String,
    #[garde(length(max = 100_000))]
    body: String,
    #[garde(email)]
    notify: Option<String>,
}

fn create_note(req: &mut Req<'_>) -> Result<Created<NoteOut>, ApiError> {
    let input: CreateNote = validate_body(req.body())?;  // parses AND validates
    // ... build columns, insert, return Created(...) ...
}
```

`ApiError` is the canonical structured-error shape (RFC 7807 / 
`application/problem+json`). Constructors for the common cases:

| Constructor | Status | Use when |
|---|---|---|
| `ApiError::bad_request(msg)` | 400 | Malformed input that doesn't fit a validation report. |
| `ApiError::unauthenticated()` | 401 | Caller is anonymous on a route that requires auth. |
| `ApiError::forbidden(msg)` | 403 | Authenticated but missing scope or permission. |
| `ApiError::not_found()` | 404 | Resource missing — also "exists but not yours" (existence-mask). |
| `ApiError::conflict(msg)` | 409 | Uniqueness violation, version mismatch. |
| `ApiError::unprocessable(msg)` | 422 | Domain invariant violation that isn't a structured `garde::Report` (e.g. "too many mentions", quota / balance violations, business-rule rejections with a freeform message). |
| `ApiError::validation(report)` | 422 | Per-field validation report from `garde::Validate::validate()`. |
| `ApiError::internal(msg)` | 500 | Unexpected failure. Don't include sensitive details. |

`ApiError` also implements `From<store::StoreError>` (and `From<String>`,
which lifts a raw String error to `internal`). The `StoreError`
conversion preserves the variant → status mapping (`Conflict` → 409,
`Unsupported` → 501, …), which is what makes bare `?` on `store::*` /
`db_*` / `find_row_by` work inside a `tx::<_, _, ApiError>(|| { ... })`
closure while still surfacing the right HTTP status. The pattern enables
raising **structured** errors (404, 409, 422) from inside a transaction
instead of flattening every failure to 500 at the boundary.

`ApiError` implements `Into<HttpResponse>` and `Into<RpcError>`, so the
same value flows through REST handlers, JSON-RPC dispatch, and MCP
tools without translation. The wire format is
`application/problem+json` with `{type, title, status, detail, errors}`
fields per RFC 7807.

The legacy `response::bad_request(msg)` / `response::not_found()` /
`response::server_error(msg)` builders are now thin wrappers over
`ApiError::*().into()` — every error response from the SDK is RFC
7807 `application/problem+json` regardless of which builder you
call. Reach for `ApiError::*` directly when you want `?`-propagation
in a Result-typed handler; reach for `response::*` when you're
already in a code path producing `HttpResponse` directly.

## Tables & storage

`init_tables()` runs on every request — every `create_table_from` call
is `IF NOT EXISTS`, so it's idempotent and effectively free. Use the
SDK's `Table` builder; pass it to `create_table_from`.

```rust
create_table_from(
    &Table::new("things")
        .text("name")                    // NOT NULL TEXT
        .nullable_text("description")
        .integer("priority")
        .boolean("done")
        .text(DEFAULT_OWNER_COL)
        .unique_index("by_name", &["name"]),
);
```

Available column types: `.text`, `.nullable_text`, `.integer`,
`.nullable_integer`, `.real`, `.boolean`, `.blob`. Constraints:
`.unique()`, `.references(table, col)`, `.on_delete(CascadeAction::*)`,
`.on_update(CascadeAction::*)`. Indexes: `.index(name, &cols)` and
`.unique_index(name, &cols)`.

**Composite + unique indexes are fully enforced.** `Table::unique_index(name, &cols)` (and the raw `IndexDef { columns, unique: true }`) creates a real composite unique constraint in the built-in store. Duplicate rows that violate the constraint are rejected at insert time with a `StoreError::Conflict`. The enforcement is required by `upsert_increment` — always create a unique index on the key columns first.

### Column + table names go in constants, not string literals

The illustrative snippet above uses bare string literals (`"things"`,
`"name"`, `"priority"`) for brevity, but **production handlers must
not**. Every table name, column name, and index name your crate uses
belongs in a single `cols` module — one tag struct per table holding
associated `pub const` items for `TABLE` plus every column. Handlers,
jobs, init_tables, and migrations reference the constants so that a
rename is one edit and a typo is a compile error, not a silent 500.

```rust
// crates/your-api/src/cols.rs
pub struct Common;
impl Common {
    /// Store auto-PK as it appears in row data + FindOptions sort/filter columns.
    pub const AUTO_ID: &str = "_id";
}

pub struct Things;
impl Things {
    pub const TABLE: &str = "things";
    pub const NAME: &str = "name";
    pub const DESCRIPTION: &str = "description";
    pub const PRIORITY: &str = "priority";
    pub const DONE: &str = "done";
    pub const OWNER: &str = DEFAULT_OWNER_COL;

    pub const IDX_BY_NAME: &str = "by_name";
}

// crates/your-api/src/lib.rs
mod cols;

fn init_tables() {
    use crate::cols::Things;
    create_table_from(
        &Table::new(Things::TABLE)
            .text(Things::NAME)
            .nullable_text(Things::DESCRIPTION)
            .integer(Things::PRIORITY)
            .boolean(Things::DONE)
            .text(Things::OWNER)
            .unique_index(Things::IDX_BY_NAME, &[Things::NAME]),
    );
}

// crates/your-api/src/handlers.rs
use crate::cols::Things;

// PK lookup: use get_row(table, id), NOT find_row_by(table, "_id", ...).
// `find_row_by` filters on a named column; the auto-PK is not a column
// the store find-scan recognises, so PK lookups go through the dedicated
// get-by-id helper.
let row = get_row(Things::TABLE, id)?;
row.as_ref().map(|r| r.text(Things::NAME))
```

The store exposes its auto-PK as `_id` in row data and in `FindOptions`
sort/filter columns. For **PK lookups** use `get_row(table, id)` (or
`t.get(table, id)` in-tx) — it routes through the host's dedicated
`store::get(table, id)` op. `find_row_by` is for filtering on **named columns** (indexed
or otherwise), not the auto-PK. (Historical: an earlier `PK_ALIAS = "id"`
constant in tokenfeed presumed the SDK aliased `"id"` → `"_id"`; it
doesn't.)

Exceptions to the constants rule:
- Migration literals (the table/column names passed to
  `m.add_column("things", &col("priority", ...))` etc.) stay literal,
  **not** cols-module constants. A shipped migration is immutable history;
  if you rename `Things::NAME`, you write a new migration that does the
  rename, not edit the old one.
- Router path-parameter names (`req.params.get("id")`) are HTTP
  route concerns, not column concerns — they live in their route
  definition, not the cols module.

Reference example: the tokenfeed example's `cols.rs` in the Boogy repository.

### Reading rows

`Row` is the typed row accessor returned by `to_sdk_row` and the
SDK helpers. Read columns by name:

```rust
row.id()            // shorthand for row.text("_id")
row.text("title")   // String
row.int("priority") // i64
row.real("score")   // f64
row.bool("done")    // bool
row.text_opt("description")  // Option<String> for nullable cols
row.to_json(&["title", "body"])  // serde_json::Value, includes id
```

| Helper | Returns | Use for |
|---|---|---|
| `get_row(table, id)` | `Result<Option<Row>, StoreError>` | Single row by `_id`. |
| `find_all_rows(table)` | `Result<(Vec<Row>, u64), StoreError>` | Unfiltered list + total count. Use only when you specifically need all rows; for principal-scoping prefer `auth::find_owned`. |
| `find_row_by(table, column, store::Value)` | `Result<Option<Row>, StoreError>` | First row matching `column = val`. Takes the WIT `store::Value` directly (e.g. `store::Value::Text("alice".into())`), same value type used by `store::insert` / `store::update`. |
| `find_rows_by(table, column, store::Value)` | `Result<Vec<Row>, StoreError>` | **All** rows matching `column = val` (no limit). Use for unbounded backer lists, owned-resource enumeration, etc. — the index makes this an indexed scan. |
| `find_rows(table, filters, sort, page)` | `Result<(Vec<Row>, u64), StoreError>` | General-purpose composite query: multi-filter, composite sort, optional page. `filters` is `Vec<store::Filter>`, `sort` is `Vec<store::SortBy>`. Composite sort lets you tiebreak (e.g. `created_at DESC, _id ASC`). For one-filter cases prefer `find_rows_by`; for an OR clause use `find_rows_grouped`. |
| `find_rows_grouped(table, filters, or_groups, sort, page)` | `Result<(Vec<Row>, u64), StoreError>` | Like `find_rows` but with an OR-of-AND clause: a row matches when `ALL(filters) AND (or_groups empty OR ANY(group: ALL(group)))`. `or_groups` is `Vec<Vec<store::Filter>>` — each inner `Vec` is one AND-group, groups are ORed. Use for composite keyset pagination (see below). Empty `or_groups` == `find_rows`. |
| `upsert_increment(table, key, counter, delta, set)` | `Result<u64, StoreError>` | Atomic keyed counter: inserts the row (counter = delta + set columns) if it doesn't exist, or increments the counter and overwrites the set columns if it does. `key` is `&[store::Column]` identifying the unique key. `counter` is the column name. `delta` must be `store::Value::Integer` or `store::Value::Real` — the host rejects other types. `set` is `&[store::Column]` for extra columns to write on every call. Returns the row id. **Requires a `unique_index` on the key columns** — the operation is undefined without it. Use for per-key aggregations (e.g. view counts, score accumulators, per-tag event counts). |
| `for_each_batch(table, filters, or_groups, order_col, dir, batch_size, f)` | `Result<(), StoreError>` | Bounded-memory ordered streaming over a table. Opens a stateless `row-cursor`, calls `f(&[Row])` once per batch of up to `batch_size` rows, and loops until the table is exhausted. Every matching row is visited exactly once, in `order_col` / `dir` order — **on the built-in engine `order_col` is an *index name*, not a column name**: pass a declared index's name, or `None` for primary-key order (a bare column name errors with `NotFound`) — with no gaps or duplicates, no offset re-scan. **Read-committed, not snapshot-isolated**: rows inserted or modified after the cursor opens may or may not appear depending on timing. **Cannot be called inside `tx(...)`** — the transaction view has no cursor, so it returns `Unsupported` (501); gather the ids you need before opening the tx, or use `find_rows` inside it. This is the bounded-memory alternative to `find_all_rows` / offset pagination for large-table batch jobs (e.g. decay sweeps, export pipelines, fan-out tasks): only `batch_size` rows are ever in memory at a time. If `f` returns `Err`, iteration stops and the error propagates. |
| `filter_eq(column, val)` | `store::Filter` | One-liner builder for `column = val`. One of a family (see below) — never hand-write the `Filter { column, op, val, in_values }` literal. |
| `sort_asc(col)` / `sort_desc(col)` | `store::SortBy` | Build a sort key without spelling out `SortBy { column, dir }`. Compose into the `Vec<SortBy>` for `find_rows`. |
| `page(limit, offset)` | `store::Page` | Build a `Page`; wrap in `Some(...)`. First page = `page(n, 0)`. |
| `now_millis()` | `u64` | Unix-millis time. Wraps `runtime::now_millis()` so submodules don't need to spell out the full bindings path. |

**Filter builders — one per `store::FilterOp`, so you never hand-write a `Filter` literal:**

| Builder | Predicate |
|---|---|
| `filter_eq(col, val)` | `col = val` |
| `filter_neq(col, val)` | `col != val` |
| `filter_gt(col, val)` / `filter_gte(col, val)` | `col > val` / `col >= val` |
| `filter_lt(col, val)` / `filter_lte(col, val)` | `col < val` / `col <= val` |
| `filter_like(col, val)` / `filter_not_like(col, val)` | `col LIKE val` / `col NOT LIKE val` (`%`/`_` wildcards) |
| `filter_is_null(col)` / `filter_is_not_null(col)` | `col IS NULL` / `col IS NOT NULL` (no value arg) |
| `filter_in(col, vals: Vec<store::Value>)` | `col IN (vals)` (the only op that uses `in_values`) |

All return `store::Filter`; compose them in the `Vec<store::Filter>` you pass to `find_rows` / `count`. Prefer these over building `Filter { column, op, val, in_values }` by hand — the literal is verbose and the `in_values` field is a footgun (only `filter_in` sets it).

**OR clauses + composite keyset pagination.** `find_rows`'s `filters` are AND-only. When you need OR — most commonly *correct* keyset pagination over a composite sort — use `find_rows_grouped`. The page after `(score, _id) = (c, cursor)` ordered `score DESC, _id DESC` is `score < c OR (score = c AND _id < cursor)`:

```rust
let (page_rows, _total) = find_rows_grouped(
    "posts",
    vec![filter_eq("deleted_at", store::Value::Text(String::new()))],   // AND-prefix
    vec![
        vec![filter_lt("score", store::Value::Integer(c))],
        vec![filter_eq("score", store::Value::Integer(c)),
             filter_lt("_id",   store::Value::Integer(cursor))],
    ],
    vec![sort_desc("score"), sort_desc("_id")],
    Some(page(limit, 0)),
)?;
```

The built-in store applies the OR-of-AND natively. Note: when `or_groups` is non-empty the query can't use the single-column index/scan fast paths, so the AND-prefix `filters` is what keeps it cheap — keep a selective prefix where you can.

**`upsert_increment` — atomic keyed counter.** Requires a unique composite index on the key columns. First call inserts; subsequent calls increment and overwrite the set columns atomically. Returns the row id (same id across all calls for the same key).

```rust
// init_tables():
create_table_from(
    &Table::new("post_views")
        .text("post_id")
        .text("region")
        .integer("count")
        .unique_index("by_post_region", &["post_id", "region"]),
);

// in a handler / background job:
upsert_increment(
    "post_views",
    &[
        store::Column { name: "post_id".into(), val: store::Value::Text(post_id.clone()) },
        store::Column { name: "region".into(),  val: store::Value::Text(region.clone()) },
    ],
    "count",
    store::Value::Integer(1),
    &[],  // no extra set columns
)?;
```

**`for_each_batch` — bounded-memory ordered streaming.** The alternative to `find_all_rows` / offset pagination for large-table batch jobs. Only `batch_size` rows are materialized at a time; the cursor resumes strictly after the last row of the prior batch (no offset re-scan). Read-committed: rows inserted or modified after the cursor opens may or may not appear.

```rust
// Stream "post_views" ordered by count DESC, 100 rows at a time.
for_each_batch(
    "post_views",
    vec![],         // no filters — all rows
    vec![],         // no or_groups
    Some("count"),  // order by count; None = primary-key order
    store::SortDir::Desc,
    100,
    |batch| {
        for row in batch {
            // row is a &Row; process it here.
            let count = row.int("count");
            // ...
        }
        Ok(())
    },
)?;
```

`StoreError` variants (9 arms, all carry a `String` message):
`QuotaExceeded(String)`, `NotFound(String)`, `Conflict(String)`,
`ConstraintViolation(String)`, `InvalidArgument(String)`,
`Unsupported(String)`, `Timeout(String)`, `VersionMismatch(String)`,
`Internal(String)`. The `From<StoreError> for ApiError` impl maps them to
507 / 404 / 409 / 409 / 400 / 501 / 504 / 409 / 500 respectively.
Unique-index violations surface as `Conflict`; FK / check / not-null
violations surface as `ConstraintViolation`.

**Error-handling pattern by return type:**

| Function returns | Idiom |
|---|---|
| `Result<_, StoreError>` (the typed helpers: `get_row`, `find_all_rows`, `find_row_by`, `find_rows`, `find_rows_by`) | Use bare `?` — the `From<StoreError> for ApiError` conversion preserves the semantic class (404 / 409 / 500). |
| `Result<_, store::StoreError>` (raw WIT calls: `store::insert`, `store::update`, `store::delete`, plus everything on the `Transaction` resource) | The host carries a typed `store-error` variant; bare `?` into an `ApiError`-returning handler preserves the semantic class (quota → 507, conflict → 409, …) via the macro-emitted `From<store::StoreError> for ApiError`. `.map_err(ApiError::internal)` still works (flattens to 500) if you want that. Inside a tx closure the error type is `String`; bare `?` lifts WIT errors to it lossily. |
| `Result<_, PeerError>` (`peer_fetch`) | Bare `?` — `From<PeerError> for ApiError` lifts a dependency failure (`TargetNotFound`/`Denied`/`Timeout`/`DepthExceeded`/`Internal`) to **502 `/errors/upstream`**, and a *this-service* misconfig (`CapabilityDenied`/`InvalidTarget`) to **500**. Match the variant before `?` if you want a different status (e.g. treat the callee's 404 as your own resource's 404). The wire `detail` carries only the failure class; the full error (target URI, policy text) is logged request-correlated to your service's log stream — debug there. |
| `Result<_, serde_json::Error>` (`PeerRequest::body_json`, `resp.json()`, `serde_json::to_*` on bodies you construct) | Bare `?` — `From<serde_json::Error> for ApiError` lifts to **500** (framing failure: a body the service itself built/parsed). Client-supplied bodies should go through `parse_body`/`validate_body` instead, which map malformed input to 400/422. |

Use `match` on `StoreError` variants when you need to react — e.g. retrying
on a unique-index collision for collision-prone slug generation. Convert the
raw WIT error with `StoreError::from_wit` and match the typed arm:

```rust
match store::insert("links", &row).map_err(StoreError::from_wit) {
    Ok(id) => return Ok(Created(...)),
    Err(StoreError::Conflict(_)) => continue,
    Err(e) => return Err(e.into()),
}
```

### Migrations

Schema evolution after first deploy uses the `migrations` runner.
Declare versioned migrations in `init_tables` (after the
`create_table_from` calls); the runner records applied versions in a
per-service `__boogy_schema_version` table and skips them on
subsequent requests.

```rust
fn init_tables() {
    create_table_from(&Table::new("notes").text("title").text(DEFAULT_OWNER_COL));

    migrations(&[
        migration(1, "add_priority", |m| {
            // Structured DDL via MigrationCtx — NOT raw SQL. The ctx has
            // add_column / rename_column / drop_column / create_table /
            // create_index / drop_index (+ data helpers for backfills).
            m.add_column("notes", &col("priority", ColType::Integer).default(Val::Integer(0)))?;
            Ok(())
        }),
        migration(2, "index_owner", |m| {
            m.create_index("notes", &store::IndexDef {
                name: "idx_notes_owner".into(),
                columns: vec![DEFAULT_OWNER_COL.into()],
                unique: false,
            })?;
            Ok(())
        }),
    ])
    .expect("migrations failed");
}
```

`migrations()` wraps each migration in one store transaction. The built-in
engine is the sole per-service store engine, so transactions (and therefore
migrations and `tx`) are always available.

Each migration runs inside its own transaction: the `up` closure (schema DDL
+ any data backfill) executes first, then the version-table insert, all
committing atomically. A failed migration rolls back and the next request
retries — never half-applied. **The whole migration is bounded by the store's
~5s / 10MB transaction envelope**: a backfill that touches a very large table
can exceed it and roll back perpetually — split such backfills across smaller
migrations (or a two-step "add column with default now, backfill later via a
background job" pattern).

Conventions:
- Versions are strictly increasing `i64`; gaps are allowed but order
  is by numeric value.
- Names are informational (recorded for audit / debugging). Keep them
  descriptive (`"add_priority_column"`, not `"v1"`).
- Migrations are append-only — never edit or delete a published
  migration. To revert, write a new migration with a higher version
  that undoes the change.
- `MigrationCtx` schema ops are **introspection-idempotent** (they check
  `list_columns` / `list_tables` / `list_indexes` and no-op if already
  applied), so a re-run after a crash mid-migration is safe — no `IF NOT
  EXISTS` strings to manage. Data backfills, however, are **not** made
  idempotent for you: write them as naturally idempotent ops (e.g.
  `m.update_where(...)` to a fixed default) since the whole migration is one
  atomic tx that either fully commits or fully rolls back.
- Migrations run on every request (idempotent fast-path: one
  `SELECT MAX(version)`); for very-hot services this is fine, but if it
  matters, declare migrations only when adding new ones.

### Transactions

Multi-row atomic writes go through a **no-arg closure** — `tx(|| { ... })`.
There is **no transaction handle**: inside the closure you call the *same*
`store::*` / `db_*` / `find_row_by` free functions you call outside a
transaction. They transparently join the host's ambient store transaction. On
`Ok` the host commits; on `Err` it rolls back; on **panic** the host tears
the request down and discards the (never-committed) transaction.

`tx` is generic over the closure's error type `E: From<store::StoreError>`,
so the closure can return `Result<R, E>` for any such `E`:

- **Common case — structured errors.** `tx::<_, _, ApiError>(|| ...)` lets
  the closure mix `store::*` with `peer::fetch`, raise domain errors
  (`ApiError::conflict(...)`, `ApiError::unprocessable(...)`), or `?`-lift
  `db_*` / `find_row_by`. `ApiError` implements `From<store::StoreError>`,
  so bare `?` lifts the typed store error and preserves its variant → HTTP
  status.
- **Store-only.** When the closure does *only* `store::*` ops, the error
  type is `store::StoreError`. Name it (`tx::<_, _, store::StoreError>`)
  when inference can't pin `E` from context.

When `E` is unambiguous from the surrounding code, the turbofish is
optional; when it isn't (e.g. a store-only closure whose result is consumed
by a `?` into `ApiError`), name it.

```rust
// store-only closure — name the error type when it can't be inferred.
let user_id = tx::<_, _, store::StoreError>(|| {
    let user_id = store::insert("users", &user_cols)?;
    store::insert("profiles", &profile_cols_for(user_id))?;
    Ok(user_id)
})?;

// mixed-error closure. Raise structured errors and call
// the same store::* / db_* / find_row_by fns as outside a tx.
let new_balance: f64 = tx::<_, _, ApiError>(|| {
    let bal_row = find_row_by(
        "balances", "principal", store::Value::Text(me.clone()),
    )?;
    let cur = bal_row
        .map(|r| r.text("balance").parse::<f64>().unwrap_or(0.0))
        .unwrap_or(default_balance);
    if cur < amount {
        // Raises a structured 422 from inside the tx; rolls back.
        return Err(ApiError::unprocessable("insufficient balance"));
    }
    store::update("balances", bal_id, &[
        store::Column { name: "balance".into(),
            val: store::Value::Text(format!("{:.6}", cur - amount)) },
    ])?;
    Ok(cur - amount)
})?;
```

No snapshot-before-tx pattern is needed: reads inside the closure see the
transaction's own writes and close the TOCTOU window, because they run on
the same ambient transaction as the writes.

**Cross-service: peer calls join the transaction.** Any `peer::fetch` made
inside an open tx **enrolls the callee's entire subtree into the SAME
transaction** — the callee's `store::*` ops auto-join, and the callee does
*not* call `tx` itself (calling `tx` from a handler already
enrolled as a peer participant fails at commit). Only the **originating
request (the owner)** commits. Any participant failure **poisons** the
transaction so commit refuses — rollback only. The whole call tree shares
one 5s / 10MB store transaction envelope.

**Denied inside a tx:** `outbound_http` and `background_jobs` cancel/status
are refused while a transaction is open (they surface as their
capability/backend errors). `background_jobs` enqueue is allowed inside a
transaction — the job is submitted only if the transaction commits.

The built-in engine is the sole per-service store engine, so `tx` is always
available.

**Error → status (via `tx::<_, _, ApiError>`):** a commit
**conflict** (store serialization abort) → `StoreError::Conflict` → HTTP
**409** (no auto-retry — the client retries the whole request). These flow
through correctly because the helper carries the typed `store::StoreError`,
not a flattened String.

**Bulk insert:** `store::insert_many(table, &[&[Column]])` works both
inside and outside a tx — returns `Result<Vec<u64>, store::StoreError>`
with the new auto-PK ids in input order. Use for batch writes (import jobs,
fan-out tagging).

**Not to be confused with migrations.** `MigrationCtx::tx` (the `t.tx(...)`
handle passed to a `migration(version, name, |t| { ... })` `up` closure) is
a *separate, unchanged* surface: `-> Result<R, String>`, DDL-oriented, no
HTTP-status semantics. The handler-facing `tx` above is the
ambient store transaction API.

### Writing rows

The WIT bindings expose `store::insert`, `store::update`, `store::delete`.
Use `store::Column { name, val: store::Value::* }` for the data; map
errors into `ApiError` (or wrap with `StoreError::from_wit` first if
you need to react to UNIQUE / FK violations) so `?` flows cleanly:

```rust
let id = store::insert("notes", &[
    store::Column { name: "title".into(), val: store::Value::Text(input.title.clone()) },
    store::Column { name: "body".into(),  val: store::Value::Text(input.body.clone()) },
    store::Column { name: DEFAULT_OWNER_COL.into(), val: store::Value::Text(principal) },
])
.map_err(ApiError::internal)?;
Ok(Created(NoteOut { id, title: input.title, body: input.body }))

store::update("notes", id, &[/* same shape */]).map_err(ApiError::internal)?;
store::delete("notes", id).map_err(ApiError::internal)?;
```

For the unique-collision retry pattern (random slug generation,
share codes), use `StoreError::from_wit` and match on
`Conflict` — see [`store::StoreError`](#tables--storage)
above.

`store::Value::*` variants: `Text(String)`, `Integer(i64)`, `Real(f64)`,
`Boolean(bool)`, `Blob(Vec<u8>)`, `Null`.

### Filtering / sorting / paginating

**Keyset pagination is THE default for any list a client pages through — reach
for it first.** It is O(page) regardless of depth (no offset re-scan), stable
under concurrent inserts (no skipped/repeated rows), and the SDK makes it a
one-liner. Offset/`find` is a fallback for tiny, fixed, non-paged sets only.

Keyset is **two halves that must match**:

1. **Declare the access pattern on the model** so the walk is an index walk, not
   a scan (this is the part agents forget — without it the keyset query
   degrades to a full scan):
   - `#[model(list_by(filter = "owner_id", newest = "created_at"))]` — a filtered
     newest-first list. Resolves to a covering composite index
     `(owner_id, created_at DESC, _id)`; its prefix also serves a plain
     `where_eq(owner_id)` equality seek, so you usually DON'T also need
     `#[index]` on that column.
   - `#[model(ranked_by(highest = "created_at"))]` — an UNfiltered newest-first
     feed (or any score column, e.g. `highest = "score"`).
   Repeat `list_by` once per filter axis a list endpoint exposes.

2. **Page it with the Query DSL terminal** — `keyset_by` + `.cursor` + `.limit`
   + `.fetch_page`, returning a `CursorPage<T>` (serializes `{ items,
   next_cursor }`):

   ```rust
   use boogy_sdk::pagination::{decode, CursorPage};
   use boogy_sdk::store::SortDir;

   fn list_orders(req: &mut Req<'_>) -> Result<Json<CursorPage<OrderOut>>, ApiError> {
       let limit  = req.query("limit").and_then(|s| s.parse().ok()).unwrap_or(50).clamp(1, 200);
       let cursor = req.query("cursor").and_then(decode);
       let page = Query::on(Order::TABLE)
           .where_eq(Order::OWNER_ID, owner.as_str())     // optional filter(s)
           .keyset_by(Order::CREATED_AT, SortDir::Desc)   // MUST match a list_by/ranked_by
           .limit(limit)
           .cursor(cursor)
           .fetch_page(|r| order_out(r))?;                // over-fetch+1, builds next_cursor
       Ok(Json(page))
   }
   ```

   `fetch_page` over-fetches by one to detect the next page and builds the opaque
   `next_cursor` for you — no manual cursor arithmetic. The client passes the
   returned `next_cursor` straight back as `?cursor=`. Extra
   `where_eq`/`where_gte`/… filters compose on the same walk (residual-filtered
   when not the indexed axis), so multi-axis admin filters Just Work.
   `next_cursor` is absent on the last page. Reference: `chat` (`list_by`),
   `notes-api` (`ranked_by`), `stripe-base` / `resend-base` (multi-axis admin).

**Bounded-memory batch jobs** (sweeps, exports — not a client page): use
`for_each_batch` (above), not keyset.

**Offset/`find` (fallback ONLY).** `store::find` takes `FindOptions { filters,
sort, page: Some(Page { limit, offset }) }`. Offset re-scans `offset` rows every
page and can skip/repeat rows under concurrent writes — use ONLY for a tiny,
fixed, non-paged set where keyset would be overkill.

```rust
let result = store::find("notes", &store::FindOptions {
    filters: vec![store::Filter {
        column: "done".into(), op: store::FilterOp::Eq, val: store::Value::Boolean(false),
    }],
    sort: vec![store::SortBy { column: "priority".into(), descending: true }],
    page: Some(store::Page { limit: 20, offset: 0 }),
})?;
```

Filter ops: `Eq`, `NotEq`, `Lt`, `Lte`, `Gt`, `Gte`, `Like`, `NotLike`,
`In`, `NotIn`, `IsNull`, `IsNotNull`.

## Responses

Prefer the typed `IntoResponse` wrappers — handler returns them directly,
`?` flows for errors:

| Wrapper | Status | Body |
|---|---|---|
| `Json(value)` | 200 | JSON via Serialize |
| `Created(value)` | 201 | JSON |
| `NoContent` | 204 | empty |
| `Redirect::to(url)` | 302 | empty + Location header |
| `Option<T>` | 404 if `None`, otherwise inner | — |

For full header control, the legacy builders are still available and
return `response::HttpResponse` (which itself implements `IntoResponse`):

| Builder | Status | Body |
|---|---|---|
| `response::ok(&body)` | 200 | JSON via Serialize |
| `response::created(&body)` | 201 | JSON |
| `response::no_content()` | 204 | empty |
| `response::raw(status, body, content_type)` | any | raw bytes |

Errors should always come from `ApiError::*` constructors (RFC 7807,
`application/problem+json`) and propagate through `?` rather than be
constructed inline.

## JSON-RPC

For "many small typed methods over one endpoint" (search, share,
admin operations), use `Router::rpc` — it registers the POST route and
captures the method shapes for `…/openrpc.json` in one call:

```rust
.rpc("/api/rpc", || boogy_sdk::rpc::Dispatcher::new()
    .method("search_notes", search_notes)
    .method("share_note",   share_note))
```

The closure runs once at registration time (for spec capture) and once
per request (for dispatch). Method handlers:

```rust
#[derive(Deserialize)]
struct SearchParams { query: String }
#[derive(Serialize)]
struct SearchResult { items: Vec<json::Value> }

fn search_notes(p: SearchParams) -> Result<SearchResult, RpcError> {
    // ... auth::find_owned / auth::load_owned available here
}
```

JSON-RPC handlers use `RpcError` for failures — propagate
`auth::find_owned` / `auth::load_owned` errors with `?` directly.

## MCP (Model Context Protocol)

For LLM clients (Claude Code, Inspector, etc.), use `Router::mcp` — it
registers the POST route and records the endpoint in `…/openapi.json`:

```rust
.mcp("/mcp", |req| {
    boogy_sdk::mcp::McpServer::new("notes-mcp", env!("CARGO_PKG_VERSION"))
        .tool_typed(mcp::tool("create_note").description("..."), create_note_tool)
        .resource(mcp::resource("notes://summary", "summary"), summary_resource)
        .resource_template(mcp::resource_template("note://{id}", "note"), note_resource)
        .prompt(mcp::prompt("summarize_notes"), summarize_prompt)
        .handle(req.request)
})
```

**Tool registrations:** prefer `tool_typed::<P, R>` — the typed
counterpart to `rpc::Dispatcher::method`. Argument struct must derive
`Deserialize + JsonSchema`; result struct must derive `Serialize + JsonSchema`.
The MCP `inputSchema` and `outputSchema` are auto-derived, so the
deserializer/serializer and the protocol surface can't drift.

```rust
#[derive(Deserialize, JsonSchema)]
struct CreateNoteArgs { title: String, body: String }

#[derive(Serialize, JsonSchema)]
struct NoteOut { id: String, title: String, body: String }

fn create_note_tool(args: CreateNoteArgs) -> Result<NoteOut, ApiError> {
    let principal = auth::current_principal().ok_or_else(ApiError::unauthenticated)?;
    let id = store::insert("notes", &columns_for(&args, &principal))
        .map_err(ApiError::internal)?;
    Ok(NoteOut { id, title: args.title, body: args.body })
}
```

`schemars` is a direct dep — add `schemars = { workspace = true }` in-repo
(or `schemars = "0.8"` for external consumers) to your `Cargo.toml`.

The legacy `tool(...)` registration with raw `Fn(Value) -> Result<ToolResult, RpcError>`
is still available as the escape hatch for hand-rolled `ToolResult`
shapes (e.g. multi-content-block responses).

MCP handler shapes:
- Tool (typed):  `fn(P) -> Result<R, ApiError>` where `P: Deserialize + JsonSchema`, `R: Serialize + JsonSchema`
- Tool (raw):    `fn(serde_json::Value) -> Result<ToolResult, RpcError>`
- Resource:      `fn(uri: &str) -> Result<Vec<ResourceContent>, RpcError>`
- Prompt:        `fn(args: HashMap<String, String>) -> Result<PromptResult, RpcError>`

MCP handlers don't see `Req` / `Ctx` — auth and resource loading must
be done explicitly. Use `auth::current_principal()` / `auth::find_owned`
/ `auth::load_owned` inside MCP handlers — they work without a router
context.

## Spec endpoints

Every deployed service automatically serves spec documents off its
route tree — no handler code needed.

| URL (relative to service subtree) | Format | Served when |
|---|---|---|
| `GET …/openapi.json` | OpenAPI 3.0.3 | Always |
| `GET …/openrpc.json` | OpenRPC 1.3.2 | One or more `Router::rpc(...)` mounts exist |

**Two-tier visibility.** Anonymous callers see only public routes — not
inside a `.group([...], …)` guard block and not taking a `Principal`
typed extractor (`Option<Principal>` doesn't hide a route).
Authenticated callers (any valid bearer) see all routes. Use
`Router::undocumented(|g| …)` to exclude routes from spec docs entirely
— they still dispatch normally, guards still apply inside the block.

**`Router::info`** sets the doc identity fields:

```rust
Router::new()
    .info("Notes API", env!("CARGO_PKG_VERSION"), Some("Note CRUD + MCP tools"))
    .get("/api/notes", list_notes)
    // ...
```

### Documenting endpoints (summary & description)

Attach human-readable docs to individual operations so other agents and
clients understand each endpoint. `.summary(...)` / `.description(...)`
are chainable and apply to the **next** route/method registered, then
clear themselves (one annotation = exactly one operation):

```rust
Router::new()
    .summary("List widgets")
    .description("Return every widget the caller owns.")
    .get("/widgets", list_widgets)   // ← annotated
    .get("/gadgets", list_gadgets);  // ← no summary/description

Dispatcher::new()
    .summary("Search notes")
    .description("Full-text search over the caller's notes.")
    .method::<SearchParams, SearchResult, _>("search_notes", search);
```

Route annotations flow into `…/openapi.json` (`summary`/`description` on
the operation); method annotations flow into `…/openrpc.json`
(`summary`/`description` on the method). Absent keys are omitted, not
emitted as null. This is per-operation; `Router::info` sets the
document-level identity instead.

**Reserved filenames.** `openapi.json` and `openrpc.json` are reserved
at the leaf of any path in your tree. An explicit `GET` route whose
literal path ends in one of those filenames overrides the generated doc.
A `{param}`-style route at the same position does NOT capture them.

**Host bypass.** The host forwards spec-doc GETs to the service even
when the manifest `[routing] methods` list excludes GET. No manifest
change needed.

**Mount path is external.** A provision-time `[routing] path` override
relocates the instance's external URL (so one module can run as several
instances at distinct mounts). The guest keeps serving its own module
routes — the host rewrites the mount prefix transparently on the way in,
and remaps the served `openapi.json`/`openrpc.json` path keys to the mount.
Write handlers against the module's own paths; don't hardcode the mount.
Mounts are unique per owner (a colliding/overlapping mount is rejected 409).

**Module-intrinsic vs deployment config.** Two tiers in the manifest.
*Module-intrinsic* (author-fixed, never changed by a provisioner):
`[capabilities]`, declared `[[websockets.channels]]`, declared secret
**names**, routing methods + internal base path. *Deployment config* (set
per instance at provision time): service id, mount path, `[ingress]`,
`[limits]`, `[outbound] allowed_hosts`, and secret **values**. The shipped
`[limits]` are defaults — a provisioner may raise or lower each within the
**platform caps** (not the module's value); capabilities can never be
widened by a provisioner. Outbound `allowed_hosts` is provisioner-set, but
the runtime IP firewall blocks internal/loopback regardless.

**`JsonSchema` derive requirement.** Typed extractors (`Json<T>`,
`Query<T>`, `Path<T>`) and typed responses (`Json<T>`, `Created<T>`)
need `schemars::JsonSchema` on the payload type to appear in the
generated schema. Add it to every DTO:

```rust
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct CreateNote { title: String, body: String }
```

For types where you want a custom schema shape, use
`.input_schema(serde_json::json!({...}))` / `.output_schema(...)` on
the `Tool` descriptor (MCP), or implement `JsonSchema` manually.

See `boogy:boogy-api-specs` for the full spec-endpoint reference.

## Cross-service calls (`peer`)

```rust
use boogy_sdk::peer::PeerRequest;

let resp = peer_fetch("boogy://alice/services/notes-api", &PeerRequest::get("/api/notes"))?;
let notes: Vec<Note> = resp.json()?;
```

Both `?`s are infallible-to-write, and two-channel: clients receive only the failure class in `detail`, while the full error text is emitted to your service's request-correlated log stream. `From<PeerError> for ApiError` lifts a
failed call to **502 upstream** (misconfig → 500), and `From<serde_json::Error>
for ApiError` lifts `body_json`/`resp.json()` to **500**. No `.map_err`
boilerplate at the call site; match the variant first only if you want a
non-default status. Requires `peer = true` in the manifest's `[capabilities]`. Manifest
`[ingress.delegation]` controls on-behalf-of (OBO) flow when one service
calls another carrying an end-user identity.

## Vector Search

Boogy services can create and query vector collections for semantic search,
recommendation, and similarity tasks. Enable the capability in the manifest
and use the `vector::*` functions exposed by `wit_glue!`.

**Manifest:**
```toml
[capabilities]
vector = true
```

Vector collections live in the per-service store (the sole store
engine); no `[store] engine` selection is needed.

**SDK functions** (available unqualified after `wit_glue!`):

| Function | Purpose |
|---|---|
| `vector::create_vector_collection(name, dims, metric)` | Create (or ensure) a named collection with `dims` dimensions and the given distance metric. |
| `vector::vector_insert(collection, id, embedding)` | Insert a single vector with caller-supplied string id. |
| `vector::vector_insert_batch(collection, items)` | Insert multiple vectors in one call. |
| `vector::vector_update(collection, id, embedding)` | Replace the embedding for an existing id. |
| `vector::vector_delete(collection, id)` | Remove a vector by id. |
| `vector::vector_search(collection, query, k)` | Return the `k` nearest neighbours with distances. |
| `vector::vector_search_filtered(collection, query, k, filters)` | Same, but restrict candidates by metadata filters before ranking. |

**Distance metrics** (`vector::Metric`): `Cosine`, `Euclidean`, `DotProduct`.

**Usage example:**

```rust
fn init_tables() {
    // Collection is idempotent — safe to call on every request.
    vector::create_vector_collection("docs", 1536, vector::Metric::Cosine)
        .expect("create_vector_collection failed");
}

fn index_doc(req: &mut Req<'_>) -> Result<Created<json::Value>, ApiError> {
    let input: IndexDocInput = validate_body(req.body())?;
    vector::vector_insert("docs", &input.id, &input.embedding)
        .map_err(ApiError::internal)?;
    Ok(Created(json::json!({ "id": input.id })))
}

fn search_docs(req: &mut Req<'_>) -> Result<Json<json::Value>, ApiError> {
    let input: SearchInput = validate_body(req.body())?;
    let results = vector::vector_search("docs", &input.embedding, 10)
        .map_err(ApiError::internal)?;
    Ok(Json(json::json!({ "results": results })))
}
```

`vector::vector_search` returns a `Vec<VectorMatch>` with `.id` (String)
and `.distance` (f32) fields. Lower distance is better for Euclidean;
higher dot product is better for DotProduct; Cosine returns values in
[0, 2] where 0 is identical.

## Websockets (real-time channels)

A service can broadcast real-time messages to subscribers over declared
channels. Subscribers connect to the platform streaming gateway; the
service only publishes. Enable the capability and declare each channel in
the manifest, then use the functions emitted by `wit_glue!`.

**Manifest:**
```toml
[capabilities]
websockets = true

[[websockets.channels]]
name = "ticker"
class = "public"          # anyone may subscribe

[[websockets.channels]]
name = "inbox"
class = "private"         # subscribers need a grant
replay = 50               # optional: keep the last N messages for late joiners
```

**SDK functions** (available unqualified after `wit_glue!`):

| Function | Purpose |
|---|---|
| `ws_publish(channel, payload) -> Result<(), PublishError>` | Broadcast a UTF-8 payload (JSON by convention, ≤ 16 KiB) to a declared channel. |
| `ws_mint_subscribe_grant(channel, ttl_seconds) -> Result<String, GrantError>` | Mint a short-lived grant for a private channel; hand it to the user via your own API so they can subscribe. `ttl_seconds` must be `10..=3600` — out of range is rejected (`InvalidTtl`), never clamped. |

**Error enums** (`boogy_sdk::websockets`):

- `PublishError`: `CapabilityDenied`, `UnknownChannel`, `PayloadTooLarge`, `RateLimited`, `BackendUnavailable`.
- `GrantError`: `CapabilityDenied`, `UnknownChannel`, `NotPrivate`, `InvalidTtl`, `RateLimited`.

**Usage example:**

```rust
fn push_price(req: &mut Req<'_>) -> Result<NoContent, ApiError> {
    let input: PriceInput = validate_body(req.body())?;
    ws_publish("ticker", &json::json!({ "px": input.px }).to_string())
        .map_err(ApiError::internal)?;
    Ok(NoContent)
}

fn subscribe_inbox(req: &mut Req<'_>) -> Result<Json<json::Value>, ApiError> {
    let grant = ws_mint_subscribe_grant("inbox", 300)
        .map_err(ApiError::internal)?;
    Ok(Json(json::json!({ "grant": grant })))
}
```

Publishing is service -> subscribers only; there is no way to publish to
another service's channels. Public channels need no grant; private
channels require the caller to present a grant minted by the owning
service.

## API keys (`api_keys_glue!`)

If you invoke `boogy_sdk::api_keys_glue!(bindings)`:

- `init_tables` must call `api_key_routes::install_table()`.
- Mount the four management endpoints in ONE call with the
  `api_key_routes::ApiKeyRoutes` extension trait — `.with_api_key_routes()`
  (conventional `/_keys`) or `.with_api_key_routes_at("/admin/keys")`
  (custom prefix). Bring it into scope: `use crate::api_key_routes::ApiKeyRoutes;`.
- For a fully custom layout, wire the handlers by hand instead:
  `api_key_routes::create` (POST), `::list` (GET),
  `::revoke` (DELETE `/_keys/{id}`), `::rotate` (POST `/_keys/{id}/rotate`).
- `api_key_routes::guard` accepts either a PASETO bearer or a
  presented `sk_*` key. The guard for YOUR routes stays a separate
  `.group([api_key_routes::guard], …)` — which routes to gate is per-service.

Wire as (recommended):
```rust
use crate::api_key_routes::ApiKeyRoutes;

Router::new()
    .with_api_key_routes()                   // /_keys create/list + /_keys/{id} revoke/rotate
    .group([api_key_routes::guard], |g| g    // gates only these routes
        .get("/api/things", list_things)
        .post("/api/things", create_thing))
```

## Conventions

- **Owner column** — always `DEFAULT_OWNER_COL` (i.e. `"owner_principal"`).
- **Path params** — `{name}` syntax. Read with `req.params.get("name")`.
- **Catch-all params** — `{*rest}` captures the remainder of the path.
- **Response shape** — JSON for success, RFC 7807
  `application/problem+json` for failures (every `ApiError::*` and
  every `response::*` error builder produces it). Mixing the two is
  fine — they converge on the same wire shape.
- **Auth** — never reach `bindings::boogy::platform::auth` directly.
  Always go through `auth::*` helpers.
- **Tables run idempotently** — `init_tables` runs on every request;
  every `create_table_from` is `IF NOT EXISTS`. Don't try to "optimize"
  by running it once.
- **Existence-mask** — always 404 for "row exists but isn't yours."
  Never 403. The SDK's auth helpers do this for you; if you're hand-
  rolling an ownership check, match the convention.

## Names available — two namespaces

Boogy ships names through **two distinct sources**. Mixing them up is a
common cause of "function not found" errors. The cheat sheet:

### From `crate::` — emitted by `wit_glue!`

These are written into your crate's root by the macro. In `lib.rs` they
work unqualified. **In submodules, you must `use crate::*` or qualify
each name with `crate::`** — they are NOT globally available.

```rust
// In a submodule like crates/examples/mycrate/src/posts.rs:
use crate::{store, find_row_by, find_rows_by, tx, auth, now_millis};
use crate::bindings;  // if you need to reach into raw WIT bindings
```

| Category | Names |
|---|---|
| Modules | `store` (= WIT `bindings::boogy::platform::store`), `auth`, `bindings`, the `peer`/`secrets`/`signing`/`background_jobs`/`websockets` binding modules, plus `response` and `json` |
| Router / request | `Router`, `Req`, `Params`, `Request`, `Path`, `FromRequest`, `Principal`, `Ctx`, `QueryExtractor` (the `Query` *request extractor*, aliased so it doesn't clash with the `Query` DSL builder — both are in scope) |
| Response wrappers | `Json`, `Created`, `NoContent`, `Redirect`, `IntoResponse` |
| Errors / parsing | `ApiError`, `parse_body`, `validate_body` |
| Serde derives | `Serialize`, `Deserialize` — **in scope already; no `use serde::…`** |
| Constants | `DEFAULT_OWNER_COL` |
| Schema | `create_table_from`, `migration`, `migrations` |
| Row reads | `to_sdk_row`, `get_row`, `find_all_rows`, `find_row_by`, `find_rows_by`, `find_rows`, `find_rows_grouped`, `upsert_increment`, `for_each_batch` |
| Transactions | `tx` (no-arg closure, generic over error type; call the same `store::*`/`db_*`/`find_row_by` fns inside) |
| Helpers | `filter_eq` (+ `filter_neq`/`filter_gt`/`filter_gte`/`filter_lt`/`filter_lte`/`filter_like`/`filter_not_like`/`filter_is_null`/`filter_is_not_null`/`filter_in`), `sort_asc`/`sort_desc`, `page`, `now_millis`, `peer_fetch` |

If `api_keys_glue!` is also invoked, add: the `api_key_routes` module
with `create` / `list` / `revoke` / `rotate` / `guard` / `resolve_caller`
/ `install_table` / `ResolvedKey`.

**All of the above are already in scope in `lib.rs` after `wit_glue!` —
do NOT also `use boogy_sdk::{Router, Json, ApiError, …}` (or `use
serde::{Serialize, Deserialize}`) there. That re-imports a name the macro
already injected (`E0252` / "unused import" / shadowing) — the exact
double-import trap.** And `wit_glue!(bindings, MyApi)` does **not** define
`struct MyApi` — you declare the unit struct and `impl Api for MyApi`
yourself. The `boogy_sdk::` import line below is for **submodules** (where
crate-root names aren't visible unless you `use crate::*`) and for crates
that don't invoke `wit_glue!`.

### From `boogy_sdk::` — re-exports from the SDK crate

```rust
// In a SUBMODULE (or a crate without wit_glue!) — in lib.rs these names
// are already injected by wit_glue!, so importing them there double-imports.
use boogy_sdk::{Api, ApiError, Created, Json, NoContent, Redirect, Req, Router,
                Row, StoreError, Table, parse_body, validate_body};
use boogy_sdk::ids::IdCodec;  // optional: opaque-id translation
```

| Category | Names |
|---|---|
| Trait | `Api` (your struct implements this) |
| Errors / parsing | `ApiError`, `parse_body`, `validate_body` |
| Response wrappers | `Json`, `Created`, `NoContent`, `Redirect`, `HttpResponse` |
| Router / request | `Req`, `Router` |
| Store types | `Row`, `StoreError`, `Table` |
| Submodules | `boogy_sdk::pagination`, `boogy_sdk::peer`, `boogy_sdk::rpc`, `boogy_sdk::mcp`, `boogy_sdk::ids` |

### Why two namespaces?

`wit_glue!`-emitted names depend on the `bindings` module of your
specific crate (different consumers may use different WIT worlds,
have different host-call signatures, etc.), so they're emitted into
the consumer's root rather than living in `boogy_sdk` itself. The
SDK re-exports (`boogy_sdk::`) are the bindings-independent surface:
they're the same across every crate.

**Practical rule:** if it has anything to do with `store::`, `tx`,
`auth::*`, or `bindings::*`, it's a `crate::` name. `ApiError` / `Req` /
`Created` / a response wrapper / the serde derives are available from
either path — but in `lib.rs` they're **already injected by `wit_glue!`,
so don't re-`use` them there**; in a submodule, import them from
`boogy_sdk::` (or `use crate::*`).

## Opaque public ids (`boogy_sdk::ids`)

Store tables have a `u64` auto-PK. For user-facing apps where you want
enumeration-resistant public ids, use `IdCodec`:

```rust
use boogy_sdk::ids::IdCodec;

// At app startup — load secret from env var or secrets module.
static CODEC: OnceLock<IdCodec> = OnceLock::new();
fn codec() -> &'static IdCodec {
    CODEC.get_or_init(|| IdCodec::new(*b"my-app-secret-16"))
}

// At service boundary:
let public_id = codec().encode(post_id);          // "wzMx8...mY" (22 chars)
let internal_id = codec().decode(&public_id);     // Some(post_id)
```

Mechanism: AES-128 block cipher applied to (magic prefix || u64) →
URL-safe base64. Deterministic, reversible with the key. See module
docs for the full threat model — it's enumeration resistance, not
crypto in isolation.

## Names you should NOT use

- `bindings::boogy::platform::auth::current_identity()` — use
  `auth::current_principal()` instead.
- Anything starting with `__` in your crate's namespace — these are
  macro-private (e.g. `__boogy_insert_row` is the SDK's internal
  api_keys plumbing; user code uses `store::insert` directly).
- `Val` (the SDK's portable read-side value type) is intentionally
  not in the unqualified namespace. Writes always use the WIT
  `store::Value::*` enum so there is one value-type per concern.
  `Row::get(name)` returns `&Val` if you genuinely need to inspect
  the read-side typed enum — qualify it as
  `boogy_sdk::store::Val` at the (rare) callsite that needs it.
- The raw WIT `store::find` for principal-scoped lists — use
  `auth::find_owned`. Only fall back to raw `store::find` when you
  need filtering / sorting / pagination beyond what `find_owned`
  provides.
