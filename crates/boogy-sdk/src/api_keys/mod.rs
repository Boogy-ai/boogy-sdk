//! Per-API key management.
//!
//! Two pieces compose into a working API-key feature inside any
//! Boogy API:
//!
//! 1. **Pure logic** in this module — schema, wire types, key generation
//!    and verification, request parsing. Testable without WIT bindings.
//! 2. **WIT-bridging glue** emitted by the [`wit_glue!`](crate::wit_glue)
//!    macro — thin handlers for the management endpoints plus a guard
//!    that resolves an inbound `Authorization: Bearer sk_*` against the
//!    local store.
//!
//! Hand-written APIs typically only need to:
//!
//! ```ignore
//! // init_tables:
//! create_table_from(&boogy_sdk::api_keys::schema_table());
//!
//! // build_router — mount the standard endpoints in one call:
//! use crate::api_key_routes::ApiKeyRoutes;
//! Router::new()
//!     .with_api_key_routes()          // POST/GET /_keys, DELETE + rotate /_keys/{id}
//!     // ... your routes, gated by the key/PASETO guard:
//!     .group([api_key_routes::guard], |g| g
//!         .get("/api/things", list).post("/api/things", create))
//! // custom paths: .with_api_key_routes_at("/admin/keys"), or wire
//! // api_key_routes::{create,list,revoke,rotate} by hand.
//! ```
//!
//! ## Storage
//!
//! All keys for a given service live in `__boogy_api_keys` inside the
//! service's own per-service store (FoundationDB) — same isolation model
//! as user tables.
//! The table prefix is reserved (`__boogy_*`) so collisions are
//! impossible without explicit user intent.
//!
//! ## Security
//!
//! - **Hash**: SHA-256 hex of the full secret (Stripe / GitHub pattern).
//!   Optional pepper via the `pepper` parameter to [`prepare_create`]
//!   for defense-in-depth: a stolen DB without the pepper is unusable.
//! - **Lookup**: indexed on `prefix` (the first 11 chars of the secret),
//!   then constant-time compare of the full hash via
//!   [`boogy_auth_core::api_key::verify`].
//! - **Format**: `sk_<env>_<26 base62>_<4 hex crc32c>` — see
//!   [`boogy_auth_core::api_key`] for details.

pub mod glue;
pub mod logic;
pub mod schema;
pub mod types;

/// Internal helper used by the [`api_keys_glue!`](crate::api_keys_glue)
/// macro emission. Exposed publicly so the emitted code can call it
/// from the user's crate; not part of the SDK's external surface.
#[doc(hidden)]
pub fn __unix_now_for_glue() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub use logic::{
    compute_lookup_prefix, key_belongs_to, key_route_paths, parse_bearer, parse_row,
    prepare_create, scope_covers, unmet_scopes, verify_against_row, KeyRoutePaths,
    PreparedCreate,
};
pub use schema::{schema_table, TABLE};
pub use types::{CreateRequest, CreateResponse, KeyDto, RevokeResponse};

// Re-export the underlying primitives so users don't need to depend on
// boogy-auth directly for casual use.
pub use boogy_auth_core::api_key::{generate, hash, hash_with_pepper, parse, verify, verify_with_pepper, ApiKey, ParsedKey};
