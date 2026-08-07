//! Routes job invocations (by name + raw payload) to typed handlers.
//! Mirrors the HTTP `Router` shape but for `Guest::handle_job`:
//! exact-named jobs plus prefix-matched (parameterized) jobs, both
//! constructed via `#[job(...)]`-emitted sibling fns.

use std::collections::HashMap;

/// One registered job's metadata + dispatch closure. Constructed by the
/// `pub fn <name>() -> JobRegistration` sibling that the `#[job]`
/// attribute emits beside each user-annotated fn.
pub struct JobRegistration {
    /// Exact handler name OR the prefix to match (the suffix is then
    /// passed as the first arg to the user fn).
    pub name: &'static str,
    /// True ⇒ `name` is a prefix; false ⇒ `name` is an exact match.
    pub is_prefix: bool,
    /// Dispatch closure: receives `(suffix_opt, payload_bytes)`.
    /// `suffix_opt` is `Some(suffix)` only for prefix matches; `None`
    /// for exact matches. Returns the serialized result bytes (or
    /// `vec![]` for `Result<(), String>` jobs) on success, or a String
    /// error that the wit_glue! Guest::handle_job wraps in
    /// `HandlerError::Terminal`.
    pub handler: fn(Option<&str>, &[u8]) -> Result<Vec<u8>, String>,
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

    /// Dispatch by `handler_name` (the full handler name from the WIT
    /// `JobContext`). Returns the handler's output bytes (typically
    /// empty for `Result<(), String>` jobs), or a string error to be
    /// wrapped in `HandlerError::Terminal` by the caller.
    pub fn dispatch(&self, handler_name: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        if let Some(reg) = self.exact.get(handler_name) {
            return (reg.handler)(None, payload);
        }
        for reg in &self.prefixes {
            if let Some(suffix) = handler_name.strip_prefix(reg.name) {
                return (reg.handler)(Some(suffix), payload);
            }
        }
        Err(format!("unknown handler: {handler_name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_exact() -> JobRegistration {
        JobRegistration {
            name: "do_thing",
            is_prefix: false,
            handler: |_suffix, _payload| Ok(b"ok".to_vec()),
        }
    }
    fn ok_prefix() -> JobRegistration {
        JobRegistration {
            name: "sweep_",
            is_prefix: true,
            handler: |suffix, _payload| Ok(suffix.unwrap_or("").as_bytes().to_vec()),
        }
    }
    fn echo_payload() -> JobRegistration {
        JobRegistration {
            name: "echo",
            is_prefix: false,
            handler: |_suffix, payload| Ok(payload.to_vec()),
        }
    }
    fn explode() -> JobRegistration {
        JobRegistration {
            name: "fail",
            is_prefix: false,
            handler: |_suffix, _payload| Err("nope".into()),
        }
    }

    #[test]
    fn dispatch_exact_match() {
        let r = JobRouter::new().exact(ok_exact);
        assert_eq!(r.dispatch("do_thing", b"").unwrap(), b"ok");
    }

    #[test]
    fn dispatch_prefix_passes_suffix() {
        let r = JobRouter::new().prefix(ok_prefix);
        assert_eq!(r.dispatch("sweep_1m", b"").unwrap(), b"1m");
        assert_eq!(r.dispatch("sweep_30m", b"").unwrap(), b"30m");
    }

    #[test]
    fn dispatch_unknown_returns_clear_error() {
        let r = JobRouter::new().exact(ok_exact);
        let err = r.dispatch("missing", b"").unwrap_err();
        assert!(err.contains("unknown handler"));
        assert!(err.contains("missing"));
    }

    #[test]
    fn dispatch_passes_payload_through() {
        let r = JobRouter::new().exact(echo_payload);
        assert_eq!(r.dispatch("echo", b"hello").unwrap(), b"hello");
    }

    #[test]
    fn dispatch_handler_error_propagates() {
        let r = JobRouter::new().exact(explode);
        assert_eq!(r.dispatch("fail", b"").unwrap_err(), "nope");
    }

    #[test]
    fn exact_takes_precedence_over_prefix() {
        // A registered exact name "sweep_1m" wins over a prefix "sweep_".
        fn exact_specific() -> JobRegistration {
            JobRegistration {
                name: "sweep_1m",
                is_prefix: false,
                handler: |_s, _p| Ok(b"exact".to_vec()),
            }
        }
        let r = JobRouter::new().prefix(ok_prefix).exact(exact_specific);
        assert_eq!(r.dispatch("sweep_1m", b"").unwrap(), b"exact");
        assert_eq!(r.dispatch("sweep_2m", b"").unwrap(), b"2m"); // falls through to prefix
    }

    #[test]
    fn multiple_prefixes_route_independently() {
        fn other_prefix() -> JobRegistration {
            JobRegistration {
                name: "queue_",
                is_prefix: true,
                handler: |s, _p| Ok(format!("q:{}", s.unwrap_or("")).into_bytes()),
            }
        }
        let r = JobRouter::new().prefix(ok_prefix).prefix(other_prefix);
        assert_eq!(r.dispatch("sweep_1m", b"").unwrap(), b"1m");
        assert_eq!(r.dispatch("queue_email", b"").unwrap(), b"q:email");
    }
}
