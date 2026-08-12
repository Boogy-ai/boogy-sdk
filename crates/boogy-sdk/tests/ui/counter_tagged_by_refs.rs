// tagged_by(tag, refs) resolves to a COVERING two-column index [tag, refs],
// so `refs` is an index key column just as much as `tag` is.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", tagged_by(tag = "tag", refs = "hits"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    pub tag: String,
    #[counter]
    pub hits: i64,
}

fn main() {}
