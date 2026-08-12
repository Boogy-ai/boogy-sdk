use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    #[default = 0]
    pub id: Id<T>,
    pub status: String,
}

fn main() {}
