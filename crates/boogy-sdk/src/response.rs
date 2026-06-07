//! HTTP responses produced by handlers.
//!
//! Two complementary surfaces:
//!
//! - **Success-path builders** ([`ok`], [`created`], [`no_content`],
//!   [`redirect`], [`raw`]) — produce a plain JSON / location-header /
//!   raw-bytes response directly. Same shape as before A2.
//! - **Error-path builders** ([`bad_request`], [`unauthenticated`],
//!   [`forbidden`], [`not_found`], [`conflict`], [`server_error`]) —
//!   thin wrappers over [`ApiError`]. Every error response from the
//!   SDK is `application/problem+json` (RFC 7807) regardless of
//!   whether the caller used `ApiError::*().into()` directly or these
//!   convenience builders.
//! - **Typed wrappers** ([`Json`], [`Created`], [`NoContent`],
//!   [`Redirect`]) — for handlers that return their typed payload and
//!   let the framework do the conversion via [`IntoResponse`].
//!
//! Result-typed handlers compose through `IntoResponse`:
//!
//! ```ignore
//! fn create_note(req: &mut Req<'_>) -> Result<Created<NoteOut>, ApiError> {
//!     let input: CreateNote = validate_body(req.body())?;
//!     let id = store::insert("notes", &columns_for(&input))?;
//!     Ok(Created(NoteOut { id, title: input.title, body: input.body }))
//! }
//! ```
//!
//! Behind the scenes, `Result<Created<NoteOut>, ApiError>` implements
//! `IntoResponse`: `Ok(Created(t))` becomes 201 + serialized JSON, and
//! `Err(api_error)` becomes the RFC 7807 `application/problem+json`
//! response that `ApiError` produces — the same shape every error path
//! in the SDK uses.

use serde::Serialize;

use crate::error::ApiError;

/// The HTTP response type from the WIT bindings.
/// We define our own here so the SDK doesn't depend on each API's bindings.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

fn json_headers() -> Vec<(String, String)> {
    vec![("content-type".to_string(), "application/json".to_string())]
}

fn json_body(body: &impl Serialize) -> Option<Vec<u8>> {
    serde_json::to_vec(body).ok()
}

/// 200 OK with JSON body.
pub fn ok(body: &impl Serialize) -> HttpResponse {
    HttpResponse { status: 200, headers: json_headers(), body: json_body(body) }
}

/// 201 Created with JSON body.
pub fn created(body: &impl Serialize) -> HttpResponse {
    HttpResponse { status: 201, headers: json_headers(), body: json_body(body) }
}

/// 204 No Content.
pub fn no_content() -> HttpResponse {
    HttpResponse { status: 204, headers: vec![], body: None }
}

/// 302 Found redirect to `location`. Use for short-link / OAuth-style
/// flows where you want the client to follow to a different URL.
pub fn redirect(location: &str) -> HttpResponse {
    HttpResponse {
        status: 302,
        headers: vec![("location".to_string(), location.to_string())],
        body: None,
    }
}

// ─── Error builders (RFC 7807) ─────────────────────────────────────────────
//
// Every error builder produces the same `application/problem+json` wire
// shape that `ApiError` emits. Mixing this module's builders and direct
// `ApiError::*().into()` calls in the same handler is safe — they both
// converge on the same structured error.

/// 400 Bad Request — RFC 7807 `application/problem+json`.
pub fn bad_request(msg: &str) -> HttpResponse {
    ApiError::bad_request(msg).into()
}

/// 401 Unauthorized — RFC 7807 `application/problem+json`.
/// Use when a route requires authentication but the caller didn't
/// present any. For "authenticated but lacks scope," use [`forbidden`].
pub fn unauthenticated() -> HttpResponse {
    ApiError::unauthenticated().into()
}

/// 403 Forbidden — RFC 7807 `application/problem+json`.
/// Use when the caller is authenticated but their identity doesn't
/// permit this action. For per-resource ownership where existence
/// should be masked, prefer [`not_found`].
pub fn forbidden(msg: &str) -> HttpResponse {
    ApiError::forbidden(msg).into()
}

/// 404 Not Found — RFC 7807 `application/problem+json`.
pub fn not_found() -> HttpResponse {
    ApiError::not_found().into()
}

/// 409 Conflict — RFC 7807 `application/problem+json`.
/// Use for uniqueness violations or state preconditions.
pub fn conflict(msg: &str) -> HttpResponse {
    ApiError::conflict(msg).into()
}

/// 500 Internal Server Error — RFC 7807 `application/problem+json`.
pub fn server_error(msg: &str) -> HttpResponse {
    ApiError::internal(msg).into()
}

/// Raw response with custom status, body, and content type.
pub fn raw(status: u16, body: &[u8], content_type: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".to_string(), content_type.to_string())],
        body: Some(body.to_vec()),
    }
}

// ─── IntoResponse ──────────────────────────────────────────────────────────
//
// Lets handlers return typed payloads (Json<T>, Created<T>, etc.) or
// Result<T, ApiError> instead of always producing a raw HttpResponse.
// The router's IntoHandler blanket converts anything with an
// IntoResponse impl into the underlying Handler closure.
//
// The trait is intentionally minimal — one method, no associated types.
// All extension happens through new impls, not new methods on the trait.

/// Convert a value into an [`HttpResponse`].
///
/// Implemented by SDK-provided wrappers ([`Json`], [`Created`],
/// [`NoContent`], [`Redirect`]) and by [`HttpResponse`] itself
/// (identity). Generic impls cover [`Result`] (mapping `Err` through
/// [`ApiError`]) and [`Option`] (mapping `None` to 404).
///
/// Downstream crates can implement `IntoResponse` on their own types
/// to control how a domain value serializes onto the wire — `impl
/// IntoResponse for User { ... }` is enough to make a handler that
/// returns `User` work directly.
///
/// **Schema capture:** the built-in [`Json<T>`] and [`Created<T>`]
/// wrappers require `T: schemars::JsonSchema` so the route's response
/// shape lands in the generated `…/openapi.json`. If you hit
/// `the trait bound …: JsonSchema is not satisfied` on a handler
/// registration, add `#[derive(schemars::JsonSchema)]` to the payload
/// struct (one line; `schemars = { workspace = true }` or `"0.8"` in
/// your Cargo.toml).
pub trait IntoResponse {
    fn into_response(self) -> HttpResponse;

    /// Spec-capture hook: what does this response type look like on the
    /// wire? `None` = undescribable (raw `HttpResponse`, custom types
    /// that don't override). Called at route-registration time by
    /// `IntoHandler::describe()` — never on the request path.
    fn describe() -> Option<crate::spec::ResponseSpec>
    where
        Self: Sized,
    {
        None
    }
}

impl IntoResponse for HttpResponse {
    fn into_response(self) -> HttpResponse {
        self
    }
}

/// 204 No Content for handlers that succeed with no payload.
impl IntoResponse for () {
    fn into_response(self) -> HttpResponse {
        no_content()
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        Some(crate::spec::ResponseSpec { status: 204, schema: None })
    }
}

/// `Some(t)` is `t`; `None` is 404. Convenience for handlers that
/// look something up and return whatever they find.
impl<T: IntoResponse> IntoResponse for Option<T> {
    fn into_response(self) -> HttpResponse {
        match self {
            Some(t) => t.into_response(),
            None => ApiError::not_found().into(),
        }
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        T::describe()
    }
}

/// `Ok(t)` becomes `t.into_response()`; `Err(e)` is converted to an
/// [`ApiError`] (so `Result<T, String>`, `Result<T, anyhow::Error>`, etc.
/// all work given the right `From` impl) and rendered as RFC 7807.
impl<T, E> IntoResponse for Result<T, E>
where
    T: IntoResponse,
    E: Into<ApiError>,
{
    fn into_response(self) -> HttpResponse {
        match self {
            Ok(t) => t.into_response(),
            Err(e) => e.into().into(),
        }
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        T::describe()
    }
}

/// 200 OK with the wrapped value serialized as JSON.
///
/// ```ignore
/// fn list(_req: &mut Req<'_>) -> Result<Json<ListResp>, ApiError> {
///     Ok(Json(ListResp { items: ... }))
///  }
/// ```
/// Newtype wrapper for a JSON-serializable value. The `T: Serialize` bound
/// is on the `IntoResponse` impl (not the struct itself) so that `Json<T>`
/// can also be used as a request-body extractor (via `FromRequest` in
/// `crate::extract`) where T only needs `DeserializeOwned`.
#[derive(Debug, Clone)]
pub struct Json<T>(pub T);

impl<T: Serialize + schemars::JsonSchema> IntoResponse for Json<T> {
    fn into_response(self) -> HttpResponse {
        ok(&self.0)
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        Some(crate::spec::ResponseSpec { status: 200, schema: Some(crate::spec::schema_value::<T>()) })
    }
}

/// 201 Created with the wrapped value serialized as JSON. Use for
/// successful POSTs that return the created resource.
pub struct Created<T>(pub T);

impl<T: Serialize + schemars::JsonSchema> IntoResponse for Created<T> {
    fn into_response(self) -> HttpResponse {
        created(&self.0)
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        Some(crate::spec::ResponseSpec { status: 201, schema: Some(crate::spec::schema_value::<T>()) })
    }
}

/// 204 No Content. The unit-struct form `NoContent` reads more clearly
/// at handler return sites than `()`.
pub struct NoContent;

impl IntoResponse for NoContent {
    fn into_response(self) -> HttpResponse {
        no_content()
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        Some(crate::spec::ResponseSpec { status: 204, schema: None })
    }
}

/// 302 redirect to the wrapped URL.
pub struct Redirect(pub String);

impl Redirect {
    pub fn to(location: impl Into<String>) -> Self {
        Self(location.into())
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> HttpResponse {
        redirect(&self.0)
    }

    fn describe() -> Option<crate::spec::ResponseSpec> {
        Some(crate::spec::ResponseSpec { status: 302, schema: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize, schemars::JsonSchema)]
    struct Greeting {
        message: &'static str,
    }

    #[test]
    fn json_wrapper_renders_200() {
        let resp = Json(Greeting { message: "hi" }).into_response();
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.body.unwrap(),
            br#"{"message":"hi"}"#
        );
    }

    #[test]
    fn created_wrapper_renders_201() {
        let resp = Created(Greeting { message: "hi" }).into_response();
        assert_eq!(resp.status, 201);
    }

    #[test]
    fn no_content_unit_renders_204() {
        let resp: HttpResponse = NoContent.into_response();
        assert_eq!(resp.status, 204);
        assert!(resp.body.is_none());
    }

    #[test]
    fn unit_renders_204() {
        let resp = ().into_response();
        assert_eq!(resp.status, 204);
    }

    #[test]
    fn redirect_renders_302_with_location() {
        let resp = Redirect::to("https://example.com").into_response();
        assert_eq!(resp.status, 302);
        assert!(resp.headers.iter().any(|(k, v)| k == "location" && v == "https://example.com"));
    }

    #[test]
    fn option_none_is_404() {
        let opt: Option<Json<Greeting>> = None;
        let resp = opt.into_response();
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn option_some_passes_through() {
        let opt = Some(Json(Greeting { message: "found" }));
        let resp = opt.into_response();
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn result_err_renders_api_error() {
        let r: Result<Json<Greeting>, ApiError> = Err(ApiError::bad_request("nope"));
        let resp = r.into_response();
        assert_eq!(resp.status, 400);
        // RFC 7807 wire shape:
        let body = String::from_utf8(resp.body.unwrap()).unwrap();
        assert!(body.contains(r#""type":"/errors/bad_request""#));
    }

    #[test]
    fn result_ok_passes_through() {
        let r: Result<Created<Greeting>, ApiError> =
            Ok(Created(Greeting { message: "hi" }));
        let resp = r.into_response();
        assert_eq!(resp.status, 201);
    }

    #[derive(Serialize, schemars::JsonSchema)]
    struct Described { id: u64 }

    #[test]
    fn json_describes_200_with_schema() {
        let spec = <Json<Described> as IntoResponse>::describe().unwrap();
        assert_eq!(spec.status, 200);
        assert_eq!(spec.schema.unwrap()["properties"]["id"]["type"], "integer");
    }

    #[test]
    fn created_describes_201_result_delegates_nocontent_204() {
        assert_eq!(<Created<Described> as IntoResponse>::describe().unwrap().status, 201);
        assert_eq!(<Result<Json<Described>, ApiError> as IntoResponse>::describe().unwrap().status, 200);
        assert_eq!(<NoContent as IntoResponse>::describe().unwrap().status, 204);
        assert!(<HttpResponse as IntoResponse>::describe().is_none(), "raw responses stay undescribed");
    }
}
