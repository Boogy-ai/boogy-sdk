//! Typed handler-signature extractors. Each implements [`FromRequest`]
//! so it can appear in a handler signature like
//! `fn handler(Path(id): Path<u64>, Json(body): Json<Foo>) -> …`.
//!
//! # How principal sourcing works
//!
//! `Principal::from_request` reads from [`crate::request_state::_request_principal`],
//! a thread-local populated at request entry by the `wit_glue!` macro.
//! The macro stashes both the WIT-layer principal (PASETO / session token)
//! and the API-key fallback into `request_state` before routing begins, so
//! no WIT call is needed here — the result is identical to what the macro-emitted
//! `auth::current_principal()` returns.

use serde::de::DeserializeOwned;

use crate::error::ApiError;
use crate::router::Req;

/// Extracted from a `&Req<'_>` (read-only). Implementors map their own
/// failure (missing / parse / auth) to an [`ApiError`] — its `IntoResponse`
/// renders the correct HTTP status uniformly.
pub trait FromRequest: Sized {
    fn from_request(req: &Req<'_>) -> Result<Self, ApiError>;
}

// ─── Path<T> ────────────────────────────────────────────────────────────────

/// Path parameters from the matched route (`/posts/{id}` etc).
///
/// `T` is deserialized from the named params the router extracted using
/// the same `serde_urlencoded` mechanism [`Req::parse_query_raw`] uses —
/// both are "string key-value pairs into T".
///
/// **Single-param convenience:** when the matched route has exactly one
/// param, `Path<u64>` / `Path<String>` / `Path<Uuid>` all work: the
/// sole param's raw value string is deserialized directly into `T` via
/// `serde_urlencoded`. This covers the dominant `/:id` / `/{id}` pattern
/// without forcing authors to define a wrapper struct.
///
/// **Multi-param:** declare a struct with `#[derive(Deserialize)]` whose
/// field names match the param names, or use `Path<HashMap<String,String>>`.
#[derive(Debug, Clone)]
pub struct Path<T>(pub T);

impl<T: DeserializeOwned> FromRequest for Path<T> {
    fn from_request(req: &Req<'_>) -> Result<Self, ApiError> {
        let pairs = req.params.as_pairs();

        // Re-encode the matched path params to a urlencoded string, then
        // drive `serde_urlencoded` — the same two-step `parse_query_raw`
        // uses. Both Path and Query share this serde path.
        let encoded = serde_urlencoded::to_string(pairs).map_err(|e| {
            ApiError::bad_request(format!("path params encode error: {e}"))
        })?;

        match serde_urlencoded::from_str::<T>(&encoded) {
            Ok(v) => Ok(Path(v)),
            Err(map_err) => {
                // Single-value convenience: exactly one param → try
                // deserializing the value itself as T (covers Path<u64>,
                // Path<String>, Path<Uuid>, etc. on /:id style routes).
                if pairs.len() == 1 {
                    let raw = &pairs[0].1;
                    // Encode as a single-value urlencoded string so
                    // serde_urlencoded can deserialize primitive T.
                    // For primitives, urlencoded wraps it in a dummy key;
                    // we use serde_plain instead for a cleaner approach,
                    // but to stay dep-free we serialize as `value=<raw>`
                    // and let the deserializer treat the sole value as T
                    // by encoding as an empty-key pair.
                    //
                    // The simplest approach: serialize the value directly
                    // as a URL-encoded string with no key, then parse.
                    // serde_urlencoded doesn't support that. Instead, use
                    // the raw string with serde_json to parse primitives
                    // and strings.
                    let single_result: Result<T, _> = serde_json::from_str(raw)
                        .or_else(|_| serde_json::from_str(&format!("\"{}\"", raw)));
                    match single_result {
                        Ok(v) => return Ok(Path(v)),
                        Err(_) => {
                            // Fall through to the map-deserialize error
                            // which is more informative (field name context).
                        }
                    }
                }
                Err(ApiError::bad_request(format!("invalid path params: {map_err}")))
            }
        }
    }
}

// ─── Query<T> ───────────────────────────────────────────────────────────────

/// Query-string parameters (`?cursor=…&limit=…`) deserialized into `T`.
///
/// Internally delegates to [`Req::parse_query_raw`] (no garde validation).
/// To add validation, declare the struct with `garde::Validate` and call
/// `req.parse_query()` directly, or compose this extractor with a guard.
#[derive(Debug, Clone)]
pub struct Query<T>(pub T);

impl<T: DeserializeOwned> FromRequest for Query<T> {
    fn from_request(req: &Req<'_>) -> Result<Self, ApiError> {
        req.parse_query_raw::<T>().map(Query)
    }
}

// ─── Json<T> ────────────────────────────────────────────────────────────────

/// `FromRequest` impl for [`crate::response::Json<T>`].
///
/// `Json<T>` is defined in `response` (where it implements `IntoResponse`)
/// and re-exported here as an extractor: when `T: DeserializeOwned` the
/// framework deserializes the request body and wraps it in `Json(v)` before
/// calling the handler. Missing body → 400; malformed JSON → 400.
///
/// This means `Json<T>` plays both roles — extractor AND response wrapper —
/// the same type, no import conflict:
/// ```ignore
/// fn create(Json(body): Json<CreateReq>) -> Result<Json<CreateResp>, ApiError> { … }
/// ```
impl<T: DeserializeOwned> FromRequest for crate::response::Json<T> {
    fn from_request(req: &Req<'_>) -> Result<Self, ApiError> {
        let bytes =
            req.body().ok_or_else(|| ApiError::bad_request("missing JSON body"))?;
        let v: T = serde_json::from_slice(bytes)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?;
        Ok(crate::response::Json(v))
    }
}

// ─── Principal ──────────────────────────────────────────────────────────────

/// The authenticated caller's principal string (newtype).
///
/// Absent → 401 (`ApiError::unauthenticated`).
///
/// Sources the principal from the same thread-local the `wit_glue!`-emitted
/// `auth::current_principal()` reads — stashed at request entry by the macro
/// so both sites agree without a WIT call inside `boogy-sdk`.
#[derive(Debug, Clone)]
pub struct Principal(pub String);

impl FromRequest for Principal {
    fn from_request(_req: &Req<'_>) -> Result<Self, ApiError> {
        crate::request_state::_request_principal()
            .map(Principal)
            .ok_or_else(ApiError::unauthenticated)
    }
}

// ─── Option<Principal> ──────────────────────────────────────────────────────

/// Optional authenticated principal. Never errors — anonymous requests
/// yield `Ok(None)`, authenticated requests yield `Ok(Some(Principal(…)))`.
impl FromRequest for Option<Principal> {
    fn from_request(_req: &Req<'_>) -> Result<Self, ApiError> {
        Ok(crate::request_state::_request_principal().map(Principal))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Ctx;
    use crate::router::Params;
    use crate::Request;

    /// Build a `Request` with the given fields.
    fn make_request(
        body: Option<Vec<u8>>,
        query_params: Vec<(String, String)>,
    ) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: vec![],
            body,
            path_params: vec![],
            query_params,
        }
    }

    /// Build a `Req<'_>` from a `Request` and path params.
    fn make_req<'a>(
        request: &'a Request,
        params: &'a Params,
    ) -> Req<'a> {
        Req {
            request,
            params,
            ctx: Ctx::new(),
        }
    }

    // ── Json ────────────────────────────────────────────────────────────────

    use crate::response::Json;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Foo {
        x: i32,
    }

    #[test]
    fn json_success() {
        let body = b"{\"x\":1}".to_vec();
        let req = make_request(Some(body), vec![]);
        let params = Params::from_pairs(vec![]);
        let r = Req { request: &req, params: &params, ctx: Ctx::new() };
        let Json(foo) = Json::<Foo>::from_request(&r).unwrap();
        assert_eq!(foo, Foo { x: 1 });
    }

    #[test]
    fn json_missing_body_returns_400() {
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let err = Json::<Foo>::from_request(&r).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.detail.as_deref().unwrap_or("").contains("missing JSON body"));
    }

    #[test]
    fn json_invalid_json_returns_400() {
        let req = make_request(Some(b"{not json}".to_vec()), vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let err = Json::<Foo>::from_request(&r).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.detail.as_deref().unwrap_or("").contains("invalid JSON body"));
    }

    // ── Query ───────────────────────────────────────────────────────────────

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct QFoo {
        x: i32,
    }

    #[test]
    fn query_success() {
        let req = make_request(None, vec![("x".to_string(), "42".to_string())]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let Query(q) = Query::<QFoo>::from_request(&r).unwrap();
        assert_eq!(q, QFoo { x: 42 });
    }

    #[test]
    fn query_missing_field_returns_400() {
        // QFoo requires `x` (no default), so missing field → parse error → 400
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let err = Query::<QFoo>::from_request(&r).unwrap_err();
        assert_eq!(err.status, 400);
    }

    // ── Path ────────────────────────────────────────────────────────────────

    #[test]
    fn path_single_param_as_u64() {
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![("id".to_string(), "5".to_string())]);
        let r = make_req(&req, &params);
        let Path(id) = Path::<u64>::from_request(&r).unwrap();
        assert_eq!(id, 5u64);
    }

    #[test]
    fn path_single_param_as_string() {
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![("slug".to_string(), "hello-world".to_string())]);
        let r = make_req(&req, &params);
        let Path(s) = Path::<String>::from_request(&r).unwrap();
        assert_eq!(s, "hello-world");
    }

    #[test]
    fn path_single_param_type_mismatch_returns_400() {
        let req = make_request(None, vec![]);
        // "abc" cannot parse as u64
        let params = Params::from_pairs(vec![("id".to_string(), "abc".to_string())]);
        let r = make_req(&req, &params);
        let err = Path::<u64>::from_request(&r).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct PathStruct {
        user_id: u64,
        post_id: u64,
    }

    #[test]
    fn path_multi_param_struct() {
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![
            ("user_id".to_string(), "10".to_string()),
            ("post_id".to_string(), "20".to_string()),
        ]);
        let r = make_req(&req, &params);
        let Path(ps) = Path::<PathStruct>::from_request(&r).unwrap();
        assert_eq!(ps, PathStruct { user_id: 10, post_id: 20 });
    }

    // ── Principal ───────────────────────────────────────────────────────────

    #[test]
    fn principal_returns_ok_when_authed() {
        crate::request_state::_set_wit_principal(Some("user_123".to_string()));
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let result = Principal::from_request(&r);
        crate::request_state::_set_wit_principal(None);
        let Principal(p) = result.unwrap();
        assert_eq!(p, "user_123");
    }

    #[test]
    fn principal_returns_401_when_anonymous() {
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let err = Principal::from_request(&r).unwrap_err();
        assert_eq!(err.status, 401);
    }

    // ── Option<Principal> ───────────────────────────────────────────────────

    #[test]
    fn option_principal_some_when_authed() {
        crate::request_state::_set_wit_principal(Some("user_456".to_string()));
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let result = <Option<Principal>>::from_request(&r);
        crate::request_state::_set_wit_principal(None);
        let opt = result.unwrap();
        assert!(opt.is_some());
        assert_eq!(opt.unwrap().0, "user_456");
    }

    #[test]
    fn option_principal_none_when_anonymous_never_errors() {
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(None);
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let result = <Option<Principal>>::from_request(&r).unwrap();
        assert!(result.is_none(), "anonymous should yield Ok(None), never Err");
    }

    #[test]
    fn option_principal_uses_fallback_when_wit_absent() {
        crate::request_state::_set_wit_principal(None);
        crate::request_state::_set_fallback_principal(Some("api_key_user".to_string()));
        let req = make_request(None, vec![]);
        let params = Params::from_pairs(vec![]);
        let r = make_req(&req, &params);
        let result = <Option<Principal>>::from_request(&r);
        crate::request_state::_set_fallback_principal(None);
        let opt = result.unwrap();
        assert_eq!(opt.map(|p| p.0).as_deref(), Some("api_key_user"));
    }
}
