// The rename bypass. `ranked_by` names the AUTHOR-written field name while
// #[model(column = ...)] renames the stored column, so the counter check —
// which compares against the stored name — never matches.
//
// Before the column-resolution check this compiled clean AND produced an index
// on a column that does not exist. Two bugs at once: a phantom index, and a
// counter check silently bypassed.
use boogy_sdk::model::Id;
use boogy_sdk::Model;

#[derive(Model)]
#[model(table = "t", ranked_by(highest = "hits"))]
pub struct T {
    #[pk]
    pub id: Id<T>,
    #[counter]
    #[model(column = "hit_count")]
    pub hits: i64,
}

fn main() {}
