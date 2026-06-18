//! `api_keys_glue!` macro — emits the API-key management endpoints and
//! a guard at the user's crate level.
//!
//! Why a second macro: the handlers reference WIT-generated types under
//! `bindings::boogy::platform::*`, which only exist inside the
//! user's crate after `wit_bindgen::generate!` runs. The pure-logic
//! helpers in [`super::logic`] don't depend on those types and stay in
//! this crate, where they're independently testable.
//!
//! Usage (in addition to the standard `wit_glue!` invocation):
//! ```ignore
//! mod bindings {
//!     wit_bindgen::generate!({ world: "service", path: "../../boogy-wit/wit" });
//! }
//! boogy_sdk::wit_glue!(bindings, MyApi);
//! boogy_sdk::api_keys_glue!(bindings);
//! ```
//!
//! After expansion the calling crate gains a module `api_key_routes`
//! with these items:
//!
//! - `install_table()` — creates the `__boogy_api_keys` table
//!   (idempotent). Call from `init_tables`.
//! - `create`, `list`, `revoke`, `rotate` — Router-compatible handlers
//!   for the management endpoints. Authenticated identity required
//!   (PASETO via the host auth middleware); local API keys cannot
//!   manage keys.
//! - `resolve_caller(req) -> Option<ResolvedKey>` — for handlers that
//!   want to read the calling key (e.g. for scope checks).
//! - `caller_principal(req) -> Option<String>` — lower-level
//!   "without going through guard, do the work yourself" helper.
//!   Prefer [`crate::auth::current_principal`] in handlers behind
//!   `guard` — the unified helper reads the per-request slot the
//!   guard populates and works across PASETO + sk_* uniformly.
//! - `guard(req: &mut Req<'_>) -> Result<(), HttpResponse>` — passes
//!   if any credential resolves (PASETO via WIT auth cap **OR**
//!   `sk_*` via the local store), stashing the resolved sk_*
//!   principal in the SDK's per-request slot so
//!   `auth::current_principal()` returns it transparently. Returns
//!   401 RFC 7807 `application/problem+json` otherwise.
//!
//! ## Operational notes
//!
//! - Management endpoints assume `init_tables` has run
//!   `install_table()` already. Forgetting that produces a 500 with
//!   `no such table`.
//! - `resolve_caller` updates `last_used_at` on success — callers
//!   don't need to track usage themselves.
//! - The handlers return JSON bodies (`{"id":..., "secret":...}` for
//!   create; `{"items":[...]}` for list).

/// Emit the API-key management handlers and guard. See module docs.
#[macro_export]
macro_rules! api_keys_glue {
    ($bindings:ident) => {
        pub mod api_key_routes {
            use $crate::api_keys;
            use $crate::response::{self, HttpResponse};
            use $crate::router::Req;
            use $crate::store::Val;
            use $crate::Request;

            /// Authenticated caller (decoded from a presented `sk_*`
            /// bearer). The handler guard typically maps it onto an
            /// `Identity` or simply uses it as a permission gate.
            #[derive(Debug, Clone)]
            pub struct ResolvedKey {
                pub id: String,
                pub prefix: String,
                pub name: String,
                pub scopes: Vec<String>,
            }

            /// Idempotent table installation. Call from `init_tables`.
            pub fn install_table() {
                super::create_table_from(&api_keys::schema_table());
            }

            // ---------- Management endpoints ----------

            pub fn create(req: &mut Req<'_>) -> HttpResponse {
                let creator = match super::__api_keys_require_identity() {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                let body = match req.body() {
                    Some(b) => b,
                    None => return response::bad_request("missing body"),
                };
                let parsed: api_keys::CreateRequest =
                    match $crate::json::from_slice(body) {
                        Ok(v) => v,
                        Err(e) => {
                            return response::bad_request(&format!("invalid JSON: {e}"))
                        }
                    };
                // Privilege-escalation guard: a creator may only mint a key with
                // scopes it already holds (wildcard-aware). Reject 403 on any
                // requested scope the creator's own scopes don't cover.
                let caller_scopes = super::__api_keys_caller_scopes();
                let unmet = api_keys::unmet_scopes(&caller_scopes, &parsed.scopes);
                if !unmet.is_empty() {
                    return $crate::error::ApiError::forbidden(format!(
                        "cannot issue a key with scopes you do not hold: {}",
                        unmet.join(", ")
                    ))
                    .into();
                }
                let prepared = match api_keys::prepare_create(&parsed, Some(&creator)) {
                    Ok(p) => p,
                    Err(e) => return response::bad_request(&e),
                };
                match super::__boogy_insert_row(api_keys::TABLE, &prepared.columns) {
                    Ok(_) => response::created(&prepared.response),
                    Err(e) => response::server_error(&e.message),
                }
            }

            pub fn list(_req: &mut Req<'_>) -> HttpResponse {
                let caller = match super::__api_keys_require_identity() {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                // Per-creator scoping is a `created_by == caller` equality
                // seek on the indexed column — same key set the in-code
                // `key_belongs_to` filter selected, but without scanning the
                // whole table. NULL-`created_by` rows (legacy/bootstrap keys,
                // owned by nobody) never equal a caller, so they are excluded
                // here exactly as `key_belongs_to(None, _)` excluded them.
                let rows = match super::find_rows_by(
                    api_keys::TABLE,
                    "created_by",
                    super::$bindings::boogy::platform::store::Value::Text(caller.clone()),
                ) {
                    Ok(r) => r,
                    Err(e) => return $crate::error::ApiError::from(e).into(),
                };
                let mut items = Vec::with_capacity(rows.len());
                for row in &rows {
                    let dto = match api_keys::parse_row(row) {
                        Ok(d) => d,
                        Err(e) => {
                            return response::server_error(&format!("schema drift: {e}"))
                        }
                    };
                    items.push(dto);
                }
                response::ok(&$crate::json::json!({ "items": items }))
            }

            pub fn revoke(req: &mut Req<'_>) -> HttpResponse {
                let caller = match super::__api_keys_require_identity() {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                // Keys are addressed by their UUID `id` column (see
                // `prepare_create`), not the store's numeric rowid.
                let id = match req.params.get("id") {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => return response::not_found(),
                };
                // Load + ownership-check before mutating (existence mask: 404 on
                // missing OR other-creator) — a non-owner gets the same 404 as a
                // missing key, so key ids are never an ownership oracle.
                let row = match super::find_row_by(
                    api_keys::TABLE,
                    "id",
                    super::$bindings::boogy::platform::store::Value::Text(id),
                ) {
                    Ok(Some(r)) => r,
                    Ok(None) => return response::not_found(),
                    Err(e) => return $crate::error::ApiError::from(e).into(),
                };
                let dto = match api_keys::parse_row(&row) {
                    Ok(d) => d,
                    Err(e) => return response::server_error(&format!("schema drift: {e}")),
                };
                if !api_keys::key_belongs_to(dto.created_by.as_deref(), &caller) {
                    return response::not_found();
                }
                let cols = vec![("revoked".to_string(), Val::Integer(1))];
                match super::__boogy_update_row(api_keys::TABLE, row.id(), &cols) {
                    Ok(true) => response::ok(&api_keys::RevokeResponse {
                        id: dto.id,
                        revoked: true,
                    }),
                    Ok(false) => response::not_found(),
                    Err(e) => response::server_error(&e.message),
                }
            }

            pub fn rotate(req: &mut Req<'_>) -> HttpResponse {
                let creator = match super::__api_keys_require_identity() {
                    Ok(id) => id,
                    Err(resp) => return resp,
                };
                // Keys are addressed by their UUID `id` column, not the
                // store's numeric rowid.
                let id = match req.params.get("id") {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => return response::not_found(),
                };
                let existing = match super::find_row_by(
                    api_keys::TABLE,
                    "id",
                    super::$bindings::boogy::platform::store::Value::Text(id),
                ) {
                    Ok(Some(row)) => row,
                    Ok(None) => return response::not_found(),
                    Err(e) => return $crate::error::ApiError::from(e).into(),
                };
                let dto = match api_keys::parse_row(&existing) {
                    Ok(d) => d,
                    Err(e) => {
                        return response::server_error(&format!("schema drift: {e}"))
                    }
                };
                // Per-creator scoping: a non-owner gets the same 404 as a
                // missing key (rotate mints a live secret — must never run for
                // someone else's key).
                if !api_keys::key_belongs_to(dto.created_by.as_deref(), &creator) {
                    return response::not_found();
                }
                // Rotation = build a new key with the same name + scopes
                // + expiry, then atomically replace prefix/hash/created_*.
                let env = match dto.prefix.split('_').nth(1) {
                    Some(e) => e.to_string(),
                    None => {
                        return response::server_error("stored prefix lacks env segment")
                    }
                };
                let new_req = api_keys::CreateRequest {
                    name: dto.name.clone(),
                    env,
                    scopes: dto.scopes.clone(),
                    expires_at: dto.expires_at,
                };
                let mut prepared = match api_keys::prepare_create(&new_req, Some(&creator)) {
                    Ok(p) => p,
                    Err(e) => return response::bad_request(&e),
                };
                // Rotation preserves the row's `id` + `created_at` (both are
                // filtered out of the update below), so the response must echo
                // the *existing* values — not the fresh ones `prepare_create`
                // minted — or the caller would be handed an id that no longer
                // addresses their key.
                prepared.response.id = dto.id.clone();
                prepared.response.created_at = dto.created_at;
                // Update the existing row in place (preserving its `id`).
                let mut updates: Vec<(String, Val)> = Vec::new();
                for (n, v) in &prepared.columns {
                    // Don't change the row's `id` — that would break
                    // anyone who keyed off it. created_at also stays.
                    if n != "id" && n != "created_at" {
                        updates.push((n.clone(), v.clone()));
                    }
                }
                match super::__boogy_update_row(api_keys::TABLE, existing.id(), &updates) {
                    Ok(true) => response::ok(&prepared.response),
                    Ok(false) => response::not_found(),
                    Err(e) => response::server_error(&e.message),
                }
            }

            // ---------- Caller resolution ----------

            /// Resolve an inbound `Authorization: Bearer sk_*` to a
            /// `ResolvedKey`. Returns `None` for missing / malformed /
            /// unknown / revoked / expired credentials. Updates
            /// `last_used_at` on success.
            pub fn resolve_caller(req: &Request) -> Option<ResolvedKey> {
                let bearer = api_keys::parse_bearer(req)?;
                // Validate format up-front (CRC + structure). Saves a
                // store round-trip on garbage input.
                api_keys::parse(bearer).ok()?;

                let prefix = api_keys::compute_lookup_prefix(bearer);
                let row = super::find_row_by(
                    api_keys::TABLE,
                    "prefix",
                    super::$bindings::boogy::platform::store::Value::Text(prefix),
                )
                .ok()??;
                let now = $crate::api_keys::__unix_now_for_glue();
                if !api_keys::verify_against_row(bearer, &row, now) {
                    return None;
                }
                let dto = api_keys::parse_row(&row).ok()?;

                // Best-effort last_used_at update. Failures here don't
                // affect authorization — log via tracing if/when the
                // SDK gains tracing.
                let _ = super::__boogy_update_row(
                    api_keys::TABLE,
                    row.id(),
                    &[("last_used_at".to_string(), Val::Integer(now as i64))],
                );

                Some(ResolvedKey {
                    id: dto.id,
                    prefix: dto.prefix,
                    name: dto.name,
                    scopes: dto.scopes,
                })
            }

            /// Guard combining PASETO and API-key resolution. Passes if
            /// either yields an authenticated caller; otherwise returns
            /// 401.
            ///
            /// Side effect on the `sk_*` path: stash the resolved
            /// principal in the SDK's per-request fallback slot so
            /// `auth::current_principal()` returns the same answer as
            /// `caller_principal(req)`. This is the C2 unification —
            /// resource-level guards (`auth::owns_resource`) and
            /// `auth::find_owned` work uniformly across PASETO and
            /// `sk_*` callers without each handler having to consult
            /// two different APIs. The slot is cleared on request exit
            /// by the `wit_glue!` RAII guard.
            pub fn guard(req: &mut Req<'_>) -> Result<(), HttpResponse> {
                if super::__api_keys_paseto_identity().is_some() {
                    return Ok(());
                }
                // Resolve the sk_* bearer ONCE and stash both the issuing
                // principal AND the key's scopes, so auth::current_principal()
                // and auth::current_scopes()/has_scope()/require_scope() all
                // resolve uniformly across PASETO and sk_* callers. (Resolving
                // here also avoids the extra store round-trip caller_principal
                // would do.)
                if let Some(key) = resolve_caller(req.request) {
                    let issuer = super::find_row_by(
                        api_keys::TABLE,
                        "id",
                        super::$bindings::boogy::platform::store::Value::Text(key.id.clone()),
                    )
                    .ok()
                    .flatten()
                    .map(|row| row.text("created_by"))
                    .unwrap_or_default();
                    if !issuer.is_empty() {
                        $crate::request_state::_set_fallback_principal(Some(issuer));
                        $crate::request_state::_set_fallback_scopes(Some(key.scopes.clone()));
                        return Ok(());
                    }
                }
                Err($crate::response::unauthenticated())
            }

            /// Resolve the caller's principal from either a PASETO
            /// session OR a presented `sk_*` bearer.
            ///
            /// **Prefer [`crate::auth::current_principal`].** After
            /// [`guard`] admits an `sk_*` request it stashes the
            /// resolved principal in the SDK's per-request slot, and
            /// `auth::current_principal()` reads it transparently —
            /// returning the same answer this helper does, with no
            /// store round-trip on the second call. The unified
            /// helper is the canonical entry point for stamping
            /// `owner_principal` on a row or scoping reads in any
            /// handler that sits behind `guard`.
            ///
            /// This raw helper is kept for the rare "without going
            /// through guard, do the work yourself" case. Returns
            /// `None` for anonymous requests; pair with `.ok_or_else
            /// (ApiError::unauthenticated)?` for Result-typed
            /// handlers. PASETO is checked first (the fast path — no
            /// store round-trip). The `sk_*` fallback hits the local
            /// `__boogy_api_keys` table by id and returns
            /// `created_by`; empty / null `created_by` → `None`.
            pub fn caller_principal(req: &mut Req<'_>) -> ::core::option::Option<::std::string::String> {
                if let Some(p) = super::__api_keys_paseto_identity() {
                    return Some(p);
                }
                let key = resolve_caller(req.request)?;
                let row = super::find_row_by(
                    api_keys::TABLE,
                    "id",
                    super::$bindings::boogy::platform::store::Value::Text(key.id),
                )
                .ok()
                .flatten()?;
                let issuer = row.text("created_by");
                if issuer.is_empty() { None } else { Some(issuer) }
            }

            /// Extension trait that mounts the standard `/_keys` management
            /// endpoints (create / list / revoke / rotate) in one call, so a
            /// service writes `Router::new().with_api_key_routes()…` instead
            /// of four manual `.post`/`.get`/`.delete` lines. `with_api_key_routes`
            /// uses the conventional `/_keys` prefix; `with_api_key_routes_at`
            /// mounts under a custom prefix (e.g. `/admin/keys`). For a fully
            /// custom layout, wire `create`/`list`/`revoke`/`rotate` by hand.
            ///
            /// Bring it into scope at the router-building site with
            /// `use crate::api_key_routes::ApiKeyRoutes;`. The guard for your
            /// own routes stays a separate `.group([api_key_routes::guard], …)`
            /// — which routes to gate is inherently per-service.
            pub trait ApiKeyRoutes {
                /// Mount the management endpoints at the conventional `/_keys`.
                fn with_api_key_routes(self) -> Self;
                /// Mount the management endpoints under a custom `prefix`
                /// (e.g. `/admin/keys`). A trailing `/` is trimmed.
                fn with_api_key_routes_at(self, prefix: &str) -> Self;
            }

            impl ApiKeyRoutes for $crate::router::Router {
                fn with_api_key_routes(self) -> Self {
                    self.with_api_key_routes_at("/_keys")
                }

                fn with_api_key_routes_at(self, prefix: &str) -> Self {
                    let paths = api_keys::key_route_paths(prefix);
                    self.post(&paths.collection, create)
                        .get(&paths.collection, list)
                        .delete(&paths.by_id, revoke)
                        .post(&paths.rotate, rotate)
                }
            }
        }

        // ---------- Module-private helpers for the macro emission ----------
        //
        // These live at the parent level (alongside `create_table_from`
        // etc.) so they have access to $bindings.

        fn __api_keys_caller_scopes() -> ::std::vec::Vec<::std::string::String> {
            $bindings::boogy::platform::auth::current_identity()
                .map(|i| i.scopes)
                .unwrap_or_default()
        }

        fn __api_keys_paseto_identity() -> ::core::option::Option<::std::string::String> {
            $bindings::boogy::platform::auth::current_identity().map(|i| i.principal)
        }

        fn __api_keys_require_identity() -> ::core::result::Result<
            ::std::string::String,
            $crate::response::HttpResponse,
        > {
            match __api_keys_paseto_identity() {
                Some(id) => Ok(id),
                None => Err($crate::response::unauthenticated()),
            }
        }

    };
}
