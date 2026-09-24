//! OAuth connections the platform holds for your service — tokens your code
//! never sees.
//!
//! A connection is declared in the manifest as `[connections.<name>]` (the
//! provider's authorize/token URLs, the scopes, the client credentials by
//! secret name, and the `inject_hosts` the token may be sent to). Your service
//! then links a *subject* — an end user, or the service itself — to that
//! connection, and afterwards names it on an outbound request. The platform
//! injects a fresh access token at the wire edge, refreshing it when due, and
//! only to the declared hosts. The same rule as `[secrets]`: you reference a
//! **name**, never a value.
//!
//! Three call-side functions are emitted into your crate by
//! [`wit_glue!`](crate::wit_glue) (like `signing_*` and `secrets_*`, they need
//! the generated bindings, which only exist in your crate):
//!
//! - `connections_begin(connection, subject, return_to) -> Result<String, ConnectionError>`
//!   — start an authorization. Returns the provider URL to send the user's
//!   browser to (a 302, typically). `return_to` must be a **URL on your
//!   service's own origin** — same scheme, host and port as the origin the
//!   browser is on; after consent the platform finishes the token
//!   exchange and redirects the browser there.
//! - `connections_status(connection, subject) -> Result<ConnectionStatus, ConnectionError>`
//!   — is this subject connected, and with which scopes? No token material.
//! - `connections_revoke(connection, subject) -> Result<(), ConnectionError>`
//!   — forget the tokens (and revoke them upstream, best effort).
//!
//! Using one on an outbound call is a field on the request, not a call:
//!
//! ```ignore, ignore_snippet: an author-facing sketch — user_id and the handler's error type are not in scope in this block. Every API name in it was read against the WIT by hand: outbound-request's seven fields and connection-ref's two.
//! use bindings::boogy::platform::outbound_http::{self, ConnectionRef, OutboundRequest};
//!
//! let resp = outbound_http::fetch(&OutboundRequest {
//!     method: "GET".into(),
//!     url: "https://www.googleapis.com/youtube/v3/channels?mine=true".into(),
//!     headers: vec![],
//!     body: None,
//!     timeout_ms: Some(5000),
//!     secret_headers: vec![],
//!     // The platform injects `Authorization: Bearer <token>` at the wire edge.
//!     connection_auth: Some(ConnectionRef {
//!         connection: "google".into(),
//!         subject: user_id.clone(),
//!     }),
//! })?;
//! ```
//!
//! # `subject` decides whose account this is — derive it, never accept it
//!
//! Every call here takes a caller-chosen `subject`, and the platform takes it
//! at face value: `connections_status`, `connections_revoke` and the token
//! injected for a `connection_auth` request all key on exactly the string you
//! pass. Derive it from the authenticated principal
//! (`auth::current_principal()`), never from a path segment, a query parameter
//! or a request body. A handler that forwards request input into `subject`
//! lets any caller link, inspect, revoke — and *use* — another user's account
//! at the provider, with your service's credentials, and nothing on the
//! platform can tell that apart from the legitimate call.
//!
//! # Not inside a transaction; `begin` needs a request
//!
//! `connections_begin` and `connections_revoke` are refused while a store
//! transaction is open — the same rule as outbound HTTP and signing writes: a
//! transaction body is re-runnable and neither an authorization nor a
//! revocation can be rolled back. They surface as
//! [`ConnectionError::CapabilityDenied`]; the refusal does **not** poison the
//! transaction. `connections_status` is a read and is allowed.
//!
//! `connections_begin` is also refused from a **background job**: the URL it
//! returns is only useful to a browser.

/// A connection's usability for one subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Usable now (the platform refreshes the access token as needed).
    Connected,
    /// The stored grant no longer works — send the subject through
    /// `connections_begin` again.
    NeedsReconnect,
    /// This subject has never linked this connection.
    Absent,
}

/// What the platform will tell you about a connection. Never a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub state: ConnectionState,
    /// The scopes the provider actually granted.
    pub scopes: Vec<String>,
    pub connected_at_ms: Option<i64>,
    pub refreshed_at_ms: Option<i64>,
    /// A short failure CLASS from the last attempt (e.g. `invalid_grant`),
    /// never a provider error body.
    pub last_error: Option<String>,
}

/// Why a connection call was refused. Match the **variant**, not the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    /// No `[connections.<name>]` of that name is declared — or the service
    /// does not grant `outbound_http`, without which a connection cannot be
    /// used at all.
    UnknownConnection(String),
    /// `begin`/`revoke` inside a transaction, or `begin` from a background
    /// job. The message says which; the fix differs.
    CapabilityDenied(String),
    /// `return_to` was not a URL on this service's own origin (scheme, host
    /// and port must all match the origin the browser is on).
    BadReturnTo(String),
    /// A platform-side failure (the connections backend, or the provider's
    /// authorize endpoint). Nothing the caller can fix by changing input.
    Internal(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::UnknownConnection(s) => write!(f, "unknown connection: {s}"),
            ConnectionError::CapabilityDenied(s) => write!(f, "capability denied: {s}"),
            ConnectionError::BadReturnTo(s) => write!(f, "bad return-to: {s}"),
            ConnectionError::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for ConnectionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_error_display_is_human_readable() {
        assert_eq!(
            ConnectionError::UnknownConnection("google".into()).to_string(),
            "unknown connection: google"
        );
        assert_eq!(
            ConnectionError::BadReturnTo("must be on this service's origin".into()).to_string(),
            "bad return-to: must be on this service's origin"
        );
        assert_eq!(
            ConnectionError::CapabilityDenied("begin needs a browser".into()).to_string(),
            "capability denied: begin needs a browser"
        );
        assert_eq!(
            ConnectionError::Internal("boom".into()).to_string(),
            "internal error: boom"
        );
    }
}
