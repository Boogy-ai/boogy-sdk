use boogy_sdk::model::{Decimal, Id, Timestamp};
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[default = "pending"]
    pub status: String,
    #[default = 0]
    pub retries: i64,
    #[default(-1)]
    pub offset: i64,
    #[default = 1.5]
    pub weight: f64,
    #[default = true]
    pub active: bool,
    #[default = 0]
    pub created_at: Timestamp,
    #[default = "none"]
    pub note: Option<String>,
    // `Decimal` still takes a string default, but it is now parsed
    // EXACTLY at compile time into scaled minor units, never through a
    // float.
    #[default = "19.990000"]
    pub score: Decimal,
}

fn main() {}
