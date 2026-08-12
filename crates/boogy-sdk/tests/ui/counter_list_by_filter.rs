use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", list_by(filter = "hits", newest = "created_at"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    pub hits: i64,
    #[index]
    pub created_at: i64,
}

fn main() {}
