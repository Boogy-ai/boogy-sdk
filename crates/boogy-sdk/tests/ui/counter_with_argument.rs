// `#[counter]` takes no arguments. `index = true` in particular names a
// feature that does not exist: a counter column cannot back an index at all.
// Silently discarding the argument would hand the author a clean build and a
// column that is not what they asked for.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter(index = true)]
    pub hits: i64,
}

fn main() {}
