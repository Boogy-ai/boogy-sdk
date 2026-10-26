//! Name the route a request matched, for the owner's totals per route.
//!
//! The SDK does this itself: [`Router`](crate::router::Router) names the REST
//! pattern it matched, the JSON-RPC dispatcher the method, the MCP server the
//! tool and the gRPC dispatcher the method, each before any handler runs, so a
//! request that fails still counts against its route. A handler never needs
//! to call [`report`].
//!
//! The first name reported in a request is the one kept; later calls do
//! nothing. The platform counts a name the service's served API documents do
//! not list as `other`, so a report cannot add routes the service does not
//! serve.

use std::cell::Cell;

/// What kind of route was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// A REST route: the method is the HTTP method, the route its pattern.
    Http,
    /// A JSON-RPC method.
    JsonRpc,
    /// An MCP tool.
    Mcp,
    /// A gRPC method, `package.Service/Method`.
    Grpc,
}

type Reporter = fn(RouteKind, Option<&str>, &str);

thread_local! {
    static REPORTER: Cell<Option<Reporter>> = const { Cell::new(None) };
    static REPORTED: Cell<bool> = const { Cell::new(false) };
}

/// Name the route this request matched. Only the first call of a request
/// counts; outside a request it does nothing.
pub fn report(kind: RouteKind, method: Option<&str>, route: &str) {
    if REPORTED.with(|r| r.replace(true)) {
        return;
    }
    if let Some(f) = REPORTER.with(|c| c.get()) {
        f(kind, method, route);
    }
}

/// Internal: registered by `wit_glue!`'s HTTP entry point on every request,
/// which also starts the request's one report afresh.
#[doc(hidden)]
pub fn _register_report_route(f: Reporter) {
    REPORTER.with(|c| c.set(Some(f)));
    REPORTED.with(|r| r.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static SEEN: RefCell<Vec<(RouteKind, Option<String>, String)>> = const { RefCell::new(Vec::new()) };
    }

    fn record(kind: RouteKind, method: Option<&str>, route: &str) {
        SEEN.with(|s| s.borrow_mut().push((kind, method.map(str::to_string), route.to_string())));
    }

    #[test]
    fn the_first_report_of_a_request_wins() {
        std::thread::spawn(|| {
            report(RouteKind::Http, Some("GET"), "/before-any-request");
            _register_report_route(record);
            report(RouteKind::Http, Some("GET"), "/notes/{id}");
            report(RouteKind::JsonRpc, None, "notes.list");
            _register_report_route(record);
            report(RouteKind::Mcp, None, "search");
            let seen = SEEN.with(|s| s.borrow().clone());
            assert_eq!(
                seen,
                vec![
                    (RouteKind::Http, Some("GET".into()), "/notes/{id}".into()),
                    (RouteKind::Mcp, None, "search".into()),
                ]
            );
        })
        .join()
        .unwrap();
    }
}
