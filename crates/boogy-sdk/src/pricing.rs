//! Report usage for a route priced by rate.
//!
//! When a route is priced per unit of something only your code can count — for
//! example tokens in a completion — call [`report_units`] with the quantity used.
//! The platform computes the charge from the published rate and caps it at the
//! route's declared maximum. Nothing here can read or change a price, a balance,
//! or who pays.
//!
//! ```ignore, ignore_snippet: needs a deployed route priced by rate to return Ok
//! boogy_sdk::pricing::report_units("tokens", completion_tokens)?;
//! ```

use std::cell::Cell;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// This request did not match a route priced by a rate you report.
    NotPriced,
    /// The matched route does not rate this unit.
    UnknownUnit(String),
    /// Called outside a request (no host function is registered).
    Unavailable,
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportError::NotPriced => write!(f, "this request is not priced by a reported rate"),
            ReportError::UnknownUnit(u) => write!(f, "the matched route does not rate unit `{u}`"),
            ReportError::Unavailable => write!(f, "report_units called outside a request"),
        }
    }
}

impl std::error::Error for ReportError {}

thread_local! {
    static REPORT_UNITS: Cell<Option<fn(&str, u64) -> Result<(), ReportError>>> =
        const { Cell::new(None) };
}

/// Report `quantity` of `unit` for the current request. Repeated calls add up.
pub fn report_units(unit: &str, quantity: u64) -> Result<(), ReportError> {
    match REPORT_UNITS.with(|c| c.get()) {
        Some(f) => f(unit, quantity),
        None => Err(ReportError::Unavailable),
    }
}

/// Internal: registered by the `wit_glue!` entry points on every invocation.
#[doc(hidden)]
pub fn _register_report_units(f: fn(&str, u64) -> Result<(), ReportError>) {
    REPORT_UNITS.with(|c| c.set(Some(f)));
}

/// Internal: the WIT-facing half of `wit_glue!`'s `__sdk_report_units`.
///
/// `wit_glue!` expands separately in every consumer crate, against that
/// crate's own generated `pricing_bindings::ReportError` — a type this crate
/// cannot name. So the macro classifies its own generated error with a
/// closure, and this function does everything else: propagate `Ok`
/// untouched, and turn a classified `Err` into the SDK's [`ReportError`]
/// without ever discarding it. Splitting the propagation out of the macro
/// body means the one line the macro still owns (a single call into this
/// function) has nothing left to get wrong that a unit test here cannot
/// already cover.
#[doc(hidden)]
pub fn _bridge_report_result<E>(result: Result<(), E>, classify: impl FnOnce(E) -> ReportError) -> Result<(), ReportError> {
    result.map_err(classify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_reports_unavailable() {
        std::thread::spawn(|| {
            assert_eq!(report_units("tokens", 1), Err(ReportError::Unavailable));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_registered_host_function_receives_the_report() {
        fn host(unit: &str, qty: u64) -> Result<(), ReportError> {
            if unit == "tokens" && qty == 42 { Ok(()) } else { Err(ReportError::UnknownUnit(unit.into())) }
        }
        std::thread::spawn(|| {
            _register_report_units(host);
            assert_eq!(report_units("tokens", 42), Ok(()));
            assert_eq!(report_units("cents", 42), Err(ReportError::UnknownUnit("cents".into())));
        })
        .join()
        .unwrap();
    }

    // `_bridge_report_result` is the whole of the SDK-side glue seam:
    // `wit_glue!`'s macro-emitted `__sdk_report_units` is a single call into
    // it, with nothing left in the macro body for a "discard the verdict"
    // mutation to hide in. These two tests are that seam's real coverage.
    #[test]
    fn bridge_propagates_ok_untouched() {
        let out = _bridge_report_result::<()>(Ok(()), |_| unreachable!("classify must not run on Ok"));
        assert_eq!(out, Ok(()));
    }

    #[test]
    fn bridge_propagates_a_classified_error_not_a_swallowed_ok() {
        let out = _bridge_report_result(Err(()), |_| ReportError::UnknownUnit("cents".into()));
        assert_eq!(out, Err(ReportError::UnknownUnit("cents".into())));
    }
}
