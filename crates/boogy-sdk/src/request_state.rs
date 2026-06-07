//! Request-scoped state set by SDK guards and read by handlers.
//!
//! The wasm component model runs each request on a fresh instance, so
//! a plain `thread_local!` is the right primitive — there is no
//! contention, and the state is naturally bounded to one in-flight
//! request. The SDK already uses the same pattern for the inbound
//! request id (see [`crate::log`]).
//!
//! Today this module owns one slot:
//!
//! - **`fallback_principal`** — set by `api_key_routes::guard` after it
//!   admits an `sk_*` bearer and resolves it to the issuing
//!   principal. Read by the `wit_glue!`-emitted `auth::current_principal()`
//!   *after* the WIT auth call returns `None`. The result: handlers
//!   call `auth::current_principal()` once and get a unified answer
//!   regardless of whether the credential was a PASETO session or a
//!   per-service key — the asymmetry that previously forced shortlinks to
//!   call `api_key_routes::caller_principal(req)` separately is gone.
//!
//! Cleared on request exit by the RAII guard in the `wit_glue!`
//! `Guest::handle` impl, so a subsequent request without an `sk_*`
//! header doesn't inherit the previous principal.

use std::cell::RefCell;

thread_local! {
    /// Per-request fallback principal stashed by SDK guards (currently
    /// only `api_key_routes::guard`). Read by `auth::current_principal()`
    /// when the WIT-level identity is `None`.
    static FALLBACK_PRINCIPAL: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Per-request WIT principal stashed at request entry by `wit_glue!`
    /// (from `auth::current_identity().principal`). This is the PASETO /
    /// session-token path. Kept separate from `FALLBACK_PRINCIPAL` so the
    /// two sources can't clobber each other; `_request_principal()` unifies
    /// them with WIT taking precedence.
    static WIT_PRINCIPAL: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Per-request fallback SCOPES stashed by `api_key_routes::guard` when it
    /// admits an `sk_*` bearer (the resolved key's granted scopes). Read by
    /// the `wit_glue!`-emitted `auth::current_scopes()` / `auth::has_scope()`
    /// when the WIT-level identity is `None`, so scope checks
    /// (`auth::require_scope`) resolve uniformly for PASETO and `sk_*`
    /// callers — without this an `sk_*` caller's scopes were invisible and
    /// `require_scope` denied every API-key request.
    static FALLBACK_SCOPES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Internal: stash a principal for `auth::current_principal()` to fall
/// back to when WIT auth is anonymous. Called by SDK guards that
/// resolve identity from non-PASETO credentials (`sk_*` keys today).
///
/// User code shouldn't call this — there is no legitimate use case
/// outside an SDK-provided guard.
#[doc(hidden)]
pub fn _set_fallback_principal(p: Option<String>) {
    FALLBACK_PRINCIPAL.with(|r| *r.borrow_mut() = p);
}

/// Internal: read the fallback principal slot. Used by the
/// `wit_glue!`-emitted `auth::current_principal()` after the primary
/// WIT auth check returns `None`.
#[doc(hidden)]
pub fn _fallback_principal() -> Option<String> {
    FALLBACK_PRINCIPAL.with(|r| r.borrow().clone())
}

/// Internal: stash the resolved `sk_*` scopes for `auth::current_scopes()` /
/// `auth::has_scope()` to fall back to when WIT auth is anonymous. Called by
/// `api_key_routes::guard`. User code shouldn't call this.
#[doc(hidden)]
pub fn _set_fallback_scopes(s: Option<Vec<String>>) {
    FALLBACK_SCOPES.with(|r| *r.borrow_mut() = s);
}

/// Internal: read the fallback scopes slot. Used by the `wit_glue!`-emitted
/// `auth::current_scopes()` / `auth::has_scope()` after the WIT auth check is
/// anonymous.
#[doc(hidden)]
pub fn _fallback_scopes() -> Option<Vec<String>> {
    FALLBACK_SCOPES.with(|r| r.borrow().clone())
}

/// Internal: stash the WIT-layer principal at request entry. Called by
/// `wit_glue!`'s `Guest::handle` before routing so that
/// `_request_principal()` — and therefore `Principal::from_request` —
/// can return the PASETO/session principal without calling back into the
/// WIT bindings (which live only in the consumer crate's `bindings`
/// module, not in the SDK proper).
#[doc(hidden)]
pub fn _set_wit_principal(p: Option<String>) {
    WIT_PRINCIPAL.with(|r| *r.borrow_mut() = p);
}

/// Internal: read the unified per-request principal.
///
/// Precedence: WIT identity (PASETO / session token) wins over the
/// API-key fallback.  Neither requires a round-trip into WIT at the
/// call site — both are stashed at request entry (`_set_wit_principal`)
/// or when the api-key guard runs (`_set_fallback_principal`).
///
/// Used by `crate::extract::Principal::from_request` so the extractor
/// can source the principal without access to the consumer crate's WIT
/// bindings.
#[doc(hidden)]
pub fn _request_principal() -> Option<String> {
    WIT_PRINCIPAL.with(|r| r.borrow().clone())
        .or_else(|| FALLBACK_PRINCIPAL.with(|r| r.borrow().clone()))
}

/// Defensive ownership match used by the `auth::owns_resource` guard and
/// `auth::load_owned` / `auth::find_owned`.
///
/// Returns `true` only when `principal` is non-blank AND exactly equals
/// `row_owner`. A **blank** principal (empty or whitespace-only) can never
/// own anything — without this guard an empty `current_principal()` would
/// match any row whose owner column is also empty/unset (a missing owner,
/// a default `""`, or a row written before ownership was assigned), turning
/// "anonymous" into "owns every un-owned resource". Fail closed instead:
/// blank principal ⇒ not-found / false.
///
/// Trimming `row_owner` would be wrong (an owner stored as `" x "` is a
/// distinct principal we must not silently coerce), so only the *caller's*
/// principal is blank-checked; the equality itself stays exact.
#[doc(hidden)]
pub fn _principal_owns(principal: &str, row_owner: &str) -> bool {
    if principal.trim().is_empty() {
        return false;
    }
    principal == row_owner
}

/// Like [`_request_principal`] but rejects a blank principal: returns
/// `None` when the resolved principal is empty/whitespace-only. The
/// resource-auth helpers use this so a blank identity is treated as
/// anonymous (401 / not-found) rather than a principal that can match
/// un-owned rows.
#[doc(hidden)]
pub fn _request_principal_nonblank() -> Option<String> {
    _request_principal().filter(|p| !p.trim().is_empty())
}

/// RAII guard that clears every per-request thread-local on drop: the
/// request id ([`crate::log`]) plus both principal slots. Used by the
/// `wit_glue!`-emitted HTTP `Guest::handle` and job `handle_job` entry
/// points so both share one definition and clean up identically even if
/// a handler panics (on wasm a panic aborts; in host test paths it
/// unwinds — either way `Drop` runs and the next request starts clean).
#[doc(hidden)]
pub struct RequestStateGuard(());

impl Drop for RequestStateGuard {
    fn drop(&mut self) {
        crate::log::_set_request_id(None);
        _set_wit_principal(None);
        _set_fallback_principal(None);
        _set_fallback_scopes(None);
    }
}

/// Enter a request/job scope: stash `wit_principal` in the WIT principal
/// slot and return a guard that clears all per-request state on drop.
/// Both entry points call this so `current_principal()`, the `Principal`
/// extractor, and every `auth::*` resource helper resolve identically on
/// the HTTP and job paths (fixes the job-path `current_principal() == None`
/// footgun).
#[doc(hidden)]
pub fn enter(wit_principal: Option<String>) -> RequestStateGuard {
    _set_wit_principal(wit_principal);
    RequestStateGuard(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_principal_round_trips() {
        _set_fallback_principal(Some("agent_42".to_string()));
        assert_eq!(_fallback_principal().as_deref(), Some("agent_42"));
        _set_fallback_principal(None);
        assert_eq!(_fallback_principal(), None);
    }

    #[test]
    fn wit_principal_takes_precedence_over_fallback() {
        _set_wit_principal(Some("paseto_user".to_string()));
        _set_fallback_principal(Some("api_key_user".to_string()));
        assert_eq!(_request_principal().as_deref(), Some("paseto_user"));
        // cleanup
        _set_wit_principal(None);
        _set_fallback_principal(None);
    }

    #[test]
    fn fallback_used_when_wit_absent() {
        _set_wit_principal(None);
        _set_fallback_principal(Some("api_key_user".to_string()));
        assert_eq!(_request_principal().as_deref(), Some("api_key_user"));
        _set_fallback_principal(None);
    }

    #[test]
    fn request_principal_none_when_anonymous() {
        _set_wit_principal(None);
        _set_fallback_principal(None);
        assert_eq!(_request_principal(), None);
    }

    #[test]
    fn blank_principal_never_owns_a_resource() {
        // An empty principal must not match a row with an empty owner column.
        assert!(!_principal_owns("", ""));
        assert!(!_principal_owns("   ", ""));
        assert!(!_principal_owns("\t", "anything"));
        // A blank principal must not match a non-empty owner either.
        assert!(!_principal_owns("", "agent_1"));
    }

    #[test]
    fn nonblank_principal_owns_only_exact_match() {
        assert!(_principal_owns("agent_1", "agent_1"));
        assert!(!_principal_owns("agent_1", "agent_2"));
        // Owner is never trimmed: a padded stored owner is a distinct principal.
        assert!(!_principal_owns("agent_1", " agent_1 "));
        assert!(!_principal_owns("agent_1", ""));
    }

    #[test]
    fn request_principal_nonblank_filters_blank() {
        _set_wit_principal(Some("".to_string()));
        assert_eq!(_request_principal_nonblank(), None, "empty WIT principal is blank");
        _set_wit_principal(Some("   ".to_string()));
        assert_eq!(_request_principal_nonblank(), None, "whitespace principal is blank");
        _set_wit_principal(Some("agent_1".to_string()));
        assert_eq!(_request_principal_nonblank().as_deref(), Some("agent_1"));
        _set_wit_principal(None);

        // Falls through to a blank fallback principal too.
        _set_fallback_principal(Some("".to_string()));
        assert_eq!(_request_principal_nonblank(), None);
        _set_fallback_principal(None);
    }

    #[test]
    fn enter_sets_wit_principal_and_guard_clears_all_on_drop() {
        // Pre-seed leftover state to prove the guard wipes everything.
        crate::log::_set_request_id(Some("req-old".to_string()));
        _set_fallback_principal(Some("leftover".to_string()));
        {
            let _g = enter(Some("agent_job".to_string()));
            // In-scope: the unified principal resolves to the job identity.
            assert_eq!(_request_principal().as_deref(), Some("agent_job"));
        }
        // Guard drop clears the WIT slot AND the fallback slot.
        assert_eq!(_request_principal(), None);
        assert_eq!(_fallback_principal(), None);
    }
}
