//! `is_null` is offered only where the column can hold one. Asking a NOT NULL
//! column whether it is null is a question with a constant answer, and the
//! schema already knows it.
use boogy_sdk::expr::Col;

const ROOM_ID: Col<i64> = Col::new("room_id");

fn main() {
    let _ = ROOM_ID.is_null();
}
