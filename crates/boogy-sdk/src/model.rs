//! Typed model layer: `#[derive(Model)]` maps a Rust struct to a store
//! table. This module holds the ordinary (host-testable) parts — the
//! `Field` and `Model` traits and the `Id`/`Decimal`/`Timestamp` value
//! types. The derive lives in `boogy-sdk-macros` (re-exported as
//! `boogy_sdk::Model`); the store-touching CRUD (`db_insert` etc.) is
//! emitted by `wit_glue!` in the consumer crate.

use core::marker::PhantomData;

use crate::error::ApiError;
use crate::store::{ColDef, ColType, Row, StoreError, Table, Val};

/// One column's worth of typing: how a Rust field maps to a stored
/// column and how it round-trips through the portable [`Val`] enum.
/// Implement this for custom field types to extend the vocabulary.
pub trait Field: Sized {
    /// The stored column's type.
    fn col_type() -> ColType;
    /// Whether the column is nullable. Only `Option<T>` overrides this.
    ///
    /// `false` means the store rejects an **explicitly supplied** null for the
    /// column. It does not mean every row carries a value: a write that omits
    /// the column is still accepted, and reads resolve it to the column's
    /// default, or null if it has none. See [`ColumnSpec::not_null`].
    ///
    /// [`ColumnSpec::not_null`]: crate::store::ColumnSpec::not_null
    fn nullable() -> bool {
        false
    }
    /// Encode for writes.
    fn to_val(&self) -> Val;
    /// Decode for reads. Infallible — a missing/`Null`/malformed value
    /// yields the type's zero value (mirrors `Row`'s accessors), except
    /// `Option<T>` which yields `None`.
    fn from_val(v: &Val) -> Self;
}

impl Field for String {
    fn col_type() -> ColType { ColType::Text }
    fn to_val(&self) -> Val { Val::Text(self.clone()) }
    fn from_val(v: &Val) -> Self { v.as_text() }
}

impl Field for i64 {
    fn col_type() -> ColType { ColType::Integer }
    fn to_val(&self) -> Val { Val::Integer(*self) }
    fn from_val(v: &Val) -> Self { v.as_int() }
}

impl Field for u64 {
    fn col_type() -> ColType { ColType::Integer }
    fn to_val(&self) -> Val { Val::Integer(*self as i64) }
    fn from_val(v: &Val) -> Self { v.as_int() as u64 }
}

impl Field for bool {
    fn col_type() -> ColType { ColType::Boolean }
    fn to_val(&self) -> Val { Val::Boolean(*self) }
    fn from_val(v: &Val) -> Self { v.as_bool() }
}

impl Field for f64 {
    fn col_type() -> ColType { ColType::Real }
    fn to_val(&self) -> Val { Val::Real(*self) }
    fn from_val(v: &Val) -> Self { v.as_real() }
}

impl<T: Field> Field for Option<T> {
    fn col_type() -> ColType { T::col_type() }
    fn nullable() -> bool { true }
    fn to_val(&self) -> Val {
        match self {
            Some(t) => t.to_val(),
            None => Val::Null,
        }
    }
    fn from_val(v: &Val) -> Self {
        match v {
            Val::Null => None,
            _ => Some(T::from_val(v)),
        }
    }
}

/// A typed row id. `Id<Post>` and `Id<User>` are distinct types, so a
/// `Post` id can't be passed where a `User` id is expected. Maps to an
/// integer column. (Opaque-id translation via `boogy_sdk::ids` is a
/// future seam, not wired here.)
pub struct Id<T> {
    raw: u64,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    pub const fn new(raw: u64) -> Self {
        Self { raw, _marker: PhantomData }
    }
    pub const fn get(&self) -> u64 {
        self.raw
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self { *self }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool { self.raw == other.raw }
}
impl<T> Eq for Id<T> {}
impl<T> core::fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Id({})", self.raw)
    }
}

impl<T> Field for Id<T> {
    fn col_type() -> ColType { ColType::Integer }
    fn to_val(&self) -> Val { Val::Integer(self.raw as i64) }
    fn from_val(v: &Val) -> Self { Id::new(v.as_int() as u64) }
}

/// Lets `Id<T>` serve as a [`Counter::Key`] for an `of = Model` counter: a
/// single-element key-tuple wrapping the row's id, same encoding
/// `Field::to_val` already gives it.
impl<T> From<Id<T>> for Vec<Val> {
    fn from(id: Id<T>) -> Vec<Val> {
        vec![id.to_val()]
    }
}

/// A fixed-point decimal — money, a score, a weight — exact to 6 decimal
/// places. Backed by a bare `i64` count of minor units (`1.000000` is
/// stored as `minor_units() == 1_000_000`): `Copy`, and constructing,
/// storing, comparing, or reading one never allocates. It stores as a
/// native `Integer` column, so it sorts and range-filters correctly at any
/// magnitude and either sign using the store's ordinary integer
/// comparison — the same path `Timestamp`/`Id<T>` already use.
///
/// Two exact ways to build one:
/// - **From a decimal literal or string** — `"19.99".parse::<Decimal>()?`,
///   or the derive's `#[default = "19.99"]`. This is the natural way to
///   write a price, a rate, or a weight, and no `f64` is ever involved.
/// - **From minor units you already have** — `Decimal::from_minor_units(1999)`,
///   for a caller that already holds cents from an external system (a
///   payment provider, an invoice import).
///
/// `Decimal::new(f64)` / `.get() -> f64` also exist for interop with code
/// that already computes in `f64` — see their docs — but a value built or
/// read that way carries ordinary binary floating-point rounding at that
/// boundary, same as any `f64`. Prefer the exact constructors above when
/// the value must be exact (most money, most exact tallies).
///
/// `+` / `-` / unary `-` are exact integer operations on minor units and
/// panic on overflow (range is ±`i64::MAX` minor units, about ±9.22
/// trillion major units at this scale) rather than silently wrapping.
/// Multiplication and division are deliberately not provided — both need a
/// rounding policy (up? down? to even?) only the caller can choose
/// correctly; operate on `.minor_units()` explicitly and re-wrap with the
/// rounding you intend.
///
/// `Display` renders the decimal form (`"19.990000"`); `Serialize` writes
/// that same string and `Deserialize` parses it back exactly — a
/// `Decimal` field never appears as a JSON number on the wire, so a
/// client never sees (or reintroduces) float rounding.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Decimal {
    minor: i64,
}

impl Decimal {
    /// Minor units per major unit. `Decimal` is exact to 6 decimal places.
    pub const SCALE: i64 = 1_000_000;
    pub const ZERO: Decimal = Decimal { minor: 0 };
    pub const MAX: Decimal = Decimal { minor: i64::MAX };
    pub const MIN: Decimal = Decimal { minor: i64::MIN };

    /// Exact: build directly from a count of minor units (e.g. cents from
    /// a payment provider). No rounding, ever.
    pub const fn from_minor_units(minor: i64) -> Self {
        Decimal { minor }
    }

    /// The exact underlying count of minor units.
    pub const fn minor_units(&self) -> i64 {
        self.minor
    }

    /// Builds from an `f64`, ROUNDING to the nearest minor unit
    /// (round-half-away-from-zero). A convenience for values that already
    /// went through floating-point arithmetic elsewhere — it is NOT exact.
    /// Prefer `"19.99".parse()` or `from_minor_units` when the value must
    /// be exact. An out-of-range magnitude saturates to `Decimal::MAX`/
    /// `MIN` (an `f64`-to-`i64` cast saturates rather than wrapping).
    pub fn new(v: f64) -> Self {
        let scaled = (v * Self::SCALE as f64).round();
        Decimal { minor: scaled as i64 }
    }

    /// An approximate `f64` view — for display, telemetry, or interop with
    /// `f64`-based arithmetic elsewhere. Round-tripping through `.get()`
    /// and back through `Decimal::new` is not guaranteed lossless at every
    /// magnitude; use `.minor_units()` for an exact read.
    pub fn get(&self) -> f64 {
        self.minor as f64 / Self::SCALE as f64
    }
}

impl core::fmt::Debug for Decimal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Decimal({self})")
    }
}

impl core::fmt::Display for Decimal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let abs = self.minor.unsigned_abs();
        let major = abs / (Self::SCALE as u64);
        let frac = abs % (Self::SCALE as u64);
        if self.minor < 0 {
            write!(f, "-")?;
        }
        write!(f, "{major}.{frac:06}")
    }
}

impl core::ops::Add for Decimal {
    type Output = Decimal;
    fn add(self, rhs: Decimal) -> Decimal {
        Decimal { minor: self.minor.checked_add(rhs.minor).expect("Decimal overflow in +") }
    }
}

impl core::ops::Sub for Decimal {
    type Output = Decimal;
    fn sub(self, rhs: Decimal) -> Decimal {
        Decimal { minor: self.minor.checked_sub(rhs.minor).expect("Decimal overflow in -") }
    }
}

impl core::ops::Neg for Decimal {
    type Output = Decimal;
    fn neg(self) -> Decimal {
        Decimal { minor: self.minor.checked_neg().expect("Decimal overflow negating MIN") }
    }
}

/// Error parsing a decimal string into a [`Decimal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalParseError(String);

impl core::fmt::Display for DecimalParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid Decimal: {}", self.0)
    }
}

impl std::error::Error for DecimalParseError {}

impl core::str::FromStr for Decimal {
    type Err = DecimalParseError;

    /// Parses a plain decimal string (`"19.99"`, `"-3.140000"`, `"5"`) into
    /// EXACT minor units — no `f64` is ever involved. At most 6 fractional
    /// digits are accepted; more than that is a parse error rather than a
    /// silent round, because a type that promises exactness should refuse
    /// a value it cannot represent exactly rather than guess.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_decimal_minor_units(s).map(|minor| Decimal { minor }).map_err(DecimalParseError)
    }
}

fn parse_decimal_minor_units(s: &str) -> Result<i64, String> {
    const SCALE_DIGITS: usize = 6;
    let err = |msg: &str| format!("{msg} (got {s:?})");
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(err("empty"));
    }
    let (neg, rest) = match trimmed.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let mut parts = rest.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next().unwrap_or("");
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(err("no digits"));
    }
    if !int_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(err("non-digit in integer part"));
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(err("non-digit in fractional part (or more than one '.')"));
    }
    if frac_part.len() > SCALE_DIGITS {
        return Err(err(&format!(
            "more than {SCALE_DIGITS} fractional digits — Decimal is exact to {SCALE_DIGITS} \
             decimal places; round explicitly before parsing"
        )));
    }
    let int_val: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().map_err(|_| err("integer part out of range"))?
    };
    let mut padded = frac_part.to_string();
    while padded.len() < SCALE_DIGITS {
        padded.push('0');
    }
    let frac_val: i64 = padded.parse().map_err(|_| err("fractional part out of range"))?;
    let magnitude = int_val
        .checked_mul(Decimal::SCALE)
        .and_then(|m| m.checked_add(frac_val))
        .ok_or_else(|| err("out of range for Decimal"))?;
    Ok(if neg { -magnitude } else { magnitude })
}

impl serde::Serialize for Decimal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Decimal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::string::String as serde::Deserialize>::deserialize(deserializer)?;
        s.parse::<Decimal>().map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Decimal {
    fn schema_name() -> std::string::String {
        "Decimal".to_string()
    }
    fn json_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            format: Some("decimal".to_string()),
            metadata: Some(std::boxed::Box::new(schemars::schema::Metadata {
                description: Some(
                    "Fixed-point decimal, exact to 6 places, as a string (e.g. \"19.990000\")"
                        .to_string(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

impl Field for Decimal {
    fn col_type() -> ColType { ColType::Integer }
    fn to_val(&self) -> Val { Val::Integer(self.minor) }
    fn from_val(v: &Val) -> Self { Decimal { minor: v.as_int() } }
}

/// A unix-millis timestamp stored as an integer column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn new(millis: i64) -> Self { Timestamp(millis) }
    pub fn get(&self) -> i64 { self.0 }
}

/// So a timestamp column can be compared against a plain millisecond value —
/// `Poll::closed_at.eq(OPEN_SENTINEL)` — without the caller wrapping it. The
/// column's type still rejects a string, which is the check that matters.
impl From<i64> for Timestamp {
    fn from(millis: i64) -> Self {
        Timestamp(millis)
    }
}

impl Field for Timestamp {
    fn col_type() -> ColType { ColType::Integer }
    fn to_val(&self) -> Val { Val::Integer(self.0) }
    fn from_val(v: &Val) -> Self { Timestamp(v.as_int()) }
}

/// A struct that maps 1:1 to a store table. Implemented by
/// `#[derive(Model)]`. The derive also emits `pub const`s for each
/// field's column name on the struct (e.g. `Edge::USER_A`).
pub trait Model: Sized {
    /// The table name.
    const TABLE: &'static str;
    /// The schema (columns + indexes) for `create_model::<Self>()`.
    fn schema() -> Table;
    /// Build from a stored row.
    fn from_row(row: &Row) -> Self;
    /// The writable columns (EXCLUDES the auto-PK `_id`).
    fn to_columns(&self) -> Vec<(String, Val)>;
    /// The `#[pk]` field as a u64, or `None` if the model has no `#[pk]`.
    fn id(&self) -> Option<u64>;
}

/// A freestanding, typed counter — either the name for a counter column
/// already declared on a `#[derive(Model)]` struct (with
/// `#[model(counter(name = "..."))]`), or a counter keyed by an arbitrary
/// tuple attached to no model at all. Implemented by `#[derive(Counter)]`:
///
/// ```ignore
/// #[derive(Counter)]
/// #[counter(of = Post, name = "vote_score")]
/// pub struct PostVoteScore;          // keyed by the post's row id
/// ```
///
/// ```ignore
/// #[derive(Counter)]
/// #[counter(key = (room_id, day))]
/// pub struct RoomDailyPosts;         // keyed by an arbitrary tuple, no model
/// ```
///
/// `of` and `key` are mutually exclusive: `of` is sugar for keying on that
/// model's row id, so combining them — or giving neither — is a compile
/// error naming the fix, not a derive that silently picks one.
///
/// For `of = Model`, this does **not** declare storage of its own: `Post`'s
/// own `#[model(counter(name = "vote_score"))]` declaration is what
/// `#[derive(Model)]` still turns into a column and a write path
/// (`upsert_increment`) — unchanged by this derive. `Counter` only gives
/// that existing cell a name and a typed key, so it can be addressed (read
/// through the store's column-scoped counter path) without fetching or
/// decoding the row it lives on. For `key = (..)`, there is no row, no
/// rowid and no model at all — the counter's cells are its own storage, one
/// per distinct key tuple ever added to.
///
/// `NAME` is `"<table>.<column>"` for `of = Model` (built at compile time
/// from `<of as Model>::TABLE` and the declared column name — so a
/// `#[model(table = "...")]` rename on the parent moves the identity with
/// it, the same guarantee `#[belongs_to]` gives a foreign key), or the
/// struct name in snake_case for `key = (..)` (override with
/// `name = "..."`).
///
/// **Unbounded key cardinality is a new way to fill a keyspace.** A
/// `key = (..)` counter allocates a cell for every DISTINCT key tuple it is
/// ever added to and never reclaims one — declaring one keyed by something
/// with no natural bound (a request id, a raw event id) grows storage
/// without limit, the same risk `rollup-def`'s `group` documents: a grouping
/// pays only when it has BOUNDED cardinality (a customer, a day, an IP),
/// not an unbounded one (a request id). Size a counter's key the way you
/// would size an index.
///
/// ## Reading: two verbs, one honest choice
///
/// [`Counter::add`] is a blind atomic increment: no read, never conflicts.
/// Reading splits into two, mapping directly onto the store's snapshot vs.
/// ordinary read:
///
/// - [`Counter::get`] — a **snapshot** read: cheap, takes no read-conflict
///   range, and is therefore **not safe to branch on** inside a
///   transaction — nothing guarantees the value is still current by the
///   time that transaction commits.
/// - [`Counter::get_for_update`] — an **ordinary** read: takes the
///   conflict range, safe to branch on, and **contends by design** — that
///   contention is the price of the guarantee, not a defect.
///
/// Today a counter used to be one number carrying a rule — *never gate a
/// write on a counter read in the same transaction* — that failed silently
/// when broken. Naming the two verbs makes the choice visible at the call
/// site instead of leaving it to be discovered in production.
pub trait Counter {
    /// `"<table>.<column>"` for `of = Model`, or the counter's own name
    /// (the struct name in snake_case, or an explicit `name = "..."`
    /// override) for `key = (..)`.
    const NAME: &'static str;
    /// The key that addresses one cell of this counter. For `of = Model`
    /// this is the parent row's id (`Id<of>`); for `key = (col_a, col_b)`
    /// this is `[Val; N]`, one value per declared column, in the declared
    /// order.
    type Key: Into<Vec<Val>>;

    /// Atomic increment. No read — never conflicts on this cell.
    fn add<S: CounterStore>(store: &S, key: Self::Key, delta: i64) -> Result<(), ApiError> {
        store.counter_add(Self::NAME, &key.into(), delta).map_err(ApiError::from)
    }

    /// Snapshot read: cheap, takes no read-conflict range, NOT safe to
    /// branch on inside a transaction. See the trait docs.
    fn get<S: CounterStore>(store: &S, key: Self::Key) -> Result<i64, ApiError> {
        store.counter_get(Self::NAME, &key.into(), true).map_err(ApiError::from)
    }

    /// Ordinary read: takes the read-conflict range, safe to branch on,
    /// contends by design. See the trait docs.
    fn get_for_update<S: CounterStore>(store: &S, key: Self::Key) -> Result<i64, ApiError> {
        store.counter_get(Self::NAME, &key.into(), false).map_err(ApiError::from)
    }
}

/// A **max accumulator**: a conflict-free cell holding the LARGEST value ever
/// observed for its key.
///
/// The same shape as [`Counter`] — declared beside the model rather than as a
/// field, addressed by a key, maintained by an atomic op that never reads — but
/// it keeps a maximum instead of a sum.
///
/// # What it is for
///
/// "When did this room last have a post?" is the canonical case. Written the
/// obvious way it is an ordinary column on the parent row, stamped on every
/// child write — and that rewrites the parent row every time, so every writer
/// contends with every other. Measured on the `board` example: **59.4% of
/// commit attempts conflicted** at 600 concurrent writers, against **2** for
/// the counter cell beside it. Observing into a max accumulator instead takes
/// no read-conflict range at all, so the writers never see each other.
///
/// # Observing is not assigning
///
/// [`MaxAccum::observe`] keeps the value only if it is larger than what is
/// stored. A smaller observation is a silent no-op, not an error: "the latest
/// post is older than the latest post" is a race between two writers, not a
/// fault, and the cell already holds the right answer.
///
/// It follows that a max accumulator can only move **forward**. There is no
/// "unobserve" — deleting the newest post does not roll the stamp back. Where
/// that matters, the value is derived data and should be recomputed, not
/// accumulated.
///
/// # Reading it
///
/// [`MaxAccum::get`] and [`MaxAccum::get_for_update`] carry exactly
/// [`Counter`]'s trade: the snapshot read is cheap and **must not be branched
/// on** inside a transaction; the update read takes the conflict range, is safe
/// to decide with, and contends by design.
///
/// `None` means nothing has ever been observed — distinct from any value that
/// has been, and the reason this returns an `Option` rather than a floor value
/// that would look like a real timestamp.
pub trait MaxAccum {
    /// `"<table>.<column>"` for `of = Model`, or the accumulator's own name
    /// for `key = (..)`. Shares one namespace with [`Counter`]: a name
    /// declared as one cannot be used as the other.
    const NAME: &'static str;
    /// The key addressing one cell, exactly as [`Counter::Key`].
    type Key: Into<Vec<Val>>;

    /// Observe a value. Kept only if larger than what is stored. No read, so
    /// this never conflicts on the cell.
    fn observe<S: CounterStore>(store: &S, key: Self::Key, value: i64) -> Result<(), ApiError> {
        store.max_observe(Self::NAME, &key.into(), value).map_err(ApiError::from)
    }

    /// Snapshot read: cheap, takes no read-conflict range, NOT safe to branch
    /// on inside a transaction. `None` = nothing observed yet.
    fn get<S: CounterStore>(store: &S, key: Self::Key) -> Result<Option<i64>, ApiError> {
        store.max_get(Self::NAME, &key.into(), true).map_err(ApiError::from)
    }

    /// Ordinary read: takes the read-conflict range, safe to branch on,
    /// contends by design.
    fn get_for_update<S: CounterStore>(store: &S, key: Self::Key) -> Result<Option<i64>, ApiError> {
        store.max_get(Self::NAME, &key.into(), false).map_err(ApiError::from)
    }
}

/// The store operations [`Counter::add`]/[`Counter::get`]/
/// [`Counter::get_for_update`] call through — binding-agnostic, the same
/// seam [`IntoStoreError`](crate::store::IntoStoreError) uses to keep this
/// crate free of any dependency on generated WIT bindings. A deployed
/// service's generated glue supplies the live implementation (calling the
/// store's `counter-add`/`counter-get`); nothing in this crate implements
/// it, so tests exercise the default methods above with their own.
pub trait CounterStore {
    /// Atomic increment — a blind add, no read.
    fn counter_add(&self, name: &str, key: &[Val], delta: i64) -> Result<(), StoreError>;
    /// `snapshot = true` takes NO read-conflict range (cheap, not safe to
    /// branch on inside a transaction); `false` takes the range (safe to
    /// branch on, contends by design).
    fn counter_get(&self, name: &str, key: &[Val], snapshot: bool) -> Result<i64, StoreError>;
    /// Observe a value for a max accumulator. See [`MaxAccum::observe`].
    fn max_observe(&self, name: &str, key: &[Val], value: i64) -> Result<(), StoreError>;
    /// Read a max accumulator. `None` = nothing observed. See [`MaxAccum`].
    fn max_get(&self, name: &str, key: &[Val], snapshot: bool)
        -> Result<Option<i64>, StoreError>;
}

/// Compile-time `"<table>.<column>"` builder for `#[derive(Counter)]`'s
/// `Counter::NAME`. `N` must equal `table.len() + 1 + column.len()` — the
/// derive computes it as a `const` expression at the call site. Not meant to
/// be called directly.
#[doc(hidden)]
pub const fn concat_counter_name<const N: usize>(table: &str, column: &str) -> [u8; N] {
    let t = table.as_bytes();
    let c = column.as_bytes();
    let mut buf = [0u8; N];
    let mut i = 0;
    while i < t.len() {
        buf[i] = t[i];
        i += 1;
    }
    buf[t.len()] = b'.';
    let mut j = 0;
    while j < c.len() {
        buf[t.len() + 1 + j] = c[j];
        j += 1;
    }
    buf
}

// Helper so the derive can build a ColDef without knowing the field's
// concrete type at macro time — it calls this with the type's Field impl.
#[doc(hidden)]
pub fn col_def_for<T: Field>(
    name: &str,
    unique: bool,
    counter: bool,
    default: Option<Val>,
) -> ColDef {
    col_def_for_accum::<T>(name, unique, counter, false, default)
}

/// `col_def_for`, with the accumulator op stated. A MAX column is a counter
/// column whose cell keeps the largest value observed rather than a sum.
#[doc(hidden)]
pub fn col_def_for_accum<T: Field>(
    name: &str,
    unique: bool,
    counter: bool,
    counter_max: bool,
    default: Option<Val>,
) -> ColDef {
    ColDef {
        name: name.to_string(),
        col_type: T::col_type(),
        nullable: T::nullable(),
        unique,
        references: None,
        counter,
        counter_max,
        default,
        renamed_from: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ColType;

    #[test]
    fn primitive_roundtrips() {
        assert_eq!(String::from_val(&"hi".to_string().to_val()), "hi");
        assert_eq!(i64::from_val(&(-7i64).to_val()), -7);
        assert_eq!(u64::from_val(&(42u64).to_val()), 42);
        assert!(bool::from_val(&true.to_val()));
        assert_eq!(f64::from_val(&(1.5f64).to_val()), 1.5);
    }

    #[test]
    fn option_maps_to_nullable_and_roundtrips() {
        assert!(<Option<i64>>::nullable());
        assert!(!<i64>::nullable());
        assert_eq!(<Option<i64>>::from_val(&None::<i64>.to_val()), None);
        assert_eq!(<Option<i64>>::from_val(&Some(5i64).to_val()), Some(5));
        // Some encodes as the inner value (not Null):
        assert!(matches!(Some(5i64).to_val(), Val::Integer(5)));
    }

    #[test]
    fn id_is_typed_and_roundtrips() {
        struct Post;
        let id: Id<Post> = Id::new(99);
        assert_eq!(<Id<Post>>::col_type() as u8, ColType::Integer as u8);
        assert_eq!(<Id<Post>>::from_val(&id.to_val()).get(), 99);
    }

    // `Decimal` used to be `col_type() -> ColType::Text` /
    // `to_val() -> Val::Text(format!("{:.6}", x))` — no padding, no scaling,
    // no numeric tag, so `String: Ord` decided ordering everywhere the value
    // was compared (row-filter evaluator, in-memory sort, index-key range
    // planner), AND every value was an `f64` under the hood, so it never
    // promised exact arithmetic either. The fix stores `Decimal` as a native
    // `ColType::Integer` column of exact minor units instead: correct
    // ordering by construction (plain integer comparison, no float/text edge
    // case survives), and exact arithmetic within scale. These tests pin the
    // ENCODER + exactness side of that; a companion pair of host-side tests
    // (`decimal_orders_numerically_through_a_real_index_walk` and
    // `decimal_range_filter_is_numeric_through_a_real_query`) pins ordering
    // through a real query against a live store — the encoder alone cannot
    // prove ordering.

    #[test]
    fn decimal_encodes_as_a_native_integer_column() {
        assert_eq!(Decimal::col_type() as u8, ColType::Integer as u8);
        assert_eq!(Decimal::from_minor_units(9_000_000).to_val(), Val::Integer(9_000_000));
        assert_eq!(Decimal::from_minor_units(10_000_000).to_val(), Val::Integer(10_000_000));
        assert_eq!(Decimal::from_minor_units(-1_000_000).to_val(), Val::Integer(-1_000_000));
        assert_eq!(Decimal::from_minor_units(-10_000_000).to_val(), Val::Integer(-10_000_000));
    }

    #[test]
    fn decimal_roundtrips_exactly_through_from_val() {
        for minor in [0i64, 9_000_000, 10_000_000, -1_000_000, -10_000_000, 420_000, -3_140_000_1] {
            let back = Decimal::from_val(&Decimal::from_minor_units(minor).to_val());
            assert_eq!(back.minor_units(), minor, "Decimal must round-trip {minor} minor units exactly");
        }
    }

    /// The canonical float-imprecision case. `Decimal` is exact, so this
    /// must hold — it would NOT hold for a bare `f64` (`0.1 + 0.2 ==
    /// 0.30000000000000004`, not `0.3`), which is exactly the property the
    /// old `f64`-backed `Decimal` did not have either.
    #[test]
    fn decimal_addition_is_exact_not_float_approximate() {
        let a: Decimal = "0.1".parse().unwrap();
        let b: Decimal = "0.2".parse().unwrap();
        let sum = a + b;
        let expected: Decimal = "0.3".parse().unwrap();
        assert_eq!(sum, expected, "0.1 + 0.2 must be EXACTLY 0.3 for Decimal, unlike f64");
        assert_eq!(sum.minor_units(), 300_000);
        // The f64 property this must NOT exhibit, stated for contrast:
        assert_ne!(0.1_f64 + 0.2_f64, 0.3_f64, "sanity: f64 really doesn't hold this");
    }

    /// A large-magnitude case chosen so a `.get()`-then-`f64`-add-then-
    /// `Decimal::new()` implementation (routing through a plain float
    /// instead of exact integer addition) is empirically caught: at
    /// `9_007_199_254_740_992` minor units (2^53 — the point where `f64`
    /// can no longer represent every integer exactly) plus 1 minor unit,
    /// float-mediated addition lands one minor unit off. Ordinary
    /// small-magnitude cases like `0.1 + 0.2` do NOT reliably catch a
    /// float-routed `Add` at this 6-decimal-place scale — verified by
    /// deliberately mutating `Add` to `Decimal::new(self.get() + rhs.get())`
    /// during this fix's mutation-testing pass, which this test failed on
    /// and the `0.1 + 0.2` test above did not.
    #[test]
    fn decimal_addition_is_exact_even_at_large_magnitude() {
        let a = Decimal::from_minor_units(9_007_199_254_740_992);
        let b = Decimal::from_minor_units(1);
        assert_eq!(
            (a + b).minor_units(),
            9_007_199_254_740_993,
            "large-magnitude addition must stay exact integer addition, not round-trip through f64",
        );
    }

    /// A money-shaped case that would drift under `f64`: summing many small
    /// amounts. `0.01` repeated 100_000 times is `1000.00` exactly under
    /// integer minor-unit addition; under repeated `f64` addition this class
    /// of sum is a well-known source of accumulated rounding error.
    #[test]
    fn decimal_sum_of_many_small_amounts_is_exact() {
        let cent: Decimal = "0.01".parse().unwrap();
        let mut total = Decimal::ZERO;
        for _ in 0..100_000 {
            total = total + cent;
        }
        assert_eq!(total, "1000.00".parse::<Decimal>().unwrap());
        assert_eq!(total.minor_units(), 1_000_000_000);
    }

    #[test]
    fn decimal_subtraction_and_negation_are_exact() {
        let a: Decimal = "5.5".parse().unwrap();
        let b: Decimal = "2.25".parse().unwrap();
        assert_eq!(a - b, "3.25".parse::<Decimal>().unwrap());
        assert_eq!(-a, "-5.5".parse::<Decimal>().unwrap());
        assert_eq!(-(-a), a);
    }

    #[test]
    #[should_panic(expected = "overflow")]
    fn decimal_addition_panics_on_overflow_rather_than_wrapping() {
        let _ = Decimal::MAX + Decimal::from_minor_units(1);
    }

    #[test]
    fn decimal_from_str_parses_exactly_no_float_involved() {
        assert_eq!("19.99".parse::<Decimal>().unwrap().minor_units(), 19_990_000);
        assert_eq!("-3.140000".parse::<Decimal>().unwrap().minor_units(), -3_140_000);
        assert_eq!("5".parse::<Decimal>().unwrap().minor_units(), 5_000_000);
        assert_eq!("0".parse::<Decimal>().unwrap(), Decimal::ZERO);
        assert_eq!("-0.5".parse::<Decimal>().unwrap().minor_units(), -500_000);
    }

    #[test]
    fn decimal_from_str_rejects_more_precision_than_the_scale_rather_than_rounding() {
        // Exactness means refusing what it cannot represent exactly, not
        // silently rounding away the 7th digit.
        assert!("1.1234567".parse::<Decimal>().is_err());
        assert!("garbage".parse::<Decimal>().is_err());
        assert!("1.2.3".parse::<Decimal>().is_err());
        assert!("".parse::<Decimal>().is_err());
    }

    #[test]
    fn decimal_display_renders_six_decimal_places() {
        assert_eq!(Decimal::from_minor_units(19_990_000).to_string(), "19.990000");
        assert_eq!(Decimal::from_minor_units(-3_140_000).to_string(), "-3.140000");
        assert_eq!(Decimal::ZERO.to_string(), "0.000000");
    }

    #[test]
    fn decimal_serde_round_trips_as_a_decimal_string_not_a_json_number() {
        let d: Decimal = "19.99".parse().unwrap();
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"19.990000\"", "must serialize as a STRING, not a float-lossy JSON number");
        let back: Decimal = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    /// A `Decimal` field flowing through a real DTO, the shape a service
    /// author actually writes — confirms the wire form stays a decimal
    /// string end to end rather than leaking scaled integers into an API
    /// response.
    #[test]
    fn decimal_field_in_a_dto_serializes_as_a_decimal_string() {
        #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
        struct Price {
            amount: Decimal,
        }
        let p = Price { amount: "42.5".parse().unwrap() };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["amount"], serde_json::json!("42.500000"));
        let back: Price = serde_json::from_value(json).unwrap();
        assert_eq!(back.amount, p.amount);
    }

    #[test]
    fn timestamp_is_integer_millis() {
        let t = Timestamp::new(1_716_000_000_000);
        assert_eq!(Timestamp::from_val(&t.to_val()), t);
        assert_eq!(Timestamp::col_type() as u8, ColType::Integer as u8);
    }

    // -----------------------------------------------------------------
    // `Counter`'s read-verb split: `get` must read snapshot=true,
    // `get_for_update` must read snapshot=false, and `add` must call the
    // increment path — pinned against a fake `CounterStore` so this is a
    // fast, host-only proof that the two verbs are genuinely different
    // calls (not the same call under two names). The live-cluster proof
    // that snapshot=true/false actually differ in the store's conflict behaviour
    // lives in `crates/tests-integration/src/standalone_counters.rs`
    // (`a_snapshot_read_takes_no_conflict_range_and_an_update_read_does`),
    // which this test does not duplicate.
    // -----------------------------------------------------------------

    #[derive(Debug, PartialEq)]
    enum RecordedCall {
        Add { name: String, key: Vec<Val>, delta: i64 },
        Get { name: String, key: Vec<Val>, snapshot: bool },
        MaxObserve { name: String, key: Vec<Val>, value: i64 },
        MaxGet { name: String, key: Vec<Val>, snapshot: bool },
    }

    struct FakeCounterStore {
        calls: std::cell::RefCell<Vec<RecordedCall>>,
    }

    impl CounterStore for FakeCounterStore {
        fn counter_add(&self, name: &str, key: &[Val], delta: i64) -> Result<(), StoreError> {
            self.calls.borrow_mut().push(RecordedCall::Add {
                name: name.to_string(),
                key: key.to_vec(),
                delta,
            });
            Ok(())
        }
        fn counter_get(&self, name: &str, key: &[Val], snapshot: bool) -> Result<i64, StoreError> {
            self.calls.borrow_mut().push(RecordedCall::Get {
                name: name.to_string(),
                key: key.to_vec(),
                snapshot,
            });
            Ok(42)
        }
        fn max_observe(&self, name: &str, key: &[Val], value: i64) -> Result<(), StoreError> {
            self.calls.borrow_mut().push(RecordedCall::MaxObserve {
                name: name.to_string(),
                key: key.to_vec(),
                value,
            });
            Ok(())
        }
        fn max_get(
            &self,
            name: &str,
            key: &[Val],
            snapshot: bool,
        ) -> Result<Option<i64>, StoreError> {
            self.calls.borrow_mut().push(RecordedCall::MaxGet {
                name: name.to_string(),
                key: key.to_vec(),
                snapshot,
            });
            Ok(Some(7))
        }
    }

    struct LastPost;
    impl MaxAccum for LastPost {
        const NAME: &'static str = "rooms.last_post_at";
        type Key = [Val; 1];
    }

    /// The two reads must reach the store with DIFFERENT snapshot flags, and
    /// `observe` must not read at all. Same claim the counter's own test makes,
    /// because the same silent-hazard rule applies.
    #[test]
    fn max_accum_verbs_carry_their_snapshot_flag_to_the_store() {
        let s = FakeCounterStore { calls: Default::default() };
        let k = || [Val::Text("general".into())];
        LastPost::observe(&s, k(), 123).unwrap();
        assert_eq!(LastPost::get(&s, k()).unwrap(), Some(7));
        assert_eq!(LastPost::get_for_update(&s, k()).unwrap(), Some(7));

        let calls = s.calls.borrow();
        assert_eq!(
            calls[0],
            RecordedCall::MaxObserve {
                name: "rooms.last_post_at".into(),
                key: vec![Val::Text("general".into())],
                value: 123,
            },
            "observe must not read — no Get precedes it",
        );
        assert!(
            matches!(&calls[1], RecordedCall::MaxGet { snapshot: true, .. }),
            "get is the SNAPSHOT read: {:?}",
            calls[1],
        );
        assert!(
            matches!(&calls[2], RecordedCall::MaxGet { snapshot: false, .. }),
            "get_for_update takes the conflict range: {:?}",
            calls[2],
        );
    }

    struct Hits;
    impl Counter for Hits {
        const NAME: &'static str = "hits";
        type Key = [Val; 1];
    }

    #[test]
    fn get_reads_snapshot_true() {
        let store = FakeCounterStore { calls: Default::default() };
        let key = [Val::Text("1.2.3.4".to_string())];
        assert_eq!(Hits::get(&store, key.clone()).unwrap(), 42);
        assert_eq!(
            store.calls.borrow()[0],
            RecordedCall::Get { name: "hits".into(), key: key.to_vec(), snapshot: true },
            "Counter::get must be a snapshot read (snapshot=true)"
        );
    }

    #[test]
    fn get_for_update_reads_snapshot_false() {
        let store = FakeCounterStore { calls: Default::default() };
        let key = [Val::Text("1.2.3.4".to_string())];
        assert_eq!(Hits::get_for_update(&store, key.clone()).unwrap(), 42);
        assert_eq!(
            store.calls.borrow()[0],
            RecordedCall::Get { name: "hits".into(), key: key.to_vec(), snapshot: false },
            "Counter::get_for_update must take the conflict range (snapshot=false)"
        );
    }

    /// The binding assertion at the SDK level: `get` and `get_for_update`
    /// must issue DIFFERENT calls (different `snapshot` values) — if they
    /// issued the same call, the two verbs would be lying about being
    /// distinct.
    #[test]
    fn get_and_get_for_update_are_not_the_same_call() {
        let store = FakeCounterStore { calls: Default::default() };
        let key = [Val::Text("1.2.3.4".to_string())];
        Hits::get(&store, key.clone()).unwrap();
        Hits::get_for_update(&store, key.clone()).unwrap();
        let calls = store.calls.borrow();
        assert_ne!(calls[0], calls[1], "get and get_for_update must not be the same call");
    }

    #[test]
    fn add_calls_the_increment_path_with_the_given_delta() {
        let store = FakeCounterStore { calls: Default::default() };
        let key = [Val::Text("1.2.3.4".to_string())];
        Hits::add(&store, key.clone(), 5).unwrap();
        assert_eq!(
            store.calls.borrow()[0],
            RecordedCall::Add { name: "hits".into(), key: key.to_vec(), delta: 5 },
            "Counter::add must call the atomic-increment path with the given delta"
        );
    }

    /// `Id<T>` (the `of = Model` key form) must also encode through the
    /// same `Into<Vec<Val>>` seam — a single-element key-tuple wrapping the
    /// row id, matching `Field::to_val`.
    #[test]
    fn id_key_encodes_as_a_single_element_key_tuple() {
        struct Room;
        let id: Id<Room> = Id::new(7);
        let values: Vec<Val> = id.into();
        assert_eq!(values, vec![Val::Integer(7)]);
    }
}
