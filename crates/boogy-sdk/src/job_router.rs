//! Routes job invocations (by name + raw payload) to typed handlers.
//! Mirrors the HTTP `Router` shape but for `Guest::handle_job`:
//! exact-named jobs plus prefix-matched (parameterized) jobs, both
//! constructed via `#[job(...)]`-emitted sibling fns.

use std::collections::HashMap;
use std::fmt;

/// Per-invocation context the worker passes alongside the payload (the SDK
/// mirror of the WIT `job-context`). A handler can take this as an optional
/// leading `ctx: JobContext` argument — `#[job]` threads it through.
///
/// The most useful field is [`attempts`](Self::attempts): a 1-based retry
/// counter (1 on the first try, 2 on the first retry, …). Compared against the
/// handler's own known `max_attempts`, it lets a handler recognize its *final*
/// attempt and record a terminal outcome before returning
/// [`JobError::Terminal`].
#[derive(Debug, Clone)]
pub struct JobContext {
    /// Stable across retries of the same logical job. Suitable as an
    /// `Idempotency-Key` for non-idempotent upstream calls.
    pub job_id: String,
    /// The handler name from `[background_jobs.handlers.<name>]`.
    pub handler: String,
    /// 1-based attempt count: 1 first try, 2 first retry, etc.
    pub attempts: u32,
    /// Unix timestamp (seconds) of the job's `not_before`.
    pub not_before_unix_s: u64,
}

/// A job handler's failure outcome. Maps onto the WIT `handler-error` variant
/// the worker consumes:
///
/// - [`JobError::Retry`] — soft fail: the worker increments `attempts` and
///   re-queues with backoff (until `max_attempts`, then dead-letters).
/// - [`JobError::Terminal`] — hard fail: straight to dead_letter, no more
///   retries.
///
/// A handler that returns `Result<_, String>` keeps working: a bare `String`
/// error converts to [`JobError::Retry`] (the historically-documented "Err is
/// retryable" contract). Reach for [`JobError::Terminal`] when a failure can
/// never succeed on retry (bad payload, a missing parent row, retries
/// exhausted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Retry(String),
    Terminal(String),
}

impl JobError {
    /// The human-readable message, regardless of variant.
    pub fn message(&self) -> &str {
        match self {
            JobError::Retry(m) | JobError::Terminal(m) => m,
        }
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Retry(m) => write!(f, "retry: {m}"),
            JobError::Terminal(m) => write!(f, "terminal: {m}"),
        }
    }
}

impl std::error::Error for JobError {}

/// A bare-`String` handler error is treated as **retryable** — aligning with
/// the contract every handler's docs already assumed. (`From<T> for T` is
/// reflexive in core, so `JobError::from(job_error)` is the identity, which is
/// what lets the `#[job]` macro `.map_err(JobError::from)` accept both error
/// types uniformly.)
impl From<String> for JobError {
    fn from(s: String) -> Self {
        JobError::Retry(s)
    }
}

/// One registered job's metadata + dispatch closure. Constructed by the
/// `pub fn <name>() -> JobRegistration` sibling that the `#[job]`
/// attribute emits beside each user-annotated fn.
pub struct JobRegistration {
    /// Exact handler name OR the prefix to match (the suffix is then
    /// passed as the first arg to the user fn).
    pub name: &'static str,
    /// True ⇒ `name` is a prefix; false ⇒ `name` is an exact match.
    pub is_prefix: bool,
    /// Dispatch closure: receives `(ctx, suffix_opt, payload_bytes)`.
    /// `suffix_opt` is `Some(suffix)` only for prefix matches; `None`
    /// for exact matches. Returns the serialized result bytes (or
    /// `vec![]` for `Result<(), _>` jobs) on success, or a [`JobError`]
    /// that `wit_glue!`'s `handle_job` maps onto the WIT
    /// `HandlerError::{retry,terminal}`.
    pub handler: fn(&JobContext, Option<&str>, &[u8]) -> Result<Vec<u8>, JobError>,
}

/// Router of registered jobs. Build with `JobRouter::new()` then chain
/// `.exact(...)` / `.prefix(...)` calls passing the `#[job]`-emitted
/// constructor fns.
#[derive(Default)]
pub struct JobRouter {
    exact: HashMap<&'static str, JobRegistration>,
    prefixes: Vec<JobRegistration>,
}

impl JobRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an exact-name job (the fn was annotated `#[job("name")]`).
    /// Takes a constructor `fn() -> JobRegistration` so the call site
    /// reads `JobRouter::new().exact(refresh_token_balances)`.
    pub fn exact(mut self, build: fn() -> JobRegistration) -> Self {
        let reg = build();
        debug_assert!(!reg.is_prefix, "exact() received a prefix registration");
        self.exact.insert(reg.name, reg);
        self
    }

    /// Register a prefix-match job (the fn was annotated `#[job(prefix = "foo_")]`).
    pub fn prefix(mut self, build: fn() -> JobRegistration) -> Self {
        let reg = build();
        debug_assert!(reg.is_prefix, "prefix() received an exact registration");
        self.prefixes.push(reg);
        self
    }

    /// Dispatch using the [`JobContext`] (the handler name comes from
    /// `ctx.handler`). Returns the handler's output bytes (typically empty for
    /// `Result<(), _>` jobs), or a [`JobError`] for `wit_glue!`'s `handle_job`
    /// to map onto the WIT `HandlerError`. An unknown handler is `Terminal`
    /// (retrying a routing miss can never help).
    pub fn dispatch(&self, ctx: &JobContext, payload: &[u8]) -> Result<Vec<u8>, JobError> {
        let handler_name = ctx.handler.as_str();
        if let Some(reg) = self.exact.get(handler_name) {
            return (reg.handler)(ctx, None, payload);
        }
        for reg in &self.prefixes {
            if let Some(suffix) = handler_name.strip_prefix(reg.name) {
                return (reg.handler)(ctx, Some(suffix), payload);
            }
        }
        Err(JobError::Terminal(format!("unknown handler: {handler_name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `JobContext` naming the handler to dispatch to.
    fn ctx(handler: &str) -> JobContext {
        JobContext {
            job_id: "job_test".to_string(),
            handler: handler.to_string(),
            attempts: 1,
            not_before_unix_s: 0,
        }
    }

    fn ok_exact() -> JobRegistration {
        JobRegistration {
            name: "do_thing",
            is_prefix: false,
            handler: |_ctx, _suffix, _payload| Ok(b"ok".to_vec()),
        }
    }
    fn ok_prefix() -> JobRegistration {
        JobRegistration {
            name: "sweep_",
            is_prefix: true,
            handler: |_ctx, suffix, _payload| Ok(suffix.unwrap_or("").as_bytes().to_vec()),
        }
    }
    fn echo_payload() -> JobRegistration {
        JobRegistration {
            name: "echo",
            is_prefix: false,
            handler: |_ctx, _suffix, payload| Ok(payload.to_vec()),
        }
    }
    fn explode() -> JobRegistration {
        JobRegistration {
            name: "fail",
            is_prefix: false,
            handler: |_ctx, _suffix, _payload| Err(JobError::Retry("nope".into())),
        }
    }
    /// A handler that reads `ctx.attempts` to decide retry-vs-terminal.
    fn attempt_aware() -> JobRegistration {
        JobRegistration {
            name: "flaky",
            is_prefix: false,
            handler: |ctx, _suffix, _payload| {
                if ctx.attempts >= 3 {
                    Err(JobError::Terminal("exhausted".into()))
                } else {
                    Err(JobError::Retry("again".into()))
                }
            },
        }
    }

    #[test]
    fn dispatch_exact_match() {
        let r = JobRouter::new().exact(ok_exact);
        assert_eq!(r.dispatch(&ctx("do_thing"), b"").unwrap(), b"ok");
    }

    #[test]
    fn dispatch_prefix_passes_suffix() {
        let r = JobRouter::new().prefix(ok_prefix);
        assert_eq!(r.dispatch(&ctx("sweep_1m"), b"").unwrap(), b"1m");
        assert_eq!(r.dispatch(&ctx("sweep_30m"), b"").unwrap(), b"30m");
    }

    #[test]
    fn dispatch_unknown_is_terminal() {
        let r = JobRouter::new().exact(ok_exact);
        let err = r.dispatch(&ctx("missing"), b"").unwrap_err();
        assert!(matches!(err, JobError::Terminal(ref m) if m.contains("missing")));
    }

    #[test]
    fn dispatch_passes_payload_through() {
        let r = JobRouter::new().exact(echo_payload);
        assert_eq!(r.dispatch(&ctx("echo"), b"hello").unwrap(), b"hello");
    }

    #[test]
    fn dispatch_handler_error_propagates() {
        let r = JobRouter::new().exact(explode);
        assert_eq!(
            r.dispatch(&ctx("fail"), b"").unwrap_err(),
            JobError::Retry("nope".into())
        );
    }

    #[test]
    fn ctx_attempts_drive_retry_vs_terminal() {
        let r = JobRouter::new().exact(attempt_aware);
        let mut c = ctx("flaky");
        c.attempts = 1;
        assert!(matches!(r.dispatch(&c, b"").unwrap_err(), JobError::Retry(_)));
        c.attempts = 3;
        assert!(matches!(r.dispatch(&c, b"").unwrap_err(), JobError::Terminal(_)));
    }

    #[test]
    fn string_error_converts_to_retry() {
        // The `#[job]` macro maps a `Result<_, String>` handler's error via
        // `JobError::from`; assert that conversion is `Retry`.
        assert_eq!(JobError::from("boom".to_string()), JobError::Retry("boom".into()));
    }

    #[test]
    fn exact_takes_precedence_over_prefix() {
        // A registered exact name "sweep_1m" wins over a prefix "sweep_".
        fn exact_specific() -> JobRegistration {
            JobRegistration {
                name: "sweep_1m",
                is_prefix: false,
                handler: |_c, _s, _p| Ok(b"exact".to_vec()),
            }
        }
        let r = JobRouter::new().prefix(ok_prefix).exact(exact_specific);
        assert_eq!(r.dispatch(&ctx("sweep_1m"), b"").unwrap(), b"exact");
        assert_eq!(r.dispatch(&ctx("sweep_2m"), b"").unwrap(), b"2m"); // falls through to prefix
    }

    #[test]
    fn multiple_prefixes_route_independently() {
        fn other_prefix() -> JobRegistration {
            JobRegistration {
                name: "queue_",
                is_prefix: true,
                handler: |_c, s, _p| Ok(format!("q:{}", s.unwrap_or("")).into_bytes()),
            }
        }
        let r = JobRouter::new().prefix(ok_prefix).prefix(other_prefix);
        assert_eq!(r.dispatch(&ctx("sweep_1m"), b"").unwrap(), b"1m");
        assert_eq!(r.dispatch(&ctx("queue_email"), b"").unwrap(), b"q:email");
    }
}
