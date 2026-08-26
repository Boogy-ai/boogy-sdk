use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "docs", dropped("headline"))]
pub struct Doc {
    #[pk]
    pub id: Id<Doc>,
    pub headline: String,
}

fn main() {}
