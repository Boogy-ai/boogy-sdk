//! Structured API errors (RFC 7807-flavored).
//!
//! `ApiError` is the canonical SDK error shape for failures that need
//! to surface structured information to the caller — most prominently,
//! per-field validation errors. The wire format follows RFC 7807
//! (`application/problem+json`):
//!
//! ```json
//! {
//!   "type": "/errors/validation_failed",
//!   "title": "Validation failed",
//!   "status": 400,
//!   "detail": "1 field failed validation",
//!   "errors": {
//!     "email": ["already taken"],
//!     "password": ["too short"]
//!   }
//! }
//! ```
//!
//! Why RFC 7807: it's the standardized "structured HTTP error" format
//! (registered IANA media type, supported by tooling), and the
//! extension-fields door it leaves open lets us add `errors` for
//! per-field detail without inventing a bespoke shape. Production APIs
//! at Stripe / GitHub / Atlassian use the same idiom.
//!
//! `ApiError` converts cleanly to both [`response::HttpResponse`] (for
//! REST handlers) and [`rpc::RpcError`] (for JSON-RPC / MCP handlers)
//! via `From` impls, so the same value can flow through either context.
//!
//! ## Quick recipes
//!
//! From a `garde::Report`:
//! ```ignore
//! match input.validate() {
//!     Ok(()) => {}
//!     Err(report) => return ApiError::validation(report).into(),
//! }
//! ```
//!
//! Or use the [`validate_body`] helper which combines JSON parsing +
//! validation in one call:
//! ```ignore
//! let input: CreateNote = match validate_body(req.body()) {
//!     Ok(v) => v,
//!     Err(e) => return e.into(),
//! };
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::response::HttpResponse;
use crate::rpc::RpcError;

/// Content type for structured error responses (RFC 7807).
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Per-field validation errors. `BTreeMap` rather than `HashMap` so
/// the JSON output is stable (alphabetical key order) — easier on
/// snapshot tests and human eyes.
pub type FieldErrors = BTreeMap<String, Vec<String>>;

/// Structured API error.
///
/// The fields are RFC 7807 standard (`type`, `title`, `status`,
/// `detail`) plus an `errors` extension for per-field validation
/// detail. Construct with the typed helpers below rather than building
/// the struct literal — they set sensible defaults and match canonical
/// status codes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// URI reference identifying the problem class. Convention:
    /// `/errors/<snake_case_name>` for Boogy-emitted problems.
    #[serde(rename = "type")]
    pub kind: String,

    /// Short human-readable summary. Should not change between
    /// occurrences of the same problem class.
    pub title: String,

    /// HTTP status code (also surfaced separately in the response so
    /// HTTP-aware tooling can read it without parsing the body).
    pub status: u16,

    /// Optional explanation specific to this occurrence. For
    /// validation failures, summarizes how many fields failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,

    /// Per-field validation errors. Empty for non-validation failures.
    /// Skipped from JSON when empty so the output is clean for the
    /// non-validation case.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub errors: FieldErrors,
}

impl ApiError {
    /// 400 Bad Request — generic client error with a free-form message.
    /// Use for malformed input that doesn't fit a validation report
    /// (bad JSON, missing required header, unparsable id, etc.).
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/bad_request".to_string(),
            title: "Bad request".to_string(),
            status: 400,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 401 Unauthorized — caller is anonymous and the route requires
    /// auth.
    pub fn unauthenticated() -> Self {
        Self {
            kind: "/errors/unauthenticated".to_string(),
            title: "Authentication required".to_string(),
            status: 401,
            detail: None,
            errors: Default::default(),
        }
    }

    /// 403 Forbidden — caller is authenticated but lacks the needed
    /// scope or permission. Prefer [`ApiError::not_found`] for "you
    /// can't see this row" cases (existence-mask convention).
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/forbidden".to_string(),
            title: "Forbidden".to_string(),
            status: 403,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 404 Not Found — also the canonical response for "the row exists
    /// but isn't owned by the caller" (existence-mask).
    pub fn not_found() -> Self {
        Self {
            kind: "/errors/not_found".to_string(),
            title: "Not found".to_string(),
            status: 404,
            detail: None,
            errors: Default::default(),
        }
    }

    /// 409 Conflict — uniqueness violation, version mismatch, etc.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/conflict".to_string(),
            title: "Conflict".to_string(),
            status: 409,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 422 Unprocessable Entity — request was syntactically valid but
    /// failed validation. Per-field failures populated from the
    /// supplied `garde::Report`.
    pub fn validation(report: garde::Report) -> Self {
        let mut errors: FieldErrors = BTreeMap::new();
        for (path, error) in report.iter() {
            errors
                .entry(path.to_string())
                .or_default()
                .push(error.message().to_string());
        }
        let n = errors.values().map(Vec::len).sum::<usize>();
        Self {
            kind: "/errors/validation_failed".to_string(),
            title: "Validation failed".to_string(),
            status: 422,
            detail: Some(format!("{n} field{} failed validation", if n == 1 { "" } else { "s" })),
            errors,
        }
    }

    /// 422 Unprocessable Entity — the request was syntactically valid
    /// but failed a domain-level invariant that isn't a per-field garde
    /// validation. Use this for limits (e.g. "too many mentions"),
    /// quota / balance violations, business-rule rejections.
    ///
    /// Use [`Self::validation`] instead when the error is a structured
    /// per-field failure produced by a `garde::Report`.
    pub fn unprocessable(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/unprocessable".to_string(),
            title: "Unprocessable entity".to_string(),
            status: 422,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 500 Internal Server Error — unexpected failure. The message
    /// reaches the caller; do not include sensitive operational
    /// details.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/internal".to_string(),
            title: "Internal server error".to_string(),
            status: 500,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 507 Insufficient Storage — the API has exceeded its storage quota
    /// and cannot grow further.
    pub fn insufficient_storage(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/insufficient_storage".to_string(),
            title: "Insufficient storage".to_string(),
            status: 507,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 409 Conflict — a foreign-key / check / not-null constraint was violated.
    pub fn constraint_violation(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/constraint_violation".to_string(),
            title: "Constraint violation".to_string(),
            status: 409,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 400 Bad Request — a caller-supplied argument was invalid.
    pub fn invalid_argument(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/invalid_argument".to_string(),
            title: "Invalid argument".to_string(),
            status: 400,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 501 Not Implemented — the storage engine does not support this operation.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/unsupported".to_string(),
            title: "Unsupported operation".to_string(),
            status: 501,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// 503 Service Unavailable — a transient host concurrency cap was hit
    /// (e.g. too many open cross-service transactions). The caller should
    /// retry shortly; the message carries a `Retry-After`-style hint so it
    /// survives the trip to the client even though `ApiError` carries no
    /// header bag of its own.
    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/service_unavailable".to_string(),
            title: "Service unavailable".to_string(),
            status: 503,
            detail: Some(format!("{} (Retry-After: 1)", msg.into())),
            errors: Default::default(),
        }
    }

    /// 504 Gateway Timeout — the storage operation timed out.
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/timeout".to_string(),
            title: "Storage timeout".to_string(),
            status: 504,
            detail: Some(msg.into()),
            errors: Default::default(),
        }
    }

    /// Render to bytes in `application/problem+json` format. Falls
    /// back to a plain `{"error": ...}` envelope on serializer failure
    /// (vanishingly unlikely with this struct, but keeps the response
    /// builder infallible).
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| {
            format!(r#"{{"title":"{}","status":{}}}"#, self.title, self.status).into_bytes()
        })
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.title, self.status)
    }
}

impl std::error::Error for ApiError {}

impl From<ApiError> for HttpResponse {
    fn from(e: ApiError) -> Self {
        HttpResponse {
            status: e.status,
            headers: vec![(
                "content-type".to_string(),
                PROBLEM_JSON.to_string(),
            )],
            body: Some(e.to_json_bytes()),
        }
    }
}

impl From<ApiError> for RpcError {
    fn from(e: ApiError) -> Self {
        // Map HTTP status onto the JSON-RPC application-error code
        // band. Callers that need the structured field-error map can
        // pull it from the embedded `data` field — RpcError exposes
        // application-defined data when both shapes need to coexist.
        // Fallback path: just preserve title + status.
        RpcError::application(e.status as i64, e.title.clone())
    }
}

/// Reverse direction: the SDK helpers in `wit_glue!` (notably
/// `auth::find_owned` and `auth::load_owned`) return `RpcError` today,
/// but Result-typed handlers want to propagate via `?` into
/// `Result<_, ApiError>`. This impl makes the conversion infallible
/// at every callsite.
///
/// Mapping uses HTTP-status-shaped application codes when the helper
/// already produced one (e.g. `RpcError::application(401, ...)` from
/// `auth::find_owned`); standard JSON-RPC negatives fall back to
/// `internal` since they are framing failures, not domain errors.
/// Lift a raw String error into an `ApiError::internal`. This impl
/// exists so the closure body inside `tx(|t| -> Result<R,
/// ApiError> { ... })` can use `?` directly on String-returning WIT
/// calls (which is what the underlying `Transaction` methods return).
/// Concrete handlers should still construct typed errors at decision
/// points (404, 422, 409); this conversion is the fallback for raw
/// store failures inside a transaction.
impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError::internal(s)
    }
}

impl From<RpcError> for ApiError {
    fn from(e: RpcError) -> Self {
        match e.code {
            400 => ApiError::bad_request(e.message),
            401 => ApiError::unauthenticated(),
            403 => ApiError::forbidden(e.message),
            404 => ApiError::not_found(),
            409 => ApiError::conflict(e.message),
            422 => ApiError::bad_request(e.message),
            // Any other 4xx/5xx HTTP-shaped code, preserved.
            n if (400..600).contains(&n) => ApiError {
                kind: "/errors/upstream".to_string(),
                title: "Upstream error".to_string(),
                status: n as u16,
                detail: Some(e.message),
                errors: Default::default(),
            },
            // JSON-RPC standard codes — framing problems, not domain.
            _ => ApiError::internal(e.message),
        }
    }
}

/// Lift a cross-service call failure into an `ApiError`. This impl
/// exists so handlers returning `Result<_, ApiError>` can use `?`
/// directly on `peer_fetch` / `PeerRequest::body_json` chains instead
/// of `.map_err` boilerplate at every call site.
///
/// Mapping: failures of the *dependency* (not found, denied by its
/// ingress policy, timeout, depth, its internal error) surface as
/// **502** `/errors/upstream` — the caller's request failed because an
/// upstream service did. Failures that mean *this* service is
/// misconfigured (peer capability not granted, malformed target URI)
/// surface as **500** internal. Handlers that want a different status
/// for a specific variant should still match on it explicitly before
/// `?` (e.g. treat `TargetNotFound` as a 404 of their own resource).
impl From<crate::peer::PeerError> for ApiError {
    fn from(e: crate::peer::PeerError) -> Self {
        use crate::peer::PeerError as P;
        match e {
            P::CapabilityDenied | P::InvalidTarget(_) => ApiError::internal(e.to_string()),
            P::TargetNotFound(_)
            | P::Denied(_)
            | P::Timeout(_)
            | P::DepthExceeded
            | P::Internal(_) => ApiError {
                kind: "/errors/upstream".to_string(),
                title: "Upstream error".to_string(),
                status: 502,
                detail: Some(e.to_string()),
                errors: Default::default(),
            },
        }
    }
}

/// Lift a serde_json failure into an `ApiError::internal`. Serializing
/// a request/response body the service itself constructed is a framing
/// failure, not a domain error — same rationale as `From<String>`.
/// (Client-supplied bodies go through `parse_body`/`validate_body`,
/// which map malformed input to 400/422 instead.)
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::internal(format!("json: {e}"))
    }
}

/// Parse + validate a JSON body in one call.
///
/// Returns the parsed `T` on success. On failure returns a structured
/// `ApiError`:
/// - Missing body → `bad_request`
/// - Malformed JSON → `bad_request` with the serde error
/// - Failed validation → `validation` with per-field detail
///
/// Pair with `?` and `Into<HttpResponse>`/`Into<RpcError>`:
///
/// ```ignore
/// let input: CreateNote = match validate_body(req.body()) {
///     Ok(v) => v,
///     Err(e) => return e.into(),
/// };
/// ```
pub fn validate_body<T>(body: Option<&[u8]>) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned + garde::Validate<Context = ()>,
{
    let bytes = body.ok_or_else(|| ApiError::bad_request("missing request body"))?;
    let parsed: T = serde_json::from_slice(bytes)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    parsed.validate().map_err(ApiError::validation)?;
    Ok(parsed)
}

/// Parse a JSON body without validation.
///
/// Sister of [`validate_body`] for types that don't implement
/// `garde::Validate` (or where validation is intentionally skipped).
/// Returns:
/// - Missing body → `bad_request("missing request body")`
/// - Malformed JSON → `bad_request("invalid JSON: ...")`
/// - Otherwise → `Ok(parsed)`
///
/// ```ignore
/// let input: CreateLink = match parse_body(req.body()) {
///     Ok(v) => v,
///     Err(e) => return e.into(),
/// };
/// ```
pub fn parse_body<T>(body: Option<&[u8]>) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = body.ok_or_else(|| ApiError::bad_request("missing request body"))?;
    serde_json::from_slice(bytes)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))
}

#[cfg(test)]
mod tests {
    // ── From<PeerError> / From<serde_json::Error> regression tests ──
    // (added with the impls: handlers must be able to `?` peer calls
    //  and body construction; see the skills/AGENTS taught patterns)

    #[test]
    fn peer_error_dependency_failures_map_to_502_upstream() {
        use crate::peer::PeerError as P;
        for e in [
            P::TargetNotFound("x".into()),
            P::Denied("x".into()),
            P::Timeout("x".into()),
            P::DepthExceeded,
            P::Internal("x".into()),
        ] {
            let a: super::ApiError = e.into();
            assert_eq!(a.status, 502);
            assert_eq!(a.kind, "/errors/upstream");
        }
    }

    #[test]
    fn peer_error_misconfig_maps_to_500_internal() {
        use crate::peer::PeerError as P;
        for e in [P::CapabilityDenied, P::InvalidTarget("x".into())] {
            let a: super::ApiError = e.into();
            assert_eq!(a.status, 500);
        }
    }

    #[test]
    fn question_mark_compiles_for_peer_and_json_errors() {
        // The whole point: `?` lifts both error types in an
        // ApiError-returning handler body.
        fn handler_shaped() -> Result<(), super::ApiError> {
            let _v = serde_json::to_value(42)?; // serde_json::Error → ApiError
            let r: Result<(), crate::peer::PeerError> =
                Err(crate::peer::PeerError::Timeout("t".into()));
            r?;
            Ok(())
        }
        let err = handler_shaped().unwrap_err();
        assert_eq!(err.status, 502);
    }


    use super::*;

    #[test]
    fn validation_error_shape() {
        let mut report = garde::Report::new();
        report.append(
            garde::Path::new("email"),
            garde::Error::new("not a valid email"),
        );
        report.append(
            garde::Path::new("password"),
            garde::Error::new("too short"),
        );
        report.append(
            garde::Path::new("password"),
            garde::Error::new("missing digit"),
        );
        let err = ApiError::validation(report);
        assert_eq!(err.status, 422);
        assert_eq!(err.errors["email"], vec!["not a valid email"]);
        assert_eq!(
            err.errors["password"],
            vec!["too short", "missing digit"]
        );
    }

    #[test]
    fn json_shape_matches_rfc_7807() {
        let err = ApiError::not_found();
        let json: serde_json::Value =
            serde_json::from_slice(&err.to_json_bytes()).unwrap();
        assert_eq!(json["type"], "/errors/not_found");
        assert_eq!(json["title"], "Not found");
        assert_eq!(json["status"], 404);
        assert!(json.get("errors").is_none(), "empty errors map omitted");
        assert!(json.get("detail").is_none(), "empty detail omitted");
    }

    #[test]
    fn validation_json_includes_field_errors() {
        let mut report = garde::Report::new();
        report.append(garde::Path::new("title"), garde::Error::new("required"));
        let err = ApiError::validation(report);
        let json: serde_json::Value =
            serde_json::from_slice(&err.to_json_bytes()).unwrap();
        assert_eq!(json["status"], 422);
        assert_eq!(json["errors"]["title"][0], "required");
    }

    #[test]
    fn into_http_response_uses_problem_json() {
        let err = ApiError::bad_request("missing field");
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status, 400);
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == PROBLEM_JSON));
    }

    #[derive(Debug, serde::Deserialize, garde::Validate)]
    struct Sample {
        #[garde(length(min = 1))]
        title: String,
    }

    #[test]
    fn validate_body_rejects_empty_string() {
        let body = br#"{"title":""}"#;
        let err = validate_body::<Sample>(Some(body)).unwrap_err();
        assert_eq!(err.status, 422);
        assert!(err.errors.contains_key("title"));
    }

    #[test]
    fn validate_body_rejects_missing_body() {
        let err = validate_body::<Sample>(None).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn validate_body_rejects_malformed_json() {
        let err = validate_body::<Sample>(Some(b"{not json}")).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn validate_body_accepts_valid_input() {
        let body = br#"{"title":"hello"}"#;
        let parsed = validate_body::<Sample>(Some(body)).unwrap();
        assert_eq!(parsed.title, "hello");
    }

    #[test]
    fn new_arm_constructors_have_correct_status() {
        assert_eq!(ApiError::constraint_violation("fk").status, 409);
        assert_eq!(ApiError::invalid_argument("bad col").status, 400);
        assert_eq!(ApiError::unsupported("no LIKE").status, 501);
        assert_eq!(ApiError::timeout("slow").status, 504);
    }
}
