use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", tagged_by(tag = "hits", refs = "other"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    pub hits: i64,
    pub other: String,
}

fn main() {}
