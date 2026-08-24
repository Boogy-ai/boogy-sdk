//! Typed query-builder DSL. Slice (a) of the SDK-ergonomics arc.
//! Spec: `docs/superpowers/specs/2026-05-23-typed-query-dsl-design.md`.

use crate::model::Id;
use crate::store::Val;

/// Convert a Rust value into a store `Val`. Implemented for the common
/// primitive types so column comparisons like `M::col.eq(0)` work
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
    // returns None (no match), so `M::flag.eq(false)` would silently
    // match zero rows if this produced Val::Integer.
    fn into_val(self) -> Val { Val::Boolean(self) }
}
impl IntoVal for Val {
    fn into_val(self) -> Val { self }
}
/// A timestamp compares as the integer it is stored as.
impl IntoVal for crate::model::Timestamp {
    fn into_val(self) -> Val {
        Val::Integer(self.get())
    }
}
/// A nullable column's value. `None` is SQL NULL.
impl<T: IntoVal> IntoVal for Option<T> {
    fn into_val(self) -> Val {
        match self {
            Some(v) => v.into_val(),
            None => Val::Null,
        }
    }
}
impl<T> IntoVal for Id<T> {
    fn into_val(self) -> Val { Val::Integer(self.get() as i64) }
}

use crate::pagination::Cursor;
use crate::store::{AggFilter, AggSpec, Filter, FilterOp, SortDir};

/// What a query can be ordered BY.
///
/// Exists so there is one ordering verb rather than one per kind of thing you
/// might order by. A column and an aggregate are both expressions in SQL's
/// `ORDER BY`, and a developer should not have to discover that this platform
/// spells the second one differently.
#[derive(Debug, Clone, PartialEq)]
pub enum SortTerm {
    Column(String, SortDir),
    Aggregate(AggSpec, SortDir),
}

/// Anything [`QueryArgs::order_by`] accepts.
pub trait OrderKey {
    fn into_sort(self, dir: SortDir) -> SortTerm;
}

impl OrderKey for &str {
    fn into_sort(self, dir: SortDir) -> SortTerm {
        SortTerm::Column(self.to_string(), dir)
    }
}

impl OrderKey for String {
    fn into_sort(self, dir: SortDir) -> SortTerm {
        SortTerm::Column(self, dir)
    }
}

/// Ordering by an aggregate. On a grouped query this ranks the GROUPS by their
/// own total; on a row query it ranks the rows by a total their children carry,
/// resolved through the relation the child declared.
impl OrderKey for AggSpec {
    fn into_sort(self, dir: SortDir) -> SortTerm {
        SortTerm::Aggregate(self, dir)
    }
}

/// Names for aggregates, so `having` and `order_by` can refer to one the
/// query already selected.
///
/// SQL has the same duality: `sum(x)` in the select list is what is computed,
/// and `sum(x)` in `HAVING` names it again. These build the identical spec a
/// selector registers, which is what makes
/// `.sum("amount").having(agg::sum("amount"), ...)` refer to one aggregate
/// rather than two.
pub mod agg {
    use crate::store::{AggFunc, AggSpec};

    pub fn sum(column: &str) -> AggSpec {
        AggSpec { kind: AggFunc::Sum, column: Some(column.to_string()) }
    }
    pub fn avg(column: &str) -> AggSpec {
        AggSpec { kind: AggFunc::Avg, column: Some(column.to_string()) }
    }
    pub fn min(column: &str) -> AggSpec {
        AggSpec { kind: AggFunc::Min, column: Some(column.to_string()) }
    }
    pub fn max(column: &str) -> AggSpec {
        AggSpec { kind: AggFunc::Max, column: Some(column.to_string()) }
    }
    /// `COUNT(*)` — rows, not values, so it takes no column.
    pub fn count_all() -> AggSpec {
        AggSpec { kind: AggFunc::CountAll, column: None }
    }
}

// ---------------------------------------------------------------------------
// The row ceiling, as a type.
// ---------------------------------------------------------------------------

/// Typestate: this query has not stated how many rows it may materialize.
///
/// The state every query starts in. `Query::on(..)` returns one, and it stays
/// one through every filter, ordering and counter merge — none of those bound
/// anything. The materializing terminals (`fetch_all`, `fetch_all_with_total`)
/// are not defined on it, so "fetch everything and hope the table is small" has
/// no spelling.
#[derive(Debug)]
pub enum Unbounded {}

/// Typestate: `.limit(n)` has stated a ceiling on the rows this query may
/// materialize.
///
/// The only way in is [`crate::query::QueryArgs::limit`]'s wrapper on the
/// macro-emitted `Query`, so a bounded query is one whose author wrote the
/// number down at the call site.
#[derive(Debug)]
pub enum Bounded {}

/// Typestate: `.group_by(col)` has been called and no ceiling has been stated.
///
/// The state that exists because **group cardinality is not row count**. An
/// UNGROUPED aggregate — `SELECT sum(x) FROM t` — returns exactly one group
/// over an empty table and exactly one over a billion rows, so it is bounded by
/// construction and needs no ceiling. The moment a `group_by(col)` is added the
/// result gains one item per DISTINCT VALUE of `col`, and that quantity is
/// data-dependent: `group_by(status)` is three, `group_by(user_id)` is one per
/// tenant user, and the platform cannot tell which from the query.
///
/// So the ceiling attaches to `group_by`, not to `fetch_groups`. `.limit(n)`
/// moves out of this state exactly as it moves out of [`Unbounded`], and
/// [`fetch_group_page`] is the resumable form for a listing that grows.
///
/// [`fetch_group_page`]: crate::wit_glue
#[derive(Debug)]
pub enum Grouped {}

mod bound_sealed {
    pub trait Sealed {}
    impl Sealed for super::Bounded {}
}

mod group_sealed {
    pub trait Sealed {}
    impl Sealed for super::Bounded {}
    impl Sealed for super::Unbounded {}
}

/// A query state whose GROUP COUNT is bounded, so it may materialize one item
/// per group into the guest's heap.
///
/// Implemented for [`Unbounded`] (no `group_by` — an aggregate over the whole
/// filtered set is exactly one group) and for [`Bounded`] (a ceiling was
/// stated), and — deliberately — **not** for [`Grouped`].
///
/// This is a different predicate from [`BoundedRead`] and must stay one. Making
/// `fetch_groups` require `BoundedRead` would force `.limit(1)` onto every
/// `SELECT sum(x) FROM t`, which teaches the reader that the one-group answer
/// is a truncation risk when it is not. Making it require nothing at all leaves
/// `group_by(user_id)` returning a row per user into a 32 MiB heap.
#[diagnostic::on_unimplemented(
    message = "this query groups by a column but states no ceiling on how many groups it \
               may materialize",
    label = "needs a `.limit(n)` after the `.group_by(..)`",
    note = "`group_by(col)` yields one item per DISTINCT VALUE of `col`. That count is a \
            property of the data, not of the query: `group_by(status)` may be three and \
            `group_by(user_id)` one per tenant user, and nothing in the query says which.",
    note = "If the number of groups is bounded by construction — a fixed status set, the \
            options on one poll — say so with `.limit(n)`.",
    note = "If it grows with the tenant, add `.order(agg::..().desc()).limit(n).cursor(token)` \
            and end on `.fetch_group_page(|g| ..)`, which returns the token that continues the \
            ranked listing.",
    note = "An UNGROUPED aggregate needs none of this: it is one group whatever the table \
            size — use `.fetch_one_group()`."
)]
pub trait BoundedGroups: group_sealed::Sealed {}

impl BoundedGroups for Unbounded {}
impl BoundedGroups for Bounded {}

/// What `.group_by(..)` does to a query's state.
///
/// An associated type rather than a plain `-> Query<Grouped>` because the
/// transition is not unconditional: a query that ALREADY stated `.limit(n)`
/// stays [`Bounded`], so `.limit(20).group_by(col)` and
/// `.group_by(col).limit(20)` are the same query in the same state. Only a
/// query that has stated no ceiling moves into [`Grouped`].
pub trait AfterGroupBy {
    /// The state a query is in once `.group_by(..)` has been called on it.
    type Out;
}

impl AfterGroupBy for Unbounded {
    type Out = Grouped;
}
impl AfterGroupBy for Bounded {
    type Out = Bounded;
}
impl AfterGroupBy for Grouped {
    type Out = Grouped;
}

/// A query state that may materialize rows into the guest's heap.
///
/// Implemented for [`Bounded`] and — deliberately, load-bearingly — **not** for
/// [`Unbounded`]. `wit_glue!` emits the two row-materializing terminals under
/// `impl<B: BoundedRead> Query<B>`, so a query that never stated a limit has no
/// `fetch_all` to call and the mistake is a compile error rather than a page
/// the host silently truncates.
///
/// Sealed: the point is that the set of bounded states is exactly one, and a
/// downstream crate that could add its own would reopen the hole.
#[diagnostic::on_unimplemented(
    message = "this query states no row limit, so it cannot materialize rows",
    label = "needs a `.limit(n)` earlier in the chain",
    note = "Without a limit the store answers with a page of its own choosing \
            (BOOGY_STORE_MAX_PAGE_ROWS, default 1000 rows) and returns no cursor and no \
            total, so a truncated answer is indistinguishable from a complete one.",
    note = "If the set is bounded by construction — a top-N, an `is_in` over N ids, an \
            existence probe — say so with `.limit(n)`.",
    note = "If it grows with the tenant, use `.limit(n).cursor(token)` and end on \
            `.fetch_page(|row| ..)`, which returns the token that continues the listing."
)]
pub trait BoundedRead: bound_sealed::Sealed {}

impl BoundedRead for Bounded {}

/// Builder state for the typed query DSL.
///
/// Holds the filters, sort, pagination, and keyset configuration that
/// terminal methods (`fetch_one`/`fetch_all`/`count`/`fetch_page`,
/// emitted by `wit_glue!`) consume. All fields are public so the
/// macro-emitted `Query` newtype can read them when constructing WIT
/// calls.
///
/// User code typically goes through the macro-emitted `Query` wrapper
/// (`Query::on(Post::TABLE).filter(Post::room_id.eq(v)).fetch_all()`); `QueryArgs` is
/// the underlying data type that holds the builder state and exposes
/// all the chainable methods that don't touch WIT.
#[derive(Debug, Clone)]
pub struct QueryArgs {
    pub table: String,
    pub limit: Option<usize>,
    pub offset: u32,
    pub cursor: Option<Cursor>,
    /// The predicate built by [`QueryArgs::filter`].
    pub predicate: Option<crate::expr::Expr>,
    /// The orderings built by [`QueryArgs::order`].
    pub ordering: Vec<crate::expr::Order>,
    /// Columns to group by. Empty = ungrouped, which yields exactly one row.
    pub group_by: Vec<String>,
    /// The opaque token this listing resumes from, kept verbatim. A ranked
    /// group page needs it whole; a row page uses [`Self::cursor`] instead.
    pub cursor_token: Option<String>,
    /// Set by `fetch_group_page`, never by the caller: it says this listing
    /// will be walked, which is what earns it a pinned ordering. `fetch_groups`
    /// leaves it off so a one-shot "top 10" pays nothing for a continuation it
    /// will not use.
    pub want_group_cursor: bool,
    /// Aggregates to compute, in selection order — results come back
    /// positional against this list.
    pub aggregates: Vec<AggSpec>,
    /// Predicates over aggregate values, applied after grouping.
    pub having: Vec<AggFilter>,
    /// Counters to merge into this query's rows — built by
    /// [`QueryArgs::with_counter`]. Empty means what it always meant before
    /// this field existed: no counter cell is read.
    pub counters: Vec<crate::store::CounterRequest>,
}

impl QueryArgs {
    /// Start a new query against `table`.
    pub fn on(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            limit: None,
            offset: 0,
            cursor: None,
            predicate: None,
            ordering: Vec::new(),
            group_by: Vec::new(),
            cursor_token: None,
            want_group_cursor: false,
            aggregates: Vec::new(),
            having: Vec::new(),
            counters: Vec::new(),
        }
    }

    // -- Aggregates --
    //
    // `count_all`, not `count`: `count()` is already a TERMINAL returning
    // `Result<u64>`, and one name cannot be both that and a `-> Self` selector.
    // Reusing it would make the same call mean two things depending on what
    // followed — which is precisely the ambiguity this surface is supposed not
    // to have.

    /// Group rows by a column. Repeat for a composite grouping.
    pub fn group_by(mut self, column: &str) -> Self {
        self.group_by.push(column.to_string());
        self
    }

    /// Total a column across each group.
    pub fn sum(mut self, column: &str) -> Self {
        self.aggregates.push(agg::sum(column));
        self
    }

    /// Mean of a column across each group. Derived from the sum and the count;
    /// NULL over a group with no non-null value, never a division by zero.
    pub fn avg(mut self, column: &str) -> Self {
        self.aggregates.push(agg::avg(column));
        self
    }

    pub fn min(mut self, column: &str) -> Self {
        self.aggregates.push(agg::min(column));
        self
    }

    pub fn max(mut self, column: &str) -> Self {
        self.aggregates.push(agg::max(column));
        self
    }

    /// Count the rows in each group.
    pub fn count_all(mut self) -> Self {
        self.aggregates.push(agg::count_all());
        self
    }

    /// Keep only groups whose aggregate satisfies a predicate.
    ///
    /// `having` filters the groups that come OUT of the aggregation; `where_*`
    /// selects the rows that go IN. The aggregate named here must be one the
    /// query selected — build it with the matching [`agg`] helper.
    pub fn having<V: IntoVal>(mut self, aggregate: AggSpec, op: FilterOp, val: V) -> Self {
        self.having.push(AggFilter { agg: aggregate, op, val: val.into_val() });
        self
    }

    // -- Counters --
    //
    // Opt-in, named at the call site. A counter's value used to arrive in a
    // row automatically whenever the table declared one — an inferred
    // behaviour invisible at the point a developer reads the query. `.with_counter`
    // is now the ONLY way a counter's cell is read: without it, a query reads
    // no counter cells at all, however many counters the tables it touches
    // declare.

    /// Merge a counter's cells into this query's rows.
    ///
    /// The cells for every row in the answer are read as ONE BATCH per live
    /// counter column, inside the same transaction the page read itself
    /// uses — never a round trip per row. Naming a counter here is per-TABLE,
    /// not per-column: it merges every live counter column the table
    /// declares, not just the one named (the store has no cheaper way to
    /// filter to a subset yet — see the store's own `find_rows_with_counters`
    /// doc). So a page of N rows on a table with C live counter columns costs
    /// at most C batches, each reading at most N cells — C·N in the worst
    /// case, not N. On the common one-counter table this is N, but the bound
    /// is C·N, and calling `.with_counter` more than once does not add more:
    /// every live counter column is already merged by the first call.
    ///
    /// `name` is the counter's declared name (`Counter::NAME` — `"<table>.<column>"`
    /// for an `of = Model` counter, the struct's own name or an explicit
    /// override for a standalone one). `key_cols` names the columns whose
    /// PER-ROW values supply the counter's key: pass `&[]` for a counter keyed
    /// by the row's own id, which every row carries; for an arbitrary-key
    /// counter, every named column must be one this query already establishes
    /// a value for on each row it returns, or [`QueryArgs::counter_build_refusal`]
    /// refuses before anything runs, naming the missing column.
    ///
    /// Repeat for more than one counter to name each one explicitly at the
    /// call site (self-documenting even though, today, one call already
    /// brings every counter on the table).
    pub fn with_counter(mut self, name: &str, key_cols: &[&str]) -> Self {
        self.counters.push(crate::store::CounterRequest {
            name: name.to_string(),
            key_cols: key_cols.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// Why `.with_counter(..)` cannot be served, if it cannot.
    ///
    /// Lives here, not in the generated terminal, so it is testable without a
    /// guest — the same reason [`QueryArgs::single_group_refusal`] does.
    ///
    /// An arbitrary-key counter's key columns must each be one this query
    /// already touches — named in a filter, a group-by, or `_id` itself,
    /// which every row carries by construction. Nothing else here can supply
    /// a per-row value for an unrelated column, so a counter whose key names
    /// one is refused BY NAME rather than either merging a key built from
    /// nothing (silently wrong) or panicking deep inside the merge (silently
    /// absent, then a crash nobody could trace back to the query that asked).
    pub fn counter_build_refusal(&self) -> Option<String> {
        let (leaves, groups) = match self.lower_predicate() {
            Ok(v) => v,
            // A malformed predicate is its own, separate refusal — count_filters/
            // fetch terminals surface it; this check has nothing useful to add.
            Err(_) => return None,
        };
        let mut touched: std::collections::HashSet<&str> =
            leaves.iter().map(|f| f.column.as_str()).collect();
        for g in &groups {
            touched.extend(g.iter().map(|f| f.column.as_str()));
        }
        touched.extend(self.group_by.iter().map(|s| s.as_str()));
        touched.insert("_id");
        for req in &self.counters {
            for col in &req.key_cols {
                if !touched.contains(col.as_str()) {
                    return Some(::std::format!(
                        "with_counter(\"{}\", ..) names key column \"{col}\" but this \
                         query never establishes a value for it on every row — filter \
                         or group by \"{col}\" so each row can supply the counter's \
                         key, or drop it from with_counter's key_cols if this counter \
                         does not actually need it.",
                        req.name
                    ));
                }
            }
        }
        None
    }


    /// Why this query cannot be read as a single group, if it cannot.
    ///
    /// Lives here rather than in the generated terminal so it is testable
    /// without a guest — a guard whose only exercise is a macro expansion is a
    /// guard nobody has watched fail.
    pub fn single_group_refusal(&self) -> Option<String> {
        if !self.group_by.is_empty() {
            return Some(::std::format!(
                "fetch_one_group is for a query with no group_by, and this one \
                 groups by {}. Use fetch_groups, which returns every group.",
                self.group_by.join(", ")
            ));
        }
        if self.aggregates.is_empty() {
            return Some(
                "this query selects no aggregates; add .sum(..)/.count_all()/… \
                 before reading a group"
                    .to_string(),
            );
        }
        None
    }

    // -- Sort --

    /// The aggregate ordering a ROW listing carries, if any.
    ///
    /// On a grouped query an aggregate ordering ranks the groups by their own
    /// total; on a row query it ranks the rows by a total their children carry.
    /// Same clause, and which one it means is decided by whether the query
    /// groups — exactly as in SQL.
    pub fn related_order(&self) -> Option<(AggSpec, SortDir)> {
        if !self.group_by.is_empty() {
            return None;
        }
        self.agg_sort()
    }

    /// The COLUMN orderings, in declaration order.
    ///
    /// Reads `ordering` and nothing else. There used to be a second field
    /// (`sort`) written by the pre-expression `order_by` verb, and a third
    /// retired-spelling: `order_by_agg` was deleted 2026-08-17; there is
    /// one ordering field, read as `order-term` values. Kept because "one
    /// clause, one field" is a conclusion, and this is its reason.
    /// (`order_by_agg`) for the aggregate case; both outlived their only writer
    /// when those verbs were deleted. A field nothing writes but something
    /// still reads is not a type error, so it compiled — and `fetch_group_page`
    /// reading the aggregate one directly refused every ranked page until the
    /// merge gate caught it. One clause, one field.
    pub fn column_sorts(&self) -> Vec<(String, SortDir)> {
        self.ordering
            .iter()
            .filter_map(|o| match o {
                crate::expr::Order::Column(c, d) => Some((c.clone(), *d)),
                crate::expr::Order::Aggregate(..) => None,
            })
            .collect()
    }

    /// The aggregate ordering, if this query ranks by one.
    pub fn agg_sort(&self) -> Option<(AggSpec, SortDir)> {
        for o in &self.ordering {
            if let crate::expr::Order::Aggregate(a, d) = o {
                return Some((a.clone(), *d));
            }
        }
        None
    }

    /// Flatten the predicate onto the wire's shape: a conjunction of leaves,
    /// plus at most one top-level disjunction.
    ///
    /// The wire carries `filters` AND-ed with an OR-of-ANDs (`or_groups`), which
    /// is one level of boolean structure. An expression tree can nest deeper, so
    /// this REFUSES what it cannot represent rather than dropping a clause —
    /// a silently-widened predicate returns rows the caller excluded, and on a
    /// scoped query that means somebody else's rows.
    pub fn lower_predicate(
        &self,
    ) -> Result<(Vec<Filter>, Vec<Vec<Filter>>), crate::error::ApiError> {
        let (mut leaves, mut groups) = (Vec::new(), Vec::new());
        if let Some(e) = &self.predicate {
            lower_into(e, &mut leaves, &mut groups)?;
        }
        Ok((leaves, groups))
    }

    /// Keep the rows this expression matches.
    ///
    /// One verb, because a predicate is one thing. Repeated calls AND together,
    /// the way every mainstream data library behaves — `.filter(a).filter(b)`
    /// and `.filter(a.and(b))` are the same query.
    ///
    /// ```ignore
    /// let room_id = 7_i64;
    /// Query::on(Post::TABLE)
    ///     .filter(Post::room_id.eq(room_id))
    ///     .filter(Post::deleted_at.is_null())
    ///     .limit(50)          // `fetch_all` has no unbounded form
    ///     .fetch_all()?;
    /// ```
    pub fn filter(mut self, e: crate::expr::Expr) -> Self {
        self.predicate = Some(match self.predicate.take() {
            Some(prev) => prev.and(e),
            None => e,
        });
        self
    }

    /// Order the result.
    ///
    /// Takes an ordering EXPRESSION — `Post::created_at.desc()` for a column, or
    /// `sum(Vote::direction).desc()` to rank by a total the children carry. Both
    /// are `ORDER BY`, so both are this call.
    ///
    /// The ordering is also what a cursor is built from, which is why there is
    /// no separate "page by this column" verb to keep in step with it.
    pub fn order(mut self, o: crate::expr::Order) -> Self {
        self.ordering.push(o);
        self
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
    /// Where a listing resumes.
    ///
    /// Takes the opaque token a client round-trips, straight from the query
    /// string — no `decode` at the call site. One verb for both kinds of
    /// listing: a row page seeks from the position inside the token, a ranked
    /// group page uses it to pin the generation of the ordering it started in.
    ///
    /// ```ignore
    /// // `token` is whatever the previous page's `next_cursor` held.
    /// let token: Option<String> = None;
    /// Query::on(Post::TABLE)
    ///     .order(Post::created_at.desc())
    ///     .cursor(token)
    ///     .fetch_page(|r| r.id())?;
    /// ```
    ///
    /// A token that cannot be read is KEPT rather than discarded. Dropping it
    /// would quietly restart the listing while the caller believed it was
    /// continuing one; keeping it lets the engine refuse.
    pub fn cursor(mut self, token: Option<String>) -> Self {
        // Decoded for the row path AND kept verbatim for the ranked one. The
        // two listings resume from different things — a row key, and the
        // generation of an ordering — and only the terminal knows which this
        // query is.
        self.cursor = token.as_deref().and_then(crate::pagination::decode);
        self.cursor_token = token;
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
    ///
    /// **Lowers the predicate.** It used to return `base_filters` directly,
    /// which only the pre-expression `where_*` verbs ever populated — so once
    /// those were deleted, a `.filter(..).count()` would have sent NO filters
    /// and counted the whole table. Silently: no compile error, no runtime
    /// error, just a number far too large. Nothing in the tree had hit it (every
    /// `.count()` here is a deliberate full-table count), which is exactly why
    /// it was worth closing before something did.
    ///
    /// The WIT count op takes a conjunction only, so an OR predicate is
    /// **refused** rather than dropped. The previous contract dropped
    /// `or_groups` silently and said so in its own doc comment — a documented
    /// wrong answer is still a wrong answer, and `count` is the one terminal
    /// whose result carries no evidence of what it counted.
    pub fn count_filters(&self) -> Result<Vec<Filter>, crate::error::ApiError> {
        let (leaves, groups) = self.lower_predicate()?;
        if !groups.is_empty() {
            return Err(crate::error::ApiError::internal(
                "count() cannot express an OR predicate: the store's count op                  takes a conjunction only, so counting this query would count                  rows the predicate excludes. Count the length of a fetch, or                  restructure the predicate with `is_in`.",
            ));
        }
        Ok(leaves)
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
        let (leaves, groups) = q.lower_predicate().expect("empty is representable");
        assert!(leaves.is_empty() && groups.is_empty(), "a fresh query has no predicate");
        assert!(q.ordering.is_empty(), "and no ordering");
        assert!(q.limit.is_none());
        assert_eq!(q.offset, 0);
        assert!(q.cursor.is_none());
    }

    /// Orderings compose in declaration order, and each keeps its own
    /// direction. Direction is the property no type check protects — `.asc()`
    /// compiles perfectly where `.desc()` was meant — so it is asserted
    /// positionally rather than by counting terms.
    #[test]
    fn order_chain_builds_composite_sort_in_declaration_order() {
        let score: crate::expr::Col<i64> = crate::expr::Col::new("score");
        let id: crate::expr::Col<i64> = crate::expr::Col::new("_id");
        let q = QueryArgs::on("t").order(score.desc()).order(id.asc());
        assert_eq!(
            q.column_sorts(),
            vec![("score".to_string(), SortDir::Desc), ("_id".to_string(), SortDir::Asc)],
        );
    }

    // -- lower_predicate: the expression tree onto the wire's shape ----------
    //
    // These replace `or_builds_or_group` / `nested_or_flattens`, which tested
    // the deleted `.or(|q| ..)` builder. `lower_predicate` had NO tests of its
    // own, so this is coverage the migration added rather than moved: it is the
    // function that decides what the store actually receives, and it REFUSES
    // what it cannot represent instead of dropping a clause — a silently
    // widened predicate returns rows the caller excluded, and on a scoped query
    // that means somebody else's rows.

    fn col(n: &'static str) -> crate::expr::Col<i64> { crate::expr::Col::new(n) }

    #[test]
    fn repeated_filters_lower_to_one_conjunction() {
        let q = QueryArgs::on("t")
            .filter(col("a").eq(1))
            .filter(col("b").gt(2));
        let (leaves, groups) = q.lower_predicate().expect("a conjunction is representable");
        assert_eq!(leaves.len(), 2, "repeated .filter() calls AND together");
        assert_eq!(leaves[0].op, FilterOp::Eq);
        assert_eq!(leaves[1].op, FilterOp::Gt);
        assert!(groups.is_empty(), "no OR structure, so no or-groups");
    }

    /// The wire is an **OR-of-AND**: `or_groups` is a list of conjunctions and a
    /// row matches when ALL(filters) AND ANY(group: ALL(group)). So each ARM of
    /// an `Or` becomes its own group — `a OR b` is two groups of one filter, not
    /// one group of two. Asserted because the opposite reading is the intuitive
    /// one and it is wrong: this test was first written expecting one group.
    #[test]
    fn each_or_arm_becomes_its_own_conjunction_group() {
        let q = QueryArgs::on("t")
            .filter(col("kind").eq(1))
            .filter(col("a").eq(2).or(col("b").eq(3)));
        let (leaves, groups) = q.lower_predicate().expect("one OR is representable");
        assert_eq!(leaves.len(), 1, "the AND-leaf stays a leaf");
        assert_eq!(groups.len(), 2, "one group per OR arm");
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 1);
    }

    /// And an arm that IS a conjunction keeps its terms together in one group —
    /// which is what makes the shape an OR *of AND* rather than a flat OR.
    #[test]
    fn an_or_arm_that_is_a_conjunction_stays_one_group() {
        let q = QueryArgs::on("t")
            .filter(col("a").eq(2).and(col("c").eq(9)).or(col("b").eq(3)));
        let (_, groups) = q.lower_predicate().expect("representable");
        assert_eq!(groups.len(), 2, "still one group per arm");
        assert_eq!(groups[0].len(), 2, "the conjunctive arm keeps both terms");
        assert_eq!(groups[1].len(), 1);
    }

    /// The wire carries ONE level of boolean structure. Two independent ORs
    /// would be evaluated as one, so this refuses rather than answering a
    /// different question than the one written.
    #[test]
    fn two_separate_or_groups_are_refused_not_merged() {
        let q = QueryArgs::on("t")
            .filter(col("a").eq(1).or(col("b").eq(2)))
            .filter(col("c").eq(3).or(col("d").eq(4)));
        let err = q.lower_predicate().expect_err("two OR groups cannot be represented");
        assert!(
            format!("{err:?}").contains("two separate OR groups"),
            "the refusal must name the problem: {err:?}",
        );
    }

    #[test]
    fn pagination_setters_round_trip() {
        let q = QueryArgs::on("t").limit(20).offset(40);
        assert_eq!(q.limit, Some(20));
        assert_eq!(q.offset, 40);
    }


    /// **One cursor verb, and no `decode` at the call site.**
    ///
    /// Every paging handler wrote `q.cursor.as_deref().and_then(decode)` before
    /// passing it in — ceremony the SDK can do itself, and which a newcomer has
    /// no way to guess. Worse, the group path did NOT need it, so the two kinds
    /// of listing were paged with differently-shaped code for no reason a
    /// developer could see.
    #[test]
    // `encode`/`decode` are the pagination module's own codec.
    fn cursor_accepts_the_opaque_token_a_client_round_trips() {
        let token = crate::pagination::encode(&Cursor::id_only("99"));
        let q = QueryArgs::on("t").cursor(Some(token.clone()));

        assert_eq!(
            q.cursor,
            crate::pagination::decode(&token),
            "a row listing gets the decoded position, without the handler decoding it"
        );
        assert_eq!(
            q.cursor_token.as_deref(),
            Some(token.as_str()),
            "a group listing gets the token verbatim, because its position is the              generation of an ordering and not a row key"
        );
    }


    /// Nonsense from a client is not a position. It must not silently start the
    /// listing over, which is what a `None` here would do.
    #[test]
    fn an_unreadable_token_is_kept_for_the_engine_to_refuse() {
        let q = QueryArgs::on("t").cursor(Some("not-a-cursor".to_string()));
        assert!(q.cursor.is_none(), "nothing to seek from");
        assert_eq!(
            q.cursor_token.as_deref(),
            Some("not-a-cursor"),
            "kept, so a ranked listing refuses it rather than restarting"
        );
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

    /// **`count()` must send the predicate the caller wrote.**
    ///
    /// `count_filters` used to return `base_filters`, which only the deleted
    /// `where_*` verbs populated — so a `.filter(..).count()` would have sent
    /// nothing and counted the whole table, silently. This is the guard.
    #[test]
    fn count_filters_carries_the_lowered_predicate() {
        let q = QueryArgs::on("t").filter(col("kind").eq(1)).filter(col("score").gt(50));
        let f = q.count_filters().expect("a conjunction is countable");
        assert_eq!(f.len(), 2, "both filters must reach the count op");
        assert_eq!(f[0].column, "kind");
        assert_eq!(f[1].op, FilterOp::Gt);
    }

    /// And it must REFUSE what it cannot express, rather than drop it. The old
    /// contract dropped `or_groups` silently and documented that it did; a
    /// count is the one terminal whose answer carries no evidence of what it
    /// counted, so a dropped clause is undetectable downstream.
    #[test]
    fn count_refuses_an_or_predicate_instead_of_dropping_it() {
        let q = QueryArgs::on("t").filter(col("a").eq(1).or(col("b").eq(2)));
        let err = q.count_filters().expect_err("an OR cannot be counted");
        assert!(
            format!("{err:?}").contains("OR predicate"),
            "the refusal must say why: {err:?}",
        );
    }

    #[test]
    fn count_filters_ignores_sort_and_page() {
        let q = QueryArgs::on("t").filter(col("kind").eq(1)).limit(5).offset(10);
        let f = q.count_filters().expect("countable");
        assert_eq!(f.len(), 1, "sort and page are not filters and must not appear");
        assert_eq!(f[0].column, "kind");
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

    // -- with_counter: opt-in, named at the call site ------------------------

    #[test]
    fn a_fresh_query_asks_for_no_counters() {
        let q = QueryArgs::on("rooms");
        assert!(q.counters.is_empty(), "nothing arrives unasked");
    }

    #[test]
    fn with_counter_records_the_name_and_key_cols() {
        let q = QueryArgs::on("rooms").with_counter("rooms.post_count", &[]);
        assert_eq!(q.counters.len(), 1);
        assert_eq!(q.counters[0].name, "rooms.post_count");
        assert!(q.counters[0].key_cols.is_empty());
    }

    #[test]
    fn repeated_with_counter_calls_accumulate_in_declaration_order() {
        let q = QueryArgs::on("posts")
            .with_counter("posts.vote_score", &[])
            .with_counter("room_post_count", &["room_id"]);
        assert_eq!(q.counters.len(), 2, "each call is its own request");
        assert_eq!(q.counters[0].name, "posts.vote_score");
        assert_eq!(q.counters[1].name, "room_post_count");
        assert_eq!(q.counters[1].key_cols, vec!["room_id".to_string()]);
    }

    #[test]
    fn an_of_model_counter_with_no_key_cols_is_never_refused() {
        // Keyed by the row's own id, which every row carries — nothing to check.
        let q = QueryArgs::on("rooms").with_counter("rooms.post_count", &[]);
        assert_eq!(q.counter_build_refusal(), None);
    }

    #[test]
    fn an_arbitrary_key_counter_keyed_by_id_is_never_refused() {
        let q = QueryArgs::on("rooms").with_counter("hits", &["_id"]);
        assert_eq!(q.counter_build_refusal(), None, "_id is always available");
    }

    #[test]
    fn an_arbitrary_key_counter_whose_column_is_filtered_is_not_refused() {
        let room_id: crate::expr::Col<i64> = crate::expr::Col::new("room_id");
        let q = QueryArgs::on("posts")
            .filter(room_id.eq(7))
            .with_counter("room_daily_posts", &["room_id"]);
        assert_eq!(q.counter_build_refusal(), None);
    }

    #[test]
    fn an_arbitrary_key_counter_whose_column_is_grouped_by_is_not_refused() {
        let q = QueryArgs::on("posts")
            .group_by("room_id")
            .with_counter("room_daily_posts", &["room_id"]);
        assert_eq!(q.counter_build_refusal(), None);
    }

    /// **The build-failure requirement.** A key column this query never
    /// touches must be refused, by name, before anything runs — not a
    /// silently-absent value and not a runtime store error.
    #[test]
    fn an_arbitrary_key_counter_naming_an_untouched_column_is_refused_by_name() {
        let q = QueryArgs::on("posts").with_counter("room_daily_posts", &["room_id", "day"]);
        let msg = q
            .counter_build_refusal()
            .expect("neither room_id nor day is established by this query");
        assert!(msg.contains("room_daily_posts"), "must name the counter: {msg}");
        assert!(msg.contains("room_id"), "must name the missing column: {msg}");
    }

    /// One column present, the other missing: the refusal must name the ONE
    /// that is actually absent, not just "something is wrong".
    #[test]
    fn a_partially_touched_arbitrary_key_counter_names_the_missing_column_specifically() {
        let room_id: crate::expr::Col<i64> = crate::expr::Col::new("room_id");
        let q = QueryArgs::on("posts")
            .filter(room_id.eq(7))
            .with_counter("room_daily_posts", &["room_id", "day"]);
        let msg = q.counter_build_refusal().expect("day is still untouched");
        assert!(msg.contains("day"), "must name the specific missing column: {msg}");
    }

    #[test]
    fn a_query_with_no_with_counter_call_has_no_refusal() {
        let q = QueryArgs::on("posts");
        assert_eq!(q.counter_build_refusal(), None, "nothing was asked for, nothing to refuse");
    }
}

#[cfg(test)]
mod aggregate_builder_tests {
    use super::*;
    use crate::store::{AggFunc, FilterOp, SortDir};

    fn q() -> QueryArgs {
        QueryArgs::on("orders")
    }

    #[test]
    fn group_by_accumulates_in_declaration_order() {
        let a = q().group_by("customer").group_by("region");
        assert_eq!(a.group_by, vec!["customer".to_string(), "region".to_string()]);
    }

    #[test]
    fn each_selector_registers_one_aggregate_in_order() {
        let a = q().sum("amount").count_all().avg("amount").min("amount").max("amount");
        let kinds: Vec<_> = a.aggregates.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![AggFunc::Sum, AggFunc::CountAll, AggFunc::Avg, AggFunc::Min, AggFunc::Max],
            "order is load-bearing: results come back positional against this list",
        );
        assert_eq!(a.aggregates[0].column.as_deref(), Some("amount"));
        assert_eq!(
            a.aggregates[1].column, None,
            "COUNT(*) counts rows, not a column, and giving it one would invite \
             the belief that it skips nulls",
        );
    }

    #[test]
    fn having_records_a_predicate_against_an_aggregate() {
        let a = q().sum("amount").having(agg::sum("amount"), FilterOp::Gt, 100);
        assert_eq!(a.having.len(), 1);
        assert_eq!(a.having[0].agg, agg::sum("amount"));
        assert_eq!(a.having[0].op, FilterOp::Gt);
    }

    /// **One verb.** `ORDER BY` in SQL takes an expression — a column or an
    /// aggregate — and a developer reaching for a column sort should not have to
    /// discover that this platform spells the aggregate case differently. The
    /// two land in different places internally (a grouped result has no columns
    /// to sort by beyond its keys), and `.order(..)` is what hides that.
    #[test]
    fn order_takes_a_column_or_an_aggregate_through_one_verb() {
        let created: crate::expr::Col<i64> = crate::expr::Col::new("created_at");
        let by_col = QueryArgs::on("t").order(created.desc());
        assert_eq!(by_col.column_sorts(), vec![("created_at".to_string(), SortDir::Desc)]);
        assert!(by_col.agg_sort().is_none(), "a column is not an aggregate sort");

        let by_agg = QueryArgs::on("t").order(agg::sum("direction").desc());
        let (spec, dir) = by_agg.agg_sort().expect("an aggregate ordering");
        assert_eq!(spec, agg::sum("direction"));
        assert_eq!(dir, SortDir::Desc);
        assert!(by_agg.column_sorts().is_empty(), "an aggregate is not a column sort");
    }

    /// Both kinds on one query, each landing where it belongs.
    #[test]
    fn a_query_can_order_by_a_column_and_an_aggregate() {
        let created: crate::expr::Col<i64> = crate::expr::Col::new("created_at");
        let a = QueryArgs::on("t").order(agg::count_all().desc()).order(created.asc());
        assert!(a.agg_sort().is_some());
        assert_eq!(a.column_sorts(), vec![("created_at".to_string(), SortDir::Asc)]);
    }


    /// The `agg::` keys and the selectors must produce the SAME spec, or a
    /// `HAVING` would silently name an aggregate the query did not select and
    /// the store would refuse a query the developer wrote consistently.
    #[test]
    fn a_selector_and_its_agg_key_describe_the_same_aggregate() {
        assert_eq!(q().sum("amount").aggregates[0], agg::sum("amount"));
        assert_eq!(q().count_all().aggregates[0], agg::count_all());
        assert_eq!(q().avg("x").aggregates[0], agg::avg("x"));
        assert_eq!(q().min("x").aggregates[0], agg::min("x"));
        assert_eq!(q().max("x").aggregates[0], agg::max("x"));
    }

    /// Filters and aggregates are different clauses and must not share state:
    /// a filter selects the rows that FEED the aggregate, `having` filters
    /// the groups that come OUT of it.
    #[test]
    fn filter_and_having_stay_separate() {
        let region: crate::expr::Col<String> = crate::expr::Col::new("region");
        let a = q()
            .filter(region.eq("north"))
            .sum("amount")
            .having(agg::sum("amount"), FilterOp::Gt, 100);
        let (leaves, _) = a.lower_predicate().expect("a conjunction");
        assert_eq!(leaves.len(), 1, "the row predicate stays a row predicate");
        assert_eq!(leaves[0].column, "region");
        assert_eq!(a.having.len(), 1, "and the group predicate stays separate");
    }

    // ── Reading a group back ────────────────────────────────────────────────

    fn group() -> crate::store::Group {
        crate::store::Group::new(
            vec![Val::Text("acme".into())],
            vec![Some(Val::Integer(650)), Some(Val::Integer(2)), None],
            vec![agg::sum("amount"), agg::count_all(), agg::sum("refunds")],
        )
    }

    #[test]
    fn a_group_reads_its_key_and_its_aggregates_by_name() {
        let g = group();
        assert_eq!(g.key(), Some(&Val::Text("acme".into())));
        assert_eq!(g.sum("amount"), Some(650));
        assert_eq!(g.count_all(), 2);
    }

    /// NULL and zero stay apart at the last possible moment, which is where it
    /// is easiest to lose: `sum("refunds")` over a group with no non-null
    /// refunds is NULL, and an accessor returning `0` would make "no refunds
    /// recorded" and "refunds totalling zero" the same answer.
    #[test]
    fn a_null_aggregate_reads_as_none_not_zero() {
        assert_eq!(group().sum("refunds"), None);
    }

    /// Asking for an aggregate the query never selected is an authoring error,
    /// not a data condition — it cannot depend on the rows — so it fails loudly
    /// rather than reading as NULL, which would be indistinguishable from an
    /// empty group forever.
    #[test]
    #[should_panic(expected = "did not select")]
    fn reading_an_unselected_aggregate_says_so() {
        let _ = group().sum("shipping");
    }

    #[test]
    fn a_grouped_query_is_refused_as_a_single_group_and_the_message_names_the_fix() {
        let msg = q()
            .group_by("customer")
            .count_all()
            .single_group_refusal()
            .expect("a grouped query has many groups, not one");
        assert!(msg.contains("customer"), "name the grouping: {msg}");
        assert!(msg.contains("fetch_groups"), "name the alternative: {msg}");
    }

    #[test]
    fn a_query_selecting_no_aggregates_is_refused() {
        assert!(q().single_group_refusal().is_some());
    }

    #[test]
    fn an_ungrouped_query_with_aggregates_is_accepted() {
        assert_eq!(q().sum("amount").single_group_refusal(), None);
    }

    #[test]
    fn an_ungrouped_group_has_no_key() {
        let g = crate::store::Group::new(vec![], vec![Some(Val::Integer(1))], vec![agg::count_all()]);
        assert_eq!(g.key(), None, "an ungrouped aggregate returns one row with no key");
        assert_eq!(g.count_all(), 1);
    }
}


/// One leaf of a predicate, as the wire carries it.
fn leaf(e: &crate::expr::Expr) -> Option<Filter> {
    use crate::expr::Expr;
    match e {
        Expr::Cmp { column, op, val } => Some(Filter {
            column: column.clone(),
            op: op.clone(),
            val: val.clone(),
            in_values: None,
        }),
        Expr::In { column, vals } => Some(Filter {
            column: column.clone(),
            op: FilterOp::In,
            val: Val::Null,
            in_values: Some(vals.clone()),
        }),
        Expr::IsNull { column, negated } => Some(Filter {
            column: column.clone(),
            op: if *negated { FilterOp::IsNotNull } else { FilterOp::IsNull },
            val: Val::Null,
            in_values: None,
        }),
        _ => None,
    }
}

/// Every leaf of a conjunction, or `None` if it is not one.
fn conjunction(e: &crate::expr::Expr) -> Option<Vec<Filter>> {
    use crate::expr::Expr;
    match e {
        Expr::And(parts) => {
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                out.extend(conjunction(p)?);
            }
            Some(out)
        }
        other => leaf(other).map(|f| vec![f]),
    }
}

fn lower_into(
    e: &crate::expr::Expr,
    leaves: &mut Vec<Filter>,
    groups: &mut Vec<Vec<Filter>>,
) -> Result<(), crate::error::ApiError> {
    use crate::expr::Expr;
    match e {
        Expr::And(parts) => {
            for p in parts {
                lower_into(p, leaves, groups)?;
            }
            Ok(())
        }
        Expr::Or(parts) => {
            if !groups.is_empty() {
                return Err(crate::error::ApiError::internal(
                    "this query has two separate OR groups, which the store \
                     evaluates as one. Combine them into a single `.or(..)` \
                     expression so the grouping you wrote is the grouping that \
                     runs.",
                ));
            }
            for p in parts {
                let Some(g) = conjunction(p) else {
                    return Err(crate::error::ApiError::internal(
                        "an OR branch of this query contains another OR. The \
                         store carries one level of grouping, so this cannot be \
                         expressed without changing what it means — rewrite it \
                         as a flat OR of ANDs.",
                    ));
                };
                groups.push(g);
            }
            Ok(())
        }
        other => {
            let Some(f) = leaf(other) else {
                return Err(crate::error::ApiError::internal(
                    "unsupported predicate shape",
                ));
            };
            leaves.push(f);
            Ok(())
        }
    }
}
