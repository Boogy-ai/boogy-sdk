use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[index]
    pub room: String,
    #[counter]
    pub hits: i64,
}

fn main() {}
