//! Wire types for the API-key management endpoints.
//!
//! Kept separate from logic so they can be re-used by alternative
//! transports (RPC, gRPC) and tested in isolation.

use serde::{Deserialize, Serialize};

/// Request body for `POST /_keys`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateRequest {
    /// Operator-supplied label. Free text, ≤128 chars.
    pub name: String,
    /// `live`, `test`, etc. ASCII alphanumeric, ≤16 chars. Becomes the
    /// `<env>` segment of the issued secret.
    pub env: String,
    /// Scopes granted to this key. Empty = no special scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional Unix-seconds expiration. `None` = never expires.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

/// Response from `POST /_keys`. The `secret` field is shown **once**;
/// the operator must capture it now or rotate the key.
#[derive(Debug, Clone, Serialize)]
pub struct CreateResponse {
    pub id: String,
    pub prefix: String,
    pub secret: String,
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub created_at: u64,
}

/// Persisted view of a key, returned from `GET /_keys` and after
/// rotation. Never includes the secret material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyDto {
    pub id: String,
    pub prefix: String,
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub revoked: bool,
}

/// Response from `DELETE /_keys/{id}`.
#[derive(Debug, Clone, Serialize)]
pub struct RevokeResponse {
    pub id: String,
    pub revoked: bool,
}
