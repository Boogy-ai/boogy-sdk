use boogy_sdk::model::{Decimal, Id};
use boogy_sdk::Model;

// `Decimal` stores as scaled `i64` minor units (an Integer column), but its
// `#[default]` is still a STRING literal — parsed EXACTLY at compile time
// into minor units, never through a float. A float literal is the wrong
// kind here.
#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[default = 1.5]
    pub weight: Decimal,
}

fn main() {}
