//! Typed query-builder DSL. Slice (a) of the SDK-ergonomics arc.
//! Spec: `docs/superpowers/specs/2026-05-23-typed-query-dsl-design.md`.

use crate::model::Id;
use crate::store::Val;

/// Convert a Rust value into a store `Val`. Implemented for the common
/// primitive types so filter builders like `.where_eq("col", 0)` work
/// without manual `Val::Integer(0_i64)` ceremony.
pub trait IntoVal {
    fn into_val(self) -> Val;
}

impl IntoVal for i32 {
    fn into_val(self) -> Val { Val::Integer(self as i64) }
}
impl IntoVal for i64 {
    fn into_val(self) -> Val { Val::Integer(self) }
}
impl IntoVal for u32 {
    fn into_val(self) -> Val { Val::Integer(self as i64) }
}
impl IntoVal for u64 {
    fn into_val(self) -> Val { Val::Integer(self as i64) }
}
impl IntoVal for &str {
    fn into_val(self) -> Val { Val::Text(self.to_string()) }
}
impl IntoVal for String {
    fn into_val(self) -> Val { Val::Text(self) }
}
impl IntoVal for f64 {
    fn into_val(self) -> Val { Val::Real(self) }
}
impl IntoVal for bool {
    // Must match `Field for bool`'s mapping in the model layer (Val::Boolean),
    // not SQLite's storage trick (Integer). Cross-type compare in the engine
    // returns None (no match), so `where_eq("flag", false)` would silently
    // match zero rows if this produced Val::Integer.
    fn into_val(self) -> Val { Val::Boolean(self) }
}
impl IntoVal for Val {
    fn into_val(self) -> Val { self }
}
impl<T> IntoVal for Id<T> {
    fn into_val(self) -> Val { Val::Integer(self.get() as i64) }
}

use crate::pagination::Cursor;
use crate::store::{Filter, FilterOp, SortDir};

/// Builder state for the typed query DSL.
///
/// Holds the filters, sort, pagination, and keyset configuration that
/// terminal methods (`fetch_one`/`fetch_all`/`count`/`fetch_page`,
/// emitted by `wit_glue!`) consume. All fields are public so the
/// macro-emitted `Query` newtype can read them when constructing WIT
/// calls.
///
/// User code typically goes through the macro-emitted `Query` wrapper
/// (`Query::on("posts").where_eq(...).fetch_all()`); `QueryArgs` is
/// the underlying data type that holds the builder state and exposes
/// all the chainable methods that don't touch WIT.
#[derive(Debug, Clone)]
pub struct QueryArgs {
    pub table: String,
    pub base_filters: Vec<Filter>,
    pub or_groups: Vec<Vec<Filter>>,
    pub sort: Vec<(String, SortDir)>,
    pub limit: Option<usize>,
    pub offset: u32,
    pub cursor: Option<Cursor>,
    pub keyset_mode: Option<(String, SortDir)>,
    pub allow_full_scan: bool,
}

impl QueryArgs {
    /// Start a new query against `table`.
    pub fn on(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            base_filters: Vec::new(),
            or_groups: Vec::new(),
            sort: Vec::new(),
            limit: None,
            offset: 0,
            cursor: None,
            keyset_mode: None,
            allow_full_scan: false,
        }
    }

    // -- Filter chaining --

    pub fn where_eq<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Eq, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_neq<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Neq, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_gt<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Gt, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_gte<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Gte, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_lt<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Lt, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_lte<V: IntoVal>(mut self, col: &str, val: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Lte, val: val.into_val(), in_values: None });
        self
    }
    pub fn where_like<V: IntoVal>(mut self, col: &str, pattern: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::Like, val: pattern.into_val(), in_values: None });
        self
    }
    pub fn where_not_like<V: IntoVal>(mut self, col: &str, pattern: V) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::NotLike, val: pattern.into_val(), in_values: None });
        self
    }
    pub fn where_null(mut self, col: &str) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::IsNull, val: Val::Null, in_values: None });
        self
    }
    pub fn where_not_null(mut self, col: &str) -> Self {
        self.base_filters.push(Filter { column: col.to_string(), op: FilterOp::IsNotNull, val: Val::Null, in_values: None });
        self
    }

    /// IN-list filter. Stores values in the `Filter`'s `in_values` field
    /// (separate from the scalar `val`); the host's `FilterOp::In`
    /// handler reads from `in_values`, not from `val`. Empty iterator
    /// → no filter added (silently skips — a legitimately empty IN-list
    /// is a common UI pattern, not necessarily a bug).
    pub fn where_in<I, V>(mut self, col: &str, vals: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: IntoVal,
    {
        let vals: Vec<Val> = vals.into_iter().map(|v| v.into_val()).collect();
        if vals.is_empty() {
            return self;
        }
        self.base_filters.push(Filter {
            column: col.to_string(),
            op: FilterOp::In,
            val: Val::Null, // ignored by the host for FilterOp::In
            in_values: Some(vals),
        });
        self
    }

    /// OR-of-AND clause. The closure receives a fresh sub-`QueryArgs`
    /// in which the user composes AND-filters; the resulting filter-vec
    /// is folded into the outer query's `or_groups`. Nested `.or(...)`
    /// calls flatten — the sub-query's own `or_groups` are merged.
    ///
    /// **Sub-query's `.order_by`/`.limit`/`.cursor`/`.keyset_by` are
    /// silently ignored** — only filter chaining is meaningful inside
    /// an OR group.
    pub fn or<F>(mut self, build: F) -> Self
    where
        F: FnOnce(QueryArgs) -> QueryArgs,
    {
        let sub = build(QueryArgs::on(&self.table));
        if !sub.base_filters.is_empty() {
            self.or_groups.push(sub.base_filters);
        }
        // Flatten nested ORs.
        self.or_groups.extend(sub.or_groups);
        self
    }

    // -- Sort --

    pub fn order_by(mut self, col: &str, dir: SortDir) -> Self {
        self.sort.push((col.to_string(), dir));
        self
    }
    pub fn order_by_asc(self, col: &str) -> Self {
        self.order_by(col, SortDir::Asc)
    }
    pub fn order_by_desc(self, col: &str) -> Self {
        self.order_by(col, SortDir::Desc)
    }

    // -- Pagination --

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
    pub fn offset(mut self, n: u32) -> Self {
        self.offset = n;
        self
    }
    pub fn cursor(mut self, c: Option<Cursor>) -> Self {
        self.cursor = c;
        self
    }
    pub fn keyset_by(mut self, col: &str, dir: SortDir) -> Self {
        self.keyset_mode = Some((col.to_string(), dir));
        self
    }

    /// Explicitly permit a full table scan for this query (suppresses the
    /// strict-mode full-scan guardrail). Pass a human reason for grep-ability.
    /// Use ONLY when the scan is genuinely intentional (tiny/bounded table).
    pub fn allow_full_scan(mut self, _reason: &str) -> Self {
        self.allow_full_scan = true;
        self
    }

    // -- Internal helpers used by the macro-emitted Query terminals.
    //    Factored out so they can be unit-tested at the SDK level (the
    //    Query newtype + terminals live in wit_glue! and need a wasm
    //    consumer to exercise — these pure-data helpers don't). --

    /// Override pagination to `(limit=1, offset=0)` for `fetch_one`. The
    /// method-name contract is "the first matching row" — not "the first
    /// matching row past N skipped" — so any prior `.offset(n)` is reset.
    pub fn for_fetch_one(mut self) -> Self {
        self.limit = Some(1);
        self.offset = 0;
        self
    }

    /// Filters to send to the underlying WIT `count` op for `Query::count`.
    /// The WIT count op is filters-only — `or_groups`, `sort`, and page are
    /// silently dropped. Exposes this as a method so the contract is
    /// unit-testable without spinning up a WIT consumer.
    pub fn count_filters(&self) -> &[Filter] {
        &self.base_filters
    }
}

/// Read a row's value at `col` and convert it to a `serde_json::Value`
/// suitable for use as a keyset cursor. Used by `Query::fetch_page`'s
/// auto-cursor mechanism (emitted by `wit_glue!`).
pub fn row_to_json_value(row: &crate::store::Row, col: &str) -> serde_json::Value {
    match row.get(col) {
        Val::Integer(i) => serde_json::json!(i),
        Val::Real(f)    => serde_json::json!(f),
        Val::Text(s)    => serde_json::json!(s),
        Val::Boolean(b) => serde_json::json!(b),
        Val::Blob(_)    => serde_json::Value::Null,
        Val::Null       => serde_json::Value::Null,
    }
}

/// Build the per-row cursor for `Query::fetch_page`. When the keyset
/// column is `"_id"`, emits `Cursor::id_only` so `keyset_resume_filter`
/// hits the indexed planner fast path; otherwise emits the composite
/// `Cursor::keyset(last_id, last_value)`.
pub fn build_keyset_cursor(row: &crate::store::Row, keyset_col: &str) -> Cursor {
    let last_id = row.id().to_string();
    if keyset_col == "_id" {
        Cursor::id_only(last_id)
    } else {
        let last_value = row_to_json_value(row, keyset_col);
        Cursor::keyset(last_id, last_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Row, Val};

    #[test]
    fn i32_coerces_to_integer() {
        assert_eq!(42_i32.into_val(), Val::Integer(42));
    }

    #[test]
    fn i64_coerces_to_integer() {
        assert_eq!((-9_i64).into_val(), Val::Integer(-9));
    }

    #[test]
    fn u32_coerces_to_integer() {
        assert_eq!(42_u32.into_val(), Val::Integer(42));
    }

    #[test]
    fn u64_coerces_to_integer() {
        assert_eq!(42_u64.into_val(), Val::Integer(42));
    }

    #[test]
    fn str_ref_coerces_to_text() {
        assert_eq!("hello".into_val(), Val::Text("hello".to_string()));
    }

    #[test]
    fn string_coerces_to_text() {
        assert_eq!("world".to_string().into_val(), Val::Text("world".to_string()));
    }

    #[test]
    fn f64_coerces_to_real() {
        // PartialEq on Val::Real(f64) — using exact equality is fine for a constructor test.
        let v = 1.5_f64.into_val();
        match v {
            Val::Real(f) => assert_eq!(f, 1.5),
            other => panic!("expected Val::Real, got {other:?}"),
        }
    }

    #[test]
    fn bool_coerces_to_boolean() {
        // Must match the model layer's Field for bool mapping (Val::Boolean).
        // See the IntoVal for bool comment above.
        assert_eq!(true.into_val(), Val::Boolean(true));
        assert_eq!(false.into_val(), Val::Boolean(false));
    }

    #[test]
    fn val_identity() {
        assert_eq!(Val::Text("x".into()).into_val(), Val::Text("x".into()));
        assert_eq!(Val::Null.into_val(), Val::Null);
    }

    #[test]
    fn id_t_coerces_to_integer() {
        struct Post;
        let id: Id<Post> = Id::new(99);
        assert_eq!(id.into_val(), Val::Integer(99));
    }

    use crate::store::{FilterOp, SortDir};

    #[test]
    fn query_on_sets_table() {
        let q = QueryArgs::on("posts");
        assert_eq!(q.table, "posts");
        assert!(q.base_filters.is_empty());
        assert!(q.or_groups.is_empty());
        assert!(q.sort.is_empty());
        assert!(q.limit.is_none());
        assert_eq!(q.offset, 0);
        assert!(q.cursor.is_none());
        assert!(q.keyset_mode.is_none());
    }

    #[test]
    fn where_eq_pushes_filter() {
        let q = QueryArgs::on("posts").where_eq("author", "alice");
        assert_eq!(q.base_filters.len(), 1);
        assert_eq!(q.base_filters[0].column, "author");
        assert_eq!(q.base_filters[0].op, FilterOp::Eq);
        assert_eq!(q.base_filters[0].val, Val::Text("alice".into()));
    }

    #[test]
    fn where_chain_accumulates() {
        let q = QueryArgs::on("posts")
            .where_eq("author", "alice")
            .where_gt("score", 100_i64)
            .where_lt("created_at", 999_u64);
        assert_eq!(q.base_filters.len(), 3);
        assert_eq!(q.base_filters[1].op, FilterOp::Gt);
        assert_eq!(q.base_filters[2].op, FilterOp::Lt);
    }

    #[test]
    fn where_null_pushes_isnull() {
        let q = QueryArgs::on("t").where_null("deleted_at");
        assert_eq!(q.base_filters[0].op, FilterOp::IsNull);
        assert_eq!(q.base_filters[0].val, Val::Null);
    }

    #[test]
    fn where_in_pushes_in_filter() {
        let q = QueryArgs::on("t").where_in("status", vec!["active", "pending"]);
        assert_eq!(q.base_filters.len(), 1);
        assert_eq!(q.base_filters[0].op, FilterOp::In);
        // The host reads `in_values`, not `val`, for FilterOp::In —
        // verify the values landed in the right field.
        assert_eq!(
            q.base_filters[0].in_values,
            Some(vec![Val::Text("active".into()), Val::Text("pending".into())]),
        );
        assert_eq!(q.base_filters[0].val, Val::Null,
            "FilterOp::In must leave `val` as Null; the host ignores it");
    }

    #[test]
    fn where_in_empty_skips_silently() {
        let q = QueryArgs::on("t").where_in::<_, &str>("status", std::iter::empty());
        assert!(q.base_filters.is_empty(), "empty where_in must skip the filter");
    }

    #[test]
    fn or_builds_or_group() {
        let q = QueryArgs::on("t")
            .where_eq("kind", "post")
            .or(|q| q.where_eq("kind", "boost").where_gt("score", 50_i64));
        assert_eq!(q.base_filters.len(), 1, "outer AND-set has the kind=post filter");
        assert_eq!(q.or_groups.len(), 1, "one OR-group added");
        assert_eq!(q.or_groups[0].len(), 2, "OR-group has 2 AND-filters");
        assert_eq!(q.or_groups[0][0].column, "kind");
        assert_eq!(q.or_groups[0][1].column, "score");
    }

    #[test]
    fn nested_or_flattens() {
        let q = QueryArgs::on("t")
            .or(|q| q.where_eq("a", 1_i64).or(|q| q.where_eq("b", 2_i64)));
        // Outer .or contributes 1 OR-group (with the a=1 filter).
        // Inner .or contributes 1 more OR-group (with the b=2 filter).
        assert_eq!(q.or_groups.len(), 2, "nested ORs flatten to top-level groups");
    }

    #[test]
    fn order_by_chain_builds_composite_sort() {
        let q = QueryArgs::on("t").order_by_desc("score").order_by_asc("_id");
        assert_eq!(q.sort.len(), 2);
        assert_eq!(q.sort[0], ("score".to_string(), SortDir::Desc));
        assert_eq!(q.sort[1], ("_id".to_string(), SortDir::Asc));
    }

    #[test]
    fn pagination_setters_round_trip() {
        let q = QueryArgs::on("t").limit(20).offset(40);
        assert_eq!(q.limit, Some(20));
        assert_eq!(q.offset, 40);
    }

    #[test]
    fn cursor_setter_round_trips_some_and_none() {
        let c = Cursor::keyset("99", serde_json::json!(42));
        let q1 = QueryArgs::on("t").cursor(Some(c.clone()));
        assert_eq!(q1.cursor, Some(c));

        let q2 = QueryArgs::on("t").cursor(None);
        assert!(q2.cursor.is_none());
    }

    #[test]
    fn keyset_by_sets_mode() {
        let q = QueryArgs::on("t").keyset_by("created_at", SortDir::Desc);
        assert_eq!(q.keyset_mode, Some(("created_at".to_string(), SortDir::Desc)));
    }

    #[test]
    fn for_fetch_one_overrides_limit_and_resets_offset() {
        // Regression guard for the T3 review fix: fetch_one must reset any
        // prior .offset(n) — the method name promises "first matching row",
        // not "first matching row after N skipped". Without the reset,
        // `.offset(10).fetch_one()` would silently return the 11th row.
        let q = QueryArgs::on("t").limit(20).offset(10).for_fetch_one();
        assert_eq!(q.limit, Some(1), "fetch_one must override limit to 1");
        assert_eq!(q.offset, 0, "fetch_one must reset offset to 0");
    }

    #[test]
    fn count_filters_returns_only_base_filters_dropping_or_groups() {
        // Regression guard for the T3 review fix: count's WIT op is
        // filters-only; or_groups (and sort/page) are silently dropped.
        // If someone later wires or_groups into count without updating
        // the docs + this contract, this test fails loudly.
        let q = QueryArgs::on("t")
            .where_eq("kind", "post")
            .or(|q| q.where_eq("kind", "boost").where_gt("score", 50_i64));
        assert_eq!(q.or_groups.len(), 1, "or-group present in QueryArgs");
        assert_eq!(q.count_filters().len(), 1, "count_filters drops or_groups");
        assert_eq!(q.count_filters()[0].column, "kind");
    }

    #[test]
    fn row_to_json_value_dispatches_all_val_variants() {
        use crate::store::Row;

        // Helper: build a Row with the given (name, val) columns.
        let row = Row {
            columns: vec![
                ("i".to_string(), Val::Integer(42)),
                ("r".to_string(), Val::Real(1.5)),
                ("t".to_string(), Val::Text("hi".to_string())),
                ("b".to_string(), Val::Boolean(true)),
                ("blob".to_string(), Val::Blob(vec![0xde, 0xad])),
                ("n".to_string(), Val::Null),
            ],
        };

        assert_eq!(row_to_json_value(&row, "i"), serde_json::json!(42));
        assert_eq!(row_to_json_value(&row, "r"), serde_json::json!(1.5));
        assert_eq!(row_to_json_value(&row, "t"), serde_json::json!("hi"));
        assert_eq!(row_to_json_value(&row, "b"), serde_json::json!(true));
        assert_eq!(row_to_json_value(&row, "blob"), serde_json::Value::Null);
        assert_eq!(row_to_json_value(&row, "n"), serde_json::Value::Null);
        // Missing column → Row::get returns &Val::Null → json::Value::Null.
        assert_eq!(row_to_json_value(&row, "missing"), serde_json::Value::Null);
    }

    #[test]
    fn build_keyset_cursor_emits_id_only_for_underscore_id() {
        let row = Row {
            columns: vec![
                ("_id".to_string(), Val::Integer(42)),
                ("score".to_string(), Val::Integer(100)),
            ],
        };
        let cursor = build_keyset_cursor(&row, "_id");
        assert_eq!(cursor, Cursor::id_only("42"));
        // Explicit: the id-only path leaves last_value Null so
        // keyset_resume_filter takes the indexed fast path.
        assert!(cursor.last_value.is_null());
        assert_eq!(cursor.last_id, "42");
    }

    #[test]
    fn build_keyset_cursor_emits_composite_for_other_cols() {
        let row = Row {
            columns: vec![
                ("_id".to_string(), Val::Integer(42)),
                ("score".to_string(), Val::Integer(100)),
            ],
        };
        let cursor = build_keyset_cursor(&row, "score");
        assert_eq!(cursor, Cursor::keyset("42", serde_json::json!(100)));
        assert_eq!(cursor.last_value, serde_json::json!(100));
        assert_eq!(cursor.last_id, "42");
    }
}
