//! Request-scoped extension bag.
//!
//! `Ctx` carries arbitrary typed data through the guard chain and into
//! the handler. Guards stash loaded resources, parsed bodies, cached
//! identity lookups, etc., and the handler reads them without
//! re-fetching.
//!
//! Slots are keyed by `(TypeId, &'static str)`. The default slot (`""`)
//! covers the common case where exactly one value of a given type is
//! stashed per request. Named slots disambiguate when more than one
//! value of the same type needs to coexist — e.g. a route that loads
//! a "source" note and a "target" note.
//!
//! ```ignore
//! // Single-resource case (default slot):
//! ctx.insert::<store::Row>(row);
//! let row = ctx.require::<store::Row>();
//!
//! // Multi-resource case (named slots):
//! ctx.insert_at::<store::Row>("source", source_row);
//! ctx.insert_at::<store::Row>("target", target_row);
//! let source = ctx.require_at::<store::Row>("source");
//! let target = ctx.require_at::<store::Row>("target");
//! ```
//!
//! Wasm components run single-threaded per request, so `Ctx` doesn't
//! impose `Send`/`Sync` bounds on stored values.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Default slot used by `insert` / `get` / `require`. The empty string
/// is reserved as the no-name slot — pass a non-empty `&'static str` to
/// the `*_at` variants to disambiguate.
pub const DEFAULT_SLOT: &str = "";

/// Per-request extension bag. Construct with [`Ctx::new`]; populate
/// from guards via [`Ctx::insert`] / [`Ctx::insert_at`]; read from
/// handlers via [`Ctx::get`] / [`Ctx::require`] (or the `*_at` variants).
pub struct Ctx {
    bag: HashMap<(TypeId, &'static str), Box<dyn Any>>,
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

impl Ctx {
    pub fn new() -> Self {
        Self { bag: HashMap::new() }
    }

    /// Insert a value at the default slot for its type.
    ///
    /// In debug builds, panics if a value of the same type is already
    /// present at the default slot — almost always indicates two guards
    /// stashing the same shape without disambiguation. In release builds
    /// the second insert silently overwrites.
    pub fn insert<T: 'static>(&mut self, val: T) {
        self.insert_at(DEFAULT_SLOT, val);
    }

    /// Insert a value at a named slot. Use this when more than one
    /// value of the same type must coexist in the bag.
    pub fn insert_at<T: 'static>(&mut self, slot: &'static str, val: T) {
        let key = (TypeId::of::<T>(), slot);
        debug_assert!(
            !self.bag.contains_key(&key),
            "Ctx::insert_at: slot ({:?}, {:?}) already populated — \
             two guards stashing the same type without distinct slot names",
            std::any::type_name::<T>(),
            slot,
        );
        self.bag.insert(key, Box::new(val));
    }

    /// Read a value at the default slot for its type. Returns `None`
    /// when nothing was stashed.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.get_at(DEFAULT_SLOT)
    }

    /// Read a value at a named slot. Returns `None` when nothing was
    /// stashed there.
    pub fn get_at<T: 'static>(&self, slot: &'static str) -> Option<&T> {
        self.bag
            .get(&(TypeId::of::<T>(), slot))
            .and_then(|b| b.downcast_ref::<T>())
    }

    /// Require a value at the default slot for its type. Panics with a
    /// readable message when missing — use this in handlers when an
    /// upstream guard is contractually expected to have stashed the
    /// value.
    pub fn require<T: 'static>(&self) -> &T {
        self.require_at(DEFAULT_SLOT)
    }

    /// Require a value at a named slot. Panics with a readable message
    /// when missing.
    pub fn require_at<T: 'static>(&self, slot: &'static str) -> &T {
        self.get_at(slot).unwrap_or_else(|| {
            panic!(
                "Ctx::require_at: no value at slot ({:?}, {:?}) — \
                 a guard upstream of the handler is expected to populate it",
                std::any::type_name::<T>(),
                slot,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Note(String);

    #[test]
    fn insert_and_get_default_slot() {
        let mut ctx = Ctx::new();
        ctx.insert(Note("a".into()));
        assert_eq!(ctx.get::<Note>(), Some(&Note("a".into())));
        assert_eq!(ctx.require::<Note>(), &Note("a".into()));
    }

    #[test]
    fn insert_and_get_named_slot() {
        let mut ctx = Ctx::new();
        ctx.insert_at("source", Note("source".into()));
        ctx.insert_at("target", Note("target".into()));
        assert_eq!(ctx.get_at::<Note>("source"), Some(&Note("source".into())));
        assert_eq!(ctx.get_at::<Note>("target"), Some(&Note("target".into())));
    }

    #[test]
    fn default_and_named_slots_coexist() {
        let mut ctx = Ctx::new();
        ctx.insert(Note("default".into()));
        ctx.insert_at("other", Note("other".into()));
        assert_eq!(ctx.get::<Note>(), Some(&Note("default".into())));
        assert_eq!(ctx.get_at::<Note>("other"), Some(&Note("other".into())));
    }

    #[test]
    fn missing_slot_returns_none() {
        let ctx = Ctx::new();
        assert!(ctx.get::<Note>().is_none());
        assert!(ctx.get_at::<Note>("nope").is_none());
    }

    #[test]
    #[should_panic(expected = "no value at slot")]
    fn require_panics_when_missing() {
        let ctx = Ctx::new();
        let _: &Note = ctx.require();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "already populated")]
    fn double_insert_same_slot_debug_asserts() {
        let mut ctx = Ctx::new();
        ctx.insert(Note("first".into()));
        ctx.insert(Note("second".into()));
    }

    #[test]
    fn different_types_share_default_slot_without_collision() {
        let mut ctx = Ctx::new();
        ctx.insert(Note("hello".into()));
        ctx.insert(42u64);
        assert_eq!(ctx.get::<Note>(), Some(&Note("hello".into())));
        assert_eq!(ctx.get::<u64>(), Some(&42));
    }
}
