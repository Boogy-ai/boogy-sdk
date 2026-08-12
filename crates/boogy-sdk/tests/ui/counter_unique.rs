//! `#[counter]` + `#[unique]` must not compile.
//!
//! The diagnostic is now the `#[unique]` one, not the counter one: `#[unique]`
//! is rejected outright at the attribute-parsing site, before the
//! counter-cannot-back-an-index check runs. The dedicated arm that named
//! `#[unique]` in that check has been removed — it rested on the premise that
//! `#[unique]` backed an index, which was never true, and it could no longer
//! fire.
//!
//! The fixture stays so the combination remains covered: whichever rule catches
//! it, this shape must never build.

use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    #[unique]
    pub hits: i64,
}

fn main() {}
