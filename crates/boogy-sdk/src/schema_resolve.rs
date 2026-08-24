//! Pure resolution of declared access patterns into the minimal physical index
//! set. No engine knowledge leaks out: callers declare *intent*
//! (`AccessPattern`); this module owns the index *shape* (covering/composite/
//! unique). Deterministic and order-independent so migration reconcile is stable.

use crate::store::{AccessPattern, Index};
use std::collections::BTreeMap;

/// A build-time diagnostic from resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// Suspicious-but-valid (e.g. unique + ranked on one column). Resolution
    /// still merges; the author should confirm intent.
    Warning(String),
    /// Impossible to satisfy; the schema is rejected.
    Error(String),
}

impl Diagnostic {
    pub fn message(&self) -> &str {
        match self { Diagnostic::Warning(m) | Diagnostic::Error(m) => m }
    }
}

/// Accumulated requirements on one ordered column tuple.
#[derive(Default)]
struct Req {
    columns: Vec<String>,
    unique: bool,
    covering: bool,
    from_lookup: bool,
    from_ranked: bool,
}

/// Stable, deterministic index name from the table + column tuple:
/// `ix_<table>_<col1>_<col2>...`.
fn index_name(table: &str, columns: &[String]) -> String {
    let mut s = format!("ix_{table}");
    for c in columns { s.push('_'); s.push_str(c); }
    s
}

/// Resolve declared `patterns` (+ any explicit low-level `indices`) into the
/// minimal physical index set, keyed by ordered column tuple. Merges flags
/// (`covering`/`unique` = any), dedupes, and reports diagnostics. The output is
/// sorted by index name for determinism.
pub fn resolve(table: &str, patterns: &[AccessPattern], explicit: &[Index]) -> (Vec<Index>, Vec<Diagnostic>) {
    // tuple (as joined key) -> Req
    let mut reqs: BTreeMap<Vec<String>, Req> = BTreeMap::new();
    let mut diags = Vec::new();

    let mut want = |columns: Vec<String>, unique: bool, covering: bool, lookup: bool, ranked: bool| {
        let e = reqs.entry(columns.clone()).or_default();
        e.columns = columns;
        e.unique |= unique;
        e.covering |= covering;
        e.from_lookup |= lookup;
        e.from_ranked |= ranked;
    };

    for p in patterns {
        match p {
            AccessPattern::ListBy { filter, order } =>
                want(vec![filter.clone(), order.column.clone()], false, true, false, false),
            // Filter-only, and NOT covering. The ordering comes from a
            // projection over the accumulator's cells, so the order column
            // cannot be in the index (its value is not in the row) and there is
            // nothing for a covering copy to carry. What the index is for is
            // enumerating the rows the filter matches, so the projection can be
            // scoped without reading the table.
            AccessPattern::ListByRanked { filter, .. } =>
                want(vec![filter.clone()], false, false, false, false),
            AccessPattern::RankedBy { order } =>
                want(vec![order.column.clone()], false, true, false, true),
            AccessPattern::LookupBy { column } =>
                want(vec![column.clone()], true, false, true, false),
            AccessPattern::TaggedBy { tag, refs } =>
                want(vec![tag.clone(), refs.clone()], false, true, false, false),
            // A rollup needs no index. It is a different mechanism — stored
            // per-group totals — rather than a different key order, so it
            // contributes nothing here and is applied on its own pass. Left as
            // an explicit arm rather than a `_`: a future pattern that DOES
            // want an index should fail to compile here, not be silently
            // ignored alongside this one.
            AccessPattern::Rollup { .. } => {}
        }
    }
    for ix in explicit {
        // The declared `name` is canonicalized away — warn if it differs so the
        // author doesn't depend on a hand-typed name (which silently drifts from
        // the real index and breaks any cursor/`for_each_batch` hint by that name).
        let canonical = index_name(table, &ix.columns);
        if !ix.name.is_empty() && ix.name != canonical {
            diags.push(Diagnostic::Warning(format!(
                "declared index name '{}' is ignored; this index is canonically named '{}'. \
                 Reference indexes via access patterns / the Query DSL, not by name.",
                ix.name, canonical)));
        }
        want(ix.columns.clone(), ix.unique, ix.covering, false, false);
    }

    let mut out = Vec::new();
    for (_key, r) in reqs {
        if r.from_lookup && r.from_ranked {
            diags.push(Diagnostic::Warning(format!(
                "'{}' is declared as both a unique lookup and a ranked feed — ranked feeds usually allow ties; confirm '{}' is unique.",
                r.columns[0], r.columns[0])));
        }
        out.push(Index {
            name: index_name(table, &r.columns),
            columns: r.columns,
            unique: r.unique,
            covering: r.covering,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, diags)
}

/// Index names present on the table but NOT in the resolved desired set —
/// left behind by a removed access pattern.
///
/// The reconcile **drops** these. It is not destructive in the way a dropped
/// column is: an index is rebuildable from the rows, so re-declaring it
/// restores it. Ignores the implicit `_id` PK and any name not matching the
/// `ix_`/`idx_` derived prefixes (hand-managed, and not ours to remove).
///
/// Kept alongside [`plan_reconcile`], which applies the same filter via
/// `is_derived`. This function is the name-only view of that diff and is
/// retained for callers that only need the orphan list.
pub fn orphaned(resolved: &[Index], actual_names: &[String]) -> Vec<String> {
    let desired: std::collections::HashSet<&str> =
        resolved.iter().map(|i| i.name.as_str()).collect();
    actual_names
        .iter()
        .filter(|n| (n.starts_with("ix_") || n.starts_with("idx_")) && !desired.contains(n.as_str()))
        .cloned()
        .collect()
}

/// What the reconcile will do to one index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// Declared, absent from the store.
    Create,
    /// Present under the same name with a DIFFERENT definition. Applied as a
    /// drop followed by a create — an index cannot be altered in place.
    Rebuild,
    /// Present in the store, no longer declared.
    Drop,
}

/// One planned change. For `Drop` only `index.name` is acted on; the remaining
/// fields carry the definition found in the store so it can be logged.
#[derive(Debug, Clone)]
pub struct IndexAction {
    pub index: Index,
    pub kind: ActionKind,
}

/// Is this a name the SDK derives, and therefore one we own?
///
/// Same filter as [`orphaned`]: anything else is hand-managed (or the implicit
/// `_id` primary key) and is never ours to remove.
fn is_derived(name: &str) -> bool {
    name.starts_with("ix_") || name.starts_with("idx_")
}

/// Compare two index definitions in full.
///
/// Comparing NAMES was the original defect: an index whose columns changed kept
/// its name, so the exists-guard skipped it and the store silently retained the
/// old definition. Column ORDER is significant — `[a, b]` and `[b, a]` serve
/// different queries.
fn same_definition(a: &Index, b: &Index) -> bool {
    a.columns == b.columns && a.unique == b.unique && a.covering == b.covering
}

/// Diff the declared index set against what the store actually holds.
///
/// Pure: performs no IO and decides the entire change set, which is what makes
/// every case unit-testable without a live store.
///
/// Action order is not significant to correctness — each is applied
/// independently — but the result is sorted by name so logs and tests are
/// stable.
pub fn plan_reconcile(desired: &[Index], actual: &[Index]) -> Vec<IndexAction> {
    let mut out: Vec<IndexAction> = Vec::new();

    for d in desired {
        match actual.iter().find(|a| a.name == d.name) {
            None => out.push(IndexAction { index: d.clone(), kind: ActionKind::Create }),
            // Carries the DESIRED definition: a rebuild drops the old index and
            // creates this one.
            Some(a) if !same_definition(a, d) => {
                out.push(IndexAction { index: d.clone(), kind: ActionKind::Rebuild })
            }
            Some(_) => {}
        }
    }

    for a in actual {
        if is_derived(&a.name) && !desired.iter().any(|d| d.name == a.name) {
            out.push(IndexAction { index: a.clone(), kind: ActionKind::Drop });
        }
    }

    out.sort_by(|x, y| x.index.name.cmp(&y.index.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{AccessPattern as AP, Order, Index};

    fn names(r: &[Index]) -> Vec<(String, Vec<String>, bool, bool)> {
        r.iter().map(|i| (i.name.clone(), i.columns.clone(), i.unique, i.covering)).collect()
    }

    #[test]
    fn list_by_derives_covering_composite() {
        let (idx, diags) = resolve("posts",
            &[AP::ListBy { filter: "author".into(), order: Order { column: "created_at".into(), desc: true } }],
            &[]);
        assert!(diags.is_empty());
        assert_eq!(names(&idx), vec![("ix_posts_author_created_at".into(),
            vec!["author".into(), "created_at".into()], false, true)]);
    }

    #[test]
    fn ranked_by_derives_single_col_covering() {
        let (idx, _) = resolve("scores", &[AP::RankedBy { order: Order { column: "score".into(), desc: true } }], &[]);
        assert_eq!(names(&idx), vec![("ix_scores_score".into(), vec!["score".into()], false, true)]);
    }

    #[test]
    fn lookup_by_derives_unique() {
        let (idx, _) = resolve("users", &[AP::LookupBy { column: "slug".into() }], &[]);
        assert_eq!(names(&idx), vec![("ix_users_slug".into(), vec!["slug".into()], true, false)]);
    }

    #[test]
    fn tagged_by_derives_covering_junction() {
        let (idx, _) = resolve("post_tags", &[AP::TaggedBy { tag: "tag".into(), refs: "post_id".into() }], &[]);
        assert_eq!(names(&idx), vec![("ix_post_tags_tag_post_id".into(),
            vec!["tag".into(), "post_id".into()], false, true)]);
    }

    // EDGE: lookup_by + ranked_by on the SAME column merge to one unique+covering
    // index serving both, and emit the suspicious-merge warning.
    #[test]
    fn lookup_plus_ranked_same_column_merges_with_warning() {
        let (idx, diags) = resolve("things",
            &[AP::LookupBy { column: "score".into() },
              AP::RankedBy { order: Order { column: "score".into(), desc: true } }],
            &[]);
        assert_eq!(names(&idx), vec![("ix_things_score".into(), vec!["score".into()], true, true)]);
        assert_eq!(diags.len(), 1);
        assert!(matches!(diags[0], Diagnostic::Warning(_)));
        assert!(diags[0].message().contains("score"));
    }

    // EDGE: two list_by on different filters → two distinct composites.
    #[test]
    fn distinct_filters_yield_distinct_indexes() {
        let (idx, _) = resolve("posts",
            &[AP::ListBy { filter: "author".into(), order: Order { column: "created_at".into(), desc: true } },
              AP::ListBy { filter: "parent_id".into(), order: Order { column: "created_at".into(), desc: true } }],
            &[]);
        assert_eq!(idx.len(), 2);
    }

    // EDGE: lookup_by("x") + list_by("x", newest("c")) → keep [x] (unique) AND [x,c].
    #[test]
    fn lookup_and_list_same_lead_keeps_both() {
        let (idx, _) = resolve("posts",
            &[AP::LookupBy { column: "author".into() },
              AP::ListBy { filter: "author".into(), order: Order { column: "created_at".into(), desc: true } }],
            &[]);
        let mut got = names(&idx); got.sort();
        assert_eq!(got, vec![
            ("ix_posts_author".into(), vec!["author".into()], true, false),
            ("ix_posts_author_created_at".into(), vec!["author".into(), "created_at".into()], false, true),
        ]);
    }

    // EDGE: duplicate identical patterns dedupe to one index.
    #[test]
    fn duplicate_patterns_dedupe() {
        let p = AP::ListBy { filter: "author".into(), order: Order { column: "created_at".into(), desc: true } };
        let (idx, _) = resolve("posts", &[p.clone(), p], &[]);
        assert_eq!(idx.len(), 1);
    }

    // EDGE: deterministic + order-independent — same set, any order → same names.
    #[test]
    fn resolution_is_order_independent() {
        let a = AP::LookupBy { column: "slug".into() };
        let b = AP::RankedBy { order: Order { column: "score".into(), desc: true } };
        let (i1, _) = resolve("t", &[a.clone(), b.clone()], &[]);
        let (i2, _) = resolve("t", &[b, a], &[]);
        let mut n1 = names(&i1); n1.sort();
        let mut n2 = names(&i2); n2.sort();
        assert_eq!(n1, n2);
    }

    // EDGE: an explicit low-level index on the same tuple merges (covering OR).
    #[test]
    fn explicit_index_merges_with_pattern() {
        let explicit = vec![Index { name: "hand".into(), columns: vec!["score".into()], unique: false, covering: false }];
        let (idx, _) = resolve("t", &[AP::RankedBy { order: Order { column: "score".into(), desc: true } }], &explicit);
        // one index on [score], covering (pattern wins covering), keeps a stable derived name
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].columns, vec!["score".to_string()]);
        assert!(idx[0].covering);
    }

    // EDGE: a declared explicit-index name that differs from the canonical
    // `ix_<table>_<cols>` emits a Warning (the declared name is ignored, and
    // depending on it — e.g. as a `for_each_batch`/`open_cursor` hint — silently
    // drifts). The resolved index still carries the canonical name.
    #[test]
    fn explicit_index_with_drifting_name_warns() {
        let explicit = vec![Index {
            name: "idx_invest_investor".into(),
            columns: vec!["investor_principal".into(), "invested_at".into()],
            unique: false, covering: false,
        }];
        let (idx, diags) = resolve("post_investments", &[], &explicit);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].name, "ix_post_investments_investor_principal_invested_at");
        assert_eq!(diags.len(), 1);
        let Diagnostic::Warning(m) = &diags[0] else { panic!("expected a Warning, got {:?}", diags[0]) };
        assert!(m.contains("idx_invest_investor"), "warning names the declared name: {m}");
        assert!(m.contains("ix_post_investments_investor_principal_invested_at"),
            "warning names the canonical name: {m}");
    }

    // EDGE: an explicit index whose declared name ALREADY equals the canonical
    // name (or is empty) emits NO warning — no false positive.
    #[test]
    fn explicit_index_with_canonical_name_is_silent() {
        let explicit = vec![Index {
            name: "ix_t_a".into(), columns: vec!["a".into()], unique: false, covering: false,
        }];
        let (_, diags) = resolve("t", &[], &explicit);
        assert!(diags.is_empty(), "canonical declared name should not warn: {diags:?}");

        let empty_named = vec![Index {
            name: String::new(), columns: vec!["a".into()], unique: false, covering: false,
        }];
        let (_, diags2) = resolve("t", &[], &empty_named);
        assert!(diags2.is_empty(), "empty declared name should not warn: {diags2:?}");
    }

    fn ix(name: &str) -> Index {
        Index { name: name.into(), columns: vec!["a".into()], unique: false, covering: true }
    }

    #[test]
    fn orphaned_indexes_detected() {
        let resolved = vec![Index { name: "ix_t_a".into(), columns: vec!["a".into()], unique: false, covering: true }];
        let actual = vec!["ix_t_a".to_string(), "ix_t_old".to_string()];
        assert_eq!(super::orphaned(&resolved, &actual), vec!["ix_t_old".to_string()]);
    }

    // EDGE: the implicit `_id` PK is never an orphan (no ix_/idx_ prefix).
    #[test]
    fn orphaned_ignores_id_pk() {
        let resolved = vec![ix("ix_t_a")];
        let actual = vec!["ix_t_a".to_string(), "_id".to_string()];
        assert!(super::orphaned(&resolved, &actual).is_empty());
    }

    // EDGE: hand-managed names without our derived prefixes are left alone,
    // even when not in the desired set (they're not ours to reconcile).
    #[test]
    fn orphaned_ignores_hand_managed_names() {
        let resolved = vec![ix("ix_t_a")];
        let actual = vec![
            "ix_t_a".to_string(),
            "uq_pair".to_string(),       // hand-named unique
            "my_custom_index".to_string(), // arbitrary hand name
        ];
        assert!(super::orphaned(&resolved, &actual).is_empty());
    }

    // EDGE: both prefixes (`ix_` from the resolver, `idx_` from field-level
    // derive / legacy) are detected when removed from the desired set.
    #[test]
    fn orphaned_detects_both_derived_prefixes() {
        let resolved = vec![ix("ix_t_keep")];
        let actual = vec![
            "ix_t_keep".to_string(),
            "ix_t_gone".to_string(),
            "idx_t_legacy".to_string(),
        ];
        let mut got = super::orphaned(&resolved, &actual);
        got.sort();
        assert_eq!(got, vec!["idx_t_legacy".to_string(), "ix_t_gone".to_string()]);
    }

    // EDGE: nothing orphaned when every actual index is still desired.
    #[test]
    fn orphaned_empty_when_all_present() {
        let resolved = vec![ix("ix_t_a"), ix("ix_t_b")];
        let actual = vec!["ix_t_a".to_string(), "ix_t_b".to_string()];
        assert!(super::orphaned(&resolved, &actual).is_empty());
    }

    // --- reconcile plan --------------------------------------------------
    // The diff that decides what the reconcile does. Pure, so every case that
    // matters is checked here without a live store.

    fn ixd(name: &str, cols: &[&str], unique: bool, covering: bool) -> Index {
        Index {
            name: name.into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
            unique,
            covering,
        }
    }

    fn kinds(plan: &[IndexAction]) -> Vec<(String, ActionKind)> {
        plan.iter().map(|a| (a.index.name.clone(), a.kind)).collect()
    }

    #[test]
    fn reconcile_creates_a_declared_index_that_does_not_exist() {
        let plan = plan_reconcile(&[ixd("ix_posts_author", &["author"], false, false)], &[]);
        assert_eq!(kinds(&plan), vec![("ix_posts_author".to_string(), ActionKind::Create)]);
    }

    #[test]
    fn reconcile_drops_an_index_no_longer_declared() {
        let plan = plan_reconcile(&[], &[ixd("ix_posts_author", &["author"], false, false)]);
        assert_eq!(kinds(&plan), vec![("ix_posts_author".to_string(), ActionKind::Drop)]);
    }

    #[test]
    fn reconcile_leaves_a_hand_managed_index_alone() {
        // Same filter as `orphaned`: an index created out-of-band is not ours to
        // remove, and neither is the implicit `_id` primary key.
        let plan = plan_reconcile(&[], &[
            ixd("my_custom_index", &["author"], false, false),
            ixd("_id", &["_id"], true, false),
        ]);
        assert!(plan.is_empty(), "expected no actions, got {:?}", kinds(&plan));
    }

    #[test]
    fn reconcile_rebuilds_an_index_whose_columns_changed_under_the_same_name() {
        // THE headline case. A name-only comparison skips this silently and the
        // store keeps the old definition, leaving the model declaring an access
        // pattern that does not physically exist.
        let plan = plan_reconcile(
            &[ixd("ix_posts_author", &["author", "created_at"], false, false)],
            &[ixd("ix_posts_author", &["author"], false, false)],
        );
        assert_eq!(kinds(&plan), vec![("ix_posts_author".to_string(), ActionKind::Rebuild)]);
        assert_eq!(
            plan[0].index.columns,
            vec!["author".to_string(), "created_at".to_string()],
            "a Rebuild must carry the DESIRED definition, since it is what gets created"
        );
    }

    #[test]
    fn reconcile_rebuilds_when_only_unique_changed() {
        let plan = plan_reconcile(
            &[ixd("ix_posts_slug", &["slug"], true, false)],
            &[ixd("ix_posts_slug", &["slug"], false, false)],
        );
        assert_eq!(kinds(&plan), vec![("ix_posts_slug".to_string(), ActionKind::Rebuild)]);
    }

    #[test]
    fn reconcile_rebuilds_when_only_covering_changed() {
        let plan = plan_reconcile(
            &[ixd("ix_posts_feed", &["created_at"], false, true)],
            &[ixd("ix_posts_feed", &["created_at"], false, false)],
        );
        assert_eq!(kinds(&plan), vec![("ix_posts_feed".to_string(), ActionKind::Rebuild)]);
    }

    #[test]
    fn reconcile_of_an_already_converged_table_is_empty() {
        // Idempotence as a test: without it, "no changes" and "the diff is
        // broken" look identical.
        let same = ixd("ix_posts_author", &["author"], false, true);
        assert!(plan_reconcile(std::slice::from_ref(&same), std::slice::from_ref(&same)).is_empty());
    }

    #[test]
    fn reconcile_column_order_is_significant() {
        // [a, b] and [b, a] are different physical indexes serving different
        // queries — comparing as sets would silently accept the wrong one.
        let plan = plan_reconcile(
            &[ixd("ix_posts_ab", &["a", "b"], false, false)],
            &[ixd("ix_posts_ab", &["b", "a"], false, false)],
        );
        assert_eq!(kinds(&plan), vec![("ix_posts_ab".to_string(), ActionKind::Rebuild)]);
    }
}
