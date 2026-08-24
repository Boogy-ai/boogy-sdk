// `#[counter]` as a struct FIELD is removed: a counter column has no field
// of its own any more. It must be a clear compile error naming the
// replacement, not a silently-accepted attribute.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t")]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    pub hits: i64,
}

fn main() {}
