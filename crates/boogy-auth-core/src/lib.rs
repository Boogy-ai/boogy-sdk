//! Format-level auth primitives shared by the Boogy SDK (wasm side) and
//! the platform (host side).
//!
//! This crate is the public, wasm-clean core split out of `boogy-auth`:
//! - [`api_key`] — the `sk_*` API-key format: generation, hashing
//!   (SHA-256 / peppered HMAC), parsing, and constant-time verification.
//! - [`error`] — the common [`AuthError`] / [`AuthResult`] types.
//!
//! Token issuance/verification (PASETO), identity shapes, and key
//! management live in `boogy-auth`, which re-exports this crate.

pub mod api_key;
pub mod error;

pub use error::{AuthError, AuthResult};
