//! retired-spelling: the field form was retired 2026-08-19; the replacement
//! is `#[model(counter(name = ..))]` on the struct plus a companion
//! `#[derive(Counter)]` marker type. This suite exists to prove the derive
//! says so rather than accepting it silently.
//! `#[counter]` is not supported as a struct FIELD, and the derive must say
//! so at COMPILE time, naming the replacement.
//!
//! A counter column has no field of its own any more — it is declared on the
//! STRUCT (`#[model(counter(name = "<column>"))]`) and read through a
//! companion `#[derive(Counter)]` marker type. A runtime reinterpretation, or
//! silently accepting and ignoring the field, would leave a model that looks
//! like it declared a counter and does not. Refusing to build is strictly
//! better.

#[test]
fn counter_field_is_rejected_naming_the_replacement() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/counter_*.rs");
}

/// A bare field marker that takes no arguments must REJECT arguments rather
/// than discard them.
///
/// `attr.path().is_ident("x")` matches `#[x]`, `#[x(..)]` and `#[x = ..]`
/// alike. Without an explicit check, an argument is silently dropped: the
/// author writes `#[pk(auto)]` or a field-level `#[index(cols = [..])]`,
/// gets a clean build, and believes they asked for something the derive
/// never saw. Silent misconfiguration of a storage attribute is exactly the
/// class of bug that surfaces as wrong data later.
#[test]
fn bare_field_markers_reject_arguments() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/marker_*.rs");
}

/// A field-level `#[unique]` must be rejected, not silently accepted.
///
/// The attribute compiled and set `ColDef.unique`, which reached the host as
/// `ColSpec.unique` — a flag no write path reads. The store's uniqueness probe
/// is driven entirely by the table's INDEX list, and `#[unique]` emitted no
/// index, so duplicates were accepted silently by a model that had declared
/// they could not be. Nothing observable distinguished it from not writing the
/// attribute at all.
///
/// Refusing to build follows the same reasoning as `deny_marker_args` and the
/// retired-spelling: the field form is retired; `#[model(counter(name =
/// ..))]` is the replacement this rejection names.
/// unconditional `#[counter]`-as-a-field rejection: a storage declaration that
/// is silently discarded surfaces later as wrong data with nothing to trace it
/// back to. Quietly emitting an index instead would change the storage layout
/// of every existing model for a guarantee nobody can currently rely on, and
/// would be ineffective anyway on a table that already holds duplicates.
#[test]
fn a_unique_field_marker_is_rejected() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/unique_*.rs");
}

/// `#[default]` is the one field marker that TAKES a value, so it is checked
/// from the opposite side of `deny_marker_args` — and for the same reason.
///
/// `attr.path().is_ident("default")` matches `#[default]`, `#[default(..)]` and
/// `#[default = ..]` alike. Without an explicit rejection, the two malformed
/// spellings would be silently discarded: the author declares a column default,
/// the build is clean, and the column reads back null forever with nothing to
/// trace it to. The same applies to a literal whose kind does not match the
/// field's column type, and to `#[default]` on `#[pk]`, where the value could
/// never be observed. (`#[default]` on a counter is covered by the
/// unconditional field rejection above — a counter column has no field left
/// to combine it with.)
#[test]
fn a_malformed_or_unusable_default_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/default_*.rs");
}

/// The permitted shapes, one per supported literal kind.
///
/// Without this, a derive that rejected EVERY `#[default]` would pass the
/// compile-fail suite above while making the attribute unusable.
#[test]
fn well_formed_defaults_compile() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/ok_default.rs");
}

/// The permitted shape: a struct-level counter, with no backing field.
///
/// retired-spelling: only the FIELD form is retired; `#[counter(...)]` on
/// a `#[derive(Counter)]` marker type is live.
/// Without this, a derive that rejected EVERY `#[counter]` — field or
/// struct-level alike — would pass the compile-fail suite above while
/// making the feature unusable in its only remaining form. This is the
/// CONTROL for the field-form rejection: the new shape compiles clean.
#[test]
fn a_struct_level_counter_compiles() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/ok_counter.rs");
}

// ---------------------------------------------------------------------------
// The write-side half: a counter column must not be in `to_columns()`
// ---------------------------------------------------------------------------

use boogy_sdk::model::{Id, Model};
use boogy_sdk::store::Val;
use boogy_sdk::Model as ModelDerive;

#[derive(ModelDerive)]
#[model(table = "posts", counter(name = "vote_score"))]
struct Post {
    #[pk]
    id: Id<Post>,
    title: String,
}

/// `to_columns()` feeds BOTH `db_insert` and `db_update`, so a counter column
/// left in it would make `db_update(id, &Post { title, ..post })` write the
/// counter value the author read earlier — discarding every atomic add since.
///
/// A struct-level counter has no field to accidentally reintroduce (there is
/// nothing on `Post` a caller could even write `post.vote_score = 42` to), so
/// this is really pinning `struct_counter_pushes` against `to_col_pushes`:
/// the counter column must land in the SCHEMA (via the former) and never in
/// `to_columns()`'s output (the latter never walks it at all). Asserted
/// structurally (the column is ABSENT), not by value — emitting the column
/// with a placeholder value would still overwrite the cell.
#[test]
fn to_columns_omits_a_struct_level_counter() {
    let post = Post {
        id: Id::new(7),
        title: "hello".into(),
    };
    let cols = post.to_columns();
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();

    assert!(
        !names.contains(&"vote_score"),
        "a counter column must never appear in to_columns(), got {names:?}"
    );
    // The ordinary column is still there — a derive that dropped every field
    // would also pass the assertion above.
    assert!(
        cols.iter()
            .any(|(n, v)| n == "title" && matches!(v, Val::Text(t) if t == "hello")),
        "the non-counter columns must still be written, got {cols:?}"
    );
    assert_eq!(names.len(), 1, "only `title` is writable, got {names:?}");

    // …and the column still EXISTS in the schema, flagged — `to_columns` omitting
    // it is about the write path, not about the column being absent from the table.
    let schema = Post::schema();
    let c = schema
        .columns
        .iter()
        .find(|c| c.name == "vote_score")
        .expect("the counter column must still be declared in the schema");
    assert!(c.counter, "and must still be flagged as a counter column");
}
