//! Host-mediated signing over keys your code never holds.
//!
//! A service declares the `signing` capability in its manifest, then
//! generates keys and signs through the host. The private key material
//! lives host-side and is never handed back to the component: `create_key`
//! returns only the public key, and there is deliberately NO read or export
//! operation — once a key exists, your code can use it to sign but can never
//! read it back out. The (owner, service) scope every operation runs under
//! is pinned host-side from routing, so a service can only ever touch its
//! own keys.
//!
//! There are two signing paths, picked by the key's algorithm:
//!
//! - [`signing_sign_digest`] — the ECDSA path (`secp256k1` / `P-256`).
//!   Takes a prehashed **32-byte digest**; non-32-byte input is rejected as
//!   [`SignError::BadInput`].
//! - [`signing_sign_message`] — the Ed25519 path. Takes the **full
//!   message**; the host does the hashing internally.
//!
//! The actual `bindings::boogy::platform::signing::*` calls are bridged by
//! the [`wit_glue!`](crate::wit_glue) macro — like `peer_fetch`,
//! `secrets_verify_hmac`, and `ws_publish`, the macro emits the call-side
//! free functions into the user's crate (the WIT bindings only exist
//! there). Five are emitted, all returning the types in this module:
//!
//! - `signing_create_key(label, alg) -> Result<Vec<u8>, SignError>`
//!   — generate a key; returns its public key.
//! - `signing_sign_digest(label, digest, alg) -> Result<Signature, SignError>`
//! - `signing_sign_message(label, message, alg) -> Result<Signature, SignError>`
//! - `signing_list_keys() -> Vec<KeyInfo>`
//! - `signing_remove_key(label) -> Result<(), SignError>` — idempotent.
//!
//! ```ignore
//! // Generate a secp256k1 key once (idempotent at the call site is your
//! // responsibility — re-creating under the same label is an error):
//! let public_key = signing_create_key("wallet", SigAlg::EcdsaSecp256k1)?;
//!
//! // Later, sign a 32-byte digest you hashed yourself:
//! let sig = signing_sign_digest("wallet", &digest, SigAlg::EcdsaSecp256k1)?;
//! // sig.recovery_id is Some(..) for secp256k1 — Ethereum's `v`.
//! ```
//!
//! Manifest:
//!
//! ```toml
//! [capabilities]
//! signing = true
//! ```

/// Signing algorithm. The ECDSA variants (`EcdsaSecp256k1`, `EcdsaP256`)
/// sign a prehashed digest via [`signing_sign_digest`]; `Ed25519` signs a
/// full message via [`signing_sign_message`]. Mirror of the WIT `sig-alg`
/// enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigAlg {
    /// EdDSA over Curve25519 — signs the full message.
    Ed25519,
    /// ECDSA over secp256k1 — signs a 32-byte digest. Recoverable: a
    /// produced [`Signature`] carries a `recovery_id` (Ethereum's `v`).
    EcdsaSecp256k1,
    /// ECDSA over NIST P-256 — signs a 32-byte digest. Non-recoverable.
    EcdsaP256,
}

/// A produced signature. Mirror of the WIT `signature` record. Carries no
/// secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The signature bytes (encoding is algorithm-defined).
    pub bytes: Vec<u8>,
    /// Set only for recoverable ECDSA (`secp256k1`) — this is Ethereum's
    /// `v`. Absent (`None`) for Ed25519 and non-recoverable P-256.
    pub recovery_id: Option<u8>,
}

/// Public descriptor for one signing key, as returned by
/// [`signing_list_keys`]. Mirror of the WIT `key-info` record — label, algorithm,
/// and public key only; never any secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// The label the key was created under.
    pub label: String,
    /// The key's algorithm.
    pub alg: SigAlg,
    /// The public key bytes.
    pub public_key: Vec<u8>,
}

/// Mirror of the WIT `sign-error` variant. Programmatic discrimination
/// should match on the variant; the inner `String` fields are for human
/// consumption only and never carry key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// No key bound under this (scope, label). The host collapses
    /// "doesn't exist" and "not yours" into this single variant
    /// (deny-by-existence-mask) — a caller can't probe which.
    UnknownKey(String),
    /// Caller-supplied input was rejected: a wrong digest length, or an
    /// algorithm/operation mismatch (e.g. an Ed25519 key sent to
    /// [`signing_sign_digest`], or an ECDSA key sent to
    /// [`signing_sign_message`]).
    BadInput(String),
    /// Capability not granted, backend unavailable, or an internal host
    /// error. Transient or operational, distinct from the caller-facing
    /// variants above.
    Internal(String),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignError::UnknownKey(s) => write!(f, "unknown key: {s}"),
            SignError::BadInput(s) => write!(f, "bad input: {s}"),
            SignError::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for SignError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_error_display_is_human_readable() {
        assert_eq!(
            SignError::UnknownKey("wallet".into()).to_string(),
            "unknown key: wallet"
        );
        assert_eq!(
            SignError::BadInput("digest must be 32 bytes".into()).to_string(),
            "bad input: digest must be 32 bytes"
        );
        assert_eq!(
            SignError::Internal("boom".into()).to_string(),
            "internal error: boom"
        );
    }

    #[test]
    fn signature_recovery_id_is_optional() {
        let ecdsa = Signature { bytes: vec![1, 2, 3], recovery_id: Some(0) };
        let eddsa = Signature { bytes: vec![4, 5, 6], recovery_id: None };
        assert_eq!(ecdsa.recovery_id, Some(0));
        assert_eq!(eddsa.recovery_id, None);
    }
}
