use boogy_sdk::model::{Id, Timestamp};
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
}

fn main() {}
