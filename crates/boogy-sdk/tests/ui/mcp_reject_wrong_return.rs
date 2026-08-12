//! An MCP mount must still REJECT a handler that returns something which is
//! not a response.
//!
//! The narrowing kept the return-type menu open (`R: IntoResponse`, so
//! `Result<_, ApiError>` and `?` still flow through). "Open" must not mean
//! "anything": a handler whose return type cannot become an `HttpResponse` has
//! no wire behaviour to give the caller, so it is refused where it is mounted
//! rather than at some later point where the route is already registered.

use boogy_sdk::router::Req;
use boogy_sdk::Router;

struct NotAResponse;

fn main() {
    let _ = Router::new().mcp("/mcp", |_req: &mut Req<'_>| NotAResponse);
}
