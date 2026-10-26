use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("token signing failed: {0}")]
    Sign(String),

    #[error("token verification failed: {0}")]
    Verify(String),

    #[error("token expired")]
    Expired,

    #[error("token not yet valid")]
    NotYetValid,

    /// The token is genuine and unexpired, but the account it names ended every
    /// session issued before a later moment (a password change). Refused like an
    /// expired token.
    #[error("session ended")]
    SessionRevoked,

    /// Whether the account ended its earlier sessions could not be checked right
    /// now (a store outage). The token is NOT judged: a caller answers 503, never
    /// 401, so an outage signs nobody out.
    #[error("session state could not be checked")]
    SessionCheckUnavailable,

    #[error("token audience mismatch: expected {expected:?}, got {actual:?}")]
    AudienceMismatch { expected: String, actual: Option<String> },

    #[error("malformed token: {0}")]
    Malformed(String),

    #[error("missing required claim: {0}")]
    MissingClaim(&'static str),

    #[error("key material error: {0}")]
    Key(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type AuthResult<T> = Result<T, AuthError>;
