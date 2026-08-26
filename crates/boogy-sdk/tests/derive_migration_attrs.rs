//! Derive-layer coverage for `#[renamed_from = "old"]` (field) and
//! `#[model(dropped(...))]` (struct) — the two column-migration attributes
//! from `docs/superpowers/specs/2026-08-25-column-migration`.
//!
//! Different homes, on purpose: `renamed_from` sits on the FIELD because the
//! field still exists to carry it — and a declaration diff can never tell a
//! rename from a drop-plus-add apart, so this annotation is the only way a
//! rename is ever expressed. `dropped(...)` sits on the MODEL because by the
//! time a column is dropped there is no field left to annotate; it doubles
//! as the record of what was removed, deleted once the column is purged.
//!
//! Lives in `tests/` rather than a `#[cfg(test)]` module for the same reason
//! as `model_derive.rs`: the derive emits absolute `::boogy_sdk::` paths that
//! only resolve from a crate depending on boogy-sdk.

use boogy_sdk::model::{Id, Model};
use boogy_sdk::Model as ModelDerive;

#[derive(ModelDerive)]
#[model(table = "docs", dropped("legacy_note", "old_flag"))]
struct Doc {
    #[pk]
    id: Id<Doc>,
    #[renamed_from = "title"]
    headline: String,
    body: String,
}

#[test]
fn dropped_list_reaches_the_reconciler() {
    assert_eq!(Doc::ALLOW_DROPPED, &["legacy_note", "old_flag"]);
}

#[test]
fn renamed_from_lands_on_the_column() {
    let t = Doc::schema();
    let c = t.columns.iter().find(|c| c.name == "headline").unwrap();
    assert_eq!(c.renamed_from.as_deref(), Some("title"));
}

#[test]
fn a_field_without_the_attribute_carries_no_rename() {
    let t = Doc::schema();
    let c = t.columns.iter().find(|c| c.name == "body").unwrap();
    assert_eq!(c.renamed_from, None);
}

/// `dropped("headline")` naming a column a field still declares is a
/// contradiction — `dropped(...)` exists because the field is GONE. Caught
/// at compile time rather than left to surface as a bad reconcile plan
/// against a live table.
#[test]
fn dropped_naming_a_live_field_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/dropped_names_existing_field.rs");
}
