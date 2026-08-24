// The permitted shape, now that a counter has no field of its own: a
// struct-level `#[model(counter(name = "..."))]` declaration alongside an
// ordinary indexed column, with no collision between the two.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", counter(name = "hits"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[index]
    pub room: String,
}

fn main() {}
