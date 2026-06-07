use boogy_sdk::model::{Id, Model, Timestamp};
use boogy_sdk::store::{ColType, Row, Val};
use boogy_sdk::Model as ModelDerive;

// Access-pattern derive parity: struct-level list_by/ranked_by + field-level
// lookup_by/tagged_by must push AccessPatterns that resolve to the same index
// set the builder verbs produce.
#[derive(ModelDerive)]
#[model(table = "feed",
        list_by(filter = "author", newest = "created_at"),
        ranked_by(highest = "score"))]
struct Feed {
    #[pk]
    id: Id<Feed>,
    author: String,
    score: i64,
    created_at: Timestamp,
}

// Field-level lookup_by (unique point lookup) + a struct-level list_by sharing
// the same lead column, and a tagged_by junction shape on a separate model.
#[derive(ModelDerive)]
#[model(table = "users", list_by(filter = "team", oldest = "joined_at"))]
struct UserRec {
    #[pk]
    id: Id<UserRec>,
    #[lookup_by]
    slug: String,
    team: String,
    joined_at: Timestamp,
}

// A junction/side table declaring its membership shape via tagged_by.
#[derive(ModelDerive)]
#[model(table = "post_tags", tagged_by(tag = "tag", refs = "post_id"))]
struct PostTag {
    tag: String,
    post_id: i64,
}

#[test]
fn derive_access_patterns_resolve_to_indexes() {
    let t = Feed::schema();
    let (idx, _diags) = t.resolved_indices();
    let names: Vec<&str> = idx.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"ix_feed_author_created_at"), "{names:?}");
    assert!(names.contains(&"ix_feed_score"), "{names:?}");
    assert!(idx.iter().find(|i| i.name == "ix_feed_score").unwrap().covering);
    // list_by(newest=...) yields a covering composite.
    let comp = idx.iter().find(|i| i.name == "ix_feed_author_created_at").unwrap();
    assert!(comp.covering);
    assert!(!comp.unique);
    assert_eq!(comp.columns, vec!["author".to_string(), "created_at".to_string()]);
}

#[test]
fn derive_lookup_by_field_yields_unique_index() {
    let t = UserRec::schema();
    let (idx, _diags) = t.resolved_indices();
    let lookup = idx.iter().find(|i| i.name == "ix_users_slug").unwrap();
    assert!(lookup.unique, "lookup_by field must derive a UNIQUE index");
    assert!(!lookup.covering);
    assert_eq!(lookup.columns, vec!["slug".to_string()]);
    // The struct-level list_by(oldest) still resolves alongside it.
    let list = idx.iter().find(|i| i.name == "ix_users_team_joined_at").unwrap();
    assert!(list.covering);
    assert_eq!(list.columns, vec!["team".to_string(), "joined_at".to_string()]);
}

#[test]
fn derive_tagged_by_yields_covering_junction() {
    let t = PostTag::schema();
    let (idx, _diags) = t.resolved_indices();
    let j = idx.iter().find(|i| i.name == "ix_post_tags_tag_post_id").unwrap();
    assert!(j.covering);
    assert!(!j.unique);
    assert_eq!(j.columns, vec!["tag".to_string(), "post_id".to_string()]);
}

#[test]
fn derive_resolves_same_index_set_as_builder() {
    use boogy_sdk::store::{highest, newest, Table};
    // Builder equivalent of the Feed model's declared patterns.
    let built = Table::new("feed")
        .text("author")
        .integer("score")
        .integer("created_at")
        .list_by("author", newest("created_at"))
        .ranked_by(highest("score"));
    let (b_idx, _) = built.resolved_indices();
    let (d_idx, _) = Feed::schema().resolved_indices();

    let key = |idx: &[boogy_sdk::store::Index]| {
        let mut v: Vec<(String, Vec<String>, bool, bool)> = idx
            .iter()
            .map(|i| (i.name.clone(), i.columns.clone(), i.unique, i.covering))
            .collect();
        v.sort();
        v
    };
    assert_eq!(key(&d_idx), key(&b_idx), "derive must match builder index set");
}

// A struct with a pk, a renamed column, an optional FK, and a unique field.
#[derive(ModelDerive)]
#[model(table = "widgets", index(name = "idx_widgets_owner", cols = ["owner", "created_at"]))]
struct Widget {
    #[pk]
    id: Id<Widget>,
    #[model(column = "owner")]
    owner_principal: String,
    parent: Option<Id<Widget>>,
    #[unique]
    slug: String,
    created_at: Timestamp,
}

// A pk-less model with a struct-level composite unique index (the Edge shape).
#[derive(ModelDerive)]
#[model(table = "pairs", unique_index(name = "uq_pair", cols = ["a", "b"]))]
struct Pair {
    a: String,
    b: String,
}

// Covering-index surface: struct-level `covering_index(...)` and field-level
// `#[covering_index]`. Plain `index(...)`/`#[index]` stay non-covering.
#[derive(ModelDerive)]
#[model(
    table = "docs",
    covering_index(name = "idx_docs_author_created", cols = ["author", "created_at"]),
    index(name = "idx_docs_created", cols = ["created_at"])
)]
struct Doc {
    #[pk]
    id: Id<Doc>,
    author: String,
    #[covering_index]
    handle: String,
    #[index]
    title: String,
    created_at: Timestamp,
}

#[test]
fn covering_index_struct_and_field_level() {
    let t = Doc::schema();

    // struct-level covering_index -> covering = true
    let c = t.indices.iter().find(|i| i.name == "idx_docs_author_created").unwrap();
    assert!(c.covering, "covering_index(...) must set covering");
    assert!(!c.unique);
    assert_eq!(c.columns, vec!["author".to_string(), "created_at".to_string()]);

    // struct-level plain index -> covering = false
    let plain = t.indices.iter().find(|i| i.name == "idx_docs_created").unwrap();
    assert!(!plain.covering, "index(...) must stay non-covering");

    // field-level #[covering_index] -> single-col covering index
    let fc = t.indices.iter().find(|i| i.name == "idx_docs_handle").unwrap();
    assert!(fc.covering, "#[covering_index] field must set covering");
    assert_eq!(fc.columns, vec!["handle".to_string()]);

    // field-level #[index] -> single-col, non-covering
    let fi = t.indices.iter().find(|i| i.name == "idx_docs_title").unwrap();
    assert!(!fi.covering, "#[index] field must stay non-covering");
    assert_eq!(fi.columns, vec!["title".to_string()]);
}

#[test]
fn table_const_and_column_consts() {
    assert_eq!(Widget::TABLE, "widgets");
    assert_eq!(Widget::OWNER_PRINCIPAL, "owner"); // renamed via #[model(column)]
    assert_eq!(Widget::PARENT, "parent");
    assert_eq!(Widget::SLUG, "slug");
    assert_eq!(Widget::CREATED_AT, "created_at");
    assert_eq!(Pair::TABLE, "pairs");
    assert_eq!(Pair::A, "a");
}

#[test]
fn schema_has_columns_indexes_nullable_unique() {
    let t = Widget::schema();
    assert_eq!(t.name, "widgets");
    // pk (`id`) is NOT a declared column (the store auto-creates _id).
    let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["owner", "parent", "slug", "created_at"]);
    // parent is Option<Id> -> nullable integer.
    let parent = t.columns.iter().find(|c| c.name == "parent").unwrap();
    assert!(parent.nullable);
    assert_eq!(parent.col_type as u8, ColType::Integer as u8);
    // slug is #[unique].
    assert!(t.columns.iter().find(|c| c.name == "slug").unwrap().unique);
    // owner string column, not nullable.
    let owner = t.columns.iter().find(|c| c.name == "owner").unwrap();
    assert!(!owner.nullable);
    assert_eq!(owner.col_type as u8, ColType::Text as u8);
    // struct-level index present.
    let idx = t.indices.iter().find(|i| i.name == "idx_widgets_owner").unwrap();
    assert_eq!(idx.columns, vec!["owner".to_string(), "created_at".to_string()]);
    assert!(!idx.unique);

    let p = Pair::schema();
    let uq = p.indices.iter().find(|i| i.name == "uq_pair").unwrap();
    assert!(uq.unique);
    assert_eq!(uq.columns, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn to_columns_excludes_pk_and_id_reads_from_underscore_id() {
    let w = Widget {
        id: Id::new(7),
        owner_principal: "u1".into(),
        parent: Some(Id::new(3)),
        slug: "abc".into(),
        created_at: Timestamp::new(1000),
    };
    let cols = w.to_columns();
    let col_names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(col_names, vec!["owner", "parent", "slug", "created_at"]); // no id
    assert_eq!(w.id(), Some(7));
    assert_eq!(Pair { a: "x".into(), b: "y".into() }.id(), None); // no pk -> None
}

#[test]
fn from_row_roundtrip() {
    let row = Row {
        columns: vec![
            ("_id".into(), Val::Integer(7)),
            ("owner".into(), Val::Text("u1".into())),
            ("parent".into(), Val::Integer(3)),
            ("slug".into(), Val::Text("abc".into())),
            ("created_at".into(), Val::Integer(1000)),
        ],
    };
    let w = Widget::from_row(&row);
    assert_eq!(w.id.get(), 7);
    assert_eq!(w.owner_principal, "u1");
    assert_eq!(w.parent.map(|p| p.get()), Some(3));
    assert_eq!(w.slug, "abc");
    assert_eq!(w.created_at, Timestamp::new(1000));

    // Null parent decodes to None.
    let row2 = Row {
        columns: vec![
            ("_id".into(), Val::Integer(8)),
            ("owner".into(), Val::Text("u2".into())),
            ("parent".into(), Val::Null),
            ("slug".into(), Val::Text("d".into())),
            ("created_at".into(), Val::Integer(0)),
        ],
    };
    assert_eq!(Widget::from_row(&row2).parent, None);
}
