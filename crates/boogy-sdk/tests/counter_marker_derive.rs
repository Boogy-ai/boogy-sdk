//! `#[derive(Counter)]` with `of = <Model>` — compile-time argument checking.
//!
//! Mirrors `counter_derive.rs`'s convention for the `#[model(counter(...))]`
//! struct-level marker: a malformed argument must be a compile error naming
//! the fix, not a silently discarded attribute (`attr.path().is_ident("counter")`
//! retired-spelling: the bare field form is retired (use
//! `#[model(counter(name = ..))]`); the parenthesised form on a
//! `#[derive(Counter)]` marker is live, and this test is about telling
//! them apart.
//! matches `#[counter]`, `#[counter(..)]` and `#[counter = ..]` alike, so
//! nothing else would catch it).

#[test]
fn a_bare_derive_counter_with_no_attribute_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_marker_missing_attr.rs");
}

#[test]
fn counter_of_without_name_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_marker_missing_name.rs");
}

#[test]
fn counter_name_without_of_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_marker_missing_of.rs");
}

#[test]
fn an_unrecognized_counter_marker_key_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_marker_unknown_key.rs");
}

/// `of` and `key` are mutually exclusive — `of` is sugar for keying on the
/// model's row id, so combining them with an arbitrary-key `key = (...)`
/// must be a compile error naming the fix, not a derive that silently
/// picks one.
#[test]
fn counter_of_and_key_together_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_marker_of_and_key_together.rs");
}

/// The permitted shapes, so the compile-fail cases above are not passing by
/// virtue of a derive that rejects everything.
#[test]
fn the_well_formed_counter_marker_compiles() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/ok_counter_marker.rs");
}

/// The `key = (...)` shape — a counter attached to no model at all — is the
/// other permitted form, not merely a variant of the `of` one.
#[test]
fn the_well_formed_arbitrary_key_counter_marker_compiles() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/ok_counter_key_form.rs");
}
