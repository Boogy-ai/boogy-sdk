//! The type safety `expr.rs` claims, proven rather than asserted in a comment.
//!
//! Its unit tests can show that `Col<i64>.eq(5)` builds the right expression;
//! they cannot show that `Col<i64>.eq("five")` FAILS to build, because a test
//! that does not compile is not a test. These are compile-fail cases, so the
//! guarantee has a way of being wrong.

#[test]
fn a_column_refuses_a_value_of_the_wrong_type() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/expr_wrong_value_type.rs");
}

#[test]
fn a_non_nullable_column_has_no_null_check() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/expr_is_null_on_non_nullable.rs");
}
