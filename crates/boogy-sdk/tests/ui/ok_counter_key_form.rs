// The permitted `key = (...)` shape: a counter keyed by an arbitrary tuple,
// attached to no model at all.
use boogy_sdk::model::Counter as CounterTrait;
use boogy_sdk::{store::Val, Counter};

#[derive(Counter)]
#[counter(key = (room_id, day))]
pub struct RoomDailyPosts;

#[derive(Counter)]
#[counter(key = (ip, window), name = "rate_limit_bucket")]
pub struct RateLimit;

fn main() {
    assert_eq!(RoomDailyPosts::NAME, "room_daily_posts");
    assert_eq!(RoomDailyPosts::KEY_COLS, &["room_id", "day"]);
    let _k: <RoomDailyPosts as CounterTrait>::Key =
        [Val::Text("room-1".into()), Val::Integer(3)];

    assert_eq!(RateLimit::NAME, "rate_limit_bucket");
    assert_eq!(RateLimit::KEY_COLS, &["ip", "window"]);
}
