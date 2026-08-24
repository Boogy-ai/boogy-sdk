//! A grouped query with no stated ceiling must not be able to materialize its
//! groups.
//!
//! `Query::fetch_groups` is emitted by `wit_glue!` under
//! `impl<B: BoundedGroups> Query<B>`, so the terminal exists on `Unbounded` (an
//! ungrouped aggregate is ONE group whatever the table size) and on `Bounded`
//! (a ceiling was stated), and not on `Grouped`. This file proves the half no
//! passing test can: `Grouped` does NOT satisfy the bound, so the terminal is
//! genuinely absent rather than merely discouraged.
//!
//! The quantity being bounded is not row count. `group_by(col)` returns one
//! item per DISTINCT VALUE of `col` — three for a status column, one per tenant
//! user for a user id — and nothing in the query text says which.
use boogy_sdk::query::{BoundedGroups, Grouped};

/// Stands in for `fetch_groups`'s `impl` header.
fn materialize_groups<B: BoundedGroups>() {}

fn main() {
    materialize_groups::<Grouped>();
}
