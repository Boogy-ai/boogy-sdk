//! Host-mediated secret operations that never expose the plaintext.
//!
//! A service declares secrets in its manifest `[secrets]` block with a
//! `hmac-verify` usage, binds a value at deploy time, and then asks the
//! host to verify an inbound webhook signature against it. The secret
//! value, the signed message, and the computed tag all stay host-side —
//! the component only ever receives the boolean outcome (or a typed
//! error).
//!
//! The actual `bindings::boogy::platform::secrets::verify-hmac` call is
//! bridged by the [`wit_glue!`](crate::wit_glue) macro — like `peer_fetch`
//! and `ws_publish`, the macro emits the call-side free functions into the
//! user's crate (the WIT bindings only exist there). Two are emitted:
//!
//! - `secrets_verify_hmac(secret_ref, algorithm, message, expected_hex)`
//!   — full form, takes a [`HmacAlgorithm`].
//! - `secrets_verify_hmac_sha256(secret_ref, message, expected_hex)`
//!   — SHA-256 convenience (the webhook common case).
//!
//! Both return `Result<bool, VerifyError>` using the types in this module.
//!
//! ```ignore
//! // Inside a webhook handler, after reconstructing the signed payload
//! // exactly as the provider signed it:
//! let ok = secrets_verify_hmac_sha256(
//!     "stripe_webhook_secret",
//!     &signed_message,
//!     &expected_hex,
//! )?;
//! if !ok {
//!     return Err(ApiError::unauthorized("bad signature"));
//! }
//! ```
//!
//! Manifest:
//!
//! ```toml
//! [secrets]
//! stripe_webhook_secret = { usage = ["hmac-verify"] }
//! ```
//!
//! Unlike `peer` / `outbound_http`, there is NO `[capabilities]` flag for
//! `secrets`: the gate is the per-secret `usage` declaration above. A
//! reference to a name not declared with `hmac-verify` (or with no value
//! bound) fails closed as [`VerifyError::UnknownSecret`].

/// Mirror of the WIT `verify-error` variant. Programmatic discrimination
/// should match on the variant; the inner `String` fields are for human
/// consumption only and never carry secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The secret ref isn't declared in `[secrets]`, or is declared
    /// without the `hmac-verify` usage, or has no value bound for this
    /// deployment. The host deliberately collapses all three into one
    /// variant (deny-by-existence-mask) — a caller can't probe which.
    UnknownSecret(String),
    /// Capability/host-side condition: no secret backend configured for
    /// this deployment, a KMS/storage failure, etc. Transient or
    /// operational, distinct from `UnknownSecret`.
    Internal(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::UnknownSecret(s) => write!(f, "unknown secret: {s}"),
            VerifyError::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// HMAC algorithm to verify with. SHA-256 is the only variant today
/// (matching the WIT `hmac-algorithm` enum); [`verify_hmac`] defaults to
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HmacAlgorithm {
    #[default]
    Sha256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_error_display_is_human_readable() {
        assert_eq!(
            VerifyError::UnknownSecret("x".into()).to_string(),
            "unknown secret: x"
        );
        assert_eq!(
            VerifyError::Internal("boom".into()).to_string(),
            "internal error: boom"
        );
    }

    #[test]
    fn hmac_algorithm_defaults_to_sha256() {
        assert_eq!(HmacAlgorithm::default(), HmacAlgorithm::Sha256);
    }
}
