// `of` without `name` leaves the column unnamed — a compile error, not a
// `Counter::NAME` that silently addresses nothing (or a made-up default).
use boogy_sdk::model::Id;
use boogy_sdk::{Counter, Model};

#[derive(Model)]
#[model(table = "rooms", counter(name = "post_count"))]
pub struct Room {
    #[pk]
    pub id: Id<Room>,
}

#[derive(Counter)]
#[counter(of = Room)]
pub struct RoomPostCount;

fn main() {}
