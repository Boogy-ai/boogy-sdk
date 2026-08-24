# boogy-sdk

The framework crate for writing Boogy services in Rust.

A Boogy service is a Wasm component that exports the WIT `http-handler` interface and runs inside the Boogy host. The SDK gives you the building blocks — request routing, typed responses, an error type that renders to RFC 7807, a typed model + query layer over the per-service store, JSON-RPC and MCP envelopes — and a single macro (`wit_glue!`) that handles the WIT plumbing so you write ordinary Rust and never touch the raw bindings.

> **Writing a handler right now?** [`AGENTS.md`](AGENTS.md) is the dense, complete reference for everything available inside a Boogy handler or guard. This README is the narrative introduction to the same surface. Where the two disagree, `AGENTS.md` wins.

## Contents

- [Quick start](#quick-start)
- [The `Api` trait](#the-api-trait)
- [The `wit_glue!` macro](#the-wit_glue-macro)
- [Routing](#routing)
- [Responses and errors](#responses-and-errors)
- [Data: models, queries, transactions](#data-models-queries-transactions)
- [Spec endpoints](#spec-endpoints)
- [JSON-RPC](#json-rpc)
- [Host capabilities](#host-capabilities)
- [Building and deploying](#building-and-deploying)

## Quick start

A service is one Rust crate. The top of `lib.rs` is the only boilerplate:

```rust ignore-snippet: the crate-level glue macro. wit_glue! emits crate-scoped trait impls, so a harness holding two invocations is a duplicate-impl error by construction.
mod bindings {
    // `wit_bindgen::generate!` needs a literal, manifest-relative path, so
    // a build.rs copies the WIT files here from the pinned `boogy-wit`
    // build-dependency. See "Building and deploying" below.
    wit_bindgen::generate!({ world: "service", path: "wit" });
}

boogy_sdk::wit_glue!(bindings, NotesApi);
```

Everything after it is your service. This is a complete owner-scoped CRUD API:

```rust
use boogy_sdk::model::Id;
use boogy_sdk::{Api, Model};

// Tables are `#[derive(Model)]` structs. The derive emits the table and
// column-name consts (`Note::TABLE`, `Note::TITLE`) and the indexes your
// declared access patterns imply — never hand-write a schema.
#[derive(Model)]
#[model(table = "notes", list_by(filter = "owner_principal", newest = "created_at"))]
struct Note {
    #[pk]
    id: Id<Note>,
    title: String,
    body: String,
    owner_principal: String,
    created_at: i64,
}

struct NotesApi;

impl Api for NotesApi {
    fn schema(s: &mut Schema) {
        s.model::<Note>();
    }

    fn build_router() -> Router {
        Router::new()
            .info("Notes", "0.1.0", Some("Owner-scoped notes."))
            .summary("List my notes")
            .get("/api/notes", list_notes)
            .summary("Create a note")
            .post("/api/notes", create_note)
            // `auth::owns_resource` loads the row by `{id}`, 404-masks it when
            // it is missing OR not the caller's, and stashes it in `req.ctx`
            // so the handler does not re-fetch.
            .group([auth::owns_resource(Note::TABLE, DEFAULT_OWNER_COL, "id")], |g| g
                .summary("Read one note")
                .get("/api/notes/{id}", get_note)
                .summary("Delete a note")
                .delete("/api/notes/{id}", delete_note))
    }
}

#[derive(Serialize, schemars::JsonSchema)]
struct NoteOut { id: u64, title: String, body: String }

impl From<&Note> for NoteOut {
    fn from(n: &Note) -> Self {
        NoteOut { id: n.id.get(), title: n.title.clone(), body: n.body.clone() }
    }
}

fn list_notes(req: &mut Req<'_>) -> Result<Json<boogy_sdk::pagination::CursorPage<NoteOut>>, ApiError> {
    // One bounded page + the cursor to continue from. There is no form of this
    // call that returns the principal's whole set.
    let page = auth::find_owned::<Note>(
        DEFAULT_OWNER_COL,
        &boogy_sdk::pagination::PageRequest::new(20, req.query("cursor").map(str::to_string)),
    )?;
    Ok(Json(page.map(|r| NoteOut::from(&Note::from_row(r)))))
}

#[derive(Deserialize, garde::Validate)]
struct CreateNote {
    #[garde(length(min = 1, max = 200))]
    title: String,
    #[garde(length(max = 100_000))]
    body: String,
}

fn create_note(req: &mut Req<'_>) -> Result<Created<NoteOut>, ApiError> {
    let principal = auth::current_principal().ok_or_else(ApiError::unauthenticated)?;
    let input: CreateNote = validate_body(req.body())?;
    // The store assigns `_id`; `Id::new(0)` is the placeholder and
    // `db_insert` returns the real id.
    let note = Note {
        id: Id::new(0),
        title: input.title,
        body: input.body,
        owner_principal: principal,
        created_at: now_millis() as i64,
    };
    let id = db_insert(&note)?;
    Ok(Created(NoteOut { id, ..NoteOut::from(&note) }))
}

fn get_note(req: &mut Req<'_>) -> Json<NoteOut> {
    // The guard already loaded and ownership-checked the row.
    let note = Note::from_row(req.ctx.require::<Row>());
    Json(NoteOut::from(&note))
}

fn delete_note(req: &mut Req<'_>) -> Result<NoContent, ApiError> {
    // The guard 404-masked anything the caller does not own, so this id is real.
    db_delete::<Note>(req.params.parse::<u64>("id")?)?;
    Ok(NoContent)
}
```

Build it with `cargo build --target wasm32-wasip2 --release`.

The `smoke/` crate shipped alongside this SDK is the smallest working version of the same shape — copy it to start a project.

## The `Api` trait

```rust
pub trait Api {
    fn schema(_s: &mut Schema) {}                         // default: no-op
    fn build_router() -> boogy_sdk::router::Router;
    fn build_job_router() -> boogy_sdk::JobRouter {       // default: empty
        boogy_sdk::JobRouter::new()
    }
}
```

All three are associated functions — they describe the service, not an instance. `init_tables` runs on **every** request before dispatch; that is safe because each table and index create is idempotent (an existing table is skipped, not recreated). `build_router` is called per request and is cheap to construct. `build_job_router` only matters for services that process background jobs.

## The `wit_glue!` macro

```rust ignore-snippet: a second wit_glue! invocation; its trait impls are crate-scoped, so a harness holding two is a duplicate-impl error by construction.
boogy_sdk::wit_glue!(bindings, NotesApi);          // world: "service"
boogy_sdk::wit_glue!(bindings, NotesApi, with_jobs);  // world: "service-with-jobs"
```

Two arguments: the module you put `wit_bindgen::generate!` in, and your API struct. The three-argument `with_jobs` form additionally implements the `job-handler` export, and is only valid against the `service-with-jobs` world.

The macro emits into **your crate's root**:

| | |
|---|---|
| `impl Guest for NotesApi` + `bindings::export!(…)` | The WIT export. Calls `init_tables`, then `build_router().handle(req)`. |
| `store` | The WIT store module, so handlers write `store::insert(...)` unqualified. |
| `create_model::<M>()`, `db_insert`, `db_get`, `db_update`, `db_delete`, `db_find_by` | The typed-model CRUD layer (see [Data](#data-models-queries-transactions)). |
| `Query`, `find_row_by`, `find_rows`, `count_rows`, `get_row`, `get_many`, `find_all_rows`, `find_many`, `for_each_batch` | Read helpers. `Query` is the fluent one; the rest are the direct forms. |
| `tx`, `migration`, `migrations` | Transactions and schema migrations. |
| `auth::*` | `current_principal`, `current_handle`, `current_scopes`, `has_scope`, `required()`, `require_scope(...)`, `owns_resource(...)`, `find_owned(..., &PageRequest) -> RowPage` (one bounded page + cursor), `load_owned(...)`. |
| `peer_fetch`, `jobs_enqueue`/`_cancel`/`_status`, `ws_publish`, `signing_*`, `secrets_verify_hmac`, `now_millis`, `random_*`, `self_identity`, `caller_is_service_owner` | Thin wrappers over the other host capabilities. |
| `use` statements | `Deserialize`, `Serialize`, `json`, `response`, `Params`, `Req`, `Router`, `Ctx`, `Row`, `Table`, `StoreError`, `ApiError`, `parse_body`, `validate_body`, `Json`, `Created`, `NoContent`, `Redirect`, `IntoResponse`, `Path`, `Principal`, `Alphabet`, `DEFAULT_OWNER_COL`. |

Two things worth knowing about that last row. `Val` is deliberately **not** imported: it is the SDK's read-side value type (what `Row` accessors return), while writes go through `store::Value::*`, and having both unqualified led authors to reach for `Val::*` in a write path that does not accept it. And there is no `boogy_sdk::auth` — the `auth` module lives in *your* crate root, so importing it from the SDK crate does not resolve.

`DEFAULT_OWNER_COL` is `"owner_principal"`. The auth helpers take the owner-column name as an argument so a service *can* choose otherwise, but using the constant keeps multi-tenant ownership uniform across the fleet.

## Routing

Declarative routing built on `matchit`, with standards-compliant method dispatch.

```rust
Router::new()
    .get("/api/users", list_users)
    .post("/api/users", create_user)
    .get("/api/users/{id}", get_user)              // named path param
    .put("/api/users/{id}", update_user)
    .delete("/api/users/{id}", delete_user)
    .get("/files/{*path}", serve_file)             // catch-all path param
    .route_many(&["GET", "POST"], "/sync", sync_handler);
```

`/{name}` captures one segment; `/{*rest}` captures everything after the prefix. Both are read with `req.params.get("name")`, or `req.params.parse::<T>("name")?` for a typed one.

**Nesting.** `.nest(prefix, sub_router)` mounts one router under another — path concatenation, so `outer.nest("/a", inner.nest("/b", …))` produces `/a/b`. A sub-router's `/` route maps to the prefix itself.

```rust
fn build_router() -> Router {
    Router::new()
        .nest("/api/v1", v1_routes())
        .nest("/admin", admin_routes())
        .get("/health", health)            // top-level routes coexist with nests
}

fn v1_routes() -> Router {
    Router::new().get("/users", list_users).post("/users", create_user)
}
fn admin_routes() -> Router { Router::new().get("/dashboard", dashboard) }
```

**Guards** are pre-handler checks that either let the request through or short-circuit it with a response. They attach with `.group([…], |g| …)` — there is no per-route `guard()` method.

```rust
fn require_admin(req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
    if req.header("x-admin-token") == Some("secret") {
        Ok(())
    } else {
        Err(response::forbidden("admin token required"))
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

- Each `.group(...)` call is self-contained: a route gets only its own group's guards.
- When a router is nested, the **outer** router's guards run first; an outer rejection short-circuits before any inner guard fires.
- HEAD requests that fall back to GET still run the GET route's guards. OPTIONS auto-responses do not run guards — they are metadata about supported methods, which matters for CORS preflight.
- Guards cannot observe or mutate the response. For that, write a wrapper handler.
- Guards *can* write into `req.ctx`, which is how `auth::owns_resource` hands a loaded row to the handler.

**Signatures.** A handler is `fn(&mut Req<'_>) -> Result<R, ApiError>` where `R: IntoResponse`, or just `-> R` when it cannot fail. A guard is `fn(&mut Req<'_>) -> Result<(), response::HttpResponse>`.

**`Req` accessors** — prefer these over reaching through `req.request`:

- `req.body() -> Option<&[u8]>`, `req.header(name) -> Option<&str>` (case-insensitive)
- `req.method() -> &str`, `req.path() -> &str`, `req.query(name) -> Option<&str>`
- `req.params.get("id")`, `req.params.require("id")?`, `req.params.parse::<u64>("id")?`
- `req.ctx.require::<T>()`, `req.ctx.get::<T>()`, and the `*_at(slot)` variants
- `req.request` — the raw `boogy_sdk::Request`, kept public for handing off to `mcp::McpServer::handle` / `rpc::Dispatcher::handle`

**Dispatch:**

| Situation | Result |
|---|---|
| Path matched, method registered | run that handler |
| Path matched, no HEAD handler, GET registered | run GET, strip the response body (RFC 9110 §9.3.2) |
| Path matched, no OPTIONS handler | `204 No Content` + `Allow:` listing supported methods |
| Path matched, method not registered | `405 Method Not Allowed` + `Allow:` |
| No path matched | `404 Not Found` |

Method matching is case-insensitive on the wire.

## Responses and errors

Handlers return a typed wrapper, optionally inside a `Result`:

- `Json<T>` (200), `Created<T>` (201), `NoContent` (204), `Redirect` (302), `Option<T>` (`None` → 404), `()` (204), or an `HttpResponse` you built yourself.
- `Result<R, ApiError>` renders failures as RFC 7807 `application/problem+json`. `?` propagates from `validate_body`, `auth::find_owned`, `db_*`, `store::*` and everything else fallible in the SDK.

Store failures keep their meaning across that boundary: `StoreError::NotFound` → 404, `Conflict`/`ConstraintViolation`/`CommitUnknown`/`Poisoned` → 409, `QuotaExceeded` → 507, `Timeout` → 504, `ResourceExhausted`/`TooContended` → 503, `Unsupported` → 501. Match the specific variant when you handle one — a `_ =>` catch-all folds together cases that need different responses (`Conflict` is safe to retry, `Poisoned` and `CommitUnknown` are not).

For the cases that need full header control there are status-typed builders:

```rust
let body = json::json!({ "ok": true });
let url = "https://example.com/next";

response::ok(&body);            // 200, JSON via Serialize
response::created(&body);       // 201
response::no_content();         // 204
response::redirect(url);        // 302
response::raw(200, b"hi", "text/plain");  // any status, raw bytes

// Error builders — every one produces application/problem+json (RFC 7807):
response::bad_request("msg");
response::unauthenticated();
response::forbidden("msg");
response::not_found();
response::conflict("msg");
response::server_error("msg");
```

`body` is anything implementing `serde::Serialize`. `json` re-exports `serde` and `serde_json` (`json::Value`, `json::json!`, `json::from_slice`, …) so you rarely need them qualified.

## Data: models, queries, transactions

Each service gets its own isolated store: relational, ACID, and queried through a structured API rather than a query language. You declare the access patterns your service needs and the store maintains the indexes that serve them.

### Tables

A table is a `#[derive(Model)]` struct registered with `create_model::<M>()` in `init_tables`. The derive emits the column-name consts and the indexes your access patterns imply, so there is no hand-written schema and no `cols` module. Raw `Table::new` / `create_table_from` remains for genuinely dynamic, unknown-at-compile-time schemas; `boogy check` flags it as a hard error in an ordinary service.

Field markers worth knowing on day one:

- `#[pk]` — maps to the store's auto-assigned `_id`. Insert with `Id::new(0)`; `db_insert` returns the real id.
- `#[index]` / `#[lookup_by]` — a single-column index; `#[lookup_by]` makes it unique and declares a point-lookup pattern. There is no field-level `#[unique]`: uniqueness is enforced by an index, so use `#[lookup_by]`, or `#[model(unique_index(cols = [...]))]` for a composite.
- `#[default = "pending"]` / `#[default = 0]` — a column default. Declaring or changing one never rewrites stored rows: a row that predates the column resolves against the current default on read. A defaulted column also satisfies the not-null requirement, which makes this the way to add a required field to a live table. Use `#[default(-1)]` for negative numbers.
<!-- retired-spelling: the bullet below names the retired field-level
     `#[counter]` only to say it is a compile error; the live declaration is
     `#[model(counter(name = "..."))]` on the struct, plus a companion
     `#[derive(Counter)]` marker type. -->
- `#[model(counter(name = "hits"))]` — a conflict-free integer counter, declared **on the struct**, with **no field of its own** (there is no field-level `#[counter]`; writing one is a compile error). The value lives in its own cell and moves by an atomic add that takes no read-conflict range, so concurrent increments compose instead of serializing. Three consequences: there is no field for a write to carry, so no write can clobber it — the store never packs it into the row and it is moved only by `upsert_increment` (or a companion `#[derive(Counter)]` marker type's add) — it **cannot back an index** (the derive rejects `#[index]`, `#[lookup_by]`, and any access-pattern column) — though you can still **order by it**: `.order(T::hits.desc()).limit(20)` is served from the counter's own cells, bounded by the page rather than the table, and is ordered as of a recent snapshot rather than live — and a counter read inside a transaction is **not** serialized against concurrent increments — so never gate a write on a counter value, or on a count derived from one, read in the same transaction. **Reading back a counter you added to in the same transaction is now refused** with a `ConstraintViolation` naming the counter: the value it would return is correct (it includes your add) but carries no read-conflict range, so ten transactions can each read 9, each decide they are under the limit, and all ten commit. If you genuinely depend on the value, read it non-snapshot and accept that concurrent writers will conflict with you; if you only want to report it, read it after the transaction commits. Note the limit: only reading back a cell **you bumped** is refused — reading a counter you did not touch and branching on it in your own code is still unserialized, and still yours to avoid.

### Reading

`Query` is the fluent read path. It compiles to the store's structured `find`, and the planner picks an index from the predicate and sort. Given a `Post` model with `room_id` and `created_at` columns and a matching `list_by(filter = "room_id", newest = "created_at")` access pattern:

```rust
let recent: Vec<Row> = Query::on(Post::TABLE)
    .filter(Post::room_id.eq(42_i64))
    .filter(Post::created_at.gt(0_i64))
    .order(Post::created_at.desc())
    .limit(50)
    .fetch_all()?;

let one: Option<Row> = Query::on(Post::TABLE).filter(Post::slug.eq("hello")).fetch_one()?;
let n: u64 = Query::on(Post::TABLE).filter(Post::room_id.eq(42_i64)).count()?;
```

Operators live on the typed column handle the derive emits (`Post::room_id`, a `Col<i64>`), so `Post::room_id.eq("nope")` does not compile and `is_null()` is offered only on a column that can hold one. Two verbs carry the query: repeated `.filter(..)` calls AND together (compose with `.and(..)`/`.or(..)` for structure), and `.order(..)` takes either a column ordering or an aggregate one, because `ORDER BY` is one clause. The uppercase const (`Post::ROOM_ID`) stays for the places a column is genuinely just a name — row accessors, `agg::sum(..)`.

**`fetch_all` requires a `.limit(n)` and will not compile without one.** The builder carries its row ceiling in its type: a query starts unbounded, `.limit(n)` moves it to the bounded state, and the two row-materializing terminals (`fetch_all`, `fetch_all_with_total`) exist only there. So `Query::on(T).order(..).fetch_all()` is a compile error naming the missing bound, not a request the store answers with a page of its own choosing. `fetch_one`, `count`, `fetch_page` and the aggregate terminals are bounded by their own construction and need no `.limit(..)`.

`fetch_all` truncates *by your instruction*: it returns the first `n` rows in the query's order and says nothing about the rest. That is the right verb for a top-N, an `is_in` over `n` ids, or an existence probe at `.limit(1)`. When the matching set grows with the tenant, use `fetch_page(|row| …)` — same `.limit(..)`, plus the cursor that continues the listing.

`fetch_all` and `fetch_one` discard the total row count, so they set the store's `skip_total` flag and the host skips computing it. Use `fetch_all_with_total()` when you actually display a total, and `fetch_page(|row| …)` plus `.cursor(token)` for cursor pagination — **the ordering is the cursor key**, so there is no second verb naming it, and `.cursor(..)` takes the opaque token the client round-trips rather than a decoded one. `count()` sends the lowered predicate and ignores ordering and paging, which cannot change a count; an **OR predicate is refused** rather than dropped, because the store's count op takes a conjunction only and a count carries no evidence of what it counted.

Declare the access pattern your query needs (`list_by`, `ranked_by`, `lookup_by`, `tagged_by`, or an explicit index) so the planner has an index to walk. A query the planner cannot serve from an index degrades to a table scan. That is **warned and metered, never refused**: the guardrail logs an actionable hint and `keys_examined` records what the read actually looked at, so the cost is visible and priced. What stops an abusive scan is the per-request budget (`[limits] cpu_deadline_ms`), which cuts the request off *and* cancels the work. There is no per-query opt-out and no strict mode — an unindexed read on a declared column is caught at BUILD time by the service-conventions gate instead.

Underneath, `store::find(table, &opts)` is the raw form, and it is what you reach for when you need a shape the builder does not express:

```rust
let opts = store::FindOptions {
    filters: vec![store::Filter {
        column: "is_active".into(),
        op: store::FilterOp::Eq,
        val: store::Value::Boolean(true),
        // Set only by the `in` op; required on every Filter regardless.
        in_values: None,
    }],
    or_groups: vec![],
    order_by: vec![store::OrderTerm::Column(
        store::SortBy { column: "created_at".into(), dir: store::SortDir::Desc },
    )],
    page: Some(store::Page { limit: 50, offset: 0 }),
    skip_total: true,
    group_cursor: None,                   // resume a ranked listing
    counters: vec![],                     // name counter columns to merge them
};
let result = store::find("users", &opts)?;
let ids: Vec<u64> = result.rows.iter().map(|r| to_sdk_row(r).id()).collect();
```

`result.total_count` is `Option<u64>` — `None` when `skip_total` declined the
count, which is a different answer from `Some(0)`. `result.has_more` says
whether more rows follow this page, and on the raw form it is the only thing
that does: the store clamps `page.limit` to its own per-call ceiling, so a short
page may be the ceiling rather than the end, and a full page may be the end
rather than the ceiling. Never derive "there is more" from the row count, and do
not try to escape it by asking for `limit + 1` — that ask is clamped too. The
`Query` terminals do this for you; this is the escape hatch's share of the work.

`to_sdk_row` converts a raw WIT row into the typed `Row` (`row.text(col)`, `row.int(col)`, `row.bool(col)`, `row.real(col)`, `row.id() -> u64`, `row.to_json(&[…])`). `M::from_row(&row)` goes one step further and rebuilds the model.

### Writing

`db_insert(&m) -> u64`, `db_update::<M>(id, &m)`, `db_delete::<M>(id)` are the typed writes. Beneath them, `store::insert(table, &[store::Column { name, val: store::Value::* }]) -> u64`, `store::update(table, id, &cols) -> bool` and `store::delete(table, id) -> bool` are the raw forms — note that ids are `u64` throughout, never strings.

`upsert(table, key, columns)` and `upsert_increment(table, key, counter, delta, columns)` are keyed on a **unique index over the key columns**, which must exist. `columns` is an `UpsertColumns { always, on_insert }`: `always` is written on every call (insert and update alike); `on_insert` is written only by the call that creates the row, then never touched again — the answer for a computed value (`now()`, a derived id) that a static `default` can't express. `upsert_increment` is only conflict-free when `counter` names a counter column *and* `always` is empty — a non-empty `always` rewrites the row and reintroduces the conflict; `on_insert` does not, since it plays no part in any call after the first.

`store::update_where(table, filters, fields)` and `store::delete_where(table, filters)` apply a predicate to many rows in one call. **They evaluate the predicate by scanning the whole table** — no index is consulted, on either the plain or the transactional path. They are correct, and they are the right way to serialize a decision against concurrent counter increments (only the *matched* rows enter the read set), but they are not a way to make a bulk update cheap.

### Transactions

`tx(|| …)` runs a closure with every `store::*` call inside it — locally and across every `peer::fetch` hop — in one atomic store transaction. On `Ok` it commits; on `Err` it rolls back.

```rust
// One vote on a post: record it and move the post's score, or neither.
// `Post::slug` is the unique lookup key; `vote_score` is a counter column.
let slug = "hello".to_string();
tx::<_, _, ApiError>(|| {
    db_insert(&Receipt {
        id: Id::new(0),
        owner_principal: principal.clone(),
        subject: slug.clone(),
        created_at: now,
    })?;
    upsert_increment(
        Post::TABLE,
        &[store::Column {
            name: Post::SLUG.into(),
            val: store::Value::Text(slug.clone()),
        }],
        Post::VOTE_SCORE,
        store::Value::Integer(1),
        UpsertColumns::none(), // a non-empty `always` rewrites the row and reintroduces the conflict
    )?;
    Ok(())
})?;
```

Name the error type with a turbofish when inference cannot: any `E: From<store::StoreError>` works, so structured errors (`ApiError::conflict(...)`) raised from inside the closure survive to the client instead of flattening to `internal`.

**A commit conflict is retried for you.** When the store aborts an attempt at commit — because a concurrent writer overlapped this transaction's read/write set, or because its read snapshot aged out of the store's version window — nothing from that attempt landed, so the SDK re-runs the closure. Only the closure: parsing, auth, guards and any computation before `tx` run exactly once. If every attempt conflicts the result is `StoreError::TooContended` → **503**, not a 409 — so a 409 out of a transaction means your write genuinely conflicts (a unique-index violation, a `Poisoned` participant, an ambiguous `CommitUnknown`) rather than that you lost a race. The 503's problem+json `detail` carries a `(Retry-After: 1)` hint as **text**; there is no `Retry-After` header, so do not parse for one. `BOOGY_TX_MAX_ATTEMPTS=1` restores the pre-retry behaviour exactly.

Because the closure may run more than once it is `Fn`, not `FnOnce`: clone what you need to consume, and do expensive work *before* opening the transaction. That single rule answers both failure causes — a small store-only closure neither contends widely nor outlives the version window.

Two things that are denied inside a transaction, neither of which poisons it: `outbound_http`, and every `signing` **write**. Both are irreversible, and the closure is re-runnable. Enqueuing a background job *is* allowed — it is staged and submitted only if the transaction commits.

One read to watch: an **unfiltered** `count()` inside a transaction reads every row key in the table, and that whole range enters the transaction's read set. It therefore conflicts with *any* write to that table, including an update to a row you never looked at. Filter the count, or take it outside the transaction.

## Spec endpoints

Every deployed service automatically serves `GET …/openapi.json` (OpenAPI 3.0.3) and — when one or more `Router::rpc(...)` mounts exist — `GET …/openrpc.json` (OpenRPC 1.3.2). No handler code required.

- `Router::info(title, version, description)` — set the doc identity.
- `Router::undocumented(|g| …)` — register routes without recording them in the spec; they still dispatch normally.
- `.summary(...)` / `.description(...)` before a route annotate it.
- `Router::mcp(path, handler)` — MCP mount; records the endpoint in `openapi.json`.
- `Router::rpc(path, || Dispatcher::new()…)` — JSON-RPC mount; captures method shapes for `openrpc.json`.
- Payload types need `schemars::JsonSchema` derived alongside `Serialize`/`Deserialize` to appear in the generated schema.

Two-tier visibility: anonymous callers see only unguarded routes; authenticated callers see everything.

## JSON-RPC

For logic that does not fit a REST shape, mount one dispatcher that routes by `method` name. The `Dispatcher` handles envelope parsing, params decoding, and error mapping — you register typed handlers:

```rust
fn build_router() -> Router {
    Router::new().rpc("/api/notes/rpc", || {
        boogy_sdk::rpc::Dispatcher::new()
            .method("search_notes", search_notes)
            .method("share_note", share_note)
    })
}
```

A method handler is `fn(P) -> Result<R, RpcError>` where `P: Deserialize + JsonSchema` and `R: Serialize + JsonSchema` — the schema bounds are what feed `openrpc.json`. `String` and `&str` convert into `RpcError::internal(...)`, so `?` works inside method bodies. Failures map to standard codes:

| Failure | Code |
|---|---|
| Missing body | `-32600 invalid_request` |
| Body not parseable as an envelope | `-32700 parse_error` |
| Unknown method | `-32601 method_not_found` |
| Params don't match the handler's `P` | `-32602 invalid_params` |
| Handler returned `Err(RpcError)` | passed through as-is |
| Result serialisation failed | `-32603 internal` |

`RpcError::application(code, msg)` carries an application-defined positive code. The dispatcher also answers the OpenRPC `rpc.discover` method in-protocol, unless you register your own handler for that name.

## Host capabilities

The host exposes these WIT interfaces to a service, each imported by the `service` world: `store`, `auth`, `runtime`, `peer`, `outbound-http`, `secrets`, `signing`, `background-jobs`, `websockets`. The `service-with-jobs` world adds the `job-handler` export.

Access is **deny-by-default** and granted in the deployment manifest's `[capabilities]` block, whose full set is:

```toml
[capabilities]
store = true            # the per-service store
auth = true             # caller identity
clock = true            # runtime::now_millis
entropy = true          # runtime::random_bytes
logging = true          # runtime::log
peer = false            # cross-service peer::fetch
outbound_http = false   # calls to third parties (requires an [outbound] allowlist)
background_jobs = false # enqueue / cancel / status
websockets = false      # publish to declared channels
signing = false         # host-mediated signing keys
```

Three of those grants — `clock`, `entropy`, `logging` — gate one function each of the single `runtime` interface: `now_millis`, `random_bytes` and `log` respectively. Denying one is graceful rather than fatal: `now_millis` returns 0, `random_bytes` returns zeroes, and `log` is silently dropped.

The SDK wraps most of this so handler code stays plain (`now_millis()`, `random_bytes(n)`, `peer_fetch(...)`, `jobs_enqueue(...)`, `ws_publish(...)`, `signing_sign_message(...)`, and the `log::info!` family). The store's WIT interface also declares raw `execute` / `query` SQL functions; they are **not implemented** and every call fails — the structured API above is the only read/write path.

## Building and deploying

**Start from the `smoke/` crate shipped alongside this SDK** — it is the minimal working service and the canonical consumer setup. Copy it and change the path deps to git deps pinned to a `rev`. Four things in it are load-bearing and easy to get wrong from scratch:

```toml
[lib]
crate-type = ["cdylib"]     # cdylib ONLY — see below

[dependencies]
wit-bindgen = "0.46"
serde = { version = "1", features = ["derive"] }
# Required, not optional: `wit_glue!` expands to `::serde_json` absolute
# paths that resolve in YOUR crate's scope.
serde_json = "1"
schemars = { version = "0.8", default-features = false, features = ["derive"] }

[build-dependencies]
boogy-wit = { git = "https://github.com/Boogy-ai/boogy-sdk" }
```

1. **`cdylib` only.** Adding `rlib` to a wasm-target lib breaks a host-triple build: `wit_bindgen::generate!` emits component-only symbols that do not host-link, so `cargo test` fails at link time. Put host-testable pure logic in a sibling crate that depends on neither `wit-bindgen` nor `boogy-sdk`.
2. **`serde_json` is mandatory** even if your code never names it, for the reason in the comment above.
3. **`boogy-wit` is a build-dependency**, and `build.rs` copies its `.wit` files into `./wit`, because `wit_bindgen::generate!` needs a literal manifest-relative path. `./wit` is generated: gitignore it, never hand-edit it. This is what keeps the bindings in step with the SDK revision in your `Cargo.lock`.
4. **`schemars`** is what makes your DTOs appear in the generated OpenAPI/OpenRPC documents.

```bash
cargo build --target wasm32-wasip2 --release
boogy deploy path/to/boogy.toml
```

`boogy deploy` is sugar for two steps you can also run separately: `boogy publish` uploads an immutable, versioned module — manifest + wasm, multipart to `POST /v1/modules` — and `boogy provision` (`POST /v1/services`) instantiates a service from it. The manifest declares the route prefix, capabilities, ingress policy and resource limits; unknown keys anywhere in it — including inside `[capabilities]` — are rejected at parse time. See the Boogy documentation for the full manifest schema.
