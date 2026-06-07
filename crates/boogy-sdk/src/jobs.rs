//! Background-jobs caller API.
//!
//! The host mediates each call; APIs only see this clean Rust
//! surface. The actual `bindings::boogy::platform::background_jobs::*`
//! call is bridged by the [`wit_glue!`](crate::wit_glue) macro, which
//! emits `jobs_enqueue` / `jobs_cancel` / `jobs_status` functions at
//! the user's crate level — call sites look like:
//!
//! ```ignore
//! let job_id = jobs_enqueue(JobSpec {
//!     handler: "send_welcome_email".into(),
//!     payload: serde_json::to_vec(&payload)?.to_vec(),
//!     idempotency_key: Some(format!("welcome:{user_id}")),
//!     ..Default::default()
//! })?;
//! log::info!("enqueued job {job_id}");
//! ```
//!
//! Capability gate: the caller's manifest must set
//! `[capabilities] background_jobs = true`. Otherwise the call
//! returns [`EnqueueError::BackendUnavailable`].
//!
//! Self-targeted: the host pins the target tenant from the calling
//! workload's identity at host-call time. There's no way to enqueue
//! a job for a different `(owner, service_id)` pair. Cross-service workflows
//! go through `peer::fetch` to the receiver, which then enqueues
//! its own job.

/// What the caller hands to enqueue. The host pins owner/service_id
/// from the calling workload identity and captures the current
/// `Identity` for replay; those fields aren't on this struct.
#[derive(Debug, Clone, Default)]
pub struct JobSpec {
    /// Manifest-declared handler name. Must match a row in
    /// `job_handlers` for the calling (owner, api).
    pub handler: String,
    /// Opaque bytes the worker hands verbatim to handle-job. SDK
    /// users typically `serde_json::to_vec(&...)?`.
    pub payload: Vec<u8>,
    /// Optional delay: if set, the job stays `pending` until this
    /// unix-seconds timestamp passes. None = run as soon as
    /// available.
    pub not_before_unix_s: Option<u64>,
    /// Override the manifest's default max_attempts. None inherits.
    pub max_attempts: Option<u32>,
    /// Caller-supplied dedup key. Combined with (owner, api,
    /// handler) in the platform's `job_idempotency` table, prevents
    /// duplicate enqueues during the active+terminal window.
    /// Recommended pattern: derive from a stable identifier
    /// (e.g. `format!("welcome:{user_id}")`) so retries of the same
    /// logical operation collapse to one job.
    pub idempotency_key: Option<String>,
}

/// Per-tenant queue depth at the moment the cap was exceeded.
#[derive(Debug, Clone)]
pub struct TenantDepth {
    pub depth: u32,
    pub cap: u32,
}

/// Enqueue failures.
#[derive(Debug, Clone)]
pub enum EnqueueError {
    /// Tenant's pending+running count is at or above the depth cap.
    /// Caller decides: shed, retry later, or escalate.
    QueueFull(TenantDepth),
    /// `(owner, api, handler)` not in `job_handlers`. Caller's
    /// manifest doesn't declare this handler. Not retryable.
    InvalidHandler(String),
    /// Caller-supplied spec failed validation (payload too large,
    /// negative max-attempts, etc.). Not retryable.
    InvalidSpec(String),
    /// Capability not granted, or transient PG / control-plane
    /// failure. Caller can retry.
    BackendUnavailable,
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull(d) => write!(f, "queue full: depth {} >= cap {}", d.depth, d.cap),
            Self::InvalidHandler(h) => write!(f, "invalid handler: {h}"),
            Self::InvalidSpec(s) => write!(f, "invalid spec: {s}"),
            Self::BackendUnavailable => write!(f, "background-jobs backend unavailable"),
        }
    }
}

impl std::error::Error for EnqueueError {}

/// Result of a cancel call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Job was pending; transitioned immediately.
    Cancelled,
    /// Job was running; flag set. Worker observes on next heartbeat
    /// (≤ lease_dur/3) and traps the wasm. Caller can poll `status`
    /// to confirm.
    CancellationRequested,
    /// Job already in a terminal state. No-op.
    AlreadyTerminal,
}

/// Cancel + status failures. `NotFound` covers both "no such row"
/// and "wrong tenant" (deny-by-existence-mask).
#[derive(Debug, Clone)]
pub enum CancelError {
    NotFound,
    BackendUnavailable,
}

impl std::fmt::Display for CancelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("job not found"),
            Self::BackendUnavailable => f.write_str("background-jobs backend unavailable"),
        }
    }
}

impl std::error::Error for CancelError {}

/// Snapshot of a job's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatusInfo {
    Pending,
    Running,
    Succeeded,
    Failed(String),
    DeadLetter(String),
    Cancelled,
}

impl JobStatusInfo {
    /// True iff the job has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed(_) | Self::DeadLetter(_) | Self::Cancelled
        )
    }
}
