//! Cross-service HTTP-style fetch.
//!
//! The host mediates each call; services only see this clean Rust
//! surface. The actual `bindings::boogy::platform::peer::fetch`
//! call is bridged by the [`wit_glue!`](crate::wit_glue) macro,
//! which emits a `peer_fetch` function at the user's crate level —
//! call sites look like:
//!
//! ```ignore
//! let resp = peer_fetch(
//!     "boogy://daniel/services/notes-api",
//!     &PeerRequest::get("/api/notes"),
//! )?;
//! if resp.is_success() {
//!     let notes: Vec<Note> = resp.json()?;
//! }
//! ```
//!
//! Capability gate: the caller's manifest must set
//! `[capabilities] peer = true`. Otherwise [`PeerError::CapabilityDenied`].
//!
//! Identity: targets see the caller as
//! `Principal::Workload { uri: "boogy://<owner>/<api>" }` in
//! their `auth` capability — no agent identity is propagated unless
//! OBO delegation is in play (lands separately).

use serde::{de::DeserializeOwned, Serialize};

/// Outbound request. Fluent builder API; common shape mirrors the
/// WIT `peer-request` record exactly.
#[derive(Debug, Clone)]
pub struct PeerRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl PeerRequest {
    pub fn new(method: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new("GET", path)
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self::new("POST", path)
    }

    pub fn put(path: impl Into<String>) -> Self {
        Self::new("PUT", path)
    }

    pub fn patch(path: impl Into<String>) -> Self {
        Self::new("PATCH", path)
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::new("DELETE", path)
    }

    /// Append a header. Doesn't deduplicate — caller is responsible
    /// for not setting `content-type` twice etc.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body_bytes(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Serialize `value` as JSON, set the body, and add a
    /// `content-type: application/json` header.
    pub fn body_json<T: Serialize>(self, value: &T) -> Result<Self, serde_json::Error> {
        let body = serde_json::to_vec(value)?;
        Ok(self
            .header("content-type", "application/json")
            .body_bytes(body))
    }
}

/// Inbound response. Same shape as the WIT `peer-response` record.
#[derive(Debug, Clone)]
pub struct PeerResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl PeerResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn body_bytes(&self) -> &[u8] {
        self.body.as_deref().unwrap_or(&[])
    }

    /// Parse the body as JSON. `Ok(None)` for empty bodies; `Err`
    /// for parse failures.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(self.body_bytes())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Mirror of the WIT `fetch-error` variant. Programmatic
/// discrimination should match on the variant; the inner `String`
/// fields are for human consumption.
#[derive(Debug, Clone)]
pub enum PeerError {
    InvalidTarget(String),
    TargetNotFound(String),
    Denied(String),
    Timeout(String),
    DepthExceeded,
    CapabilityDenied,
    Internal(String),
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerError::InvalidTarget(s) => write!(f, "invalid target: {s}"),
            PeerError::TargetNotFound(s) => write!(f, "target not found: {s}"),
            PeerError::Denied(s) => write!(f, "denied by ingress policy: {s}"),
            PeerError::Timeout(s) => write!(f, "timeout: {s}"),
            PeerError::DepthExceeded => write!(f, "recursion depth exceeded"),
            PeerError::CapabilityDenied => write!(f, "peer capability not granted"),
            PeerError::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for PeerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_chain_sets_fields() {
        let req = PeerRequest::post("/api/x")
            .header("x-trace", "abc")
            .body_bytes(b"hello".to_vec());
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/x");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.body.as_deref(), Some(b"hello".as_ref()));
    }

    #[test]
    fn body_json_sets_content_type_and_body() {
        let req = PeerRequest::post("/x").body_json(&serde_json::json!({"k": 1})).unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == "application/json"));
        assert!(!req.body.as_ref().unwrap().is_empty());
    }

    #[test]
    fn response_helpers() {
        let resp = PeerResponse {
            status: 201,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Some(br#"{"ok":true}"#.to_vec()),
        };
        assert!(resp.is_success());
        assert_eq!(resp.header("Content-Type"), Some("application/json"));
        let parsed: serde_json::Value = resp.json().unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn non_2xx_is_not_success() {
        let resp = PeerResponse { status: 503, headers: vec![], body: None };
        assert!(!resp.is_success());
    }
}
