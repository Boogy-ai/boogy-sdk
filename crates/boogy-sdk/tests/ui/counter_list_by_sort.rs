use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", list_by(filter = "room", newest = "hits"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[index]
    pub room: String,
    #[counter]
    pub hits: i64,
}

fn main() {}
