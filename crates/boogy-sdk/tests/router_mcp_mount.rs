//! `Router::mcp` inference: the un-annotated closure must compile, and the
//! bound must still be tight enough to reject a handler that is not an MCP
//! dispatch handler.
//!
//! The property under test is INFERENCE, which is a compile-time property, so
//! these are compile tests rather than runtime ones.

/// The natural call — `.mcp("/mcp", |req| { … })` — must compile.
///
/// It did not: `mcp<H, Args> where H: IntoHandler<Args>` leaves the closure's
/// parameter type unconstrained (the `Args` marker could be `RawReq` or any
/// extractor tuple), so rustc cannot infer it and fails with E0282. The
/// spelling that does not compile was written independently in five places,
/// two of them the SDK's own documentation — a signature the SDK's own
/// reference page gets wrong is a signature problem, not an author problem.
#[test]
fn a_bare_closure_infers_at_an_mcp_mount() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/mcp_bare_closure.rs");
}

/// The narrowed bound must not be so loose that it accepts anything.
///
/// Without this case, replacing the bound with a fully generic one would also
/// satisfy the pass case above. `mcp_extractor_handler.rs` passes an
/// extractor-shaped handler (the generality that was removed) and
/// `mcp_wrong_return.rs` passes a handler returning a type that is not a
/// response — both must be refused at the mount.
#[test]
fn an_mcp_mount_rejects_a_non_dispatch_handler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/mcp_reject_*.rs");
}
