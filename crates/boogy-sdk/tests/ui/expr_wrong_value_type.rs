//! An integer column must refuse a string. Today every column is a `&'static
//! str` and this mistake reaches the store as a filter that matches nothing.
use boogy_sdk::expr::Col;

const ROOM_ID: Col<i64> = Col::new("room_id");

fn main() {
    let _ = ROOM_ID.eq("not an integer");
}
