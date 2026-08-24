// The permitted shape: `#[derive(Counter)]` with both `of` and `name` set,
// naming a real struct-level counter column on a real model.
use boogy_sdk::model::{Counter as CounterTrait, Id};
use boogy_sdk::{Counter, Model};

#[derive(Model)]
#[model(table = "rooms", counter(name = "post_count"))]
pub struct Room {
    #[pk]
    pub id: Id<Room>,
}

#[derive(Counter)]
#[counter(of = Room, name = "post_count")]
pub struct RoomPostCount;

fn main() {
    assert_eq!(RoomPostCount::NAME, "rooms.post_count");
    let _k: <RoomPostCount as CounterTrait>::Key = Id::<Room>::new(1);
}
