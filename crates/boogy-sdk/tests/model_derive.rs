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

// ---------------------------------------------------------------------------
// #[default = ...] — the declaration surface for a column default
// ---------------------------------------------------------------------------

#[derive(ModelDerive)]
#[model(table = "orders")]
struct Order {
    #[pk]
    id: Id<Order>,
    #[default = "pending"]
    status: String,
    #[default = 0]
    retries: i64,
    #[default(-1)]
    offset: i64,
    #[default = 1.5]
    weight: f64,
    #[default = true]
    active: bool,
    // No default — the negative control lives on the same model, so a derive
    // that attached one to every column cannot pass the asserts below.
    note: String,
}

/// The derive must put the declared literal on the `ColDef`, with the right
/// `Val` arm for each literal kind.
///
/// One case per kind because the mapping is per-kind: a derive that emitted
/// `Val::Text` for everything would satisfy a test that only checked `status`.
#[test]
fn the_default_attribute_lands_on_the_col_def() {
    use boogy_sdk::store::Val;

    let schema = Order::schema();
    let get = |name: &str| {
        schema
            .columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("column {name} missing from the derived schema"))
            .default
            .clone()
    };

    assert_eq!(get("status"), Some(Val::Text("pending".into())));
    assert_eq!(get("retries"), Some(Val::Integer(0)));
    assert_eq!(get("offset"), Some(Val::Integer(-1)), "the parenthesized negative form");
    assert_eq!(get("weight"), Some(Val::Real(1.5)));
    assert_eq!(get("active"), Some(Val::Boolean(true)));
}

/// Negative control: a field with no `#[default]` must carry none.
///
/// Without this, a derive that defaulted every column (to the type's zero value,
/// say) would satisfy every assertion above.
#[test]
fn a_field_without_the_attribute_carries_no_default() {
    let schema = Order::schema();
    let note = schema.columns.iter().find(|c| c.name == "note").expect("note column");
    assert_eq!(note.default, None, "an undeclared column must have no default");
}

// ───────────────────────────────────────────────────────────────────────────
// #[belongs_to(Parent)] — the relation the platform ranks a parent by
// ───────────────────────────────────────────────────────────────────────────

#[derive(ModelDerive)]
#[model(table = "posts")]
struct Post {
    #[pk]
    id: Id<Post>,
    room_id: i64,
}

#[derive(ModelDerive)]
#[model(table = "post_votes")]
struct PostVote {
    #[pk]
    id: Id<PostVote>,
    /// ON post_votes.post_id = posts._id
    #[belongs_to(Post)]
    post_id: i64,
    room_id: i64,
    direction: i64,
}

/// The declaration a developer writes has to reach the store as a foreign key,
/// because that is the only thing telling the platform which table a group key
/// points at — and therefore the only thing that lets a parent with no children
/// be given a group at all.
///
/// The parent is named by TYPE rather than by string, so its table name comes
/// from `<Post as Model>::TABLE` and a renamed table cannot leave a stale
/// relation behind.
#[test]
fn belongs_to_emits_a_foreign_key_to_the_parents_table() {
    let cols = PostVote::schema().columns;
    let post_id = cols.iter().find(|c| c.name == "post_id").expect("post_id column");

    let fk = post_id
        .references
        .as_ref()
        .expect("#[belongs_to(Post)] must emit a relation");
    assert_eq!(fk.references_table, Post::TABLE);
    assert_eq!(
        fk.references_column, "_id",
        "a relation targets the parent's key, which is the value the child holds"
    );

    // Control: an ordinary column must not acquire one.
    let room_id = cols.iter().find(|c| c.name == "room_id").expect("room_id column");
    assert!(room_id.references.is_none(), "only the declared column has a parent");
}
