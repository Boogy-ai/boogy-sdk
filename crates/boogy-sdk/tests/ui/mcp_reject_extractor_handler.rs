//! An MCP mount must still REJECT an extractor-shaped handler.
//!
//! This is the generality that was removed from `Router::mcp`, and it is what
//! keeps the narrowed bound honest: without this case a fully generic bound
//! would satisfy the `mcp_bare_closure.rs` pass case just as well. MCP dispatch
//! always needs the whole request, so a handler that never receives one cannot
//! dispatch — refusing it at the mount beats a route that answers every MCP
//! call with something that is not an MCP response.

use boogy_sdk::extract::Principal;
use boogy_sdk::response;
use boogy_sdk::Router;

fn main() {
    let _ = Router::new().mcp("/mcp", |_p: Principal| response::no_content());
}
