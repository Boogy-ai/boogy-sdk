//! Request router with path parameter extraction and standards-compliant
//! method dispatch.
//!
//! ```ignore
//! Router::new()
//!     .get("/api/todos", list_todos)
//!     .post("/api/todos", create_todo)
//!     .get("/api/todos/{id}", get_todo)
//!     .delete("/api/todos/{id}", delete_todo)
//!     .get("/files/{*path}", serve_file)         // catch-all path param
//!     .route(&["GET", "POST"], "/api/sync", sync_handler)  // multi-method
//!     .handle(&req)
//! ```
//!
//! Dispatch behaviour:
//! - **404 Not Found** — no path matched.
//! - **405 Method Not Allowed** — path matched but method isn't registered.
//!   Response includes an `Allow:` header listing the methods that ARE
//!   registered for that path (plus HEAD when GET is present, plus OPTIONS).
//! - **HEAD** — if HEAD isn't explicitly registered, the matching GET
//!   handler runs and the response body is stripped.
//! - **OPTIONS** — if OPTIONS isn't explicitly registered, returns
//!   `204 No Content` with the `Allow:` header.
//!
//! Path parameter syntax (matchit-flavoured):
//! - `/{name}` — single-segment named parameter.
//! - `/{*rest}` — catch-all, captures the remainder of the path.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::ctx::Ctx;
use crate::response::{self, IntoResponse};

/// Extracted path parameters from a matched route.
pub struct Params {
    pairs: Vec<(String, String)>,
}

impl Params {
    /// Borrow the underlying (name, value) pairs. Used by in-crate code
    /// that needs to re-encode params for serde (e.g. `Path<T>::from_request`).
    pub(crate) fn as_pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Construct from a vec of (name, value) pairs. Only used by
    /// in-crate unit tests (e.g. `extract::tests`).
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self { pairs }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn require(&self, name: &str) -> Result<&str, String> {
        self.get(name).ok_or_else(|| format!("missing path param: {name}"))
    }

    /// Parse a path param into a typed value via [`std::str::FromStr`].
    ///
    /// The most common use is integer ids (`req.params.parse::<i64>("id")?`),
    /// but anything that implements `FromStr` works — `uuid::Uuid`,
    /// custom newtype wrappers, etc.
    ///
    /// Failure modes:
    /// - Param missing → `ApiError::bad_request("missing path param: <name>")`
    /// - Parse failure → `ApiError::bad_request("invalid <name>: <reason>")`
    ///
    /// Both surface as RFC 7807 `application/problem+json` to the
    /// caller, the same as every other SDK error path.
    pub fn parse<T>(&self, name: &str) -> Result<T, crate::error::ApiError>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        let raw = self
            .get(name)
            .ok_or_else(|| crate::error::ApiError::bad_request(
                format!("missing path param: {name}"),
            ))?;
        raw.parse().map_err(|e| {
            crate::error::ApiError::bad_request(format!("invalid {name}: {e}"))
        })
    }
}

/// Per-request bundle passed to guards and handlers.
///
/// `request` and `params` are borrowed from the dispatch context; `ctx`
/// is owned by `Req` so guards can mutate it (`req.ctx.insert(...)`)
/// and the value flows through the rest of the chain into the handler.
///
/// In handler / guard code, prefer the accessor methods
/// (`req.body()`, `req.header(...)`, `req.method()`, etc.) over reaching
/// through `req.request.X` — the public field stays available for the
/// occasional case where you need the raw `Request` (e.g. handing it
/// off to `mcp::McpServer::handle` or `rpc::Dispatcher::handle`).
pub struct Req<'a> {
    pub request: &'a crate::Request,
    pub params: &'a Params,
    pub ctx: Ctx,
}

impl<'a> Req<'a> {
    /// HTTP method (`GET`, `POST`, ...). Case as-received from the
    /// wire — match against `eq_ignore_ascii_case` if you need to
    /// branch on it.
    pub fn method(&self) -> &str {
        &self.request.method
    }

    /// Request path, including any owner prefix the host did not strip.
    pub fn path(&self) -> &str {
        &self.request.path
    }

    /// Request body bytes, if any. Returns `None` for body-less methods
    /// (GET / DELETE / OPTIONS) and for empty bodies on other methods.
    pub fn body(&self) -> Option<&[u8]> {
        self.request.body.as_deref()
    }

    /// Look up a header by name, case-insensitively. Returns the first
    /// matching value; Boogy's host coalesces duplicates so this is
    /// almost always the right choice.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Look up a query-string parameter by exact (case-sensitive) name.
    pub fn query(&self, name: &str) -> Option<&str> {
        self.request
            .query_params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Decode the request's query string into a typed struct, with
    /// optional `garde::Validate` checks. Symmetric to
    /// [`crate::error::validate_body`] but for `?cursor=…&limit=…`
    /// instead of JSON bodies.
    ///
    /// `T` must implement `serde::Deserialize` and
    /// `garde::Validate<Context = ()>`. Failure modes:
    /// - Malformed encoding or missing required field → `ApiError::bad_request`
    /// - Field-level validation failure → `ApiError::validation` with
    ///   per-field detail (RFC 7807 `errors` map).
    ///
    /// Common pattern for paginated list endpoints:
    /// ```ignore
    /// #[derive(Deserialize, garde::Validate)]
    /// struct ListQuery {
    ///     #[garde(range(min = 1, max = 100))]
    ///     #[serde(default = "default_limit")]
    ///     limit: u32,
    ///     #[garde(skip)]
    ///     cursor: Option<String>,
    /// }
    /// fn default_limit() -> u32 { 20 }
    ///
    /// fn list(req: &mut Req<'_>) -> Result<Json<Page>, ApiError> {
    ///     let q: ListQuery = req.parse_query()?;
    ///     // ...
    /// }
    /// ```
    pub fn parse_query<T>(&self) -> Result<T, crate::error::ApiError>
    where
        T: for<'de> serde::Deserialize<'de> + garde::Validate<Context = ()>,
    {
        let parsed = self.parse_query_raw::<T>()?;
        parsed
            .validate()
            .map_err(crate::error::ApiError::validation)?;
        Ok(parsed)
    }

    /// Like [`Self::parse_query`] but skips `garde` validation. Use
    /// when the query struct has no validation rules — saves pulling
    /// `garde` into the crate's Cargo.toml just to satisfy a trait
    /// bound. Returns the deserialized struct directly with no
    /// validation step.
    ///
    /// ```ignore
    /// #[derive(Deserialize)]
    /// struct Cursor { cursor: Option<String> }
    ///
    /// fn list(req: &mut Req<'_>) -> Result<Json<Page>, ApiError> {
    ///     let q: Cursor = req.parse_query_raw()?;
    ///     // ...
    /// }
    /// ```
    pub fn parse_query_raw<T>(&self) -> Result<T, crate::error::ApiError>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        // The host hands us the query as a `Vec<(String, String)>` it
        // already decoded from the wire. Re-encode to the
        // `serde_urlencoded` shape so the deserializer can drive the
        // typed mapping. Two-step round-trip is cheap (query strings
        // are small) and keeps us off any bespoke parser.
        let encoded = serde_urlencoded::to_string(&self.request.query_params)
            .map_err(|e| {
                crate::error::ApiError::bad_request(format!("query encode: {e}"))
            })?;
        serde_urlencoded::from_str(&encoded).map_err(|e| {
            crate::error::ApiError::bad_request(format!("invalid query: {e}"))
        })
    }
}

/// Handler function signature.
///
/// `Rc`-shared so a single handler can be registered against multiple
/// methods (e.g. via `route_many`) without re-allocating, and so the
/// guard chain can be cloned cheaply into each route's snapshot.
/// Wasm components run single-threaded per request, so `Rc` is the
/// right cost; no `Send`/`Sync` bounds needed.
pub type Handler = Rc<dyn Fn(&mut Req<'_>) -> crate::response::HttpResponse>;

/// Pre-handler guard. Returns `Ok(())` to allow the request through to
/// the handler, or `Err(response)` to short-circuit with that response.
///
/// Guards may write into `req.ctx` to pass loaded resources, parsed
/// bodies, etc. through to the handler without re-fetching.
pub type Guard = Rc<dyn Fn(&mut Req<'_>) -> Result<(), crate::response::HttpResponse>>;

/// Convert closures and SDK-provided builder types into a [`Handler`].
///
/// The `Args` type parameter is a marker that distinguishes different
/// handler shapes so their `IntoHandler` impls don't coherence-conflict:
///
/// - `Args = RawReq` — the existing `Fn(&mut Req<'_>) -> R` shape.
/// - `Args = HandlerMarker` — an already-constructed `Handler`.
/// - `Args = (E1,)` .. `(E1,E2,E3,E4,E5,E6)` — typed extractor handlers
///   where each `Ei: FromRequest`.
///
/// Handlers can return:
/// - `HttpResponse` (identity)
/// - `Json<T>`, `Created<T>`, `NoContent`, `Redirect` (wrapper types)
/// - `Result<R, ApiError>` (or any `E: Into<ApiError>`) — `?` flows through
/// - `Option<R>` (`None` becomes 404)
/// - `()` (becomes 204)
///
/// Downstream crates can `impl IntoResponse` on their own domain types
/// and have handlers return them directly.
pub trait IntoHandler<Args> {
    fn into_handler(self) -> Handler;

    /// Spec-capture hook, called once at registration. Default: an empty
    /// spec (path+method appear in the doc with no shape detail).
    fn describe() -> crate::spec::OperationSpec
    where
        Self: Sized,
    {
        crate::spec::OperationSpec::default()
    }
}

/// Marker for the `Fn(&mut Req<'_>) -> R` handler shape.
///
/// Keeps this `IntoHandler` impl distinct from the extractor impls
/// (each has a different `Args` type ⇒ no coherence overlap).
pub struct RawReq;

impl<F, R> IntoHandler<RawReq> for F
where
    F: Fn(&mut Req<'_>) -> R + 'static,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req| self(req).into_response())
    }

    fn describe() -> crate::spec::OperationSpec {
        crate::spec::OperationSpec { response: R::describe(), ..Default::default() }
    }
}

/// Marker for the identity passthrough: an already-constructed `Handler`
/// (e.g. shared via `Rc::clone` in `route_many`) registers as itself, no rewrap.
///
/// `Handler` is `Rc<dyn Fn(...) -> HttpResponse>`. On stable Rust the
/// `Fn`-trait blanket impls for `Rc<F>` are unstable, so `Handler`
/// does NOT satisfy the `F: Fn(&mut Req) -> R` blanket above —
/// meaning this explicit impl coexists without overlap.
pub struct HandlerMarker;

impl IntoHandler<HandlerMarker> for Handler {
    fn into_handler(self) -> Handler {
        self
    }
}

// ─── Extractor arity impls (1..=6) ─────────────────────────────────────────
//
// Each arity uses a distinct tuple-marker `Args` type so the impls don't
// coherence-conflict with each other or with `RawReq`/`HandlerMarker`.
// Extractors run in argument order; the first `Err` short-circuits.

impl<F, R, E1> IntoHandler<(E1,)> for F
where
    F: Fn(E1) -> R + 'static,
    E1: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        op
    }
}

impl<F, R, E1, E2> IntoHandler<(E1, E2)> for F
where
    F: Fn(E1, E2) -> R + 'static,
    E1: crate::extract::FromRequest,
    E2: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e2 = match <E2 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1, e2).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        E2::describe(&mut op);
        op
    }
}

impl<F, R, E1, E2, E3> IntoHandler<(E1, E2, E3)> for F
where
    F: Fn(E1, E2, E3) -> R + 'static,
    E1: crate::extract::FromRequest,
    E2: crate::extract::FromRequest,
    E3: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e2 = match <E2 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e3 = match <E3 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1, e2, e3).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        E2::describe(&mut op);
        E3::describe(&mut op);
        op
    }
}

impl<F, R, E1, E2, E3, E4> IntoHandler<(E1, E2, E3, E4)> for F
where
    F: Fn(E1, E2, E3, E4) -> R + 'static,
    E1: crate::extract::FromRequest,
    E2: crate::extract::FromRequest,
    E3: crate::extract::FromRequest,
    E4: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e2 = match <E2 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e3 = match <E3 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e4 = match <E4 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1, e2, e3, e4).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        E2::describe(&mut op);
        E3::describe(&mut op);
        E4::describe(&mut op);
        op
    }
}

impl<F, R, E1, E2, E3, E4, E5> IntoHandler<(E1, E2, E3, E4, E5)> for F
where
    F: Fn(E1, E2, E3, E4, E5) -> R + 'static,
    E1: crate::extract::FromRequest,
    E2: crate::extract::FromRequest,
    E3: crate::extract::FromRequest,
    E4: crate::extract::FromRequest,
    E5: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e2 = match <E2 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e3 = match <E3 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e4 = match <E4 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e5 = match <E5 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1, e2, e3, e4, e5).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        E2::describe(&mut op);
        E3::describe(&mut op);
        E4::describe(&mut op);
        E5::describe(&mut op);
        op
    }
}

impl<F, R, E1, E2, E3, E4, E5, E6> IntoHandler<(E1, E2, E3, E4, E5, E6)> for F
where
    F: Fn(E1, E2, E3, E4, E5, E6) -> R + 'static,
    E1: crate::extract::FromRequest,
    E2: crate::extract::FromRequest,
    E3: crate::extract::FromRequest,
    E4: crate::extract::FromRequest,
    E5: crate::extract::FromRequest,
    E6: crate::extract::FromRequest,
    R: IntoResponse,
{
    fn into_handler(self) -> Handler {
        Rc::new(move |req: &mut Req<'_>| {
            let e1 = match <E1 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e2 = match <E2 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e3 = match <E3 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e4 = match <E4 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e5 = match <E5 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            let e6 = match <E6 as crate::extract::FromRequest>::from_request(req) {
                Ok(v) => v,
                Err(err) => return crate::response::HttpResponse::from(err),
            };
            self(e1, e2, e3, e4, e5, e6).into_response()
        })
    }

    fn describe() -> crate::spec::OperationSpec {
        let mut op = crate::spec::OperationSpec { response: R::describe(), ..Default::default() };
        E1::describe(&mut op);
        E2::describe(&mut op);
        E3::describe(&mut op);
        E4::describe(&mut op);
        E5::describe(&mut op);
        E6::describe(&mut op);
        op
    }
}

/// Convert closures and SDK-provided builder types into a [`Guard`].
///
/// Implemented for any `Fn(&mut Req) -> Result<(), HttpResponse> +
/// 'static`. SDK guard factories (e.g. `owns_resource(...).slot(...)`)
/// implement this directly so they can be passed to `.group([...], |g| ...)`
/// without an explicit `.build()` call.
pub trait IntoGuard {
    fn into_guard(self) -> Guard;
}

impl<F> IntoGuard for F
where
    F: Fn(&mut Req<'_>) -> Result<(), crate::response::HttpResponse> + 'static,
{
    fn into_guard(self) -> Guard {
        Rc::new(self)
    }
}

impl IntoGuard for Guard {
    fn into_guard(self) -> Guard {
        self
    }
}

/// Construct a `Vec<Guard>` from multiple heterogeneous `IntoGuard` values.
///
/// Each element is converted via [`IntoGuard::into_guard`] and collected
/// into a vec. Use with [`Router::group`] when guards stacked at a route
/// subset have different Rust types (e.g. a fn-item + a struct guard
/// like [`OwnsResource`](crate::auth::OwnsResource)).
///
/// For a single guard, an array literal works directly:
/// `.group([require_scope("admin")], |g| ...)`. The macro is for the
/// 2+ heterogeneous case where the array would otherwise fail to type-check.
///
/// ```ignore
/// use boogy_sdk::guards;
///
/// Router::new()
///     .group(guards![api_key_routes::guard, auth::owns_resource("links", "owner_id", "id")],
///            |g| g.get("/links/{id}", get_link)
///                 .patch("/links/{id}", update_link)
///                 .delete("/links/{id}", delete_link))
/// ```
#[macro_export]
macro_rules! guards {
    ($($g:expr),* $(,)?) => {
        ::std::vec![$(<_ as $crate::router::IntoGuard>::into_guard($g)),*]
    };
}

/// One registered route's payload: the handler plus the chain of guards
/// that must all pass before the handler runs. Outer guards (declared
/// on a parent router that nests this one) come first; inner guards
/// (declared on the same router as the route) come after.
struct RouteEntry {
    handler: Handler,
    guards: Vec<Guard>,
}

/// Per-path map from HTTP method to (handler + guards). BTreeMap so the
/// `Allow:` header has a stable, sorted method order.
struct MethodTable {
    handlers: BTreeMap<String, RouteEntry>,
}

impl MethodTable {
    fn new() -> Self {
        Self { handlers: BTreeMap::new() }
    }

    /// Compose the `Allow:` header value for this path. Always includes
    /// HEAD (if GET is registered) and OPTIONS, since the router answers
    /// both automatically.
    fn allow_header(&self) -> String {
        let mut methods: Vec<String> = self.handlers.keys().cloned().collect();
        if self.handlers.contains_key("GET") && !self.handlers.contains_key("HEAD") {
            methods.push("HEAD".to_string());
        }
        if !self.handlers.contains_key("OPTIONS") {
            methods.push("OPTIONS".to_string());
        }
        methods.sort();
        methods.dedup();
        methods.join(", ")
    }
}

/// HTTP request router.
pub struct Router {
    /// Routes grouped by path pattern, preserving registration order.
    /// Keeping a Vec (rather than a HashMap) makes the iteration order
    /// deterministic — matters for matchit insertion: the FIRST insert
    /// wins on conflict, so a user-supplied static path beats a
    /// catch-all that was registered later under a different builder
    /// chain (matchit itself rejects ambiguous patterns at insert).
    routes_by_path: Vec<(String, MethodTable)>,
    /// Guards that apply to routes registered inside a `.group()` or
    /// inherited via `.nest()` internally. Already-registered routes
    /// carry their guards in their own RouteEntry.guards — populated
    /// only by `.group()` and `.nest()` internally.
    group_guards: Vec<Guard>,
    /// Spec entries captured at registration; serialized on demand by the
    /// auto-mounted doc endpoints.
    specs: Vec<crate::spec::SpecEntry>,
    /// Identity block for generated docs. None → spec::DocInfo::default().
    doc_info: Option<crate::spec::DocInfo>,
    /// When `true`, `route()` skips the spec push — used by
    /// `Router::undocumented()` to register internal routes without
    /// leaking them in the generated spec.
    undocumented: bool,
    /// JSON-RPC method tables keyed by mount path. Each entry is
    /// `(path, guarded, methods)`. Populated by `Router::rpc` (Task 5);
    /// declared here so `rpc_method_specs()` and the `undocumented()`
    /// builder can reference it now.
    rpc_specs: Vec<(String, bool, Vec<crate::spec::MethodSpec>)>,
    /// Summary staged by `Router::summary`, applied to (and cleared by) the
    /// NEXT route registered. `None` between annotations.
    pending_summary: Option<String>,
    /// Description staged by `Router::description`, applied to (and cleared
    /// by) the NEXT route registered. `None` between annotations.
    pending_description: Option<String>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Self {
            routes_by_path: vec![],
            group_guards: vec![],
            specs: vec![],
            doc_info: None,
            undocumented: false,
            rpc_specs: vec![],
            pending_summary: None,
            pending_description: None,
        }
    }

    /// Attach a one-line summary to the NEXT route/method registered. Flows into
    /// the generated openapi.json (`summary`) so REST clients + agents see what
    /// the endpoint does.
    pub fn summary(mut self, s: impl Into<String>) -> Self {
        self.pending_summary = Some(s.into());
        self
    }

    /// Attach a longer description to the NEXT route registered (openapi `description`).
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.pending_description = Some(d.into());
        self
    }

    /// Register a single (method, path, handler) triple.
    ///
    /// If `path` was already declared with a different method, this adds
    /// to its method table. If the same (method, path) pair is registered
    /// twice, the second registration replaces the first.
    ///
    /// Each registration captures the router's current `group_guards`
    /// — the route's effective guard chain at handle time. Use `.group()`
    /// to scope guards to a subset of routes.
    ///
    /// Spec capture: unless `self.undocumented` is set, a [`SpecEntry::Rest`]
    /// is pushed before the route is inserted, recording the handler's
    /// operation shape for `…/openapi.json`.
    pub fn route<H, Args>(mut self, method: &str, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        let mut op = H::describe();
        op.summary = self.pending_summary.take();
        op.description = self.pending_description.take();
        if !self.undocumented {
            self.specs.push(crate::spec::SpecEntry::Rest {
                method: method.to_uppercase(),
                path: path.to_string(),
                op,
                guarded: !self.group_guards.is_empty(),
            });
        }
        let h = handler.into_handler();
        self.route_inner(method, path, h)
    }

    /// Core route-insertion logic (no spec push). Called by `route()` and
    /// directly by `route_many()` / `rpc()` / `mcp()` so they can manage
    /// spec entries themselves without triggering a double-push.
    fn route_inner(mut self, method: &str, path: &str, handler: Handler) -> Self {
        let method = method.to_uppercase();
        let entry = RouteEntry {
            handler,
            guards: self.group_guards.clone(),
        };
        self.merge_route_entry(path, method, entry);
        self
    }

    /// Insert one (path, method) → RouteEntry, creating the path's
    /// MethodTable on first use. The single insertion point shared by
    /// `route_inner` and the `nest`/`group`/`undocumented` merge loops.
    /// `method` must already be uppercase (every caller registers through
    /// `route_inner`, which uppercases on insertion).
    fn merge_route_entry(&mut self, path: &str, method: String, entry: RouteEntry) {
        debug_assert_eq!(method, method.to_uppercase(),
            "RouteEntry method should be uppercase by the time it reaches merge_route_entry");
        if let Some((_, mt)) = self.routes_by_path.iter_mut().find(|(p, _)| p == path) {
            mt.handlers.insert(method, entry);
        } else {
            let mut mt = MethodTable::new();
            mt.handlers.insert(method, entry);
            self.routes_by_path.push((path.to_string(), mt));
        }
    }

    /// Register the same handler against multiple methods on one path.
    /// Useful for endpoints that intentionally handle e.g. both GET and POST.
    pub fn route_many<H, Args>(mut self, methods: &[&str], path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        // Capture the spec once (before type erasure) then push per method.
        // Using route_inner for each registration avoids double spec entries.
        let mut op = H::describe();
        op.summary = self.pending_summary.take();
        op.description = self.pending_description.take();
        let h = handler.into_handler();
        for m in methods {
            if !self.undocumented {
                self.specs.push(crate::spec::SpecEntry::Rest {
                    method: m.to_uppercase(),
                    path: path.to_string(),
                    op: op.clone(),
                    guarded: !self.group_guards.is_empty(),
                });
            }
            self = self.route_inner(m, path, h.clone());
        }
        self
    }

    pub fn get<H, Args>(self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        self.route("GET", path, handler)
    }

    pub fn post<H, Args>(self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        self.route("POST", path, handler)
    }

    pub fn put<H, Args>(self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        self.route("PUT", path, handler)
    }

    pub fn patch<H, Args>(self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        self.route("PATCH", path, handler)
    }

    pub fn delete<H, Args>(self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        self.route("DELETE", path, handler)
    }

    /// Mount another router under a path prefix.
    ///
    /// Each route in `sub` is re-registered on `self` with `prefix`
    /// prepended. Nests can be nested arbitrarily — composition is
    /// just path concatenation, so `outer.nest("/a", inner.nest("/b", x))`
    /// produces routes under `/a/b`.
    ///
    /// ```ignore
    /// fn build_router() -> Router {
    ///     Router::new()
    ///         .nest("/api/v1", v1_routes())
    ///         .nest("/api/v2", v2_routes())
    ///         .nest("/admin", admin_routes())
    /// }
    /// ```
    ///
    /// Path-joining rules:
    /// - A trailing slash on the prefix is dropped: `/api/v1/` becomes
    ///   `/api/v1`.
    /// - A bare `/` path on the sub-router maps to the prefix itself
    ///   (so a sub-router's index handler ends up at the prefix).
    /// - An empty prefix is a no-op (sub-router routes mount at the
    ///   same paths they were registered at).
    pub fn nest(mut self, prefix: &str, sub: Router) -> Self {
        let prefix = normalize_prefix(prefix);
        // Destructure sub to move out routes and specs independently before
        // the route-merge loop consumes them.
        let Router { routes_by_path: sub_routes, specs: sub_specs, rpc_specs: sub_rpc_specs, .. } = sub;
        // Snapshot of guards on `self` at the time of nesting. These are
        // outer guards — they should run BEFORE the sub-router's own
        // guards on each nested route.
        let outer_guards = self.group_guards.clone();
        for (path, mt) in sub_routes {
            let combined = join_paths(&prefix, &path);
            for (method, mut entry) in mt.handlers {
                // Outer guards first, then the route's pre-existing
                // guards (which already include any sub-router guards
                // captured at sub registration time).
                let mut chain = outer_guards.clone();
                chain.extend(entry.guards);
                entry.guards = chain;
                self.merge_route_entry(&combined, method.to_uppercase(), entry);
            }
        }
        // Mirror the spec path rewrite for every entry in the sub-router.
        // If `self` has active group guards, mark each merged entry as guarded
        // (the nest is happening inside a guarded group).
        let nest_is_guarded = !outer_guards.is_empty();
        for entry in sub_specs {
            let combined = join_paths(&prefix, entry.path());
            let entry = entry.with_path(combined);
            // Propagate the outer guard flag if needed.
            let entry = if nest_is_guarded {
                match entry {
                    crate::spec::SpecEntry::Rest { method, path, op, .. } =>
                        crate::spec::SpecEntry::Rest { method, path, op, guarded: true },
                    crate::spec::SpecEntry::Mcp { path, .. } =>
                        crate::spec::SpecEntry::Mcp { path, guarded: true },
                    crate::spec::SpecEntry::Rpc { path, .. } =>
                        crate::spec::SpecEntry::Rpc { path, guarded: true },
                }
            } else {
                entry
            };
            self.specs.push(entry);
        }
        // Mirror rpc_specs path rewrite — and the SAME guard upgrade the
        // SpecEntry loop applies, so an rpc mount nested inside a guarded
        // group never leaks its method specs to anonymous openrpc.json
        // readers.
        for (path, guarded, methods) in sub_rpc_specs {
            let combined = join_paths(&prefix, &path);
            self.rpc_specs.push((combined, guarded || nest_is_guarded, methods));
        }
        self
    }

    /// Apply a guard collection to every route registered inside the
    /// closure. The closure receiver is a [`RouteSet`], not a [`Router`]
    /// — so guards cannot be extended inside the body. Each `.group()`
    /// call is a self-contained, lexically-bounded unit.
    ///
    /// To find a route's guards: locate the route's line, scan up to
    /// the enclosing `.group([...], |g| ...)` declaration — its array
    /// literal is the complete guard set (plus outer guards if this
    /// Router is itself nested inside another `.group()` via `.nest()`).
    ///
    /// ```ignore
    /// Router::new()
    ///     .group([require_scope("admin")], |g| g
    ///         .get("/_admin/dashboard", dashboard)
    ///         .post("/_admin/posts/{id}/hide", hide_post))
    ///     .get("/health", health)  // public — no guards
    /// ```
    ///
    /// **Heterogeneous-typed guards.** An array literal requires all guards
    /// to have the same concrete type. When stacking guards of different
    /// types (e.g. a fn-item + a struct guard), use the
    /// [`guards!`](crate::guards) macro:
    ///
    /// ```ignore
    /// .group(guards![api_key_routes::guard, auth::owns_resource("links", "owner_id", "id")],
    ///        |g| g.get("/links/{id}", get_link))
    /// ```
    pub fn group<G, F>(mut self, guards: impl IntoIterator<Item = G>, build: F) -> Self
    where
        G: IntoGuard,
        F: FnOnce(RouteSet) -> RouteSet,
    {
        // Build the inner Router with this group's guards layered on top
        // of any outer guards (relevant when this Router is itself nested
        // via .nest() inside another .group()).
        let mut inner_guards = self.group_guards.clone();
        inner_guards.extend(guards.into_iter().map(|g| g.into_guard()));
        let inner = Router {
            routes_by_path: vec![],
            group_guards: inner_guards,
            specs: vec![],
            doc_info: None,
            undocumented: false,
            rpc_specs: vec![],
            pending_summary: None,
            pending_description: None,
        };
        let built = build(RouteSet(inner)).0;
        // Merge built's routes back into self. Each route's RouteEntry
        // already carries its complete guard chain (captured at
        // registration inside the inner Router).
        for (path, mt) in built.routes_by_path {
            for (method, entry) in mt.handlers {
                self.merge_route_entry(&path, method, entry);
            }
        }
        // Merge the inner Router's spec entries; they already carry guarded: true
        // because the inner Router had non-empty group_guards at registration.
        self.specs.extend(built.specs);
        self.rpc_specs.extend(built.rpc_specs);
        self
    }

    /// Dispatch a request to the matching handler.
    ///
    /// Method dispatch order:
    /// 1. Exact (method, path) match → run that handler.
    /// 2. `HEAD` request, no HEAD handler, GET handler exists → run GET,
    ///    strip the response body.
    /// 3. `OPTIONS` request, no OPTIONS handler → return 204 with
    ///    `Allow:` header.
    /// 4. Any other method on a known path → 405 Method Not Allowed with
    ///    `Allow:` header.
    /// 5. Path doesn't match anything → 404.
    pub fn handle(&self, req: &crate::Request) -> response::HttpResponse {
        // Spec-doc requests short-circuit BEFORE pattern matching: the
        // canonical CRUD layout (`/api/notes/{id}`) would otherwise
        // capture `/api/notes/openapi.json` as `id = "openapi.json"` and
        // shadow the doc behind that route's guards. "User routes win"
        // means an EXPLICIT literal registration at the doc path — an
        // incidental param capture doesn't count. (Consequence: the doc
        // filenames are reserved ids within a param segment.)
        if req.method.eq_ignore_ascii_case("GET")
            && (doc_path(&req.path, "openapi.json") || doc_path(&req.path, "openrpc.json"))
        {
            let user_claims_path = self.routes_by_path.iter()
                .any(|(p, mt)| p == &req.path && mt.handlers.contains_key("GET"));
            if !user_claims_path {
                return self.serve_spec_doc_or_404(req);
            }
        }

        let mut matcher = matchit::Router::new();
        for (i, (path, _)) in self.routes_by_path.iter().enumerate() {
            // Insertion failure (e.g. duplicate or invalid pattern) is
            // ignored — a malformed pattern just means that route can't
            // match, not that the whole router is broken.
            let _ = matcher.insert(path.as_str(), i);
        }

        let matched = match matcher.at(&req.path) {
            Ok(m) => m,
            Err(_) => return self.serve_spec_doc_or_404(req),
        };
        let idx = *matched.value;
        let mt = &self.routes_by_path[idx].1;
        let pairs: Vec<(String, String)> = matched
            .params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let params = Params { pairs };

        let method_upper = req.method.to_uppercase();

        // Construct one Req per dispatch. Ctx starts empty; guards
        // populate it; handler reads from it. Drops at end of handle().
        let mut req_obj = Req {
            request: req,
            params: &params,
            ctx: Ctx::new(),
        };

        if let Some(entry) = mt.handlers.get(method_upper.as_str()) {
            for g in &entry.guards {
                if let Err(resp) = g(&mut req_obj) {
                    return resp;
                }
            }
            return (entry.handler)(&mut req_obj);
        }

        // HEAD fallback to GET (RFC 9110 §9.3.2): respond with the GET
        // headers but no body. Guards on the GET route still run.
        if method_upper == "HEAD" {
            if let Some(entry) = mt.handlers.get("GET") {
                for g in &entry.guards {
                    if let Err(resp) = g(&mut req_obj) {
                        return resp;
                    }
                }
                let resp = (entry.handler)(&mut req_obj);
                return response::HttpResponse {
                    status: resp.status,
                    headers: resp.headers,
                    body: None,
                };
            }
        }

        // OPTIONS auto-response: 204 No Content + Allow header.
        if method_upper == "OPTIONS" {
            return response::HttpResponse {
                status: 204,
                headers: vec![("allow".to_string(), mt.allow_header())],
                body: None,
            };
        }

        // 405 Method Not Allowed — RFC 7807 wire shape, with `Allow:`
        // alongside `content-type`. Custom-built rather than going
        // through `ApiError` because the response needs the
        // `Allow` header that `ApiError::*().into()` doesn't produce.
        let body = serde_json::json!({
            "type": "/errors/method_not_allowed",
            "title": "Method not allowed",
            "status": 405,
        })
        .to_string()
        .into_bytes();
        response::HttpResponse {
            status: 405,
            headers: vec![
                ("allow".to_string(), mt.allow_header()),
                (
                    "content-type".to_string(),
                    crate::error::PROBLEM_JSON.to_string(),
                ),
            ],
            body: Some(body),
        }
    }

    /// Set the identity block for generated spec documents
    /// (`…/openapi.json`, `…/openrpc.json`). Optional — defaults to a
    /// generic title and version 0.0.0.
    pub fn info(mut self, title: &str, version: &str, description: Option<&str>) -> Self {
        self.doc_info = Some(crate::spec::DocInfo {
            title: title.to_string(),
            version: version.to_string(),
            description: description.map(str::to_string),
        });
        self
    }

    /// Register routes in the closure without recording them in the generated
    /// spec. The routes handle requests normally; they simply won't appear in
    /// `…/openapi.json` or `…/openrpc.json`.
    ///
    /// Useful for internal health, debug, or admin endpoints that should not
    /// be advertised to external consumers.
    ///
    /// ```ignore
    /// Router::new()
    ///     .post("/api/notes", create_note)
    ///     .undocumented(|g| g.get("/internal/health", health_check))
    /// ```
    pub fn undocumented<F>(mut self, build: F) -> Self
    where
        F: FnOnce(RouteSet) -> RouteSet,
    {
        // Build an inner Router with undocumented=true so route() skips spec pushes.
        let inner = Router {
            routes_by_path: vec![],
            group_guards: self.group_guards.clone(),
            specs: vec![],
            doc_info: None,
            undocumented: true,
            rpc_specs: vec![],
            pending_summary: None,
            pending_description: None,
        };
        let built = build(RouteSet(inner)).0;
        // Merge only the routes (no specs — that's the whole point).
        // This also covers `nest()` inside the block: the sub-router's
        // specs land in built.specs via the nest merge and are dropped
        // wholesale here.
        for (path, mt) in built.routes_by_path {
            for (method, entry) in mt.handlers {
                self.merge_route_entry(&path, method, entry);
            }
        }
        // Intentionally: built.specs and built.rpc_specs are NOT merged.
        self
    }

    /// Mount a JSON-RPC dispatcher at `path` (registered as POST).
    ///
    /// The `build` closure runs once at registration time to capture the
    /// dispatcher's method shapes for `…/openrpc.json` and the OpenAPI
    /// protocol stub, then again per request to dispatch calls (same
    /// per-request idiom as `McpServer`).
    ///
    /// When `self.undocumented` is set both the `SpecEntry` and the
    /// `rpc_specs` entry are skipped — the route still dispatches
    /// normally. `guarded` mirrors the surrounding `group_guards`:
    /// anonymous callers see only unguarded mounts in the generated
    /// `…/openrpc.json`.
    ///
    /// ```ignore
    /// Router::new()
    ///     .rpc("/rpc", || Dispatcher::new()
    ///         .method("search_notes", search_notes)
    ///         .method("share_note", share_note))
    /// ```
    pub fn rpc<F>(mut self, path: &str, build: F) -> Self
    where
        F: Fn() -> crate::rpc::Dispatcher + 'static,
    {
        let guarded = !self.group_guards.is_empty();
        if !self.undocumented {
            let registration_probe = build();
            self.rpc_specs.push((path.to_string(), guarded, registration_probe.method_specs().to_vec()));
            self.specs.push(crate::spec::SpecEntry::Rpc { path: path.to_string(), guarded });
        }
        let handler: Handler = std::rc::Rc::new(move |req: &mut Req<'_>| build().handle(req.request));
        self.route_inner("POST", path, handler)
    }

    /// Mount an MCP dispatch handler at `path` (registered as POST) and
    /// record it as an MCP endpoint in the generated OpenAPI document.
    /// Capability discovery stays in-protocol (`tools/list` etc.).
    ///
    /// When `self.undocumented` is set the `SpecEntry` is skipped — the
    /// route still dispatches normally. `guarded` mirrors the surrounding
    /// `group_guards`, same as [`Router::rpc`].
    ///
    /// ```ignore
    /// Router::new()
    ///     .mcp("/mcp", |req| {
    ///         McpServer::new("my-service", "1.0")
    ///             .tool_typed(tool("do_thing").description("…"), do_thing)
    ///             .handle(req.request)
    ///     })
    /// ```
    pub fn mcp<H, Args>(mut self, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        let guarded = !self.group_guards.is_empty();
        if !self.undocumented {
            self.specs.push(crate::spec::SpecEntry::Mcp { path: path.to_string(), guarded });
        }
        self.route_inner("POST", path, handler.into_handler())
    }

    /// Unmatched request fallback: serve a generated spec document when
    /// the request is a GET for a doc filename anywhere under the
    /// service's subtree (the guest never learns its manifest routing
    /// prefix, so suffix-matching is the reachable contract — the host
    /// only forwards paths inside this service's own prefix). User
    /// routes always win: this only runs when nothing matched.
    ///
    /// Two-tier visibility: guarded routes are hidden from anonymous callers
    /// (no principal in the thread-local) and shown to authenticated ones.
    fn serve_spec_doc_or_404(&self, req: &crate::Request) -> response::HttpResponse {
        if !req.method.eq_ignore_ascii_case("GET") {
            return response::not_found();
        }
        if doc_path(&req.path, "openapi.json") {
            let info = self.doc_info.clone().unwrap_or_default();
            let caller = crate::request_state::_request_principal();
            let authenticated = caller.is_some();
            // Filter entries: anonymous callers see only unguarded routes.
            let entries: Vec<crate::spec::SpecEntry> = self.specs.iter()
                .filter(|e| authenticated || !e.is_guarded())
                .cloned()
                .collect();
            let reg = crate::spec::SpecRegistry { entries };
            let doc = crate::spec::build_openapi(&info, &reg);
            return response::raw(200, doc.to_string().as_bytes(), "application/json");
        }
        if doc_path(&req.path, "openrpc.json") {
            if let Some(methods) = self.rpc_method_specs() {
                let info = self.doc_info.clone().unwrap_or_default();
                let doc = crate::spec::build_openrpc(&info, &methods);
                return response::raw(200, doc.to_string().as_bytes(), "application/json");
            }
        }
        response::not_found()
    }

    /// Return the flattened list of JSON-RPC method specs from all mounted
    /// `Router::rpc` dispatchers, respecting the same two-tier visibility
    /// as `serve_spec_doc_or_404`. Returns `None` when no RPC mounts exist
    /// — or when no methods remain visible (every mount guarded and the
    /// caller anonymous, or only empty dispatchers): both mean 404.
    fn rpc_method_specs(&self) -> Option<Vec<crate::spec::MethodSpec>> {
        if self.rpc_specs.is_empty() {
            return None;
        }
        let caller = crate::request_state::_request_principal();
        let authenticated = caller.is_some();
        let methods: Vec<crate::spec::MethodSpec> = self.rpc_specs.iter()
            .filter(|(_, guarded, _)| authenticated || !guarded)
            .flat_map(|(_, _, methods)| methods.iter().cloned())
            .collect();
        // Return None (→ 404) when all mounts are guarded and caller is
        // anonymous — the openrpc.json document would be empty, which
        // is more confusing than a 404.
        if methods.is_empty() { None } else { Some(methods) }
    }
}

/// Closure receiver for [`Router::group`] — exposes only route-registration
/// methods. (`rpc()`/`mcp()` are intentionally absent: protocol mounts
/// carry spec-registry side effects that must record the group's guard
/// state — mount them on the `Router` and `.nest()` the result instead.) Has no `.group()` method by design, so guards declared at the
/// enclosing `.group([...], |g| ...)` cannot be extended from inside the
/// closure body.
///
/// To attach a different guard set to a different route subset, call
/// `.group()` again on the outer `Router` — each `.group()` call is its
/// own self-contained unit.
pub struct RouteSet(Router);

impl RouteSet {
    /// Register a single (method, path, handler) triple under this
    /// group's guards. Same semantics as [`Router::route`] except the
    /// guards are the group's guards.
    pub fn route<H, Args>(self, method: &str, path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        Self(self.0.route(method, path, handler))
    }

    /// Register the same handler against multiple methods on one path.
    pub fn route_many<H, Args>(self, methods: &[&str], path: &str, handler: H) -> Self
    where
        H: IntoHandler<Args> + 'static,
    {
        Self(self.0.route_many(methods, path, handler))
    }

    /// Set the OpenAPI summary for the NEXT route registered in this group
    /// (same pending-doc semantics as [`Router::summary`]).
    pub fn summary(self, s: impl Into<String>) -> Self {
        Self(self.0.summary(s))
    }

    /// Set the OpenAPI description for the NEXT route registered in this group
    /// (same pending-doc semantics as [`Router::description`]).
    pub fn description(self, d: impl Into<String>) -> Self {
        Self(self.0.description(d))
    }

    pub fn get<H, Args>(self, path: &str, handler: H) -> Self
    where H: IntoHandler<Args> + 'static { Self(self.0.get(path, handler)) }

    pub fn post<H, Args>(self, path: &str, handler: H) -> Self
    where H: IntoHandler<Args> + 'static { Self(self.0.post(path, handler)) }

    pub fn put<H, Args>(self, path: &str, handler: H) -> Self
    where H: IntoHandler<Args> + 'static { Self(self.0.put(path, handler)) }

    pub fn patch<H, Args>(self, path: &str, handler: H) -> Self
    where H: IntoHandler<Args> + 'static { Self(self.0.patch(path, handler)) }

    pub fn delete<H, Args>(self, path: &str, handler: H) -> Self
    where H: IntoHandler<Args> + 'static { Self(self.0.delete(path, handler)) }

    /// Mount a sub-router under a prefix inside this group. Sub-router
    /// routes carry both this group's guards (via the standard nest-merge)
    /// AND any guards declared inside the sub-router's own `.group()` calls.
    pub fn nest(self, prefix: &str, sub: Router) -> Self {
        Self(self.0.nest(prefix, sub))
    }
}

// ─── Path joining (used by nest) + doc-path predicate ───────────────────────

/// `/<filename>` itself or any path ending `/<filename>`.
///
/// Used by `serve_spec_doc_or_404` to serve spec docs via suffix matching —
/// the guest Router never knows the manifest `[routing] path` prefix, so the
/// host can only reach it through the service's own routed subtree.
fn doc_path(path: &str, filename: &str) -> bool {
    // Allocation-free equivalent of `path == "/<filename>" ||
    // path.ends_with("/<filename>")`: stripping the filename suffix must
    // leave something ending in '/' ("/" itself for the root case).
    path.strip_suffix(filename).is_some_and(|rest| rest.ends_with('/'))
}

fn normalize_prefix(p: &str) -> String {
    let p = p.trim_end_matches('/');
    if p.is_empty() {
        return String::new();
    }
    if p.starts_with('/') { p.to_string() } else { format!("/{p}") }
}

fn join_paths(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        return path.to_string();
    }
    if path == "/" || path.is_empty() {
        return prefix.to_string();
    }
    if path.starts_with('/') {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}/{path}")
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;
    use crate::response;

    fn req(method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers: vec![],
            body: None,
            path_params: vec![],
            query_params: vec![],
        }
    }

    fn ok_handler(_req: &mut Req<'_>) -> response::HttpResponse {
        response::ok(&serde_json::json!({"ok": true}))
    }

    fn body_handler(_req: &mut Req<'_>) -> response::HttpResponse {
        response::ok(&serde_json::json!({"hello": "world"}))
    }

    fn echo_id(req: &mut Req<'_>) -> response::HttpResponse {
        response::ok(&serde_json::json!({"id": req.params.get("id").unwrap_or("")}))
    }

    fn echo_path(req: &mut Req<'_>) -> response::HttpResponse {
        response::ok(&serde_json::json!({"path": req.params.get("path").unwrap_or("")}))
    }

    #[test]
    fn match_exact_method_and_path() {
        let r = Router::new().get("/users", ok_handler);
        let resp = r.handle(&req("GET", "/users"));
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn unknown_path_returns_404() {
        let r = Router::new().get("/users", ok_handler);
        let resp = r.handle(&req("GET", "/missing"));
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn known_path_wrong_method_returns_405() {
        let r = Router::new()
            .get("/users", ok_handler)
            .post("/users", ok_handler);
        let resp = r.handle(&req("DELETE", "/users"));
        assert_eq!(resp.status, 405);
        let allow = resp.headers.iter().find(|(k, _)| k == "allow").map(|(_, v)| v.as_str());
        let allow_val = allow.expect("405 must include Allow header");
        // GET, POST registered; HEAD added (since GET present); OPTIONS added.
        assert!(allow_val.contains("GET"));
        assert!(allow_val.contains("POST"));
        assert!(allow_val.contains("HEAD"));
        assert!(allow_val.contains("OPTIONS"));
    }

    #[test]
    fn options_returns_204_with_allow() {
        let r = Router::new().get("/users", ok_handler);
        let resp = r.handle(&req("OPTIONS", "/users"));
        assert_eq!(resp.status, 204);
        let allow = resp.headers.iter().find(|(k, _)| k == "allow").map(|(_, v)| v.as_str());
        assert!(allow.expect("OPTIONS must include Allow").contains("GET"));
    }

    #[test]
    fn explicit_options_handler_wins_over_auto_response() {
        fn custom_options(_req: &mut Req<'_>) -> response::HttpResponse {
            response::HttpResponse {
                status: 200,
                headers: vec![("x-custom".to_string(), "yes".to_string())],
                body: None,
            }
        }
        let r = Router::new()
            .get("/users", ok_handler)
            .route("OPTIONS", "/users", custom_options);
        let resp = r.handle(&req("OPTIONS", "/users"));
        assert_eq!(resp.status, 200);
        assert!(resp.headers.iter().any(|(k, _)| k == "x-custom"));
    }

    #[test]
    fn head_falls_back_to_get_with_body_stripped() {
        let r = Router::new().get("/users", body_handler);
        let resp = r.handle(&req("HEAD", "/users"));
        assert_eq!(resp.status, 200);
        assert!(resp.body.is_none(), "HEAD response must have no body");
    }

    #[test]
    fn explicit_head_handler_wins_over_get_fallback() {
        fn custom_head(_req: &mut Req<'_>) -> response::HttpResponse {
            response::HttpResponse {
                status: 200,
                headers: vec![("x-head".to_string(), "explicit".to_string())],
                body: None,
            }
        }
        let r = Router::new()
            .get("/users", body_handler)
            .route("HEAD", "/users", custom_head);
        let resp = r.handle(&req("HEAD", "/users"));
        assert!(resp.headers.iter().any(|(k, v)| k == "x-head" && v == "explicit"));
    }

    #[test]
    fn named_path_param_extracted() {
        let r = Router::new().get("/users/{id}", echo_id);
        let resp = r.handle(&req("GET", "/users/abc-123"));
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("abc-123"));
    }

    #[test]
    fn catch_all_path_param_captures_remainder() {
        let r = Router::new().get("/files/{*path}", echo_path);
        let resp = r.handle(&req("GET", "/files/a/b/c.txt"));
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("a/b/c.txt"), "got: {body}");
    }

    #[test]
    fn route_many_registers_each_method() {
        let r = Router::new().route_many(&["GET", "POST", "PUT"], "/sync", ok_handler);
        for m in ["GET", "POST", "PUT"] {
            let resp = r.handle(&req(m, "/sync"));
            assert_eq!(resp.status, 200, "method {m} should match");
        }
        let resp = r.handle(&req("DELETE", "/sync"));
        assert_eq!(resp.status, 405, "DELETE not registered → 405");
    }

    #[test]
    fn case_insensitive_method() {
        let r = Router::new().get("/users", ok_handler);
        let resp = r.handle(&req("get", "/users"));
        assert_eq!(resp.status, 200, "lowercase method should match");
    }

    // -- nest --

    #[test]
    fn nest_basic() {
        let v1 = Router::new()
            .get("/users", ok_handler)
            .post("/users", ok_handler);
        let r = Router::new().nest("/api/v1", v1);

        assert_eq!(r.handle(&req("GET",  "/api/v1/users")).status, 200);
        assert_eq!(r.handle(&req("POST", "/api/v1/users")).status, 200);
        assert_eq!(r.handle(&req("GET",  "/users")).status, 404, "without prefix should be 404");
    }

    #[test]
    fn nest_root_path_maps_to_prefix() {
        let sub = Router::new().get("/", ok_handler);
        let r = Router::new().nest("/api/v1", sub);
        // The sub's "/" handler should be reachable at the prefix itself.
        assert_eq!(r.handle(&req("GET", "/api/v1")).status, 200);
        assert_eq!(r.handle(&req("GET", "/api/v1/")).status, 404,
            "we don't synthesize trailing-slash routes");
    }

    #[test]
    fn nest_strips_trailing_slash_on_prefix() {
        let sub = Router::new().get("/users", ok_handler);
        let r = Router::new().nest("/api/v1/", sub);
        assert_eq!(r.handle(&req("GET", "/api/v1/users")).status, 200);
    }

    #[test]
    fn nest_preserves_path_params() {
        let sub = Router::new().get("/{id}", echo_id);
        let r = Router::new().nest("/users", sub);
        let resp = r.handle(&req("GET", "/users/abc-123"));
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("abc-123"), "got: {body}");
    }

    #[test]
    fn nest_can_be_nested() {
        let leaf  = Router::new().get("/list", ok_handler);
        let mid   = Router::new().nest("/users", leaf);
        let outer = Router::new().nest("/api/v1", mid);
        assert_eq!(outer.handle(&req("GET", "/api/v1/users/list")).status, 200);
    }

    #[test]
    fn nest_preserves_method_table_for_405_and_options() {
        let sub = Router::new()
            .get("/users", ok_handler)
            .post("/users", ok_handler);
        let r = Router::new().nest("/api", sub);

        // 405 still works on the nested path with the right Allow header.
        let resp = r.handle(&req("DELETE", "/api/users"));
        assert_eq!(resp.status, 405);
        let allow = resp.headers.iter().find(|(k, _)| k == "allow")
            .map(|(_, v)| v.as_str()).expect("allow");
        assert!(allow.contains("GET"));
        assert!(allow.contains("POST"));

        // OPTIONS auto-response on the nested path.
        let opt = r.handle(&req("OPTIONS", "/api/users"));
        assert_eq!(opt.status, 204);
    }

    #[test]
    fn nest_with_empty_prefix_is_passthrough() {
        let sub = Router::new().get("/users", ok_handler);
        let r = Router::new().nest("", sub);
        assert_eq!(r.handle(&req("GET", "/users")).status, 200);
    }

    #[test]
    fn nest_outer_routes_coexist_with_nested_routes() {
        let v1 = Router::new().get("/users", ok_handler);
        let r = Router::new()
            .get("/health", ok_handler)
            .nest("/api/v1", v1);
        assert_eq!(r.handle(&req("GET", "/health")).status, 200);
        assert_eq!(r.handle(&req("GET", "/api/v1/users")).status, 200);
    }

    // -- path-joining helpers (lightweight unit tests on the pure fns) --

    #[test]
    fn normalize_prefix_handles_edge_cases() {
        assert_eq!(normalize_prefix("/api"), "/api");
        assert_eq!(normalize_prefix("/api/"), "/api");
        assert_eq!(normalize_prefix("api"), "/api");
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("/"), "");
    }

    #[test]
    fn join_paths_handles_edge_cases() {
        assert_eq!(join_paths("/api", "/users"), "/api/users");
        assert_eq!(join_paths("/api", "/"), "/api");
        assert_eq!(join_paths("/api", ""), "/api");
        assert_eq!(join_paths("", "/users"), "/users");
        assert_eq!(join_paths("/api", "users"), "/api/users");
    }

    // -- guards --

    fn allow_guard(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
        Ok(())
    }

    fn deny_guard(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
        Err(response::HttpResponse {
            status: 403,
            headers: vec![],
            body: Some(b"forbidden".to_vec()),
        })
    }

    #[test]
    fn guard_passes_through_when_ok() {
        let r = Router::new()
            .group([allow_guard], |g| g.get("/users", ok_handler));
        assert_eq!(r.handle(&req("GET", "/users")).status, 200);
    }

    #[test]
    fn guard_short_circuits_with_err_response() {
        let r = Router::new()
            .group([deny_guard], |g| g.get("/users", ok_handler));
        let resp = r.handle(&req("GET", "/users"));
        assert_eq!(resp.status, 403);
    }

    #[test]
    fn nested_router_inherits_outer_guards() {
        let admin = Router::new().get("/users", ok_handler);
        let r = Router::new()
            .group([deny_guard], |g| g.nest("/admin", admin));
        assert_eq!(r.handle(&req("GET", "/admin/users")).status, 403,
            "outer guard must apply to nested routes");
    }

    #[test]
    fn nested_routes_keep_their_own_guards_too() {
        // sub-router has its own guard on its routes. When nested under
        // an unguarded outer, the sub guards still fire.
        let sub = Router::new().group([deny_guard], |g| g.get("/users", ok_handler));
        let r = Router::new().nest("/api", sub);
        assert_eq!(r.handle(&req("GET", "/api/users")).status, 403);
    }

    #[test]
    fn outer_guards_run_before_inner_guards() {
        // Use a static counter to verify order. Outer must reach Err
        // before inner can execute.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OUTER_HITS: AtomicUsize = AtomicUsize::new(0);
        static INNER_HITS: AtomicUsize = AtomicUsize::new(0);
        OUTER_HITS.store(0, Ordering::SeqCst);
        INNER_HITS.store(0, Ordering::SeqCst);

        fn outer(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            OUTER_HITS.fetch_add(1, Ordering::SeqCst);
            Err(response::HttpResponse { status: 401, headers: vec![], body: None })
        }
        fn inner(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            INNER_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        let sub = Router::new().group([inner], |g| g.get("/users", ok_handler));
        let r = Router::new().group([outer], |g| g.nest("/api", sub));

        let resp = r.handle(&req("GET", "/api/users"));
        assert_eq!(resp.status, 401);
        assert_eq!(OUTER_HITS.load(Ordering::SeqCst), 1);
        assert_eq!(INNER_HITS.load(Ordering::SeqCst), 0,
            "inner guard should not run if outer rejects");
    }

    #[test]
    fn guard_runs_for_head_fallback_too() {
        let r = Router::new()
            .group([deny_guard], |g| g.get("/users", ok_handler));
        let resp = r.handle(&req("HEAD", "/users"));
        assert_eq!(resp.status, 403,
            "HEAD-fallback-to-GET must also run the GET route's guards");
    }

    #[test]
    fn options_auto_response_does_not_run_guards() {
        // OPTIONS should be answerable without auth — it's just metadata
        // about which methods are supported. (CORS preflight depends on this.)
        let r = Router::new()
            .group([deny_guard], |g| g.get("/users", ok_handler));
        let resp = r.handle(&req("OPTIONS", "/users"));
        assert_eq!(resp.status, 204);
    }

    // -- group --

    #[test]
    fn group_routes_guarded_by_array() {
        let r = Router::new()
            .group([deny_guard], |g| g.get("/admin", ok_handler));
        assert_eq!(r.handle(&req("GET", "/admin")).status, 403,
            "/admin must be guarded by deny_guard");
    }

    #[test]
    fn group_multi_guard_array_runs_all_in_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FIRST_HITS: AtomicUsize = AtomicUsize::new(0);
        static SECOND_HITS: AtomicUsize = AtomicUsize::new(0);
        FIRST_HITS.store(0, Ordering::SeqCst);
        SECOND_HITS.store(0, Ordering::SeqCst);

        fn first(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            FIRST_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())  // pass through — second guard should also run
        }
        fn second(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            SECOND_HITS.fetch_add(1, Ordering::SeqCst);
            Err(response::HttpResponse { status: 403, headers: vec![], body: None })
        }

        let r = Router::new()
            .group([first, second], |g| g.get("/x", ok_handler));

        let resp = r.handle(&req("GET", "/x"));
        assert_eq!(resp.status, 403, "second guard rejects");
        assert_eq!(FIRST_HITS.load(Ordering::SeqCst), 1,
            "first guard must have run (proves the chain is populated, not just short-circuited)");
        assert_eq!(SECOND_HITS.load(Ordering::SeqCst), 1,
            "second guard must have run");
    }

    #[test]
    fn groups_are_independent() {
        // Two groups on the same Router. Each route gets only its own group's guards.
        let r = Router::new()
            .group([deny_guard], |g| g.get("/a", ok_handler))
            .group([allow_guard], |g| g.get("/b", ok_handler));
        assert_eq!(r.handle(&req("GET", "/a")).status, 403, "/a in deny group");
        assert_eq!(r.handle(&req("GET", "/b")).status, 200, "/b in allow group, not deny");
    }

    #[test]
    fn ungrouped_routes_have_no_guards() {
        let r = Router::new().get("/health", ok_handler);
        assert_eq!(r.handle(&req("GET", "/health")).status, 200,
            "ungrouped /health must run without any guard interference");
    }

    #[test]
    fn group_then_public_route_isolates() {
        let r = Router::new()
            .group([deny_guard], |g| g.get("/admin", ok_handler))
            .get("/health", ok_handler);
        assert_eq!(r.handle(&req("GET", "/admin")).status, 403, "/admin still guarded");
        assert_eq!(r.handle(&req("GET", "/health")).status, 200,
            "/health registered AFTER the group must NOT be guarded by it");
    }

    #[test]
    fn reorder_within_group_does_not_change_guards() {
        let r1 = Router::new()
            .group([deny_guard], |g| g.get("/a", ok_handler).get("/b", ok_handler));
        let r2 = Router::new()
            .group([deny_guard], |g| g.get("/b", ok_handler).get("/a", ok_handler));
        assert_eq!(r1.handle(&req("GET", "/a")).status, 403);
        assert_eq!(r1.handle(&req("GET", "/b")).status, 403);
        assert_eq!(r2.handle(&req("GET", "/a")).status, 403);
        assert_eq!(r2.handle(&req("GET", "/b")).status, 403);
    }

    #[test]
    fn group_inside_nest_composes_guards() {
        // Inner sub-router has .group([deny_guard]).
        // Outer wraps in .group([allow_guard]).nest("/v1", inner).
        // Net: /v1/items denied — outer allow + inner deny means deny wins.
        let inner = Router::new()
            .group([deny_guard], |g| g.get("/items", ok_handler));
        let r = Router::new()
            .group([allow_guard], |g| g.nest("/v1", inner));
        assert_eq!(r.handle(&req("GET", "/v1/items")).status, 403,
            "outer allow + inner deny via group+nest: deny wins");
    }

    #[test]
    fn guards_macro_homogenizes_heterogeneous_input() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static A_HITS: AtomicUsize = AtomicUsize::new(0);
        static B_HITS: AtomicUsize = AtomicUsize::new(0);
        A_HITS.store(0, Ordering::SeqCst);
        B_HITS.store(0, Ordering::SeqCst);

        // Two bare-fn guards have identical types — the array case already works.
        // The macro's real value is heterogeneous *callable structures*. We
        // simulate heterogeneity by mixing a fn-item and a closure (different
        // concrete types in Rust's type system, both impl IntoGuard via the
        // blanket `impl<F: Fn...> IntoGuard for F`).
        fn fn_guard(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            A_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        let closure_guard = |_req: &mut Req<'_>| -> Result<(), response::HttpResponse> {
            B_HITS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };

        let r = Router::new()
            .group(crate::guards![fn_guard, closure_guard], |g|
                g.get("/x", ok_handler));

        let resp = r.handle(&req("GET", "/x"));
        assert_eq!(resp.status, 200);
        assert_eq!(A_HITS.load(Ordering::SeqCst), 1, "fn-item guard must run");
        assert_eq!(B_HITS.load(Ordering::SeqCst), 1, "closure guard must run");
    }

    #[test]
    fn empty_guard_array_is_a_pass_through() {
        // .group([], |g| g.get(...)) — zero guards, route runs unconditionally.
        let guards: [Guard; 0] = [];
        let r = Router::new().group(guards, |g| g.get("/anything", ok_handler));
        assert_eq!(r.handle(&req("GET", "/anything")).status, 200,
            "empty guard array must produce an unguarded route");
    }

    // -- parse_query (B3) --

    fn req_with_query(pairs: Vec<(&str, &str)>) -> Request {
        Request {
            method: "GET".into(),
            path: "/x".into(),
            headers: vec![],
            body: None,
            path_params: vec![],
            query_params: pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn make_req<'a>(request: &'a Request, params: &'a Params) -> Req<'a> {
        Req { request, params, ctx: Ctx::new() }
    }

    fn empty_params() -> Params {
        Params { pairs: vec![] }
    }

    #[derive(Debug, serde::Deserialize, garde::Validate)]
    struct PageQuery {
        #[garde(range(min = 1, max = 100))]
        #[serde(default = "default_limit")]
        limit: u32,
        #[garde(skip)]
        cursor: Option<String>,
    }
    fn default_limit() -> u32 { 20 }

    #[test]
    fn parse_query_decodes_typed_struct() {
        let r = req_with_query(vec![("limit", "50"), ("cursor", "abc")]);
        let p = empty_params();
        let req = make_req(&r, &p);
        let q: PageQuery = req.parse_query().unwrap();
        assert_eq!(q.limit, 50);
        assert_eq!(q.cursor.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_query_applies_serde_default_when_field_missing() {
        let r = req_with_query(vec![]);
        let p = empty_params();
        let req = make_req(&r, &p);
        let q: PageQuery = req.parse_query().unwrap();
        assert_eq!(q.limit, 20, "default fired");
        assert!(q.cursor.is_none());
    }

    #[test]
    fn parse_query_runs_garde_validation() {
        let r = req_with_query(vec![("limit", "1000")]);
        let p = empty_params();
        let req = make_req(&r, &p);
        let err = req.parse_query::<PageQuery>().unwrap_err();
        assert_eq!(err.status, 422);
        assert!(err.errors.contains_key("limit"));
    }

    #[test]
    fn parse_query_rejects_garbage_with_400() {
        let r = req_with_query(vec![("limit", "not-a-number")]);
        let p = empty_params();
        let req = make_req(&r, &p);
        let err = req.parse_query::<PageQuery>().unwrap_err();
        assert_eq!(err.status, 400);
    }

    // -- Params::parse (C3) --

    #[test]
    fn params_parse_decodes_typed_value() {
        let params = Params { pairs: vec![("id".into(), "42".into())] };
        let id: i64 = params.parse("id").unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn params_parse_missing_field_is_400() {
        let params = empty_params();
        let err = params.parse::<i64>("id").unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.detail.unwrap().contains("missing path param: id"));
    }

    #[test]
    fn params_parse_invalid_value_is_400() {
        let params = Params { pairs: vec![("id".into(), "not-a-num".into())] };
        let err = params.parse::<i64>("id").unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.detail.unwrap().contains("invalid id"));
    }

    // ── Extractor dispatch tests (Task 3) ────────────────────────────────────

    use crate::response::Json;
    use crate::extract::{Path, Principal, Query};

    /// Shared body struct for extractor dispatch tests.
    #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, schemars::JsonSchema)]
    struct FooBody {
        v: i32,
    }

    /// Helper: build a Request with a JSON body and path params baked into
    /// the URL (the router extracts them via matchit, so we just set the path).
    fn req_with_body(method: &str, path: &str, body: Vec<u8>) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: Some(body),
            path_params: vec![],
            query_params: vec![],
        }
    }

    fn req_with_query_and_path(method: &str, path: &str, query_params: Vec<(&str, &str)>) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            headers: vec![],
            body: None,
            path_params: vec![],
            query_params: query_params
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Test 1: Variadic dispatch with arity-2 (Path + Json) and arity-1 (Path only)
    /// handlers both route and extract correctly.
    #[test]
    fn extractor_handler_dispatches_with_args_in_order() {
        // Arity 2: Path<u64> + Json<FooBody> — return Json<FooBody> (implements IntoResponse)
        fn h_two(Path(id): Path<u64>, Json(body): Json<FooBody>) -> Result<Json<FooBody>, crate::error::ApiError> {
            // Echo id into `v` to prove both extractors ran
            Ok(Json(FooBody { v: id as i32 * 100 + body.v }))
        }
        // Arity 1: Path<u64> only — return Json with the id
        fn h_one(Path(id): Path<u64>) -> Result<Json<FooBody>, crate::error::ApiError> {
            Ok(Json(FooBody { v: id as i32 }))
        }
        // Arity 3: Path<u64> + Query<QFoo2> + Option<Principal>
        #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
        struct QFoo2 { limit: u32 }
        #[derive(Debug, serde::Serialize, schemars::JsonSchema)]
        struct Resp3 { id: u64, limit: u32, who: String }
        fn h_three(
            Path(id): Path<u64>,
            Query(q): Query<QFoo2>,
            caller: Option<Principal>,
        ) -> Result<Json<Resp3>, crate::error::ApiError> {
            let who = caller.map(|p| p.0).unwrap_or_else(|| "anon".into());
            Ok(Json(Resp3 { id, limit: q.limit, who }))
        }

        let router = Router::new()
            .post("/x/{id}", h_two)
            .get("/y/{id}", h_one)
            .get("/z/{id}", h_three);

        // Arity-2 dispatch: id=5, body v=7 → result v = 5*100+7 = 507
        let body = serde_json::to_vec(&FooBody { v: 7 }).unwrap();
        let resp = router.handle(&req_with_body("POST", "/x/5", body));
        assert_eq!(resp.status, 200, "arity-2 dispatch must succeed");
        let text = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(text.contains("507"), "id=5, body.v=7 must combine to 507 in response: {text}");

        // Arity-1 dispatch: id=42 → v=42
        let resp = router.handle(&req("GET", "/y/42"));
        assert_eq!(resp.status, 200, "arity-1 dispatch must succeed");
        let text = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(text.contains("42"), "path id=42 must appear: {text}");

        // Arity-3 dispatch (anonymous — Option<Principal> → None)
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let resp = router.handle(&req_with_query_and_path("GET", "/z/99", vec![("limit", "10")]));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        assert_eq!(resp.status, 200, "arity-3 dispatch must succeed");
        let text = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(text.contains("99"), "path id must appear: {text}");
        assert!(text.contains("10"), "query limit must appear: {text}");
        assert!(text.contains("anon"), "anonymous principal must appear as 'anon': {text}");
    }

    /// Test 2: Coexistence — a raw `&mut Req` handler and a typed extractor
    /// handler both register via the same `.get()` call on one Router and
    /// both dispatch correctly.
    #[test]
    fn coexistence_raw_and_extractor_handlers_in_same_router() {
        // Raw handler: the classic &mut Req signature
        fn raw(_req: &mut Req<'_>) -> response::HttpResponse {
            response::ok(&serde_json::json!({"kind": "raw"}))
        }
        // Typed handler: extractor signature — Path<u64>
        fn typed(Path(id): Path<u64>) -> Result<Json<FooBody>, crate::error::ApiError> {
            Ok(Json(FooBody { v: id as i32 }))
        }

        // Both register through the same .get() call — the key coexistence proof.
        let router = Router::new()
            .get("/a", raw)
            .get("/b/{id}", typed);

        // raw handler on /a
        let resp = router.handle(&req("GET", "/a"));
        assert_eq!(resp.status, 200);
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("raw"), "raw handler must return 'raw': {body}");

        // typed handler on /b/:id
        let resp = router.handle(&req("GET", "/b/7"));
        assert_eq!(resp.status, 200);
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        // v=7 proves Path<u64> extracted the id correctly
        assert!(body.contains("7"), "typed handler must echo id=7 in response: {body}");
    }

    /// Test 3: First extractor error short-circuits — Path fails (non-numeric id)
    /// and the response carries the Path error.  The body is deliberately a
    /// valid JSON doc so that if Json ran it would succeed; the response must
    /// NOT mention "JSON" proving Json::from_request was never called.
    #[test]
    fn first_extractor_error_short_circuits() {
        fn h(
            Path(_id): Path<u64>,
            Json(_body): Json<FooBody>,
        ) -> Result<Json<FooBody>, crate::error::ApiError> {
            Ok(Json(FooBody { v: 0 }))
        }

        let router = Router::new().post("/item/{id}", h);

        // Path will fail ("not-a-number" can't parse to u64).
        // Body is valid JSON so if Json ran it would succeed — not a 400 from Json.
        let body = serde_json::to_vec(&FooBody { v: 99 }).unwrap();
        let resp = router.handle(&req_with_body("POST", "/item/not-a-number", body));

        // Must be 400 (Path error), not 200 and not a Json-sourced error.
        assert_eq!(resp.status, 400, "Path failure must produce 400");
        let text = String::from_utf8(resp.body.unwrap()).unwrap();
        // Error must originate from Path, not Json (proves short-circuit order).
        assert!(
            !text.contains("JSON"),
            "Json extractor must not have run — no JSON error expected in: {text}"
        );
    }

    /// Test 4: `Principal` extractor returns 401 when the request is anonymous
    /// (no principal stashed in the thread-local).
    #[test]
    fn principal_required_returns_401_when_anonymous_through_router() {
        fn h(_p: Principal) -> Result<Json<FooBody>, crate::error::ApiError> {
            Ok(Json(FooBody { v: 1 }))
        }

        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);

        let router = Router::new().get("/me", h);
        let resp = router.handle(&req("GET", "/me"));

        // Clean up thread-local after dispatch
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);

        assert_eq!(resp.status, 401, "anonymous Principal extractor must yield 401");
    }

    /// Test 5: `Option<Principal>` yields Ok(None) when anonymous — no error,
    /// handler runs and returns a response with "anon" in it.
    #[test]
    fn option_principal_returns_none_when_anonymous_through_router() {
        #[derive(Debug, serde::Serialize, schemars::JsonSchema)]
        struct WhoResp { who: String }

        fn h(p: Option<Principal>) -> Result<Json<WhoResp>, crate::error::ApiError> {
            let who = p.map(|p| p.0).unwrap_or_else(|| "anon".into());
            Ok(Json(WhoResp { who }))
        }

        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);

        let router = Router::new().get("/me", h);
        let resp = router.handle(&req("GET", "/me"));

        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);

        assert_eq!(resp.status, 200, "Option<Principal> must succeed for anonymous");
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("anon"), "anonymous must yield 'anon' in response: {body}");
    }

    // ── Spec / doc tests (Task 4 + Amendment A) ──────────────────────────────

    #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct SpecNote { title: String }

    fn typed_create(crate::response::Json(n): crate::response::Json<SpecNote>)
        -> crate::response::Created<SpecNote> { crate::response::Created(n) }

    #[test]
    fn openapi_served_on_unmatched_get() {
        let r = Router::new()
            .info("notes", "1.2.3", Some("test service"))
            .post("/api/notes", typed_create);
        let resp = r.handle(&req("GET", "/api/notes/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["info"]["title"], "notes");
        let op = &doc["paths"]["/api/notes"]["post"];
        assert_eq!(op["requestBody"]["content"]["application/json"]["schema"]["properties"]["title"]["type"], "string");
        assert!(op["responses"]["201"].is_object());
    }

    #[test]
    fn user_route_wins_over_generated_doc() {
        fn custom(_req: &mut Req<'_>) -> response::HttpResponse {
            response::raw(200, b"mine", "text/plain")
        }
        let r = Router::new().get("/openapi.json", custom);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.body.unwrap(), b"mine".to_vec());
    }

    #[test]
    fn param_route_does_not_shadow_doc_path() {
        // The canonical CRUD layout: /api/notes/{id} would pattern-match
        // /api/notes/openapi.json (id = "openapi.json") and bury the doc
        // behind that route's guards. The doc short-circuit must win over
        // incidental param captures — only an explicit literal GET route
        // at the doc path defers to the user.
        let r = Router::new()
            .get("/api/notes", ok_handler)
            .group([deny_guard], |g| g.get("/api/notes/{id}", echo_id));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let resp = r.handle(&req("GET", "/api/notes/openapi.json"));
        assert_eq!(resp.status, 200, "doc served, not captured by {{id}}");
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["openapi"], "3.0.3");
        // …while real ids still dispatch (and hit the guard).
        assert_eq!(r.handle(&req("GET", "/api/notes/abc")).status, 403);
    }

    #[test]
    fn nest_rewrites_spec_paths_and_group_marks_guarded() {
        let sub = Router::new().post("/notes", typed_create);
        let r = Router::new()
            .group([deny_guard], |g| g.nest("/api", sub));
        // Set an authenticated caller so guarded routes are included in the spec.
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(Some("agent_test".into()));
        let resp = r.handle(&req("GET", "/openapi.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert!(doc["paths"]["/api/notes"]["post"].is_object(), "nested path rewritten in spec");
        assert!(doc["paths"]["/api/notes"]["post"]["security"].is_array(), "guarded route marked");
    }

    #[test]
    fn raw_req_handlers_appear_with_response_only() {
        // ok_handler is the Fn(&mut Req) shape returning HttpResponse — undescribed body, listed path.
        let r = Router::new().get("/users", ok_handler);
        let resp = r.handle(&req("GET", "/users/openapi.json"));
        // suffix-match serves the doc anywhere under the service subtree
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert!(doc["paths"]["/users"]["get"]["responses"]["200"].is_object());
    }

    // Amendment A tests

    #[test]
    fn undocumented_routes_excluded_from_spec() {
        let r = Router::new()
            .post("/api/notes", typed_create)
            .undocumented(|g| g.get("/internal/health", ok_handler));
        let resp = r.handle(&req("GET", "/openapi.json"));
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        // The documented route appears.
        assert!(doc["paths"]["/api/notes"].is_object(), "documented route must appear");
        // The undocumented route must NOT appear in the spec.
        assert!(doc["paths"]["/internal/health"].is_null(), "undocumented route must not appear in spec");
        // But the route still handles requests normally.
        assert_eq!(r.handle(&req("GET", "/internal/health")).status, 200);
    }

    #[test]
    fn visibility_filter_hides_guarded_from_anonymous() {
        // Guarded route inside a group: the spec should omit it for anonymous callers,
        // but show it for authenticated ones.
        let r = Router::new()
            .get("/public", ok_handler)
            .group([deny_guard], |g| g.post("/private", typed_create));

        // Anonymous caller (no principal)
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let resp = r.handle(&req("GET", "/openapi.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert!(doc["paths"]["/public"].is_object(), "public route visible to anonymous");
        assert!(doc["paths"]["/private"].is_null(), "guarded route hidden from anonymous");

        // Authenticated caller
        crate::request_state::_set_fallback_principal(Some("agent_test".into()));
        let resp = r.handle(&req("GET", "/openapi.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert!(doc["paths"]["/public"].is_object(), "public route visible to authenticated");
        assert!(doc["paths"]["/private"].is_object(), "guarded route visible to authenticated");
    }

    // ── RPC mount + openrpc.json tests (Task 5) ─────────────────────────────

    #[test]
    fn rpc_mount_serves_openrpc_json() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct P { q: String }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct R2 { hits: u32 }
        fn search(_p: P) -> Result<R2, crate::rpc::RpcError> { Ok(R2 { hits: 0 }) }

        let r = Router::new().rpc("/rpc", || crate::rpc::Dispatcher::new().method("search", search));
        let resp = r.handle(&req("GET", "/openrpc.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["methods"][0]["name"], "search");
        // and the POST endpoint itself still dispatches
        // (covered by dispatcher tests; here just confirm the route exists)
        assert_eq!(r.handle(&req("OPTIONS", "/rpc")).status, 204);
    }

    #[test]
    fn no_rpc_mounts_no_openrpc_json() {
        let r = Router::new().get("/users", ok_handler);
        assert_eq!(r.handle(&req("GET", "/openrpc.json")).status, 404);
    }

    #[test]
    fn guarded_rpc_mount_hidden_from_anonymous_openrpc() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct P2 { id: u64 }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct R3 { val: String }
        fn lookup(_p: P2) -> Result<R3, crate::rpc::RpcError> { Ok(R3 { val: "x".into() }) }

        // Router::rpc can't appear inside RouteSet (RouteSet has no rpc() method).
        // A guarded rpc mount is produced by building the sub-router first, then
        // nesting it inside a group on the outer router.
        let sub = Router::new().rpc("/rpc", || crate::rpc::Dispatcher::new().method("lookup", lookup));
        let r = Router::new().group([deny_guard], |g| g.nest("/api", sub));

        // Anonymous caller: all mounts are guarded → openrpc.json must 404.
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let resp = r.handle(&req("GET", "/openrpc.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        assert_eq!(resp.status, 404, "guarded-only rpc mount must be invisible to anonymous");

        // Authenticated caller: methods are present.
        crate::request_state::_set_fallback_principal(Some("agent_x".into()));
        let resp = r.handle(&req("GET", "/openrpc.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        assert_eq!(resp.status, 200, "authenticated caller must see guarded rpc methods");
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["methods"][0]["name"], "lookup");
    }

    /// Test 6: Guard runs before extractors — a 403-returning guard short-circuits
    /// before any extractor is called. An atomic flag proves the handler body
    /// (and therefore all its extractors) never executed.
    #[test]
    fn guard_runs_before_extractors() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static HANDLER_CALLED: AtomicBool = AtomicBool::new(false);
        HANDLER_CALLED.store(false, Ordering::SeqCst);

        fn blocking_guard(_req: &mut Req<'_>) -> Result<(), response::HttpResponse> {
            Err(response::HttpResponse {
                status: 403,
                headers: vec![],
                body: Some(b"guard blocked".to_vec()),
            })
        }

        // If the guard is bypassed, the handler runs and sets the flag.
        fn h(Path(_id): Path<u64>) -> Result<Json<FooBody>, crate::error::ApiError> {
            HANDLER_CALLED.store(true, Ordering::SeqCst);
            Ok(Json(FooBody { v: 0 }))
        }

        let router = Router::new()
            .group([blocking_guard], |g| g.get("/item/{id}", h));

        let resp = router.handle(&req("GET", "/item/5"));

        assert_eq!(resp.status, 403, "guard must short-circuit with 403");
        assert!(
            !HANDLER_CALLED.load(Ordering::SeqCst),
            "handler (and its extractors) must not run when guard blocks"
        );
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains("guard blocked"), "guard response body must be returned: {body}");
    }

    // ─── Task 6: Router::mcp ─────────────────────────────────────────────

    #[test]
    fn mcp_mount_appears_in_openapi() {
        fn mcp_handler(req: &mut Req<'_>) -> response::HttpResponse {
            crate::mcp::McpServer::new("t", "1.0").handle(req.request)
        }
        let r = Router::new().mcp("/mcp", mcp_handler);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert!(
            doc["paths"]["/mcp"]["post"]["description"].as_str().unwrap().contains("tools/list"),
            "MCP stub description must mention tools/list"
        );
        // The MCP endpoint itself still dispatches (anonymous caller, unguarded)
        assert_eq!(r.handle(&req("OPTIONS", "/mcp")).status, 204);
    }

    // ─── Hardening sweep (spec-endpoints) ─────────────────────────────────

    /// Test 1 (SECURITY): a sub-router with an UNGUARDED `.rpc("/rpc", …)`
    /// is nested via `.group([deny_guard], |g| g.nest("/api", sub))`.
    /// Anonymous GET /openrpc.json must 404 because the nest upgrades the
    /// rpc_specs guarded flag — commit 8ec1984's fix is pinned here.
    /// An authenticated caller must see the methods.
    #[test]
    fn nest_inside_group_hides_rpc_methods_from_anonymous() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct P3 { q: String }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct R4 { hits: u32 }
        fn search(_p: P3) -> Result<R4, crate::rpc::RpcError> { Ok(R4 { hits: 0 }) }

        // The sub-router itself has no guards — .rpc() registers guarded=false.
        let sub = Router::new()
            .rpc("/rpc", || crate::rpc::Dispatcher::new().method("search", search));
        // Nesting inside a group must upgrade guarded to true on the rpc entry.
        let r = Router::new().group([deny_guard], |g| g.nest("/api", sub));

        // Anonymous: all rpc mounts now guarded → openrpc.json must 404.
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let resp = r.handle(&req("GET", "/openrpc.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        assert_eq!(resp.status, 404,
            "anonymous must not see rpc methods from a sub-router nested inside a guarded group");

        // Authenticated: methods are present.
        crate::request_state::_set_fallback_principal(Some("agent_x".into()));
        let resp = r.handle(&req("GET", "/openrpc.json"));
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        assert_eq!(resp.status, 200,
            "authenticated caller must see the rpc methods after nest-inside-group");
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["methods"][0]["name"], "search");
    }

    /// Test 2: `.route_many(&["GET","POST"], "/sync", typed_handler)` then
    /// GET /openapi.json: `paths["/sync"]` has exactly `get` and `post` keys,
    /// each with the captured request/response shape.
    #[test]
    fn route_many_records_one_spec_entry_per_method() {
        #[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
        struct SyncBody { value: i32 }
        fn sync_handler(crate::response::Json(b): crate::response::Json<SyncBody>)
            -> crate::response::Json<SyncBody> { crate::response::Json(b) }

        let r = Router::new().route_many(&["GET", "POST"], "/sync", sync_handler);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        let path_item = &doc["paths"]["/sync"];
        assert!(path_item["get"].is_object(), "GET must be in spec");
        assert!(path_item["post"].is_object(), "POST must be in spec");
        // Neither PATCH nor DELETE should appear — only the two registered methods.
        assert!(path_item.get("patch").is_none() || path_item["patch"].is_null(),
            "PATCH must not appear in spec");
        // Prove describe() ran: requestBody schema must mention the `value` field.
        let req_body_schema = &path_item["post"]["requestBody"]["content"]["application/json"]["schema"];
        assert!(req_body_schema["properties"]["value"].is_object(),
            "requestBody schema must mention 'value'; got: {req_body_schema}");
    }

    /// Test 3: a 3-extractor typed handler `fn(Path<u64>, Query<Q>, Json<B>) -> Created<R>`
    /// produces an operation with path param + query params + requestBody + 201 response.
    #[test]
    fn multi_extractor_describe_folds_all_parts() {
        use crate::extract::{Path, Query};

        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct MultiQuery { limit: u32 }
        #[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
        struct MultiBody { name: String }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct MultiResp { id: u64 }

        fn h(
            Path(_id): Path<u64>,
            Query(_q): Query<MultiQuery>,
            crate::response::Json(_b): crate::response::Json<MultiBody>,
        ) -> crate::response::Created<MultiResp> {
            crate::response::Created(MultiResp { id: 1 })
        }

        let r = Router::new().post("/items/{id}", h);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        let op = &doc["paths"]["/items/{id}"]["post"];

        // Path param must appear.
        let params = op["parameters"].as_array().expect("parameters must be present");
        assert!(params.iter().any(|p| p["in"] == "path"),
            "path parameter must be present; params: {params:?}");
        // Query param must appear.
        assert!(params.iter().any(|p| p["in"] == "query" && p["name"] == "limit"),
            "query param 'limit' must be present; params: {params:?}");
        // requestBody must mention 'name'.
        let body_schema = &op["requestBody"]["content"]["application/json"]["schema"];
        assert!(body_schema["properties"]["name"].is_object(),
            "requestBody schema must mention 'name'");
        // 201 response.
        assert!(op["responses"]["201"].is_object(), "Created response must be 201");
    }

    /// Test 4: no `.info()` call → doc title "boogy-service", version "0.0.0".
    #[test]
    fn info_defaults_when_unset() {
        let r = Router::new().get("/ping", ok_handler);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();
        assert_eq!(doc["info"]["title"], "boogy-service");
        assert_eq!(doc["info"]["version"], "0.0.0");
    }

    /// `.summary()/.description()` annotate exactly the NEXT route and flow
    /// into the OpenAPI operation; an un-annotated route carries neither key.
    #[test]
    fn summary_description_flow_into_openapi_per_route() {
        let r = Router::new()
            .summary("List widgets")
            .description("Return every widget the caller owns.")
            .get("/widgets", ok_handler)
            .get("/gadgets", ok_handler);
        let resp = r.handle(&req("GET", "/openapi.json"));
        assert_eq!(resp.status, 200);
        let doc: serde_json::Value = serde_json::from_slice(&resp.body.unwrap()).unwrap();

        // Annotated route carries both keys.
        let widgets = &doc["paths"]["/widgets"]["get"];
        assert_eq!(widgets["summary"], "List widgets");
        assert_eq!(widgets["description"], "Return every widget the caller owns.");

        // The annotation is per-route (cleared via take()): the next route
        // registered without annotations must have neither key.
        let gadgets = &doc["paths"]["/gadgets"]["get"];
        assert!(gadgets.get("summary").is_none(), "summary must not leak to the next route");
        assert!(gadgets.get("description").is_none(), "description must not leak to the next route");
    }

    /// Test 5: direct unit test of `doc_path` covering the boundary cases
    /// specified in the task.
    #[test]
    fn doc_path_boundary_cases() {
        // True cases.
        assert!(doc_path("/openapi.json", "openapi.json"),
            "/openapi.json must match");
        assert!(doc_path("/api/openapi.json", "openapi.json"),
            "/api/openapi.json must match");
        // False cases.
        assert!(!doc_path("xopenapi.json", "openapi.json"),
            "xopenapi.json (no leading slash) must not match");
        assert!(!doc_path("/xopenapi.json", "openapi.json"),
            "/xopenapi.json must not match (strip_suffix leaves '/x', not '/')");
        // Wrong filename for its own endpoint.
        assert!(!doc_path("/openrpc.json", "openapi.json"),
            "openrpc.json path must not match openapi.json filename check");
        // Deeper path with correct suffix.
        assert!(doc_path("/v1/api/openapi.json", "openapi.json"),
            "deeper path must still match");
    }

    /// Test 6: HEAD /openapi.json (unrouted) → 404.
    /// The spec-doc short-circuit is GET-only: HEAD is not handled by
    /// `serve_spec_doc_or_404` and falls through to normal dispatch, which
    /// finds no route and returns 404.
    /// This is intentional — HEAD /openapi.json is an undocumented non-use-case.
    #[test]
    fn head_request_for_doc_path_is_404() {
        // No explicit route registered for /openapi.json — the router should
        // NOT auto-serve the spec for HEAD requests (the short-circuit is GET-only).
        let r = Router::new().get("/ping", ok_handler);
        let resp = r.handle(&req("HEAD", "/openapi.json"));
        // Current behavior: 404. HEAD is not in the spec-doc arm, so it falls
        // through to normal dispatch which finds no matching route.
        // This comment intentionally documents that the behavior is by design,
        // not an oversight.
        assert_eq!(resp.status, 404,
            "HEAD /openapi.json (unregistered) must 404 — the doc short-circuit is GET-only");
    }
}
