//! Request-correlated logging for wasm handlers.
//!
//! Two pieces of state, both wasm-thread-local:
//!
//! 1. The **request id** (set by [`wit_glue!`](crate::wit_glue) on
//!    every request, cleared on return) — pulled from the inbound
//!    `x-boogy-request-id` header that the host plumbs through
//!    `dispatch_request`.
//! 2. A **runtime-log function pointer** — registered once on first
//!    request entry by [`wit_glue!`]. Lets the SDK stay agnostic to
//!    which user-side `bindings` module owns the WIT runtime
//!    capability while still funnelling logs through it.
//!
//! Handlers call [`info!`], [`warn!`], [`error!`], [`debug!`], or
//! [`trace!`]. Each macro formats its arguments with `format!`, the
//! dispatcher prepends `[req=<id>]` when an id is in scope, and the
//! result lands in the host's log via the runtime cap. No request id
//! ⇒ no prefix (the macro stays usable in init paths).
//!
//! Wasm components are single-threaded per instance, so plain
//! `thread_local!` is the simplest correct primitive — `OnceCell` /
//! `RefCell` here never see contention.

use std::cell::{Cell, RefCell};

thread_local! {
    /// Inbound request id for the in-flight handler invocation.
    /// Set by the [`wit_glue!`](crate::wit_glue)-emitted `Guest::handle`
    /// before dispatch; cleared before return so a subsequent call
    /// without an id doesn't inherit the previous one.
    static REQUEST_ID: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Function pointer to the user-crate-local
    /// `bindings::boogy::platform::runtime::log`. Set lazily
    /// from inside the [`wit_glue!`] entry point because the
    /// bindings module is private to the user's crate and not
    /// reachable from the SDK by name.
    ///
    /// `Cell<Option<...>>` (vs `OnceCell`) because we want
    /// idempotent overwrite cheap — `Guest::handle` re-installs on
    /// every invocation rather than gating on a "set once" check.
    static RUNTIME_LOG: Cell<Option<fn(&str, &str)>> = const { Cell::new(None) };
}

/// Internal: set by the [`wit_glue!`](crate::wit_glue)-emitted
/// `Guest::handle` from the request's `x-boogy-request-id`
/// header. User code shouldn't call this — the request id is owned
/// by the dispatch entry point.
#[doc(hidden)]
pub fn _set_request_id(id: Option<String>) {
    REQUEST_ID.with(|r| *r.borrow_mut() = id);
}

/// Internal: register the user-crate-local runtime log function.
/// [`wit_glue!`](crate::wit_glue) calls this on every request entry
/// to keep the pointer fresh; the cost is one Cell write.
#[doc(hidden)]
pub fn _register_runtime_log(f: fn(&str, &str)) {
    RUNTIME_LOG.with(|c| c.set(Some(f)));
}

/// Current request id, if a handler is in flight and the inbound
/// HTTP request carried (or the host minted) one. `None` outside a
/// request context (e.g. inside `init_tables` on first call), or
/// when the request lacked the header.
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.with(|r| r.borrow().clone())
}

/// Internal dispatch path the [`info!`] / [`warn!`] / etc. macros
/// emit calls to. Auto-prepends `[req=<id>] ` when an id is in
/// scope; falls through to a no-op when no runtime log function has
/// been registered yet (e.g. unit-test paths that don't go through
/// [`wit_glue!`]).
#[doc(hidden)]
pub fn _dispatch(level: &str, msg: &str) {
    let line = match current_request_id() {
        Some(id) => format!("[req={id}] {msg}"),
        None => msg.to_string(),
    };
    if let Some(f) = RUNTIME_LOG.with(|c| c.get()) {
        f(level, &line);
    }
}

/// Emit an `info`-level log line. Auto-includes the inbound
/// request id when one is in scope.
#[macro_export]
macro_rules! sdk_log_info {
    ($($arg:tt)*) => {
        $crate::log::_dispatch("info", &::std::format!($($arg)*))
    };
}

/// Emit a `warn`-level log line.
#[macro_export]
macro_rules! sdk_log_warn {
    ($($arg:tt)*) => {
        $crate::log::_dispatch("warn", &::std::format!($($arg)*))
    };
}

/// Emit an `error`-level log line.
#[macro_export]
macro_rules! sdk_log_error {
    ($($arg:tt)*) => {
        $crate::log::_dispatch("error", &::std::format!($($arg)*))
    };
}

/// Emit a `debug`-level log line.
#[macro_export]
macro_rules! sdk_log_debug {
    ($($arg:tt)*) => {
        $crate::log::_dispatch("debug", &::std::format!($($arg)*))
    };
}

/// Emit a `trace`-level log line.
#[macro_export]
macro_rules! sdk_log_trace {
    ($($arg:tt)*) => {
        $crate::log::_dispatch("trace", &::std::format!($($arg)*))
    };
}

// User-facing aliases under the `log::` namespace, matching the
// `log` crate's naming so handlers can write `log::info!(...)`,
// `log::error!(...)`, etc. The `#[macro_export]` above already
// places the underlying macros at crate root; these `pub use`
// re-exports rename them for the public surface.
pub use crate::sdk_log_debug as debug;
pub use crate::sdk_log_error as error;
pub use crate::sdk_log_info as info;
pub use crate::sdk_log_trace as trace;
pub use crate::sdk_log_warn as warn;

#[cfg(test)]
mod tests {
    use super::*;

    /// Prove the formatting branches independently of the runtime
    /// log function (which only resolves inside a wasm crate). We
    /// register a capturing thunk that pushes lines into a
    /// thread-local Vec, then assert.
    #[test]
    fn dispatch_prepends_request_id_when_set() {
        thread_local! {
            static CAPTURED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
        }
        fn capture(level: &str, msg: &str) {
            CAPTURED.with(|c| c.borrow_mut().push((level.to_string(), msg.to_string())));
        }
        _register_runtime_log(capture);

        _set_request_id(Some("req-abc".to_string()));
        _dispatch("info", "hello");
        _dispatch("error", "boom");
        _set_request_id(None);
        _dispatch("info", "after-clear");

        let captured: Vec<(String, String)> =
            CAPTURED.with(|c| c.borrow().clone());
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0], ("info".into(), "[req=req-abc] hello".into()));
        assert_eq!(captured[1], ("error".into(), "[req=req-abc] boom".into()));
        // After clear, no prefix.
        assert_eq!(captured[2], ("info".into(), "after-clear".into()));
    }

    #[test]
    fn dispatch_is_noop_without_runtime_log_registered() {
        // Brand-new thread (so RUNTIME_LOG starts None) — Rust runs
        // each test on its own thread by default.
        // No registration call. _dispatch must NOT panic.
        _dispatch("info", "should not crash");
    }
}
