//! Derive-layer coverage for the `#[derive(Model)]` access-pattern verbs.
//!
//! `schema_resolve`'s unit tests all construct `AccessPattern` values by hand,
//! so none of them exercises the *attribute syntax*. A derive regression to
//! overwrite-instead-of-push (or one that rejected a repeated verb as a
//! duplicate key) would leave every one of those tests passing while models
//! silently lost indexes — and a query with no index degrades to a full scan.
//!
//! These live in `tests/` rather than as a `#[cfg(test)]` module because the
//! derive emits absolute `::boogy_sdk::` paths, which only resolve from a crate
//! that depends on boogy-sdk. An integration test is exactly that, so this also
//! exercises the derive the way a real service does.

use boogy_sdk::model::{Id, Model};
use boogy_sdk::store::AccessPattern;
use boogy_sdk::Model as ModelDerive;

/// Three `list_by` verbs on ONE model, two of them sharing a filter column and
/// two sharing an order column — so a naive dedupe would also collapse them.
#[derive(ModelDerive)]
#[model(
    table = "multi",
    list_by(filter = "room_id", newest = "created_at"),
    list_by(filter = "owner_principal", newest = "created_at"),
    list_by(filter = "room_id", newest = "score")
)]
struct Multi {
    #[pk]
    id: Id<Multi>,
    room_id: String,
    owner_principal: String,
    score: i64,
    created_at: i64,
}

/// Mixed verbs on one model: the accumulator must not be verb-specific.
#[derive(ModelDerive)]
#[model(
    table = "mixed",
    ranked_by(highest = "score"),
    list_by(filter = "owner_principal", newest = "created_at"),
    unique_index(name = "by_slug_owner", cols = ["slug", "owner_principal"])
)]
struct Mixed {
    #[pk]
    id: Id<Mixed>,
    #[lookup_by]
    slug: String,
    owner_principal: String,
    score: i64,
    created_at: i64,
}

#[test]
fn repeated_list_by_accumulates() {
    let schema = Multi::schema();
    let list_bys: Vec<&AccessPattern> = schema
        .access_patterns
        .iter()
        .filter(|p| matches!(p, AccessPattern::ListBy { .. }))
        .collect();
    assert_eq!(
        list_bys.len(),
        3,
        "expected 3 ListBy patterns, got {:?}",
        schema.access_patterns
    );
}

#[test]
fn repeated_list_by_resolves_to_distinct_indexes() {
    let schema = Multi::schema();
    let (indexes, _diags) = schema.resolved_indices();
    let mut names: Vec<&str> = indexes.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "ix_multi_owner_principal_created_at",
            "ix_multi_room_id_created_at",
            "ix_multi_room_id_score",
        ]
    );
}

#[test]
fn mixed_verbs_all_survive_and_resolve() {
    let schema = Mixed::schema();

    let n_ranked = schema
        .access_patterns
        .iter()
        .filter(|p| matches!(p, AccessPattern::RankedBy { .. }))
        .count();
    let n_list = schema
        .access_patterns
        .iter()
        .filter(|p| matches!(p, AccessPattern::ListBy { .. }))
        .count();
    let n_lookup = schema
        .access_patterns
        .iter()
        .filter(|p| matches!(p, AccessPattern::LookupBy { .. }))
        .count();
    assert_eq!(
        (n_ranked, n_list, n_lookup),
        (1, 1, 1),
        "one verb of each kind must survive alongside the others: {:?}",
        schema.access_patterns
    );

    let (indexes, _diags) = schema.resolved_indices();
    let mut names: Vec<&str> = indexes.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "ix_mixed_owner_principal_created_at",
            "ix_mixed_score",
            "ix_mixed_slug",
            "ix_mixed_slug_owner_principal",
        ]
    );
}

// --- Index-name drift: the warning must be a real signal ---------------------
//
// The resolver canonicalizes every index name to `ix_<table>_<cols>` and WARNS
// when a *declared* name differs. That warning only carries information if the
// derive itself never declares a name — otherwise every `#[index]` field trips
// it and the signal is 100% false positives.

/// A model that declares indexes only through the derive: field-level
/// `#[index]` / `#[covering_index]`, a nameless struct-level composite, and the
/// access-pattern verbs. The author typed no index name anywhere, so resolution
/// must be silent.
#[derive(ModelDerive)]
#[model(
    table = "quiet",
    index(cols = ["room_id", "created_at"]),
    covering_index(cols = ["owner_principal", "created_at"]),
    ranked_by(highest = "score")
)]
struct Quiet {
    #[pk]
    id: Id<Quiet>,
    #[lookup_by]
    slug: String,
    #[index]
    room_id: String,
    #[covering_index]
    owner_principal: String,
    score: i64,
    created_at: i64,
}

/// The same shape, but with a hand-typed index name that can never match the
/// canonical form. THIS is what the warning exists to catch.
#[derive(ModelDerive)]
#[model(table = "noisy", index(name = "by_room", cols = ["room_id", "created_at"]))]
struct Noisy {
    #[pk]
    id: Id<Noisy>,
    room_id: String,
    created_at: i64,
}

#[test]
fn derive_only_model_emits_no_diagnostics() {
    let (indexes, diags) = Quiet::schema().resolved_indices();
    assert!(
        diags.is_empty(),
        "a model whose index names are all derive-assigned must resolve silently, got: {diags:?}"
    );
    // ...and the physical index set is unchanged by dropping the fictional names.
    let mut names: Vec<&str> = indexes.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "ix_quiet_owner_principal",
            "ix_quiet_owner_principal_created_at",
            "ix_quiet_room_id",
            "ix_quiet_room_id_created_at",
            "ix_quiet_score",
            "ix_quiet_slug",
        ]
    );
}

#[test]
fn hand_declared_non_canonical_name_still_warns() {
    let (indexes, diags) = Noisy::schema().resolved_indices();
    assert_eq!(diags.len(), 1, "expected exactly one drift warning, got {diags:?}");
    let m = diags[0].message();
    assert!(m.contains("by_room"), "warning names the declared name: {m}");
    assert!(
        m.contains("ix_noisy_room_id_created_at"),
        "warning names the canonical name: {m}"
    );
    // The index itself is created either way, under the canonical name.
    assert!(indexes.iter().any(|i| i.name == "ix_noisy_room_id_created_at"));
}
