use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    #[lookup_by]
    pub hits: i64,
}

fn main() {}
