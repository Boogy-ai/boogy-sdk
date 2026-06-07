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
            AccessPattern::RankedBy { order } =>
                want(vec![order.column.clone()], false, true, false, true),
            AccessPattern::LookupBy { column } =>
                want(vec![column.clone()], true, false, true, false),
            AccessPattern::TaggedBy { tag, refs } =>
                want(vec![tag.clone(), refs.clone()], false, true, false, false),
        }
    }
    for ix in explicit {
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
/// candidates left behind by a removed access pattern. Reconcile WARNS about
/// these; it never auto-drops (destructive). Ignores the implicit `_id` PK and
/// any name not matching our `ix_`/`idx_` derived prefixes (hand-managed).
pub fn orphaned(resolved: &[Index], actual_names: &[String]) -> Vec<String> {
    let desired: std::collections::HashSet<&str> =
        resolved.iter().map(|i| i.name.as_str()).collect();
    actual_names
        .iter()
        .filter(|n| (n.starts_with("ix_") || n.starts_with("idx_")) && !desired.contains(n.as_str()))
        .cloned()
        .collect()
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
}
