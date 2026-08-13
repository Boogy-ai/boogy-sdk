use boogy_sdk::model::{Decimal, Id};
use boogy_sdk::Model;

// `Decimal` is exact to 6 decimal places. A 7th fractional digit is a
// compile error (refused, not silently rounded away) — pins the
// compile-time exact parser's own error path, distinct from the generic
// literal-kind mismatch other `default_wrong_literal_kind*` fixtures cover.
#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[default = "1.1234567"]
    pub weight: Decimal,
}

fn main() {}
