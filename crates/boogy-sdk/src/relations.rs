//! Eager-loading helper for parent → many-children relationships.
//!
//! The cellular DB model deliberately makes cross-service JOINs
//! unavailable, but cohesion *within* a service is the whole point of
//! per-service tables. Listing "users with their post counts" or "notes
//! with their comments" hits the N+1 trap if every parent issues
//! its own child query.
//!
//! [`load_has_many`] batches the children: one parameterised
//! `SELECT * FROM <child> WHERE <fk> IN (?, ?, ...)` regardless of
//! how many parents are in scope, then groups the result by FK so
//! handlers can splice children onto parents in O(1) per parent.
//!
//! ## Typical handler
//!
//! ```ignore
//! use boogy_sdk::relations::load_has_many;
//!
//! fn list_notes_with_comments(_req: &mut Req<'_>) -> Result<Json<json::Value>, ApiError> {
//!     // 1) parents
//!     let notes = auth::find_owned("notes", DEFAULT_OWNER_COL)?;
//!     let note_ids: Vec<String> = notes.iter().map(|n| n.id()).collect();
//!
//!     // 2) children, batched
//!     let comments_by_note = load_has_many("comments", "note_id", &note_ids)?;
//!
//!     // 3) splice
//!     let items: Vec<_> = notes.iter().map(|n| {
//!         let comments: Vec<json::Value> = comments_by_note
//!             .get(&n.id())
//!             .map(|cs| cs.iter().map(|c| c.to_json(&["body"])).collect())
//!             .unwrap_or_default();
//!         json::json!({ "id": n.id(), "title": n.text("title"), "comments": comments })
//!     }).collect();
//!     Ok(Json(json::json!(items)))
//! }
//! ```
//!
//! Cross-call this is one parent query + one child query regardless
//! of how many parents are in the page.
//!
//! ## Note on shape
//!
//! The pure-SDK API surface in this module is the
//! [`group_by_column`] helper — it's a `Vec<Row>` → `HashMap`
//! transform with no I/O. The `load_has_many` function that does
//! the actual store query is emitted by [`wit_glue!`](crate::wit_glue)
//! because it needs to reach into the user crate's
//! `bindings::boogy::platform::store`. Tests in this module
//! cover `group_by_column` directly; the end-to-end shape is
//! covered by `tests-integration/src/eager_loading.rs`.

use std::collections::HashMap;

use crate::store::Row;

/// Group a flat list of rows by the value of a chosen column,
/// preserving each row's relative order within its group.
///
/// Used internally by [`load_has_many`](crate::wit_glue) to bucket
/// fetched children by FK; exposed publicly because the same
/// pattern shows up in joins-without-relations (e.g. fanning a
/// single audit query into per-resource buckets).
///
/// Rows whose `column` value is empty get bucketed under the empty
/// string — surfaces obvious-bug cases (FK column with NULL where
/// it shouldn't be) rather than silently dropping rows.
pub fn group_by_column(rows: Vec<Row>, column: &str) -> HashMap<String, Vec<Row>> {
    let mut out: HashMap<String, Vec<Row>> = HashMap::new();
    for row in rows {
        let key = row.text(column).to_string();
        out.entry(key).or_default().push(row);
    }
    out
}

/// Same as [`group_by_column`] but keyed by `u64` — used when the FK
/// column holds an integer ID (the standard since the string→u64 ID
/// migration).
pub fn group_by_column_u64(rows: Vec<Row>, column: &str) -> HashMap<u64, Vec<Row>> {
    let mut out: HashMap<u64, Vec<Row>> = HashMap::new();
    for row in rows {
        let key = row.int(column) as u64;
        out.entry(key).or_default().push(row);
    }
    out
}

/// Build the parameterised `SELECT ... IN (?, ?, ...)` string for a
/// batched child fetch. Public so handlers that need a tweaked
/// query (different `SELECT` columns, extra `WHERE` clauses) can
/// reuse the placeholder-construction logic without copy-pasting
/// the format machinery.
///
/// Returns `(sql, num_placeholders)`. Callers pass `num_placeholders`
/// rows of actual parameter values to `store::query`.
///
/// Identifier escaping: the SQL the WIT store talks to is SQLite,
/// where unquoted identifiers are case-insensitive ASCII; the SDK
/// already restricts table/column names to that shape elsewhere
/// (manifest validators, `Table::new` builders), so direct
/// interpolation is safe here. The values themselves go through
/// `?` placeholders so untrusted ids never reach the SQL string.
pub fn build_in_query(
    child_table: &str,
    fk_column: &str,
    parent_count: usize,
) -> (String, usize) {
    if parent_count == 0 {
        // No parents → no query. Caller should short-circuit before
        // calling store::query; if they don't, a SELECT with an
        // empty IN list is a SQL parse error.
        return (String::new(), 0);
    }
    let mut placeholders = String::with_capacity(parent_count * 2);
    for i in 0..parent_count {
        if i > 0 {
            placeholders.push(',');
        }
        placeholders.push('?');
    }
    let sql = format!(
        "SELECT * FROM {child_table} WHERE {fk_column} IN ({placeholders})"
    );
    (sql, parent_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Val;

    fn row(cols: &[(&str, Val)]) -> Row {
        Row {
            columns: cols
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn group_by_buckets_rows() {
        let rows = vec![
            row(&[("note_id", Val::Text("n1".into())), ("body", Val::Text("a".into()))]),
            row(&[("note_id", Val::Text("n2".into())), ("body", Val::Text("b".into()))]),
            row(&[("note_id", Val::Text("n1".into())), ("body", Val::Text("c".into()))]),
        ];
        let grouped = group_by_column(rows, "note_id");
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped.get("n1").unwrap().len(), 2);
        assert_eq!(grouped.get("n2").unwrap().len(), 1);
    }

    #[test]
    fn group_by_preserves_within_bucket_order() {
        // Insertion order MUST be preserved within a bucket — a
        // sort that respects an upstream ORDER BY would otherwise
        // become re-randomised.
        let rows = vec![
            row(&[("k", Val::Text("g".into())), ("seq", Val::Integer(0))]),
            row(&[("k", Val::Text("g".into())), ("seq", Val::Integer(1))]),
            row(&[("k", Val::Text("g".into())), ("seq", Val::Integer(2))]),
        ];
        let grouped = group_by_column(rows, "k");
        let bucket = grouped.get("g").unwrap();
        let seqs: Vec<i64> = bucket
            .iter()
            .map(|r| match r.get("seq") {
                Val::Integer(i) => *i,
                _ => panic!("expected integer"),
            })
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn group_by_handles_missing_column_with_empty_key() {
        // `text()` returns empty string for missing/non-text columns,
        // so rows missing the FK get bucketed under "" — visible
        // bug rather than silently dropped.
        let rows = vec![row(&[("body", Val::Text("orphan".into()))])];
        let grouped = group_by_column(rows, "missing_col");
        assert!(grouped.contains_key(""));
        assert_eq!(grouped.get("").unwrap().len(), 1);
        assert_eq!(grouped.get("").unwrap()[0].text("body"), "orphan");
    }

    #[test]
    fn group_by_empty_input_returns_empty_map() {
        let grouped = group_by_column(Vec::new(), "anything");
        assert!(grouped.is_empty());
    }

    #[test]
    fn build_in_query_zero_parents_yields_empty_sql() {
        let (sql, n) = build_in_query("comments", "note_id", 0);
        assert!(sql.is_empty());
        assert_eq!(n, 0);
    }

    #[test]
    fn build_in_query_single_parent() {
        let (sql, n) = build_in_query("comments", "note_id", 1);
        assert_eq!(sql, "SELECT * FROM comments WHERE note_id IN (?)");
        assert_eq!(n, 1);
    }

    #[test]
    fn build_in_query_multiple_parents() {
        let (sql, n) = build_in_query("comments", "note_id", 4);
        assert_eq!(sql, "SELECT * FROM comments WHERE note_id IN (?,?,?,?)");
        assert_eq!(n, 4);
    }
}
