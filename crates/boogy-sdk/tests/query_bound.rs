//! The row ceiling `Query`'s materializing terminals require, proven at the
//! type level.
//!
//! # Why this is not a doc comment
//!
//! retired-spelling: that label is obsolete — `find_owned` returns a
//! bounded `RowPage`. Quoted because it is the exact text that was
//! believed and was not enforcement.
//! `auth::find_owned` carried the label "small bounded sets ONLY" — friction-log
//! S-009 was CLOSED by adding that very label — and a guest still accumulated a
//! few thousand rows and died on `handle_alloc_error` against `memory_mb = 32`.
//! `Query::fetch_all` carried the same kind of label ("subject to `limit` if
//! set") and had the same hole. A doc comment is not an enforcement site;
//! `docs/relational-semantics.md` Tell 3 asks for correct-by-construction or an
//! error. This file is the construction.
//!
//! The terminals themselves live inside `wit_glue!`, which only expands in a
//! crate that ran `wit_bindgen::generate!`, so there is nothing here to call.
//! Two things are asserted instead, and together they pin both ends:
//!
//! 1. the type-level predicate (`BoundedRead`) admits `Bounded` and refuses
//!    `Unbounded` — the compile-fail half is `tests/ui/query_unbounded_fetch_all.rs`;
//! 2. the emitted terminals are actually gated ON that predicate — a source
//!    assertion, the same instrument `store.rs` uses for "no SDK helper may
//!    page by OFFSET any more", because the code under test is a macro body.

use boogy_sdk::query::{AfterGroupBy, Bounded, BoundedGroups, BoundedRead, Grouped, Unbounded};

/// The control for the compile-fail case: the bounded state must still satisfy
/// the terminal's bound. Without this, deleting `impl BoundedRead for Bounded`
/// would make the compile-fail test pass for the wrong reason.
#[test]
fn a_bounded_query_satisfies_the_terminal_bound() {
    fn materialize_rows<B: BoundedRead>() -> &'static str {
        "compiled"
    }
    assert_eq!(materialize_rows::<Bounded>(), "compiled");
}

/// An unbounded query has no materializing terminal.
#[test]
fn an_unbounded_query_has_no_materializing_terminal() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/query_unbounded_fetch_all.rs");
}

// -- The GROUP ceiling ------------------------------------------------------
//
// A second predicate, not the same one, because it bounds a different quantity.
// `fetch_all` is bounded on ROW COUNT; `fetch_groups` is bounded on GROUP
// CARDINALITY, and a grouped query over a million rows may return three groups.
// Requiring `BoundedRead` for `fetch_groups` would force `.limit(1)` onto every
// `SELECT sum(x) FROM t`, teaching the reader that a one-group answer is a
// truncation risk when it is not.

/// The control for the grouped compile-fail case, and the half of the design
/// that says WHY it is a separate predicate.
///
/// Both of these must satisfy `BoundedGroups`: `Unbounded` because an aggregate
/// with no `group_by` is exactly one group over any table at all, and `Bounded`
/// because a ceiling was stated. Without this control, deleting either `impl`
/// would make `tests/ui/query_grouped_fetch_groups.rs` pass for the wrong
/// reason.
#[test]
fn an_ungrouped_or_bounded_query_satisfies_the_group_bound() {
    fn materialize_groups<B: BoundedGroups>() -> &'static str {
        "compiled"
    }
    assert_eq!(materialize_groups::<Unbounded>(), "compiled");
    assert_eq!(materialize_groups::<Bounded>(), "compiled");
}

/// A grouped query that stated no ceiling has no group-materializing terminal.
#[test]
fn a_grouped_query_with_no_ceiling_has_no_materializing_terminal() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/query_grouped_fetch_groups.rs");
}

/// `.group_by(..)` is what moves an unbounded query into the grouped state —
/// and what LEAVES a bounded one alone.
///
/// The second half is the one worth pinning. If `group_by` transitioned
/// unconditionally to `Grouped`, then `.limit(20).group_by(c)` and
/// `.group_by(c).limit(20)` would be different queries: the first would lose the
/// ceiling it had already stated and fail to compile, for no reason a reader
/// could derive from the two chains. Chain order is not a semantic.
#[test]
fn group_by_bounds_by_transition_not_by_chain_order() {
    fn after<B: AfterGroupBy>() -> &'static str {
        std::any::type_name::<B::Out>()
    }
    assert!(after::<Unbounded>().ends_with("Grouped"), "got {}", after::<Unbounded>());
    assert!(after::<Bounded>().ends_with("Bounded"), "got {}", after::<Bounded>());
    assert!(after::<Grouped>().ends_with("Grouped"), "got {}", after::<Grouped>());
}

// -- Source assertions over the macro body ----------------------------------
//
// `Query` is emitted into the *user's* crate by `wit_glue!`, so these cannot be
// expressed as calls. They are still exact: the strings below are the gate.

const GLUE: &str = include_str!("../src/glue.rs");

/// The two row-materializing terminals must sit behind the `BoundedRead` bound.
///
/// Moving either back into the ungated `impl<B> Query<B>` block restores the
/// unbounded call verbatim, and every other test in the tree would still pass.
#[test]
fn the_materializing_terminals_are_gated_on_a_stated_bound() {
    let gated = GLUE
        .split("impl<B: $crate::query::BoundedRead> Query<B> {")
        .nth(1)
        .expect(
            "glue.rs must carry an `impl<B: $crate::query::BoundedRead> Query<B>` block — it is \
             the only thing standing between a caller and an unbounded table read",
        );
    // Ends at the next top-level `impl` or the end of the macro body; the
    // terminals must appear before anything else opens an impl.
    let block = gated.split("\n        impl").next().unwrap();
    for terminal in ["pub fn fetch_all(", "pub fn fetch_all_with_total("] {
        assert!(
            block.contains(terminal),
            "`{terminal}` is not inside the BoundedRead-gated impl block. A row-materializing \
             terminal reachable from an unbounded query is the defect this gate exists for: with \
             no `.limit()` the page is `None` and the host substitutes its own cap, so the guest \
             gets a silently truncated answer it cannot detect."
        );
    }
}

/// `.limit(n)` must be what moves a query into the bounded state — otherwise
/// the gate above is unreachable and every terminal is dead code.
#[test]
fn stating_a_limit_is_what_bounds_a_query() {
    assert!(
        GLUE.contains("pub fn limit(self, n: usize) -> Query<$crate::query::Bounded>"),
        "`Query::limit` must return the bounded state; it is the only transition into it"
    );
}

/// The group-materializing terminal must sit behind the `BoundedGroups` bound.
///
/// Moving `fetch_groups` back into the ungated `impl<B> Query<B>` block restores
/// the unbounded call verbatim, and every other test in the tree would still
/// pass — which is exactly what was true before this change.
#[test]
fn the_group_terminal_is_gated_on_a_stated_group_ceiling() {
    let gated = GLUE
        .split("impl<B: $crate::query::BoundedGroups> Query<B> {")
        .nth(1)
        .expect(
            "glue.rs must carry an `impl<B: $crate::query::BoundedGroups> Query<B>` block — it \
             is the only thing standing between a caller and one item per distinct group of a \
             table they did not bound",
        );
    let block = gated.split("\n        impl").next().unwrap();
    assert!(
        block.contains("pub fn fetch_groups<T, F>"),
        "`fetch_groups` is not inside the BoundedGroups-gated impl block. `group_by(col)` \
         returns one item per DISTINCT VALUE of `col`, which is a property of the data and \
         invisible in the query, so an ungated `fetch_groups` lets a caller materialize a row \
         per tenant user into a 32 MiB guest heap.",
    );
}

/// `.group_by(..)` must be the transition, or the gate above is unreachable.
#[test]
fn stating_a_group_by_is_what_makes_a_ceiling_required() {
    assert!(
        GLUE.contains(
            "pub fn group_by(self, column: &str) -> Query<<B as $crate::query::AfterGroupBy>::Out>"
        ),
        "`Query::group_by` must transition the typestate; it is the only thing that can tell a \
         one-group aggregate from a group-per-key one, and the SIGNATURE is where that \
         distinction lives",
    );
}

// -- The last looping verb --------------------------------------------------

/// No SDK helper may drain a listing page by page into one `Vec`.
///
/// A source assertion, for the reason `store.rs` uses one for the offset walk:
/// the helper lives inside `wit_glue!` and is emitted into the user crate, so
/// there is nothing here to call.
///
/// `db_find_by::<M>` on a NON-UNIQUE column with a declared order used to loop
/// keyset pages until an empty one and concatenate every page. Safe paging is
/// not a bound — it is what makes a bound possible — and the same shape trapped
/// a guest on `handle_alloc_error` at ~2k rows when `find_owned` had it. The
/// loop is gone; `db_find_by_page` is the pageable form.
#[test]
fn db_find_by_reads_one_page_and_does_not_loop() {
    let body = GLUE
        .split("fn db_find_by<M: $crate::model::Model>(")
        .nth(1)
        .expect("glue.rs must emit `db_find_by`")
        .split("\n        /// One BOUNDED page")
        .next()
        .expect("`db_find_by` must be followed by `db_find_by_page`");
    assert!(
        !body.contains("loop {"),
        "`db_find_by` has a loop again. Every arm of its read strategy must serve at most ONE \
         page and return; continuing past a page is what turns a typed lookup into an \
         unbounded materialization of the whole matching set.",
    );
    assert_eq!(
        body.matches("refuse_beyond_one_page").count(),
        3,
        "each of the three read strategies (PointLookup, SinglePageOnly, Keyset) must carry its \
         own one-page refusal with its own remedy — a shared one cannot name the right fix, and \
         a missing one is an arm that drains silently",
    );
    assert!(
        GLUE.contains("fn db_find_by_page<M: $crate::model::Model>("),
        "`db_find_by_page` must exist: removing the loop without supplying the pageable form \
         leaves the listing shape with no expression at all, which is how a bound becomes a \
         truncation",
    );
}

/// The batch read is bounded per query, not per batch.
///
/// `find_many` takes `Vec<Query<Bounded>>`. An unbounded query inside a batch
/// sends `page: None` exactly as it would alone, so N of them in one round trip
/// is N times the same defect rather than a cheaper version of it.
#[test]
fn the_batch_read_takes_only_bounded_queries() {
    assert!(
        GLUE.contains("queries: ::std::vec::Vec<Query<$crate::query::Bounded>>"),
        "`find_many` must take `Query<Bounded>`; batching is not a bound",
    );
}

/// The free-function row reads take a page, not an `Option<Page>`.
///
/// `None` was not "no limit" — it put `page: None` on the wire, whereupon the
/// store substituted `BOOGY_STORE_MAX_PAGE_ROWS` and answered with that many
/// rows and no cursor. That is `fetch_all`'s defect reached through the
/// free-function surface, and the typestate does not cover it.
#[test]
fn the_free_function_reads_require_a_page() {
    assert!(
        GLUE.contains("            page: $bindings::boogy::platform::store::Page,\n"),
        "`find_rows` / `MigrationCtx::find_rows` must take a `Page`, not an `Option<Page>`",
    );
    let find_rows = GLUE
        .split("        fn find_rows(\n")
        .nth(1)
        .expect("glue.rs must emit `find_rows`");
    let sig = find_rows.split(") -> ").next().unwrap();
    assert!(
        !sig.contains("::core::option::Option<$bindings::boogy::platform::store::Page>"),
        "`find_rows` accepts an absent page again: {sig}",
    );
}
