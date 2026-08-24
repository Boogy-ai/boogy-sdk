// An unrecognized key in `#[counter(...)]` must be rejected, not silently
// ignored — the same reasoning `deny_marker_args` applies to the Model
// derive's bare field markers.
use boogy_sdk::model::Id;
use boogy_sdk::{Counter, Model};

#[derive(Model)]
#[model(table = "rooms", counter(name = "post_count"))]
pub struct Room {
    #[pk]
    pub id: Id<Room>,
}

#[derive(Counter)]
#[counter(of = Room, name = "post_count", index = true)]
pub struct RoomPostCount;

fn main() {}
