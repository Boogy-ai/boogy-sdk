use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", ranked_by(highest = "hits"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    pub hits: i64,
}

fn main() {}
