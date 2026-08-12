//! The natural spelling of an MCP mount — a closure whose parameter is NOT
//! annotated — must compile.
//!
//! `Router::mcp` is the one mount point where the handler always needs the raw
//! request (MCP dispatch hands `req.request` to `McpServer::handle`), so the
//! extractor-shaped generality of `IntoHandler<Args>` is never used there while
//! costing type inference at every call site: with an open `Args` marker the
//! closure's parameter type is unconstrained and rustc cannot infer it (E0282).

use boogy_sdk::mcp::McpServer;
use boogy_sdk::Router;

fn main() {
    let _ = Router::new().mcp("/mcp", |req| {
        McpServer::new("t", "1.0").handle(req.request)
    });
}
