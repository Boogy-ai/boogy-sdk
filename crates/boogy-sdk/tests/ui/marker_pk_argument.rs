use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk(auto)]
    pub id: Id<T>,
    pub room: String,
}

fn main() {}
