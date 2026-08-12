//! A bare field-level `#[unique]` must be REJECTED at compile time.
//!
//! It enforces nothing. The derive sets `ColDef.unique`, which travels to the
//! host as `ColSpec.unique` and is read by nothing on any write path — the only
//! uniqueness probe the store performs is driven by the table's INDEX list, and
//! `#[unique]` emits no index. So a duplicate insert succeeds silently.
//!
//! Rejecting is deliberate rather than quietly emitting an index: a declaration
//! that is silently discarded surfaces later as duplicate rows with nothing to
//! trace it back to. The error must name both working alternatives.

use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[unique]
    pub email: String,
}

fn main() {}
