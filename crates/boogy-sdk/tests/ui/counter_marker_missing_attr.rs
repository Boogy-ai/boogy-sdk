// `#[derive(Counter)]` with no `#[counter(...)]` attribute at all names
// neither the model nor the column — a compile error, not a derive that
// silently emits nothing usable.
use boogy_sdk::Counter;

#[derive(Counter)]
pub struct RoomPostCount;

fn main() {}
