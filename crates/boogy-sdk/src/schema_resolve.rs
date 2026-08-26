//! Pure resolution of declared access patterns into the minimal physical index
//! set. No engine knowledge leaks out: callers declare *intent*
//! (`AccessPattern`); this module owns the index *shape* (covering/composite/
//! unique). Deterministic and order-independent so migration reconcile is stable.

use crate::store::{AccessPattern, ColDef, ColumnInfo, Index, Val};
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

/// One planned change to a single column, produced by [`plan_column_reconcile`].
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnAction {
    /// Declared, absent from the store — `add-column`.
    Add(ColDef),
    /// Same column, shape unchanged, only the declared default moved.
    SetDefault { column: String, value: Val },
    /// `#[renamed_from = "old"]` matched a column actually present under the
    /// old name. Claims that name so the add/drop arms never see it.
    Rename { from: String, to: String },
    /// Present in the store, no longer declared, and named in `allow_dropped`.
    SoftDrop(String),
    /// Present in the store as soft-dropped, re-declared with the same shape.
    ///
    /// `default` is the value to install as the revived column's default, or
    /// `None` to leave the stored one alone. It exists for the same reason
    /// [`ColumnAction::Add`] synthesises one: a soft drop does not stop the
    /// table taking writes, so rows written WHILE the column was dropped carry
    /// no value for it. Clearing the tombstone alone would leave those rows
    /// resolving a required column against nothing.
    Revive { column: String, default: Option<Val> },
    /// A change `add-column`/`set-default` cannot express, or a stored column
    /// that is undeclared AND required — so the table refuses every write.
    /// `reason` is developer-facing prose.
    Conflict { column: String, reason: String },
    /// Worth saying out loud, but nothing is wrong and nothing is applied.
    ///
    /// Distinct from [`ColumnAction::Conflict`] by CONSEQUENCE, not by
    /// severity: a conflict is a schema the service cannot run against and is
    /// reported back through the deploy; a warning is a schema that works,
    /// reported through the guest log. Collapsing the two in either direction
    /// is a real failure — a warning promoted to a conflict blocks deploys that
    /// should succeed, and a conflict demoted to a warning is a table that
    /// refuses every write, wearing a log line.
    Warn { column: String, reason: String },
}

impl ColumnAction {
    /// The column this action concerns — what the stable sort orders by.
    pub fn column_name(&self) -> &str {
        match self {
            ColumnAction::Add(c) => &c.name,
            ColumnAction::SetDefault { column, .. } => column,
            // A Rename is keyed by its NEW name: that's the name the desired
            // schema will look up when deciding whether this column is done.
            ColumnAction::Rename { to, .. } => to,
            ColumnAction::SoftDrop(n) => n,
            ColumnAction::Revive { column, .. } => column,
            ColumnAction::Conflict { column, .. } => column,
            ColumnAction::Warn { column, .. } => column,
        }
    }
}

/// Diff the declared column set against what the store actually holds.
///
/// Pure, like [`plan_reconcile`] above it, and for the same reason: the entire
/// decision table is unit-testable with no store.
///
/// `allow_dropped` carries the model-level `dropped(...)` list — the only way a
/// column is actually removed, because losing one must be something the
/// developer wrote down, and the field itself is gone so the annotation cannot
/// live on it.
///
/// A stored column that is NOT named there is never silently dropped. It is
/// either a [`ColumnAction::Conflict`] — when it is required and so refuses
/// every write ([`refuses_writes`]) — or a [`ColumnAction::Warn`], which
/// applies nothing and blocks nothing. Both name `dropped(..)` as the remedy.
///
/// Action order is not significant to correctness, but the result is sorted by
/// column name so logs and tests are stable.
pub fn plan_column_reconcile(
    desired: &[ColDef],
    actual: &[ColumnInfo],
    allow_dropped: &[&str],
) -> Vec<ColumnAction> {
    let mut out = Vec::new();
    // Actual-column names already accounted for by a rename or a compare arm,
    // so the leftover pass below doesn't also treat them as orphans.
    let mut consumed: Vec<&str> = Vec::new();

    for d in desired {
        // A rename claims the OLD name, so it must be resolved before the
        // add/compare arms below — resolving it after would let the old
        // column look like an orphan and the new one look like an add, which
        // is exactly the data-losing misread this function exists to prevent.
        if let Some(old) = d.renamed_from.as_deref() {
            if actual.iter().any(|a| a.name == old) && !actual.iter().any(|a| a.name == d.name) {
                out.push(ColumnAction::Rename { from: old.to_string(), to: d.name.clone() });
                consumed.push(old);
                continue;
            }
        }
        match actual.iter().find(|a| a.name == d.name) {
            None => {
                if !d.nullable && d.default.is_none() && d.references.is_some() {
                    out.push(ColumnAction::Conflict {
                        column: d.name.clone(),
                        reason: required_fk_conflict(
                            &d.name,
                            "added to a table that may already hold rows",
                            "every existing row",
                        ),
                    });
                } else {
                    let mut c = d.clone();
                    // Counter columns read from their own cell and may not carry a default.
                    if !c.nullable && c.default.is_none() && !c.counter {
                        c.default = Some(Val::zero_for(c.col_type));
                    }
                    out.push(ColumnAction::Add(c));
                }
            }
            Some(a) => {
                consumed.push(a.name.as_str());
                if let Some(reason) = shape_conflict(d, a) {
                    out.push(ColumnAction::Conflict { column: d.name.clone(), reason });
                } else if a.dropped {
                    // Would the revive have to synthesise a value? The same
                    // question the `Add` arm asks, and it has the same two
                    // answers — including the same refusal. A foreign key is the
                    // one column a synthesised zero cannot be legal for, so it
                    // is refused HERE too rather than only on the add path: the
                    // two arms reach the identical state (a required column with
                    // no value for some rows) and must not disagree about
                    // whether that state is allowed.
                    let must_synthesise =
                        d.default.is_none() && !d.nullable && !d.counter && a.default.is_none();
                    if must_synthesise && d.references.is_some() {
                        out.push(ColumnAction::Conflict {
                            column: d.name.clone(),
                            reason: required_fk_conflict(
                                &d.name,
                                "revived on a table that may have taken writes while it \
                                 was dropped, which carry no value for it",
                                "every such row",
                            ),
                        });
                    } else {
                        // Otherwise mirrors the `Add` arm: a declared default
                        // wins, a required column with neither a declared nor a
                        // stored default gets a synthesised zero so the rows
                        // written while it was dropped resolve, and `None` when
                        // the stored default already covers it — reviving must
                        // not silently replace a default the developer never
                        // mentioned.
                        let default = if d.default.is_some() {
                            d.default.clone()
                        } else if must_synthesise {
                            Some(Val::zero_for(d.col_type))
                        } else {
                            None
                        };
                        out.push(ColumnAction::Revive { column: d.name.clone(), default });
                    }
                } else if d.default.is_some() && a.default != d.default {
                    out.push(ColumnAction::SetDefault {
                        column: d.name.clone(),
                        value: d.default.clone().expect("is_some checked"),
                    });
                }
            }
        }
    }

    for a in actual {
        if a.dropped || consumed.contains(&a.name.as_str()) {
            continue;
        }
        if desired.iter().any(|d| d.name == a.name) {
            continue;
        }
        if allow_dropped.contains(&a.name.as_str()) {
            out.push(ColumnAction::SoftDrop(a.name.clone()));
        } else if refuses_writes(a) {
            out.push(ColumnAction::Conflict {
                column: a.name.clone(),
                reason: format!(
                    "column `{}` is in the store, is required, and is no longer \
                     declared — so every write to this table is refused: it omits a \
                     column that has no value to fall back on. Re-declare the field, \
                     add `dropped(\"{}\")` to the model attribute to soft-drop it, or \
                     `#[renamed_from = \"{}\"]` to the field that replaced it.",
                    a.name, a.name, a.name
                ),
            });
        } else {
            out.push(ColumnAction::Warn {
                column: a.name.clone(),
                reason: format!(
                    "column `{}` is in the store but not declared by the model. \
                     Writes omit it and reads resolve it to its default, so nothing \
                     is broken — but it is invisible to the model. Add \
                     `dropped(\"{}\")` to the model attribute to remove it, or \
                     `#[renamed_from = \"{}\"]` to the field that replaced it.",
                    a.name, a.name, a.name
                ),
            });
        }
    }

    out.sort_by(|x, y| x.column_name().cmp(y.column_name()));
    out
}

/// The refusal the `Add` and `Revive` arms share.
///
/// One function rather than two near-identical `format!`s, because the two arms
/// reach the same state by different doors and a remedy that drifted between
/// them would be a remedy that is right on one path and wrong on the other.
/// `arrival` names how the column got here and `rows` names which rows have no
/// value for it; the refusal and the remedy are identical either way.
fn required_fk_conflict(name: &str, arrival: &str, rows: &str) -> String {
    format!(
        "`{name}` is a required foreign key {arrival}. A synthesised zero would \
         point {rows} at a row that does not exist. Declare it `Option<T>`, or \
         give it a default naming a real row."
    )
}

/// Would a write to this table be REFUSED because of this stored column?
///
/// The line between an orphan that must fail the deploy and one that merely
/// deserves a log line. A stored-but-undeclared column normally breaks nothing:
/// a write omits it and a read resolves it to its default. It becomes fatal
/// only when there is nothing to fall back on — not nullable, no default, not a
/// counter (which lives in its own cell and is never written by an ordinary
/// row write). A table in that state refuses every write — the exact failure
/// this reconcile exists to remove — and it is the only orphan worth blocking
/// a deploy over.
///
/// Why this matters more than it looks: hand-written migrations legitimately
/// own columns the model never declares — `MigrationCtx::add_column` on a table
/// whose `Model` has no such field is the pattern the migration docs teach.
/// Treating every orphan as a conflict would turn that documented pattern into
/// a permanent deploy failure on every later resolution, which is a worse
/// regression than the bug being fixed.
///
/// `dropped` is in the predicate so it is true standalone; the caller's loop
/// has already skipped tombstones by the time it asks.
fn refuses_writes(a: &ColumnInfo) -> bool {
    !a.nullable && a.default.is_none() && !a.counter && !a.dropped
}

/// Differences `add-column` cannot reconcile in place, named in the
/// developer's terms.
///
/// Deliberately does NOT compare `unique`: it is derived host-side by scanning
/// single-column UNIQUE indexes, so an author who reaches uniqueness via
/// `.unique_index(...)` rather than `.unique()` would otherwise produce a
/// false conflict. Uniqueness is an index and is already reconciled by
/// [`plan_reconcile`] beside this function; `add-column` cannot alter it
/// anyway.
fn shape_conflict(d: &ColDef, a: &ColumnInfo) -> Option<String> {
    if d.col_type != a.col_type {
        return Some(format!(
            "the model declares {:?} and the store holds {:?}. Changing a \
             column's type rewrites every stored row, which this pass does not \
             do. Either restore the field to {:?} — the type the deployed table \
             actually has — or add a NEW field of the type you want, deploy, \
             copy the values across in a migration, and then remove the old \
             field with `dropped(\"{}\")`.",
            d.col_type, a.col_type, a.col_type, d.name
        ));
    }
    if d.nullable != a.nullable {
        // Which direction it went decides which remedy is even available, so
        // the two are not one message with a flipped pair of booleans.
        // NEITHER arm may name a backfill as the remedy. There is no operation
        // anywhere that alters a stored column's nullability: `add-column`
        // refuses a shape change ("column exists"), and `MigrationCtx` no-ops
        // on a live column, so writing values into every row leaves
        // `nullable` exactly where it was. A message saying "backfill and
        // re-declare it required" sends the developer straight back into the
        // identical conflict — the precise defect this pass exists to remove.
        return Some(if d.nullable {
            format!(
                "the model declares `{}` optional (`Option<T>`) and the store \
                 holds it required. Nothing can change a stored column's \
                 nullability in place, so there are two remedies: drop the \
                 `Option<..>` to match the column the deployed table actually \
                 has, or add a NEW optional field, deploy, copy the values \
                 across in a migration, and remove this one with \
                 `dropped(\"{}\")`.",
                d.name, d.name
            )
        } else {
            format!(
                "the model declares `{}` required and the store holds it \
                 optional, so stored rows may hold no value for it and there \
                 is nothing to read into a non-`Option` field. Nothing can \
                 change a stored column's nullability in place — writing a \
                 value into every row does not make the column required — so \
                 there are two remedies: declare the field `Option<T>` to \
                 match the column the deployed table actually has (the smaller \
                 change, and always available), or add a NEW required field, \
                 deploy, copy the values across in a migration, and remove \
                 this one with `dropped(\"{}\")`. A column created by a \
                 hand-written `MigrationCtx::add_column` is nullable unless it \
                 was given `.not_null()`, which is the usual way a table \
                 arrives in this state.",
                d.name, d.name
            )
        });
    }
    if d.counter != a.counter || d.counter_max != a.counter_max {
        return Some(format!(
            "`{}` is declared as an accumulator and the store holds it as an \
             ordinary column (or the other way round). An accumulator's value \
             lives in its own per-row cell, which only an insert seeds, so \
             flipping the declaration would read zero for every row already \
             there — materialising it needs a backfill this pass does not do. \
             Restore the declaration the deployed table was created with, or \
             create a NEW table declaring `{}` the way you want it up front \
             and migrate the rows across.",
            d.name, d.name
        ));
    }
    None
}

/// The value for the `x-boogy-schema-conflict` response header a resolution
/// pass emits, or `None` when it recorded nothing.
///
/// Pulled out as its own function so the emission rule — no header at all when
/// the list is empty, otherwise every message joined with `" | "` — is checked
/// by an ordinary unit test. The caller (the `wit_glue!`-emitted `ApplyOnly`
/// response builder) only ever runs inside a compiled wasm guest, so this is
/// the level at which "a conflict produces the header" and "no conflict means
/// no header" can be proven without standing up a live deployment.
///
/// A `Conflict` is the only action that ever reaches this list — a `Warn` is a
/// harmless stored-but-undeclared column and is logged, never recorded, so it
/// can never contribute to this value.
pub fn schema_conflict_header_value(conflicts: &[String]) -> Option<String> {
    if conflicts.is_empty() {
        None
    } else {
        Some(conflicts.join(" | "))
    }
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

    // --- column reconcile plan -------------------------------------------
    // `plan_column_reconcile`: the pure decision function that diffs a
    // model's declared columns against what the store actually holds. Each
    // test below is one row of the decision table; no store involved.

    use crate::store::{ColDef, ColType as CT, ColumnInfo, Val};

    fn col(name: &str, ty: CT, nullable: bool) -> ColDef {
        ColDef {
            name: name.into(), col_type: ty, nullable, unique: false,
            references: None, counter: false, counter_max: false,
            default: None, renamed_from: None,
        }
    }
    fn info(name: &str, ty: CT, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(), col_type: ty, nullable, unique: false,
            counter: false, counter_max: false, dropped: false,
            has_references: false, default: None,
        }
    }

    #[test]
    fn new_nullable_field_is_added() {
        let d = vec![col("a", CT::Text, true)];
        assert!(matches!(plan_column_reconcile(&d, &[], &[]).as_slice(),
            [ColumnAction::Add(c)] if c.name == "a"));
    }

    #[test]
    fn unchanged_column_produces_no_action() {
        let d = vec![col("a", CT::Text, true)];
        let a = vec![info("a", CT::Text, true)];
        assert!(plan_column_reconcile(&d, &a, &[]).is_empty());
    }

    #[test]
    fn type_change_is_a_conflict_not_an_add() {
        let d = vec![col("a", CT::Integer, true)];
        let a = vec![info("a", CT::Text, true)];
        assert!(matches!(plan_column_reconcile(&d, &a, &[]).as_slice(),
            [ColumnAction::Conflict { column, .. }] if column == "a"));
    }

    #[test]
    fn accumulator_status_change_is_a_conflict() {
        let mut d = col("hits", CT::Integer, false);
        d.counter = true;
        let a = vec![info("hits", CT::Integer, false)]; // stored as plain
        assert!(matches!(plan_column_reconcile(&[d], &a, &[]).as_slice(),
            [ColumnAction::Conflict { column, .. }] if column == "hits"));
    }

    // An undeclared stored column fails the deploy on ONE condition: it is
    // required, so a write that omits it — which every write now does — is
    // refused. That is the failure this reconcile exists to remove, and the
    // only orphan worth blocking on. The three tests below are the decision
    // table.

    #[test]
    fn removed_field_without_permission_is_a_conflict_not_a_silent_drop() {
        // NOT NULL, no default: writes are refused, so this must hard-fail.
        let a = vec![info("gone", CT::Text, false)];
        assert!(matches!(plan_column_reconcile(&[], &a, &[]).as_slice(),
            [ColumnAction::Conflict { column, .. }] if column == "gone"));
    }

    #[test]
    fn a_harmless_orphan_column_warns_and_does_not_conflict() {
        // Three ways a stored orphan stays writable. None may fail a deploy:
        // hand-written migrations legitimately own columns the model never
        // declares, and conflicting on those would block every later
        // resolution of a service that uses them.
        let nullable = info("note", CT::Text, true);

        let mut defaulted = info("state", CT::Text, false);
        defaulted.default = Some(Val::Text("pending".into()));

        let mut counter = info("hits", CT::Integer, false);
        counter.counter = true;

        for a in [nullable, defaulted, counter] {
            let name = a.name.clone();
            match plan_column_reconcile(&[], &[a], &[]).as_slice() {
                [ColumnAction::Warn { column, reason }] => {
                    assert_eq!(column, &name);
                    // The remedy has to be IN the message: a warning nobody can
                    // act on is noise, and `dropped(..)` is not guessable.
                    assert!(reason.contains("dropped(\""), "no remedy named: {reason}");
                }
                other => panic!("{name}: expected a warning, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_harmless_orphan_named_in_dropped_is_still_soft_dropped() {
        // The warning is not a substitute for `dropped(..)`; declaring it still
        // removes the column. Without this, demoting harmless orphans to
        // warnings could quietly have disabled the drop path for exactly the
        // columns most likely to use it.
        let a = vec![info("note", CT::Text, true)];
        assert!(matches!(
            plan_column_reconcile(&[], &a, &["note"]).as_slice(),
            [ColumnAction::SoftDrop(n)] if n == "note"));
    }

    #[test]
    fn removed_field_named_in_dropped_is_soft_dropped() {
        let a = vec![info("gone", CT::Text, true)];
        assert!(matches!(
            plan_column_reconcile(&[], &a, &["gone"]).as_slice(),
            [ColumnAction::SoftDrop(n)] if n == "gone"));
    }

    #[test]
    fn renamed_from_emits_rename_and_not_add_plus_drop() {
        let mut d = col("headline", CT::Text, true);
        d.renamed_from = Some("title".into());
        let a = vec![info("title", CT::Text, true)];
        assert!(matches!(plan_column_reconcile(&[d], &a, &[]).as_slice(),
            [ColumnAction::Rename { from, to }] if from == "title" && to == "headline"));
    }

    #[test]
    fn re_declaring_a_soft_dropped_column_revives_it() {
        let d = vec![col("back", CT::Text, true)];
        let mut a = info("back", CT::Text, true);
        a.dropped = true;
        // Nullable, so nothing has to be synthesised: an absent value is a
        // legal one and the rows written while it was dropped read null.
        assert!(matches!(plan_column_reconcile(&d, &[a], &[]).as_slice(),
            [ColumnAction::Revive { column, default: None }] if column == "back"));
    }

    /// Reviving a REQUIRED column synthesises the same zero default an `Add`
    /// would, and for the same reason.
    ///
    /// A soft drop does not stop the table taking writes. A row written while
    /// `back` was dropped carries no value for it, so clearing the tombstone
    /// alone leaves a required column with nothing to resolve those rows
    /// against — the identical hole the `Add` arm closes up front. Delete the
    /// synthesis and this asserts `None` against `Some(Text(""))`.
    #[test]
    fn reviving_a_required_column_synthesises_the_default_add_would() {
        let d = vec![col("back", CT::Text, false)];
        let mut a = info("back", CT::Text, false);
        a.dropped = true;
        match plan_column_reconcile(&d, &[a], &[]).as_slice() {
            [ColumnAction::Revive { column, default }] => {
                assert_eq!(column, "back");
                assert_eq!(default.as_ref(), Some(&Val::Text(String::new())));
            }
            other => panic!("expected a Revive carrying a synthesised default, got {other:?}"),
        }
    }

    /// A revive never replaces a default the developer did not mention.
    ///
    /// The stored column already resolves — that is what a default IS — so
    /// overwriting it with a synthesised zero would change the value every
    /// unwritten row reads, silently, as a side effect of re-declaring a field.
    #[test]
    fn reviving_over_a_stored_default_leaves_it_alone() {
        let d = vec![col("back", CT::Text, false)];
        let mut a = info("back", CT::Text, false);
        a.dropped = true;
        a.default = Some(Val::Text("kept".into()));
        assert!(matches!(plan_column_reconcile(&d, &[a], &[]).as_slice(),
            [ColumnAction::Revive { column, default: None }] if column == "back"));
    }

    /// A declared default wins over both the stored one and the synthesised
    /// one — the revive installs it, so a re-declared column does not need a
    /// second deploy to pick its default up.
    #[test]
    fn reviving_installs_a_declared_default() {
        let mut d = col("back", CT::Text, false);
        d.default = Some(Val::Text("fresh".into()));
        let mut a = info("back", CT::Text, false);
        a.dropped = true;
        a.default = Some(Val::Text("stale".into()));
        match plan_column_reconcile(&[d], &[a], &[]).as_slice() {
            [ColumnAction::Revive { default, .. }] =>
                assert_eq!(default.as_ref(), Some(&Val::Text("fresh".into()))),
            other => panic!("expected a Revive carrying the declared default, got {other:?}"),
        }
    }

    #[test]
    fn reviving_with_a_different_shape_is_a_conflict() {
        let d = vec![col("back", CT::Integer, true)];
        let mut a = info("back", CT::Text, true);
        a.dropped = true;
        assert!(matches!(plan_column_reconcile(&d, &[a], &[]).as_slice(),
            [ColumnAction::Conflict { .. }]));
    }

    #[test]
    fn changed_default_sets_the_default() {
        let mut d = col("a", CT::Text, true);
        d.default = Some(Val::Text("new".into()));
        let mut a = info("a", CT::Text, true);
        a.default = Some(Val::Text("old".into()));
        assert!(matches!(plan_column_reconcile(&[d], &[a], &[]).as_slice(),
            [ColumnAction::SetDefault { column, .. }] if column == "a"));
    }

    #[test]
    fn plan_is_stable_and_order_independent() {
        let d = vec![col("b", CT::Text, true), col("a", CT::Text, true)];
        let p1 = plan_column_reconcile(&d, &[], &[]);
        let mut rev = d.clone(); rev.reverse();
        assert_eq!(p1, plan_column_reconcile(&rev, &[], &[]));
    }

    use crate::store::{ForeignKey, CascadeAction};

    #[test]
    fn not_null_add_gets_a_synthesised_default_so_existing_rows_read() {
        let d = col("required", CT::Text, false);
        match plan_column_reconcile(&[d], &[], &[]).as_slice() {
            [ColumnAction::Add(c)] => assert_eq!(c.default, Some(Val::Text(String::new()))),
            other => panic!("expected Add with default, got {other:?}"),
        }
    }

    #[test]
    fn not_null_add_keeps_a_declared_default_rather_than_overwriting_it() {
        let mut d = col("required", CT::Text, false);
        d.default = Some(Val::Text("pending".into()));
        match plan_column_reconcile(&[d], &[], &[]).as_slice() {
            [ColumnAction::Add(c)] => assert_eq!(c.default, Some(Val::Text("pending".into()))),
            other => panic!("{other:?}"),
        }
    }

    /// The `!c.counter` guard on the synthesis, which nothing else covers.
    ///
    /// An accumulator column is NOT NULL and carries no default, so it matches
    /// the synthesis predicate on every clause but that one — and the store
    /// refuses a counter that carries a default outright (`constraint-violation`),
    /// because a counter's value is read from its own cell and a default could
    /// never be observed. Reattaching one here would therefore not degrade a migration;
    /// it would break `add-column` for every model that declares a counter, on
    /// every deploy. That failure is a long way from this line, so the guard
    /// is pinned at the line.
    #[test]
    fn not_null_counter_add_gets_no_synthesised_default() {
        let mut d = col("hits", CT::Integer, false);
        d.counter = true;
        match plan_column_reconcile(&[d], &[], &[]).as_slice() {
            [ColumnAction::Add(c)] => {
                assert!(c.counter, "the planned add must still be the counter column");
                assert_eq!(
                    c.default, None,
                    "a counter column may not carry a default — the store rejects one",
                );
            }
            other => panic!("expected a single Add, got {other:?}"),
        }
    }

    #[test]
    fn nullable_add_gets_no_synthesised_default() {
        let d = col("optional", CT::Text, true);
        match plan_column_reconcile(&[d], &[], &[]).as_slice() {
            [ColumnAction::Add(c)] => assert_eq!(c.default, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn not_null_foreign_key_add_is_a_conflict_no_value_can_be_invented() {
        let mut d = col("owner_id", CT::Integer, false);
        d.references = Some(ForeignKey {
            references_table: "users".into(),
            references_column: "id".into(),
            on_delete: CascadeAction::NoAction,
            on_update: CascadeAction::NoAction,
        });
        assert!(matches!(plan_column_reconcile(&[d], &[], &[]).as_slice(),
            [ColumnAction::Conflict { column, .. }] if column == "owner_id"));
    }

    /// The `Revive` arm inherits the `Add` arm's foreign-key refusal.
    ///
    /// Both arms reach the same state — a required column that some rows hold
    /// no value for — and a synthesised zero is a dangling reference either
    /// way. Delete the guard on the revive path and this plans a `Revive`
    /// carrying `Some(Integer(0))`, pointing every row written during the drop
    /// window at a row that does not exist.
    #[test]
    fn reviving_a_required_foreign_key_is_a_conflict_like_adding_one() {
        let mut d = col("owner_id", CT::Integer, false);
        d.references = Some(ForeignKey {
            references_table: "users".into(),
            references_column: "id".into(),
            on_delete: CascadeAction::NoAction,
            on_update: CascadeAction::NoAction,
        });
        let mut a = info("owner_id", CT::Integer, false);
        a.dropped = true;
        a.has_references = true;
        match plan_column_reconcile(&[d], &[a], &[]).as_slice() {
            [ColumnAction::Conflict { column, reason }] => {
                assert_eq!(column, "owner_id");
                assert!(
                    reason.contains("row that does not exist"),
                    "the refusal must say why a synthesised value is illegal here: {reason}"
                );
            }
            other => panic!("expected a Conflict, got {other:?}"),
        }
    }

    /// …and only when a value would actually have to be invented. A stored
    /// default names a real row already, so the revive is ordinary.
    #[test]
    fn reviving_a_foreign_key_that_already_has_a_default_is_not_a_conflict() {
        let mut d = col("owner_id", CT::Integer, false);
        d.references = Some(ForeignKey {
            references_table: "users".into(),
            references_column: "id".into(),
            on_delete: CascadeAction::NoAction,
            on_update: CascadeAction::NoAction,
        });
        let mut a = info("owner_id", CT::Integer, false);
        a.dropped = true;
        a.has_references = true;
        a.default = Some(Val::Integer(7));
        assert!(matches!(plan_column_reconcile(&[d], &[a], &[]).as_slice(),
            [ColumnAction::Revive { column, default: None }] if column == "owner_id"));
    }

    /// The message a developer following the nullability conflict would act on
    /// must not send them back into the identical conflict.
    ///
    /// There is no operation anywhere that alters a stored column's
    /// nullability: `add-column` refuses a shape change, and `MigrationCtx`
    /// no-ops on a live column — so writing values into every row leaves
    /// `nullable` exactly where it was. "Backfill and re-declare it required"
    /// was the remedy this arm used to name, and it is unperformable.
    #[test]
    fn the_nullability_conflict_names_no_backfill_remedy() {
        let d = vec![col("note", CT::Text, false)];
        let a = vec![info("note", CT::Text, true)];
        match plan_column_reconcile(&d, &a, &[]).as_slice() {
            [ColumnAction::Conflict { column, reason }] => {
                assert_eq!(column, "note");
                assert!(
                    reason.contains("Option<T>") && reason.contains("dropped(\"note\")"),
                    "both remedies must be named: {reason}"
                );
                assert!(
                    !reason.to_lowercase().contains("backfill"),
                    "a backfill cannot change a stored column's nullability, so naming \
                     one sends the developer back into this same conflict: {reason}"
                );
            }
            other => panic!("expected a Conflict, got {other:?}"),
        }
    }

    #[test]
    fn no_conflicts_produce_no_header_value() {
        assert_eq!(schema_conflict_header_value(&[]), None);
    }

    #[test]
    fn one_conflict_is_the_header_value_verbatim() {
        assert_eq!(
            schema_conflict_header_value(&["rooms.topic: declared Text, stored Integer".to_string()]),
            Some("rooms.topic: declared Text, stored Integer".to_string()),
        );
    }

    #[test]
    fn multiple_conflicts_are_joined_with_a_pipe() {
        assert_eq!(
            schema_conflict_header_value(&[
                "a.x: bad".to_string(),
                "b.y: also bad".to_string(),
            ]),
            Some("a.x: bad | b.y: also bad".to_string()),
        );
    }
}
