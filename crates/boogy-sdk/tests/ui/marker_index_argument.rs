// `#[index]` is a bare field marker. The `cols = [...]` form is the
// STRUCT-level `#[model(index(cols = [...]))]`; writing it on a field must
// not silently do nothing.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[index(cols = ["room", "at"])]
    pub room: String,
    pub at: i64,
}

fn main() {}
