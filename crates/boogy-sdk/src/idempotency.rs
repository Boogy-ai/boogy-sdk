//! Idempotency-key middleware for handlers that mutate state.
//!
//! Standard contract (Stripe / GitHub / etc.):
//!
//! - The client sends an `Idempotency-Key` header on a write
//!   request. The server caches the response under
//!   `(key, method, path)` for a TTL.
//! - A retry of the same `(key, method, path)` with the **same body**
//!   replays the cached response — the underlying handler does not
//!   re-run.
//! - A retry with a **different body** is rejected as a key-reuse
//!   error (409 Conflict). Catches the common bug where a client
//!   reuses an idempotency key by mistake.
//! - Requests without an `Idempotency-Key` header pass through
//!   unchanged.
//!
//! The table-backed wrapper is emitted by [`wit_glue!`](crate::wit_glue)
//! since it needs to reach the user crate's `bindings::store`.
//! This module owns the constants and the pure-logic primitives
//! (`scope_key`, `body_fingerprint`) so they're unit-testable
//! without a wasm runtime.
//!
//! ## Caveats
//!
//! - Best-effort under concurrent retries: two parallel requests
//!   with the same key + body race to the table; one wins, the
//!   other re-runs the handler. Closes the duplicate-write window
//!   for sequential retries (the common case); doesn't replace a
//!   distributed lock for high-fanout concurrency.
//! - TTL pruning is the caller's responsibility — there's no
//!   background sweeper. The SDK's `migrations` runner is a fine
//!   place to drop old rows during a deploy.
//! - Response bodies above ~64 KiB inflate the per-service store
//!   noticeably; consider scoping idempotency to small-write
//!   endpoints (POST /orders, POST /charges) rather than bulk.

/// Header that carries the idempotency key. Standard across modern
/// REST APIs (Stripe, GitHub, RFC draft).
pub const HEADER: &str = "Idempotency-Key";

/// Table name for the cache. The double-underscore prefix matches
/// `__boogy_schema_version` and signals "internal SDK table —
/// don't query directly".
pub const TABLE: &str = "__boogy_idempotency";

/// Default TTL for cached responses (24 hours). Matches Stripe's
/// public default. Callers can prune sooner via direct SQL.
pub const DEFAULT_TTL_SECONDS: i64 = 24 * 60 * 60;

/// Compose the lookup key for the cache table. Composite of
/// `(idempotency_key, method, path, principal)` so a retry of the same
/// key against a different route — or by a different caller — doesn't
/// replay another request's response. `principal` is the caller's
/// resolved principal (empty string for anonymous callers, which still
/// get per-route idempotency but never collide with an authenticated
/// caller's entry).
///
/// `|` is the separator. Idempotency keys are typically opaque uuid/hex
/// strings that don't include `|`.
pub fn scope_key(idempotency_key: &str, method: &str, path: &str, principal: &str) -> String {
    format!("{idempotency_key}|{}|{path}|{principal}", method.to_uppercase())
}

/// Stable fingerprint of the request body for mismatch detection.
/// Used to catch the "client reused a key with a different body"
/// bug — the typical SDK retry path repeats the body verbatim, so
/// a fingerprint mismatch is a strong signal of caller error.
///
/// FNV-1a 64-bit. Not cryptographic — just a fast,
/// distribution-friendly hash that fits in a TEXT column. The host
/// already verifies the request body's integrity at the TLS layer;
/// this fingerprint guards against caller-side mistakes, not
/// adversarial collision attempts.
pub fn body_fingerprint(body: Option<&[u8]>) -> String {
    let bytes = body.unwrap_or(&[]);
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Hex-encode for storage in a TEXT column; 16 chars.
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_key_includes_method_path_and_principal() {
        let k = scope_key("abc", "POST", "/orders", "agent_a");
        assert_eq!(k, "abc|POST|/orders|agent_a");
        // Same key on a different route must NOT collide.
        assert_ne!(k, scope_key("abc", "POST", "/payments", "agent_a"));
    }

    #[test]
    fn scope_key_normalises_method_case() {
        assert_eq!(
            scope_key("k", "POST", "/x", "p"),
            scope_key("k", "post", "/x", "p"),
        );
    }

    #[test]
    fn scope_key_distinguishes_principals() {
        // Same idempotency key + route, different callers => different scope:
        // closes the cross-caller replay within one API.
        assert_ne!(
            scope_key("k", "POST", "/x", "agent_a"),
            scope_key("k", "POST", "/x", "agent_b"),
        );
    }

    #[test]
    fn body_fingerprint_is_deterministic() {
        let a = body_fingerprint(Some(b"hello"));
        let b = body_fingerprint(Some(b"hello"));
        assert_eq!(a, b);
    }

    #[test]
    fn body_fingerprint_distinguishes_distinct_bodies() {
        let a = body_fingerprint(Some(b"hello"));
        let b = body_fingerprint(Some(b"world"));
        assert_ne!(a, b);
    }

    #[test]
    fn body_fingerprint_handles_empty_and_none_distinctly_from_data() {
        let none = body_fingerprint(None);
        let empty = body_fingerprint(Some(&[]));
        assert_eq!(none, empty); // both fingerprint as "empty"
        let one = body_fingerprint(Some(&[0u8]));
        assert_ne!(empty, one);
    }
}
