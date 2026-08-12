use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", index(name = "by_hits", cols = ["hits"]))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    pub hits: i64,
}

fn main() {}
