// `name` without `of` leaves the model unnamed — a compile error, not a
// `Counter::Key` that silently resolves to nothing.
use boogy_sdk::Counter;

#[derive(Counter)]
#[counter(name = "post_count")]
pub struct RoomPostCount;

fn main() {}
