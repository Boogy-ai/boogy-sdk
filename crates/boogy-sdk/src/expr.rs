//! Typed columns and the expressions built from them.
//!
//! Design: `docs/superpowers/specs/2026-08-17-query-expressions.md`.
//!
//! Operators live on the **column**, not on the query. That is the shape every
//! mainstream data library converged on — SQLAlchemy, Diesel, Ecto,
//! ActiveRecord — and the reason is not taste: it is the only arrangement where
//! the API surface stays constant as the number of operators grows. A verb per
//! operator gives you eleven `where_*` methods and eleven more to learn; an
//! operator on a column gives you one `filter`.
//!
//! ```ignore
//! let room_id = 7_i64;
//! Query::on(Post::TABLE)
//!     .filter(Post::room_id.eq(room_id))
//!     .filter(Post::deleted_at.is_null())
//!     .order(Post::created_at.desc())
//!     .limit(20)
//!     .fetch_all()?;
//! ```
//!
//! Columns are typed, so `Post::room_id.eq("nope")` does not compile and
//! `is_null()` is offered only where a column can actually be null.

use std::marker::PhantomData;

use crate::query::IntoVal;
use crate::store::{AggSpec, FilterOp, SortDir, Val};

/// A column of `T`, on some table.
///
/// Carries its type only to check comparisons — at the wire it is a name, which
/// [`Col::name`] returns for the places a column genuinely is just a name (row
/// accessors, schema declarations). Keeping that available is what stops this
/// becoming a second vocabulary running alongside the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Col<T> {
    name: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> Col<T> {
    /// Emitted by `#[derive(Model)]`; `const` so a column is an associated
    /// constant rather than a call.
    pub const fn new(name: &'static str) -> Self {
        Col { name, _type: PhantomData }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    fn cmp_expr(&self, op: FilterOp, val: Val) -> Expr {
        Expr::Cmp { column: self.name.to_string(), op, val }
    }

    /// Ascending by this column.
    pub fn asc(&self) -> Order {
        Order::Column(self.name.to_string(), SortDir::Asc)
    }

    /// Descending by this column.
    pub fn desc(&self) -> Order {
        Order::Column(self.name.to_string(), SortDir::Desc)
    }
}

/// The comparisons, on any column whose type can cross the wire.
///
/// `impl Into<T>` rather than `T` so `Post::room_id.eq(5)` works without a
/// suffix while `Post::room_id.eq("five")` still fails to build.
impl<T: IntoVal> Col<T> {
    pub fn eq(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Eq, v.into().into_val())
    }
    pub fn ne(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Neq, v.into().into_val())
    }
    pub fn gt(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Gt, v.into().into_val())
    }
    pub fn gte(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Gte, v.into().into_val())
    }
    pub fn lt(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Lt, v.into().into_val())
    }
    pub fn lte(&self, v: impl Into<T>) -> Expr {
        self.cmp_expr(FilterOp::Lte, v.into().into_val())
    }

    /// `lo <= this <= hi`, both ends inclusive — SQL's `BETWEEN`.
    pub fn between(&self, lo: impl Into<T>, hi: impl Into<T>) -> Expr {
        self.gte(lo).and(self.lte(hi))
    }

    /// Membership. An EMPTY list matches nothing, which is what SQL's
    /// `IN ()` means — not "no filter", the reading that silently turns a
    /// scoped query into an unscoped one.
    pub fn is_in<I, V>(&self, vals: I) -> Expr
    where
        I: IntoIterator<Item = V>,
        V: Into<T>,
    {
        Expr::In {
            column: self.name.to_string(),
            vals: vals.into_iter().map(|v| v.into().into_val()).collect(),
        }
    }
}

/// Pattern matching, only where a pattern means something.
impl Col<String> {
    pub fn like(&self, pattern: impl Into<String>) -> Expr {
        self.cmp_expr(FilterOp::Like, Val::Text(pattern.into()))
    }
    pub fn not_like(&self, pattern: impl Into<String>) -> Expr {
        self.cmp_expr(FilterOp::NotLike, Val::Text(pattern.into()))
    }
}

/// Null checks, only where a column can be null.
///
/// A `Col<i64>` has no `is_null`, because the column cannot hold one — the type
/// says so and the schema agrees. Today every column is a string and every
/// query may ask any question of it.
impl<T> Col<Option<T>> {
    pub fn is_null(&self) -> Expr {
        Expr::IsNull { column: self.name.to_string(), negated: false }
    }
    pub fn is_not_null(&self) -> Expr {
        Expr::IsNull { column: self.name.to_string(), negated: true }
    }
}

/// A predicate. Composes with [`Expr::and`] / [`Expr::or`], and nests — which
/// `or_groups`, the flattened stand-in it replaces, cannot.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Cmp { column: String, op: FilterOp, val: Val },
    In { column: String, vals: Vec<Val> },
    IsNull { column: String, negated: bool },
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

impl Expr {
    /// Both. Flattens, so `a.and(b).and(c)` is one three-way `And` rather than a
    /// nest — the planner sees a conjunction it can push down whole.
    pub fn and(self, other: Expr) -> Expr {
        match (self, other) {
            (Expr::And(mut a), Expr::And(b)) => {
                a.extend(b);
                Expr::And(a)
            }
            (Expr::And(mut a), o) => {
                a.push(o);
                Expr::And(a)
            }
            (s, Expr::And(mut b)) => {
                b.insert(0, s);
                Expr::And(b)
            }
            (s, o) => Expr::And(vec![s, o]),
        }
    }

    /// Either. Flattens for the same reason.
    pub fn or(self, other: Expr) -> Expr {
        match (self, other) {
            (Expr::Or(mut a), Expr::Or(b)) => {
                a.extend(b);
                Expr::Or(a)
            }
            (Expr::Or(mut a), o) => {
                a.push(o);
                Expr::Or(a)
            }
            (s, Expr::Or(mut b)) => {
                b.insert(0, s);
                Expr::Or(b)
            }
            (s, o) => Expr::Or(vec![s, o]),
        }
    }
}

/// An ordering — by a column, or by an aggregate over a related table.
///
/// Both are `ORDER BY` in SQL and both are one type here, so a query that ranks
/// by a total reads exactly like one that sorts by a column.
#[derive(Debug, Clone, PartialEq)]
pub enum Order {
    Column(String, SortDir),
    Aggregate(AggSpec, SortDir),
}

/// `.desc()` / `.asc()` on an aggregate, so `sum(Vote::direction).desc()` reads
/// the same as `Post::created_at.desc()` — and, since these are INHERENT,
/// costs the same nothing to reach.
///
/// This was a trait (`OrderByAgg`) until 2026-08-17, and the trait was the
/// whole defect: a column ordering needed no import while an aggregate ordering
/// needed one, so two expressions that read identically in the documentation
/// behaved differently at the call site, and the compiler's advice for the
/// second was to go and find a name the reader had no reason to know. That is
/// the same "a method you only find if you already knew it existed" problem
/// retired-spelling: `order_by_agg` is history — ordering is one
/// `list<order-term>`, reached by `.order(<expr>)`. Named here because
/// the trait it replaced made the same mistake.
/// `order_by_agg` was deleted for, wearing a `use` statement instead of a verb.
/// `AggSpec` is defined in this crate, so an inherent impl is available and
/// there is no reason to charge for the trait.
impl AggSpec {
    /// Ascending by this aggregate.
    pub fn asc(self) -> Order {
        Order::Aggregate(self, SortDir::Asc)
    }

    /// Descending by this aggregate.
    pub fn desc(self) -> Order {
        Order::Aggregate(self, SortDir::Desc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM_ID: Col<i64> = Col::new("room_id");
    const TITLE: Col<String> = Col::new("title");
    const DELETED_AT: Col<Option<i64>> = Col::new("deleted_at");

    #[test]
    fn a_comparison_carries_its_column_operator_and_value() {
        assert_eq!(
            ROOM_ID.eq(7),
            Expr::Cmp { column: "room_id".into(), op: FilterOp::Eq, val: Val::Integer(7) }
        );
        assert_eq!(
            TITLE.like("hello%"),
            Expr::Cmp {
                column: "title".into(),
                op: FilterOp::Like,
                val: Val::Text("hello%".into())
            }
        );
    }

    /// Each operator maps to its own arm. A table rather than six tests: the
    /// failure this guards is a copy-paste that leaves two operators pointing at
    /// the same one, which no single-operator test can see.
    #[test]
    fn every_comparison_maps_to_a_distinct_operator() {
        let got = [
            ROOM_ID.eq(1),
            ROOM_ID.ne(1),
            ROOM_ID.gt(1),
            ROOM_ID.gte(1),
            ROOM_ID.lt(1),
            ROOM_ID.lte(1),
        ]
        .map(|e| match e {
            Expr::Cmp { op, .. } => op,
            other => panic!("not a comparison: {other:?}"),
        });
        assert_eq!(
            got,
            [
                FilterOp::Eq,
                FilterOp::Neq,
                FilterOp::Gt,
                FilterOp::Gte,
                FilterOp::Lt,
                FilterOp::Lte
            ]
        );
    }

    #[test]
    fn between_is_inclusive_on_both_ends() {
        assert_eq!(
            ROOM_ID.between(1, 9),
            Expr::And(vec![
                Expr::Cmp { column: "room_id".into(), op: FilterOp::Gte, val: Val::Integer(1) },
                Expr::Cmp { column: "room_id".into(), op: FilterOp::Lte, val: Val::Integer(9) },
            ])
        );
    }

    /// An empty `IN` matches nothing, which is what SQL says. The dangerous
    /// reading is "no filter", which turns a scoped query into an unscoped one —
    /// silently, and usually on someone else's data.
    #[test]
    fn an_empty_in_list_matches_nothing_rather_than_everything() {
        let e = ROOM_ID.is_in(Vec::<i64>::new());
        assert_eq!(e, Expr::In { column: "room_id".into(), vals: vec![] });
    }

    #[test]
    fn conjunctions_flatten_instead_of_nesting() {
        let e = ROOM_ID.eq(1).and(ROOM_ID.gt(0)).and(ROOM_ID.lt(9));
        match e {
            Expr::And(parts) => assert_eq!(parts.len(), 3, "one three-way And: {parts:?}"),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn disjunctions_flatten_too_and_do_not_absorb_conjunctions() {
        let e = ROOM_ID.eq(1).or(ROOM_ID.eq(2)).or(ROOM_ID.eq(3));
        match &e {
            Expr::Or(parts) => assert_eq!(parts.len(), 3),
            other => panic!("expected Or, got {other:?}"),
        }
        // An Or inside an And must stay nested, or the meaning changes.
        let mixed = TITLE.eq("a".to_string()).and(e);
        match mixed {
            Expr::And(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[1], Expr::Or(_)), "the Or must survive intact");
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn null_checks_are_offered_only_where_a_column_can_be_null() {
        assert_eq!(
            DELETED_AT.is_null(),
            Expr::IsNull { column: "deleted_at".into(), negated: false }
        );
        assert_eq!(
            DELETED_AT.is_not_null(),
            Expr::IsNull { column: "deleted_at".into(), negated: true }
        );
        // `ROOM_ID.is_null()` does not compile: Col<i64> has no such method.
    }

    #[test]
    fn a_column_orders_in_either_direction() {
        assert_eq!(TITLE.asc(), Order::Column("title".into(), SortDir::Asc));
        assert_eq!(TITLE.desc(), Order::Column("title".into(), SortDir::Desc));
    }

    /// Ranking by a related total reads exactly like sorting by a column, which
    /// is the point: it is the same clause.
    #[test]
    fn an_aggregate_orders_the_same_way_a_column_does() {
        let a = AggSpec { kind: crate::store::AggFunc::Sum, column: Some("direction".into()) };
        assert_eq!(a.clone().desc(), Order::Aggregate(a, SortDir::Desc));
    }

    /// The type is the check. A string column takes a string, an integer column
    /// takes an integer, and neither takes the other — which today nothing
    /// enforces, because every column is a `&str`.
    #[test]
    fn a_value_is_converted_by_the_columns_type() {
        match TITLE.eq("borrowed") {
            Expr::Cmp { val: Val::Text(t), .. } => assert_eq!(t, "borrowed"),
            other => panic!("a String column must take a &str: {other:?}"),
        }
        match ROOM_ID.eq(5i32) {
            Expr::Cmp { val: Val::Integer(n), .. } => assert_eq!(n, 5),
            other => panic!("an i64 column must take a smaller int: {other:?}"),
        }
    }
}
