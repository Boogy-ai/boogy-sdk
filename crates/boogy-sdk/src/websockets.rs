//! Service-published real-time channels.
//!
//! The host mediates each call; APIs only see this clean Rust surface.
//! The actual `bindings::boogy::platform::websockets::*` call is bridged
//! by the [`wit_glue!`](crate::wit_glue) macro, which emits `ws_publish`
//! / `ws_mint_subscribe_grant` functions at the user's crate level —
//! call sites look like:
//!
//! ```ignore
//! ws_publish("ticker", &serde_json::json!({"px": 42}).to_string())?;
//! let grant = ws_mint_subscribe_grant("inbox", 300)?;
//! ```
//!
//! Publish is service -> subscribers only; payloads are UTF-8 strings
//! (JSON by convention), at most 16 KiB. Subscribers connect to the
//! streaming gateway; for a private channel the service hands its user a
//! short-lived grant minted via `ws_mint_subscribe_grant`, which the
//! user presents when subscribing.
//!
//! Capability gate: the caller's manifest must set
//! `[capabilities] websockets = true` and declare each channel under
//! `[[websockets.channels]]`. Otherwise publish returns
//! [`PublishError::CapabilityDenied`] (and grant
//! [`GrantError::CapabilityDenied`]).
//!
//! Self-targeted: the host pins the target tenant from the calling
//! workload's identity at host-call time. There's no way to publish to
//! a different `(owner, service_id)` pair's channels.

use serde::Serialize;

/// The required websocket message envelope. Every payload sent over a channel
/// is `{type, v, ts, data}` — never a bare object — so one channel can carry
/// heterogeneous, independently-versioned event types that clients dispatch on
/// the `type` field.
///
/// Build one with [`Envelope::new`] and serialize to a string with
/// [`Envelope::to_json`], then pass the result to `ws_publish_to_principal` or
/// `ws_publish`. The `wit_glue!`-emitted `ws_publish_event` helper combines
/// both steps and fills `ts` from the host clock.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    /// Namespaced app event type the client switches on (e.g. `"order.status"`).
    #[serde(rename = "type")]
    pub type_: String,
    /// Per-`type` schema version. Increment when the `data` shape changes.
    pub v: u32,
    /// Publish time, milliseconds since Unix epoch.
    pub ts: u64,
    /// Type-specific body.
    pub data: serde_json::Value,
}

impl Envelope {
    /// Construct an envelope.
    pub fn new(type_: impl Into<String>, v: u32, ts: u64, data: serde_json::Value) -> Self {
        Self {
            type_: type_.into(),
            v,
            ts,
            data,
        }
    }

    /// Serialize to a JSON string. Panics only if `data` contains a
    /// non-finite float (which `serde_json::Value` cannot hold anyway).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Envelope serializes")
    }
}

/// Publish failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// Capability not granted: manifest lacks `[capabilities] websockets`.
    /// Not retryable without a manifest change.
    CapabilityDenied,
    /// Channel name not declared under `[[websockets.channels]]`. Not
    /// retryable without a manifest change.
    UnknownChannel,
    /// Payload exceeds the per-message size limit (16 KiB). Not retryable
    /// as-is; the caller must shrink the payload.
    PayloadTooLarge,
    /// Per-service publish rate exceeded. Caller can retry after a backoff.
    RateLimited,
    /// Transient backend failure (gateway/queue unavailable). The publish
    /// was not accepted; caller can retry.
    BackendUnavailable,
    /// Wrong publish function for this channel's class: broadcast `publish`
    /// is for public/private channels; the per-principal publish is for
    /// `principal` channels. Not retryable without a logic change.
    WrongClass,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapabilityDenied => f.write_str("websockets capability not granted"),
            Self::UnknownChannel => f.write_str("unknown channel"),
            Self::PayloadTooLarge => f.write_str("payload too large"),
            Self::RateLimited => f.write_str("publish rate limited"),
            Self::BackendUnavailable => f.write_str("websockets backend unavailable"),
            Self::WrongClass => f.write_str("channel is not the right class for this operation"),
        }
    }
}

impl std::error::Error for PublishError {}

/// Subscription-grant minting failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    /// Capability not granted: manifest lacks `[capabilities] websockets`.
    /// Not retryable without a manifest change.
    CapabilityDenied,
    /// Channel name not declared under `[[websockets.channels]]`. Not
    /// retryable without a manifest change.
    UnknownChannel,
    /// Channel is not private; grants only apply to private channels.
    /// Public channels need no grant. Not retryable.
    NotPrivate,
    /// Requested ttl is outside the permitted range. Not retryable as-is;
    /// the caller must supply a valid ttl.
    InvalidTtl,
    /// Per-service grant rate exceeded. Caller can retry after a backoff.
    RateLimited,
    /// Wrong grant function for this channel's class: use the broadcast grant
    /// for a `private` channel and the per-principal grant for a `principal`
    /// channel. Not retryable without a logic change.
    WrongClass,
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapabilityDenied => f.write_str("websockets capability not granted"),
            Self::UnknownChannel => f.write_str("unknown channel"),
            Self::NotPrivate => f.write_str("channel is not private"),
            Self::InvalidTtl => f.write_str("invalid grant ttl"),
            Self::RateLimited => f.write_str("grant rate limited"),
            Self::WrongClass => f.write_str("channel is not the right class for this operation"),
        }
    }
}

impl std::error::Error for GrantError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_error_display_is_neutral() {
        assert_eq!(
            PublishError::PayloadTooLarge.to_string(),
            "payload too large"
        );
        assert_eq!(PublishError::RateLimited.to_string(), "publish rate limited");
        assert_eq!(
            PublishError::WrongClass.to_string(),
            "channel is not the right class for this operation"
        );
    }

    #[test]
    fn grant_error_display_covers_variants() {
        assert_eq!(GrantError::NotPrivate.to_string(), "channel is not private");
        assert_eq!(GrantError::InvalidTtl.to_string(), "invalid grant ttl");
        assert_eq!(GrantError::RateLimited.to_string(), "grant rate limited");
        assert_eq!(
            GrantError::WrongClass.to_string(),
            "channel is not the right class for this operation"
        );
    }

    #[test]
    fn errors_are_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&PublishError::CapabilityDenied);
        assert_error(&GrantError::CapabilityDenied);
    }

    #[test]
    fn envelope_serializes_with_type_v_ts_data() {
        let env = Envelope::new("order.status", 1, 1739000000000,
            serde_json::json!({ "order_id": 42, "status": "paid" }));
        let s = serde_json::to_string(&env).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["type"], "order.status");
        assert_eq!(v["v"], 1);
        assert_eq!(v["ts"], 1739000000000u64);
        assert_eq!(v["data"]["status"], "paid");
    }
}
