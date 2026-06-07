//! Typed model layer: `#[derive(Model)]` maps a Rust struct to a store
//! table. This module holds the ordinary (host-testable) parts — the
//! `Field` and `Model` traits and the `Id`/`Decimal`/`Timestamp` value
//! types. The derive lives in `boogy-sdk-macros` (re-exported as
//! `boogy_sdk::Model`); the store-touching CRUD (`db_insert` etc.) is
//! emitted by `wit_glue!` in the consumer crate.

use core::marker::PhantomData;

use crate::store::{ColDef, ColType, Row, Table, Val};

/// One column's worth of typing: how a Rust field maps to a stored
/// column and how it round-trips through the portable [`Val`] enum.
/// Implement this for custom field types to extend the vocabulary.
pub trait Field: Sized {
    /// The stored column's type.
    fn col_type() -> ColType;
    /// Whether the column is nullable. Only `Option<T>` overrides this.
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

/// A fixed-precision decimal stored as 6-decimal-place text (the
/// convention used by tokenfeed for amounts/scores/weights). Owns the
/// `format!("{:.6}")` / parse round-trip so handlers stop doing it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decimal(pub f64);

impl Decimal {
    pub fn new(v: f64) -> Self { Decimal(v) }
    pub fn get(&self) -> f64 { self.0 }
}

impl Field for Decimal {
    fn col_type() -> ColType { ColType::Text }
    fn to_val(&self) -> Val { Val::Text(format!("{:.6}", self.0)) }
    fn from_val(v: &Val) -> Self { Decimal(v.as_text().parse().unwrap_or(0.0)) }
}

/// A unix-millis timestamp stored as an integer column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn new(millis: i64) -> Self { Timestamp(millis) }
    pub fn get(&self) -> i64 { self.0 }
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

// Helper so the derive can build a ColDef without knowing the field's
// concrete type at macro time — it calls this with the type's Field impl.
#[doc(hidden)]
pub fn col_def_for<T: Field>(name: &str, unique: bool) -> ColDef {
    ColDef {
        name: name.to_string(),
        col_type: T::col_type(),
        nullable: T::nullable(),
        unique,
        references: None,
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

    #[test]
    fn decimal_uses_6dp_text() {
        assert_eq!(Decimal::new(0.42).to_val(), Val::Text("0.420000".to_string()));
        let back = Decimal::from_val(&Val::Text("0.420000".to_string()));
        assert!((back.get() - 0.42).abs() < 1e-9);
        assert_eq!(Decimal::col_type() as u8, ColType::Text as u8);
    }

    #[test]
    fn timestamp_is_integer_millis() {
        let t = Timestamp::new(1_716_000_000_000);
        assert_eq!(Timestamp::from_val(&t.to_val()), t);
        assert_eq!(Timestamp::col_type() as u8, ColType::Integer as u8);
    }
}
