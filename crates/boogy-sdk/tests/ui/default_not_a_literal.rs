use boogy_sdk::model::Id;
use boogy_sdk::Model;

const PENDING: &str = "pending";

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[default = PENDING]
    pub status: String,
}

fn main() {}
