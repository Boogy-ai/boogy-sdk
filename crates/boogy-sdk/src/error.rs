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
//! use garde::Validate as _;   // brings `.validate()` into scope
//!
//! #[derive(Deserialize, garde::Validate)]
//! struct CreateNote { #[garde(length(min = 1, max = 200))] title: String }
//!
//! fn create_note(req: &mut Req<'_>) -> response::HttpResponse {
//!     let input: CreateNote = match parse_body(req.body()) {
//!         Ok(v) => v,
//!         Err(e) => return e.into(),
//!     };
//!     match input.validate() {
//!         Ok(()) => {}
//!         Err(report) => return ApiError::validation(report).into(),
//!     }
//!     response::no_content()
//! }
//! ```
//!
//! Or use the [`validate_body`] helper which combines JSON parsing +
//! validation in one call:
//! ```ignore
//! #[derive(Deserialize, garde::Validate)]
//! struct CreateNote { #[garde(length(min = 1, max = 200))] title: String }
//!
//! fn create_note(req: &mut Req<'_>) -> response::HttpResponse {
//!     let input: CreateNote = match validate_body(req.body()) {
//!         Ok(v) => v,
//!         Err(e) => return e.into(),
//!     };
//!     response::no_content()
//! }
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

    /// Machine-readable cause code, present when this problem class covers
    /// more than one underlying condition that a caller might need to act
    /// on differently. The canonical example: every `/errors/service_unavailable`
    /// 503 shares `kind`/`title`/`status`, but "a host-wide transaction cap
    /// was hit" (back off) and "this transaction's rows are hot" (fix the
    /// data model) call for different client behaviour — see the
    /// [`cause`] module. Absent from the wire when `None` (same
    /// `skip_serializing_if` convention as `detail`); most `ApiError`s
    /// carry no cause at all. A generic client should branch on `cause`
    /// when present rather than parsing `detail` prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,

    /// Seconds a caller should wait before retrying, when retrying is a
    /// meaningful response to this error. **Not part of the JSON body** —
    /// RFC 7231 defines `Retry-After` as a header, so
    /// `From<ApiError> for HttpResponse` emits it as a real header rather
    /// than serializing it into `detail` prose (the previous behaviour;
    /// see the removed note on [`Self::service_unavailable`]). `None` when
    /// retrying wouldn't help — e.g. a per-request op ceiling, where an
    /// identical retry trips the same ceiling again.
    #[serde(skip)]
    pub retry_after_secs: Option<u64>,
}

/// Stable, `snake_case`, machine-readable cause tokens. Each names a
/// specific emission site so a generic HTTP client can branch on `cause`
/// without parsing `detail` prose — see
/// `docs/superpowers/audits/2026-08-platform-audit.md` F-07/F-08.
///
/// All five causes below render as HTTP 503 with the SAME
/// `kind = "/errors/service_unavailable"` — that collapse, with nothing
/// distinguishing the causes on the wire, is exactly the defect this
/// module fixes. Four are per-request store congestion (routed through
/// [`StoreError`](crate::store::StoreError)); the fifth is the host's fair
/// scheduler shedding a request before it ever reaches store code.
pub mod cause {
    /// The host-wide cap on concurrently open cross-service transactions
    /// was hit (`begin_transaction` admission). Not a data-model problem —
    /// back off and retry; if it recurs, reduce how many transactions this
    /// caller holds open at once.
    pub const TX_ADMISSION_EXHAUSTED: &str = "tx_admission_exhausted";
    /// A single request performed more store operations than
    /// `[limits] store_max_ops_per_request` allows. Retrying the identical
    /// request trips the same ceiling again — do fewer store operations
    /// per request instead. This is why `service_unavailable_with_cause`
    /// is called with `retry_after_secs: None` for this cause.
    pub const STORE_OP_CEILING_EXCEEDED: &str = "store_op_ceiling_exceeded";
    /// This request's origin exceeded its store-operation rate budget.
    /// Slow down; the token bucket refills continuously so a short
    /// backoff clears it.
    pub const STORE_OP_RATE_LIMITED: &str = "store_op_rate_limited";
    /// A transaction exhausted its retry budget against repeated
    /// serialization conflicts (`tx()`'s auto-retry gave up). Unlike the
    /// other three causes, this DOES point at the data model or query
    /// shape: split a hot key, use a counter column, or narrow a search
    /// so it doesn't take a whole table as its read set.
    pub const TX_CONTENDED: &str = "tx_contended";
    /// The host's fair scheduler shed this request before it reached the
    /// guest at all — no instance slot was available under contention.
    /// Pure capacity signal; back off and retry.
    pub const SCHEDULER_SHED: &str = "scheduler_shed";
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
        }
    }

    /// 409 Conflict — uniqueness violation, version mismatch, etc.
    /// The thing the request referred to existed and is now gone, and no
    /// retry will bring it back. Distinct from `not_found`, which covers a
    /// thing that never existed or is not the caller's — a client can retry
    /// that after creating it; there is nothing to do about this one but
    /// start over.
    pub fn gone(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/gone".to_string(),
            title: "Gone".to_string(),
            status: 410,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: None,
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/conflict".to_string(),
            title: "Conflict".to_string(),
            status: 409,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
        }
    }

    /// 409 Conflict — a constraint was violated: a unique index, a foreign key,
    /// a check, a not-null, or an "already exists" schema change.
    ///
    /// Distinct from [`ApiError::conflict`] despite the shared status. This one
    /// is deterministic — the same request fails the same way every time — so a
    /// caller must change the request (pick a different value), not repeat it.
    pub fn constraint_violation(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/constraint_violation".to_string(),
            title: "Constraint violation".to_string(),
            status: 409,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
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
            cause: None,
            retry_after_secs: None,
        }
    }

    /// 503 Service Unavailable — a generic transient failure with no
    /// specific [`cause`] to report (e.g. a catalog service proxying an
    /// upstream 503). Carries a real `Retry-After: 1` header (see
    /// `From<ApiError> for HttpResponse`) — 1s is a safe generic hint for
    /// "transient, try again shortly" when the emitting site doesn't know
    /// more.
    ///
    /// Store-congestion call sites should prefer
    /// [`Self::service_unavailable_with_cause`], which additionally sets a
    /// machine-readable `cause` distinguishing WHY (see F-07: four
    /// different store congestion causes used to collapse onto this exact
    /// shape with nothing on the wire telling them apart).
    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/service_unavailable".to_string(),
            title: "Service unavailable".to_string(),
            status: 503,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: Some(1),
        }
    }

    /// 503 Service Unavailable with a machine-readable [`cause`] token and
    /// an optional `Retry-After` hint. Same `kind`/`title`/`status` as
    /// [`Self::service_unavailable`] — the four/five store-congestion sites
    /// this exists for are deliberately still ONE problem class on the
    /// wire (splitting `kind` would be a breaking change for any client
    /// already matching `/errors/service_unavailable`) — but `cause` lets a
    /// client branch on which congestion condition fired.
    ///
    /// `retry_after_secs: None` is a deliberate, meaningful choice: pass it
    /// for a cause where waiting helps (the default for every cause in
    /// [`cause`] except [`cause::STORE_OP_CEILING_EXCEEDED`], where an
    /// identical retry trips the same per-request ceiling again).
    pub fn service_unavailable_with_cause(
        msg: impl Into<String>,
        cause: &str,
        retry_after_secs: Option<u64>,
    ) -> Self {
        Self {
            kind: "/errors/service_unavailable".to_string(),
            title: "Service unavailable".to_string(),
            status: 503,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: Some(cause.to_string()),
            retry_after_secs,
        }
    }

    /// 429 Too Many Requests — the ingress rate limiter rejected this
    /// request. RFC 6585 / RFC 7231: carries a real `Retry-After` header
    /// (see `From<ApiError> for HttpResponse`) rather than a bare
    /// `{"error": "rate_limited"}` body with no problem+json envelope —
    /// see F-08. Token buckets refill continuously, so `retry_after_secs`
    /// is always a safe (if approximate) earliest-retry hint.
    pub fn rate_limited(msg: impl Into<String>, retry_after_secs: u64) -> Self {
        Self {
            kind: "/errors/rate_limited".to_string(),
            title: "Too many requests".to_string(),
            status: 429,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: Some(retry_after_secs),
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
            cause: None,
            retry_after_secs: None,
        }
    }

    /// 504 Gateway Timeout — the request exceeded its overall wall-clock
    /// budget (`[limits] cpu_deadline_ms`'s outer `tokio::time::timeout`
    /// backstop, `B_req`). Distinct from [`Self::timeout`] (a single
    /// storage operation timing out): this is the WHOLE request — including
    /// any `peer::fetch` fan-out — running too long. Not usefully retryable
    /// with an identical request (it would very likely exceed budget
    /// again), so this carries no `Retry-After`.
    pub fn request_budget_exceeded(msg: impl Into<String>) -> Self {
        Self {
            kind: "/errors/request_budget_exceeded".to_string(),
            title: "Request budget exceeded".to_string(),
            status: 504,
            detail: Some(msg.into()),
            errors: Default::default(),
            cause: None,
            retry_after_secs: None,
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
        let mut headers = vec![(
            "content-type".to_string(),
            PROBLEM_JSON.to_string(),
        )];
        // F-07: a real header, not a prose hint baked into `detail` — a
        // generic client can read this without parsing the body at all.
        if let Some(secs) = e.retry_after_secs {
            headers.push(("retry-after".to_string(), secs.to_string()));
        }
        let body = Some(e.to_json_bytes());
        HttpResponse { status: e.status, headers, body }
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
                cause: None,
                retry_after_secs: None,
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
///
/// Two audiences, two channels: the **wire** detail carries only the
/// failure CLASS — `PeerError` messages can embed workload URIs and
/// ingress-policy text, which must not cross the boundary to this
/// service's clients. The **full error** goes to the service's own
/// log stream (request-correlated, owner-visible) so the developer
/// debugging the service loses nothing. Handlers wanting a specific
/// response can still match the variant before `?`.
/// A files failure renders straight to HTTP.
///
/// Deliberately unlike `PublishError`, which does NOT convert: a websocket
/// publish failure has no obvious status, while every files error does. This
/// is what makes a bare `?` work on a files call inside a handler, which the
/// `files` module's docs promise.
///
/// The two arms that are the SERVICE's own misconfiguration rather than the
/// caller's fault are logged and rendered as 500 — the same treatment
/// `PeerError::CapabilityDenied` gets, and for the same reason: a client can
/// do nothing about a manifest that does not grant the capability, so telling
/// them "403" would send them looking for credentials they do not need.
impl From<crate::files::FilesError> for ApiError {
    fn from(e: crate::files::FilesError) -> Self {
        use crate::files::FilesError as F;
        match &e {
            F::CapabilityDenied | F::UnknownCollection(_) => {
                crate::log::error!("files call misconfigured: {e} -> returned to client as 500");
                ApiError::internal("file storage is not configured for this service")
            }
            F::Internal(m) => {
                crate::log::error!("files call failed: {m}");
                ApiError::internal("file storage error")
            }
            _ => ApiError {
                kind: "/errors/file_storage".to_string(),
                title: "File storage error".to_string(),
                status: e.status(),
                detail: Some(e.to_string()),
                errors: Default::default(),
                cause: None,
                retry_after_secs: None,
            },
        }
    }
}

impl From<crate::peer::PeerError> for ApiError {
    fn from(e: crate::peer::PeerError) -> Self {
        use crate::peer::PeerError as P;
        let class = match &e {
            P::TargetNotFound(_) => "target not found",
            P::Denied(_) => "denied",
            P::Timeout(_) => "timeout",
            P::DepthExceeded => "depth exceeded",
            P::Internal(_) => "internal",
            // The peer reached the wire and explicitly rejected the request
            // (non-2xx) — a dependency failure like the others above, not a
            // misconfiguration of THIS service, so it gets the same 502
            // treatment rather than the 500 the misconfig arm below produces.
            P::Rejected(_) => "rejected",
            P::CapabilityDenied | P::InvalidTarget(_) => {
                crate::log::error!("peer call misconfigured: {e} -> returned to client as 500");
                return ApiError::internal("peer call misconfigured");
            }
        };
        crate::log::warn!("peer call failed: {e} -> returned to client as 502 upstream ({class})");
        ApiError {
            kind: "/errors/upstream".to_string(),
            title: "Upstream error".to_string(),
            status: 502,
            detail: Some(format!("upstream call failed: {class}")),
            errors: Default::default(),
            cause: None,
            retry_after_secs: None,
        }
    }
}

/// Lift a serde_json failure into an `ApiError::internal`. Serializing
/// a request/response body the service itself constructed is a framing
/// failure, not a domain error — same rationale as `From<String>`.
/// (Client-supplied bodies go through `parse_body`/`validate_body`,
/// which map malformed input to 400/422 instead.)
/// The serde message (field names, types, positions) is deliberately
/// NOT included — it can disclose schema details if a handler
/// mistakenly `?`s deserialization of client input here instead of
/// going through `parse_body`/`validate_body`.
impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        crate::log::error!("json (de)serialization failed: {e} -> returned to client as 500");
        ApiError::internal("failed to (de)serialize JSON")
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
/// #[derive(Deserialize, garde::Validate)]
/// struct CreateNote { #[garde(length(min = 1, max = 200))] title: String }
///
/// fn create_note(req: &mut Req<'_>) -> response::HttpResponse {
///     let input: CreateNote = match validate_body(req.body()) {
///         Ok(v) => v,
///         Err(e) => return e.into(),
///     };
///     response::no_content()
/// }
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
/// #[derive(Deserialize)]
/// struct CreateLink { target: String }
///
/// fn create_link(req: &mut Req<'_>) -> response::HttpResponse {
///     let input: CreateLink = match parse_body(req.body()) {
///         Ok(v) => v,
///         Err(e) => return e.into(),
///     };
///     response::no_content()
/// }
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
            P::TargetNotFound("SECRET-INNER".into()),
            P::Denied("SECRET-INNER".into()),
            P::Timeout("SECRET-INNER".into()),
            P::DepthExceeded,
            P::Internal("SECRET-INNER".into()),
            P::Rejected(crate::peer::PeerResponse {
                status: 422,
                headers: vec![],
                body: Some(b"SECRET-INNER".to_vec()),
            }),
        ] {
            let a: super::ApiError = e.into();
            assert_eq!(a.status, 502);
            assert_eq!(a.kind, "/errors/upstream");
            // Leak guard: inner strings (target URIs, policy text)
            // must never reach the response detail.
            assert!(!a.detail.as_deref().unwrap_or("").contains("SECRET-INNER"));
        }
    }

    #[test]
    fn peer_error_misconfig_maps_to_500_internal() {
        use crate::peer::PeerError as P;
        for e in [P::CapabilityDenied, P::InvalidTarget("SECRET-INNER".into())] {
            let a: super::ApiError = e.into();
            assert_eq!(a.status, 500);
            assert!(!a.detail.as_deref().unwrap_or("").contains("SECRET-INNER"));
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

    /// F-07: when `retry_after_secs` is set, `From<ApiError> for
    /// HttpResponse` must emit a REAL `Retry-After` header — the previous
    /// behaviour only ever appended prose to `detail`, which no HTTP-aware
    /// client (or the platform's own `docs/compute-fairness.md`, which
    /// promised the header) could observe.
    #[test]
    fn into_http_response_emits_a_real_retry_after_header_when_set() {
        let err = ApiError::rate_limited("too fast", 7);
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status, 429);
        assert!(
            resp.headers.iter().any(|(k, v)| k == "retry-after" && v == "7"),
            "expected a real retry-after header, got {:?}",
            resp.headers
        );
    }

    /// The inverse: no `retry_after_secs` set → no header at all (not an
    /// empty one). `not_found` is a representative error with no retry
    /// semantics.
    #[test]
    fn into_http_response_omits_retry_after_when_unset() {
        let resp: HttpResponse = ApiError::not_found().into();
        assert!(!resp.headers.iter().any(|(k, _)| k == "retry-after"));
    }

    /// `service_unavailable` (the generic, cause-less constructor still used
    /// by unrelated call sites like the catalog wallet/stripe services) now
    /// carries a real 1s `Retry-After` header instead of the old
    /// header-less prose hint — additive for those callers, not a behaviour
    /// they need to change anything for.
    #[test]
    fn service_unavailable_carries_a_real_retry_after_header() {
        let resp: HttpResponse = ApiError::service_unavailable("upstream down").into();
        assert_eq!(resp.status, 503);
        assert!(resp.headers.iter().any(|(k, v)| k == "retry-after" && v == "1"));
    }

    /// F-07/F-08 wire vocabulary: `cause` is present and correct for the
    /// causes this task introduces, and absent (not `null`) for an
    /// ApiError that doesn't set one.
    #[test]
    fn cause_field_present_only_when_set() {
        let with_cause = ApiError::service_unavailable_with_cause(
            "too many concurrent transactions",
            cause::TX_ADMISSION_EXHAUSTED,
            Some(1),
        );
        let json: serde_json::Value = serde_json::from_slice(&with_cause.to_json_bytes()).unwrap();
        assert_eq!(json["cause"], cause::TX_ADMISSION_EXHAUSTED);

        let without_cause = ApiError::not_found();
        let json: serde_json::Value = serde_json::from_slice(&without_cause.to_json_bytes()).unwrap();
        assert!(json.get("cause").is_none(), "cause must be omitted, not null, when unset");
    }

    /// `retry_after_secs` must NEVER appear in the JSON body — it is an
    /// HTTP header per RFC 7231, not a wire-body field. Regression guard
    /// against `#[serde(skip)]` being dropped from the struct field.
    #[test]
    fn retry_after_secs_never_serializes_into_the_body() {
        let err = ApiError::rate_limited("too fast", 3);
        let json: serde_json::Value = serde_json::from_slice(&err.to_json_bytes()).unwrap();
        assert!(
            json.get("retry_after_secs").is_none() && json.get("retryAfterSecs").is_none(),
            "retry_after_secs leaked into the JSON body: {json}"
        );
    }

    #[test]
    fn rate_limited_is_429_with_retry_after() {
        let err = ApiError::rate_limited("slow down", 1);
        assert_eq!(err.status, 429);
        assert_eq!(err.kind, "/errors/rate_limited");
        assert_eq!(err.retry_after_secs, Some(1));
    }

    /// Distinct from a store operation timeout: this is the WHOLE request's
    /// wall-clock budget, so it gets its own `kind` (not `timeout`'s) and no
    /// retry hint — retrying identically would very likely exceed budget
    /// again.
    #[test]
    fn request_budget_exceeded_is_504_with_a_distinct_kind_and_no_retry_after() {
        let err = ApiError::request_budget_exceeded("exceeded 30000ms");
        assert_eq!(err.status, 504);
        assert_eq!(err.kind, "/errors/request_budget_exceeded");
        assert_ne!(err.kind, ApiError::timeout("x").kind);
        assert_eq!(err.retry_after_secs, None);
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
