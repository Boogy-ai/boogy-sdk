//! An unbounded query must not be able to materialize rows.
//!
//! `Query::fetch_all` / `fetch_all_with_total` are emitted by `wit_glue!` under
//! `impl<B: BoundedRead> Query<B>`, so the terminal exists only once `.limit(n)`
//! has moved the builder into the `Bounded` state. This file proves the half of
//! that which no passing test can: that `Unbounded` does NOT satisfy the bound,
//! so the terminal is genuinely absent rather than merely discouraged.
use boogy_sdk::query::{BoundedRead, Unbounded};

/// Stands in for `fetch_all`'s `impl` header.
fn materialize_rows<B: BoundedRead>() {}

fn main() {
    materialize_rows::<Unbounded>();
}
