// `of` and `key` together — `of` is sugar for keying on the model's row id,
// so combining it with an arbitrary-key `key = (...)` is ambiguous about
// which key the counter actually has. Must be a compile error naming the
// fix, not a derive that silently picks one of the two.
use boogy_sdk::model::Id;
use boogy_sdk::{Counter, Model};

#[derive(Model)]
#[model(table = "rooms", counter(name = "post_count"))]
pub struct Room {
    #[pk]
    pub id: Id<Room>,
}

#[derive(Counter)]
#[counter(of = Room, name = "post_count", key = (room_id, day))]
pub struct RoomPostCount;

fn main() {}
