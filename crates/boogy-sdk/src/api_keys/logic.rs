//! Pure logic for API key management.
//!
//! Everything here is testable without WIT bindings. The `wit_glue!`
//! macro emits thin handlers that bridge these helpers to the user's
//! `bindings::boogy::platform::store::*` calls.

use std::time::{SystemTime, UNIX_EPOCH};

use boogy_auth_core::api_key::{self, ApiKey};

use crate::store::{Row, Val};

use super::types::{CreateRequest, CreateResponse, KeyDto};

/// Maximum length of the operator-supplied `name` field. Hard-coded
/// rather than configurable to keep the schema compact and predictable.
pub const MAX_NAME_LEN: usize = 128;

/// Result of preparing a key for insertion. Holds both the column
/// values to write and the response to return to the operator. The
/// caller (typically a macro-emitted handler) bridges `columns` to the
/// WIT `store::insert` call.
pub struct PreparedCreate {
    /// Columns ready to insert. Use the `wit_glue!`-emitted converter
    /// to map to `bindings::boogy::platform::store::Column` shape.
    pub columns: Vec<(String, Val)>,
    /// Response body, including the secret (shown once).
    pub response: CreateResponse,
}

/// Validate a [`CreateRequest`] and produce the row + response. The
/// secret is generated here; subsequent calls always produce different
/// values.
///
/// `created_by` should be the agent_id of the caller, when known
/// (e.g. resolved from a PASETO bearer). `None` is permitted for
/// bootstrapping flows.
pub fn prepare_create(
    req: &CreateRequest,
    created_by: Option<&str>,
) -> Result<PreparedCreate, String> {
    if req.name.is_empty() {
        return Err("name is required".into());
    }
    if req.name.len() > MAX_NAME_LEN {
        return Err(format!("name exceeds {MAX_NAME_LEN} chars"));
    }
    let key: ApiKey =
        api_key::generate(&req.env).map_err(|e| format!("generate: {e}"))?;

    let now = unix_now();
    let id = uuid::Uuid::now_v7().simple().to_string();
    let scopes_str = req.scopes.join(",");

    let columns = vec![
        ("id".to_string(), Val::Text(id.clone())),
        ("prefix".to_string(), Val::Text(key.prefix.clone())),
        ("hash".to_string(), Val::Text(key.hash.clone())),
        ("name".to_string(), Val::Text(req.name.clone())),
        ("scopes".to_string(), Val::Text(scopes_str)),
        (
            "created_by".to_string(),
            match created_by {
                Some(s) => Val::Text(s.to_string()),
                None => Val::Null,
            },
        ),
        ("created_at".to_string(), Val::Integer(now as i64)),
        ("last_used_at".to_string(), Val::Null),
        (
            "expires_at".to_string(),
            match req.expires_at {
                Some(ts) => Val::Integer(ts as i64),
                None => Val::Null,
            },
        ),
        ("revoked".to_string(), Val::Integer(0)),
    ];

    let response = CreateResponse {
        id,
        prefix: key.prefix,
        secret: key.secret,
        name: req.name.clone(),
        scopes: req.scopes.clone(),
        expires_at: req.expires_at,
        created_at: now,
    };

    Ok(PreparedCreate { columns, response })
}

/// Convert a stored row into a public-facing [`KeyDto`]. Returns an
/// error if required columns are missing or have unexpected types,
/// which would indicate schema drift.
pub fn parse_row(row: &Row) -> Result<KeyDto, String> {
    Ok(KeyDto {
        id: text(row, "id")?,
        prefix: text(row, "prefix")?,
        name: text(row, "name")?,
        scopes: parse_scopes(&text(row, "scopes")?),
        created_by: optional_text(row, "created_by"),
        created_at: integer(row, "created_at")? as u64,
        last_used_at: optional_integer(row, "last_used_at").map(|v| v as u64),
        expires_at: optional_integer(row, "expires_at").map(|v| v as u64),
        revoked: integer(row, "revoked")? != 0,
    })
}

/// True iff a stored key row's `created_by` matches `caller`. The
/// management handlers use this for per-creator scoping with a
/// deny-by-existence mask: a non-owner is treated identically to a
/// missing row (404), so a caller can't distinguish "key N doesn't
/// exist" from "key N isn't yours". A row with NULL/empty `created_by`
/// (legacy / bootstrap keys) is owned by nobody and never matches.
pub fn key_belongs_to(created_by: Option<&str>, caller: &str) -> bool {
    matches!(created_by, Some(c) if c == caller)
}

/// True iff `held` (a scope the caller possesses) covers `requested`.
/// Wildcard semantics, modeled on host `boogy_ingress::ScopeMatcher` with
/// `*` as the canonical full wildcard:
///   - `*` (and the equivalent `*:*`) covers everything
///   - `resource:*` covers any `resource:<action>`
///   - `*:action` covers any `<resource>:action`
///   - otherwise exact match
///
/// Scopes are `resource:action`; a malformed scope (no `:`) only matches
/// itself exactly. Note: the host's matcher *parser* rejects `*:*` as an
/// input form (it canonicalizes to `*`), so this is intentionally a touch
/// more lenient than the host on that one spelling — harmless, since
/// matching `*:*` only ever broadens what a holder of an already-total
/// grant may mint.
pub fn scope_covers(held: &str, requested: &str) -> bool {
    if held == "*" || held == "*:*" {
        return true;
    }
    if held == requested {
        return true;
    }
    let (hr, ha) = match held.split_once(':') {
        Some(p) => p,
        None => return false, // malformed held scope: exact-only (already checked above)
    };
    let (rr, ra) = match requested.split_once(':') {
        Some(p) => p,
        None => return false, // malformed requested scope: exact-only
    };
    (hr == "*" || hr == rr) && (ha == "*" || ha == ra)
}

/// The subset of `requested` scopes NOT covered by any scope in `held`.
/// Empty result ⇒ the caller may mint every requested scope. Used by the
/// api-key `create` handler to reject privilege escalation at issuance.
pub fn unmet_scopes(held: &[String], requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|req| !held.iter().any(|h| scope_covers(h, req)))
        .cloned()
        .collect()
}

/// Constant-time check that `candidate` is the secret behind `row`.
/// Returns `false` for revoked or expired keys regardless of secret
/// match (handlers can rely on this single boolean).
pub fn verify_against_row(candidate: &str, row: &Row, now_unix: u64) -> bool {
    let revoked = integer(row, "revoked").unwrap_or(0) != 0;
    if revoked {
        return false;
    }
    if let Some(exp) = optional_integer(row, "expires_at") {
        if (exp as u64) < now_unix {
            return false;
        }
    }
    let stored_hash = match text(row, "hash") {
        Ok(h) => h,
        Err(_) => return false,
    };
    api_key::verify(candidate, &stored_hash)
}

/// Strip `Authorization: Bearer ` and return the token only if it has
/// the `sk_` prefix. Returns `None` for missing headers, non-bearer
/// schemes, or tokens that don't claim to be API keys (PASETOs and
/// future formats are left for their own resolvers).
pub fn parse_bearer(req: &crate::Request) -> Option<&str> {
    let header = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))?;
    let value = header.1.as_str();
    let stripped = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if stripped.starts_with("sk_") && !stripped.is_empty() {
        Some(stripped)
    } else {
        None
    }
}

/// First 11 chars of an `sk_*` secret — the indexed lookup prefix.
/// Re-exported for handlers that need to issue a `find` query before
/// calling [`verify_against_row`].
pub fn compute_lookup_prefix(secret: &str) -> String {
    api_key::compute_prefix(secret)
}

// -----------------------------------------------------------------------------
// Internals
// -----------------------------------------------------------------------------

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_scopes(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn text(row: &Row, name: &str) -> Result<String, String> {
    match row.get(name) {
        Val::Text(s) => Ok(s.clone()),
        Val::Null => Err(format!("column {name}: unexpected null")),
        other => Err(format!("column {name}: expected text, got {other:?}")),
    }
}

fn integer(row: &Row, name: &str) -> Result<i64, String> {
    match row.get(name) {
        Val::Integer(i) => Ok(*i),
        Val::Null => Err(format!("column {name}: unexpected null")),
        other => Err(format!("column {name}: expected integer, got {other:?}")),
    }
}

fn optional_text(row: &Row, name: &str) -> Option<String> {
    match row.get(name) {
        Val::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn optional_integer(row: &Row, name: &str) -> Option<i64> {
    match row.get(name) {
        Val::Integer(i) => Some(*i),
        _ => None,
    }
}

/// The three URL paths the API-key management endpoints mount at, derived
/// from a prefix. `collection` carries POST (create) + GET (list); `by_id`
/// carries DELETE (revoke); `rotate` carries POST (rotate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRoutePaths {
    pub collection: String,
    pub by_id: String,
    pub rotate: String,
}

/// Build the management-endpoint paths for `prefix` (e.g. `/_keys` →
/// `/_keys`, `/_keys/{id}`, `/_keys/{id}/rotate`). A trailing `/` on
/// `prefix` is trimmed so `/_keys/` and `/_keys` behave identically.
/// Used by the `ApiKeyRoutes` extension trait the `api_keys_glue!` macro
/// emits; kept here so the path logic is unit-testable without the macro.
pub fn key_route_paths(prefix: &str) -> KeyRoutePaths {
    let base = prefix.trim_end_matches('/');
    KeyRoutePaths {
        collection: base.to_string(),
        by_id: format!("{base}/{{id}}"),
        rotate: format!("{base}/{{id}}/rotate"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_route_paths_conventional() {
        let p = key_route_paths("/_keys");
        assert_eq!(p.collection, "/_keys");
        assert_eq!(p.by_id, "/_keys/{id}");
        assert_eq!(p.rotate, "/_keys/{id}/rotate");
    }

    #[test]
    fn key_route_paths_custom_prefix_trims_trailing_slash() {
        let p = key_route_paths("/admin/keys/");
        assert_eq!(p.collection, "/admin/keys");
        assert_eq!(p.by_id, "/admin/keys/{id}");
        assert_eq!(p.rotate, "/admin/keys/{id}/rotate");
    }

    fn fake_request(authorization: Option<&str>) -> crate::Request {
        let mut headers = Vec::new();
        if let Some(v) = authorization {
            headers.push(("authorization".to_string(), v.to_string()));
        }
        crate::Request {
            method: "GET".into(),
            path: "/".into(),
            headers,
            body: None,
            path_params: vec![],
            query_params: vec![],
        }
    }

    fn build_row(prepared: &PreparedCreate) -> Row {
        Row {
            columns: prepared.columns.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    #[test]
    fn prepare_create_emits_complete_row() {
        let req = CreateRequest {
            name: "ci-deploy".into(),
            env: "live".into(),
            scopes: vec!["notes:read".into(), "notes:write".into()],
            expires_at: None,
        };
        let prepared = prepare_create(&req, Some("agent_creator")).unwrap();

        let names: Vec<&str> = prepared.columns.iter().map(|(n, _)| n.as_str()).collect();
        for required in [
            "id",
            "prefix",
            "hash",
            "name",
            "scopes",
            "created_by",
            "created_at",
            "last_used_at",
            "expires_at",
            "revoked",
        ] {
            assert!(names.contains(&required), "missing column {required}");
        }
        assert!(prepared.response.secret.starts_with("sk_live_"));
        assert_eq!(prepared.response.scopes, req.scopes);
    }

    #[test]
    fn prepare_create_rejects_empty_name() {
        let req = CreateRequest {
            name: "".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        assert!(prepare_create(&req, None).is_err());
    }

    #[test]
    fn prepare_create_rejects_oversize_name() {
        let req = CreateRequest {
            name: "x".repeat(MAX_NAME_LEN + 1),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        assert!(prepare_create(&req, None).is_err());
    }

    #[test]
    fn prepare_create_propagates_invalid_env() {
        let req = CreateRequest {
            name: "k".into(),
            env: "with space".into(),
            scopes: vec![],
            expires_at: None,
        };
        assert!(prepare_create(&req, None).is_err());
    }

    #[test]
    fn parse_row_round_trips_through_prepare_create() {
        let req = CreateRequest {
            name: "deploy".into(),
            env: "live".into(),
            scopes: vec!["read".into()],
            expires_at: Some(1_700_000_000),
        };
        let prepared = prepare_create(&req, Some("agent_creator")).unwrap();
        let row = build_row(&prepared);
        let dto = parse_row(&row).unwrap();
        assert_eq!(dto.id, prepared.response.id);
        assert_eq!(dto.prefix, prepared.response.prefix);
        assert_eq!(dto.name, "deploy");
        assert_eq!(dto.scopes, vec!["read".to_string()]);
        assert_eq!(dto.created_by.as_deref(), Some("agent_creator"));
        assert_eq!(dto.expires_at, Some(1_700_000_000));
        assert!(!dto.revoked);
    }

    #[test]
    fn verify_against_row_succeeds_for_correct_secret() {
        let req = CreateRequest {
            name: "k".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        let prepared = prepare_create(&req, None).unwrap();
        let secret = prepared.response.secret.clone();
        let row = build_row(&prepared);
        assert!(verify_against_row(&secret, &row, unix_now()));
    }

    #[test]
    fn verify_against_row_fails_for_wrong_secret() {
        let req = CreateRequest {
            name: "k".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        let prepared = prepare_create(&req, None).unwrap();
        let row = build_row(&prepared);
        assert!(!verify_against_row("sk_live_wrongwrongwrongwrongwrong_abcd", &row, unix_now()));
    }

    #[test]
    fn verify_against_row_fails_when_revoked() {
        let req = CreateRequest {
            name: "k".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        let mut prepared = prepare_create(&req, None).unwrap();
        // Flip the revoked flag in the columns list.
        for (name, val) in prepared.columns.iter_mut() {
            if name == "revoked" {
                *val = Val::Integer(1);
            }
        }
        let secret = prepared.response.secret.clone();
        let row = build_row(&prepared);
        assert!(!verify_against_row(&secret, &row, unix_now()));
    }

    #[test]
    fn verify_against_row_fails_when_expired() {
        let req = CreateRequest {
            name: "k".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: Some(1_000), // long ago
        };
        let prepared = prepare_create(&req, None).unwrap();
        let secret = prepared.response.secret.clone();
        let row = build_row(&prepared);
        // "Now" is well past 1_000s after epoch.
        assert!(!verify_against_row(&secret, &row, unix_now()));
    }

    #[test]
    fn parse_bearer_extracts_sk_token() {
        let req = fake_request(Some("Bearer sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd"));
        assert_eq!(
            parse_bearer(&req),
            Some("sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd"),
        );
    }

    #[test]
    fn parse_bearer_ignores_paseto_bearer() {
        let req = fake_request(Some("Bearer v4.public.something"));
        assert!(parse_bearer(&req).is_none());
    }

    #[test]
    fn parse_bearer_ignores_missing_or_non_bearer() {
        assert!(parse_bearer(&fake_request(None)).is_none());
        assert!(parse_bearer(&fake_request(Some("Basic abc"))).is_none());
    }

    #[test]
    fn compute_lookup_prefix_matches_generated_prefix() {
        let req = CreateRequest {
            name: "k".into(),
            env: "live".into(),
            scopes: vec![],
            expires_at: None,
        };
        let prepared = prepare_create(&req, None).unwrap();
        assert_eq!(
            compute_lookup_prefix(&prepared.response.secret),
            prepared.response.prefix,
        );
    }

    #[test]
    fn parse_row_rejects_missing_columns() {
        // Construct a minimal Row with only `id` populated to confirm the
        // helper returns Err rather than panicking on schema drift.
        let row = Row {
            columns: vec![("id".to_string(), Val::Text("x".into()))],
        };
        assert!(parse_row(&row).is_err());
    }

    #[test]
    fn key_belongs_to_matches_only_exact_creator() {
        assert!(key_belongs_to(Some("agent_a"), "agent_a"));
        assert!(!key_belongs_to(Some("agent_b"), "agent_a"));
        // NULL/empty created_by (legacy/bootstrap keys) is owned by nobody.
        assert!(!key_belongs_to(None, "agent_a"));
    }

    #[test]
    fn scope_covers_wildcard_semantics() {
        assert!(scope_covers("notes:read", "notes:read")); // exact
        assert!(scope_covers("notes:*", "notes:write"));    // resource wildcard
        assert!(scope_covers("*:read", "billing:read"));    // action wildcard
        assert!(scope_covers("*", "anything:goes"));        // full wildcard
        assert!(scope_covers("*:*", "anything:goes"));      // full wildcard (split form)
        assert!(!scope_covers("notes:read", "notes:write")); // action mismatch
        assert!(!scope_covers("notes:*", "billing:read"));   // resource mismatch
        assert!(!scope_covers("notes", "notes:read"));       // malformed held: exact-only
    }

    #[test]
    fn unmet_scopes_flags_only_uncovered() {
        let held = vec!["notes:read".to_string(), "billing:*".to_string()];
        // All covered → empty.
        assert!(unmet_scopes(&held, &["notes:read".into(), "billing:write".into()]).is_empty());
        // notes:write is NOT covered (only notes:read held).
        assert_eq!(
            unmet_scopes(&held, &["notes:write".into()]),
            vec!["notes:write".to_string()]
        );
        // Empty request → always allowed.
        assert!(unmet_scopes(&held, &[]).is_empty());
        // Creator with "*" mints anything.
        assert!(unmet_scopes(&["*".to_string()], &["x:y".into(), "z:w".into()]).is_empty());
    }
}
