//! Opaque-id translation for apps that want enumeration-resistant
//! public identifiers while keeping the store's int (`u64`) primary keys
//! internally.
//!
//! ## When to use this
//!
//! Most apps can expose the raw `u64` PK in their API and be done.
//! That's fine for back-office tooling, integration tests, internal
//! services. For **user-facing** apps where exposing sequential ids
//! reveals signal you don't want public (post counts, user counts,
//! "what was post #42") an opaque mapping is warranted.
//!
//! `IdCodec` provides a deterministic, reversible mapping:
//!
//! ```ignore
//! let codec = IdCodec::new(*b"my-app-secret-16");
//! let public = codec.encode(42);          // "wzMx8...mY" (22 chars)
//! let back = codec.decode(&public);       // Some(42)
//! ```
//!
//! ## What it is
//!
//! - 16-byte secret key (AES-128).
//! - Encrypts a single 16-byte block containing an 8-byte magic
//!   pattern + the 8-byte big-endian u64. AES-128 is a deterministic
//!   permutation: same key + same id ⇒ same opaque output forever.
//! - Output is URL-safe base64 without padding — 22 chars.
//! - Decode validates the magic pattern after decryption, so garbage
//!   input or wrong-key input returns `None` cleanly.
//!
//! ## What it isn't
//!
//! - **Not a security primitive in isolation.** Sequential ids are
//!   leaked the moment any one is published outside the codec. The
//!   key is to use the codec at every API boundary and never expose
//!   the raw u64.
//! - **Not an HMAC.** The mapping is reversible (this is intentional —
//!   the API has to look up the row). An attacker who steals the key
//!   can enumerate ids; rotate keys via a new `IdCodec` instance and
//!   serve both during a transition window.
//! - **Not collision-free under key rotation.** A given u64 maps to a
//!   different opaque string under a different key. Apps that need
//!   stable opaque ids across key rotations should store the opaque
//!   id alongside the row (in a TEXT `public_id` column with a unique
//!   index) and look up by it.
//!
//! For most cases, "construct once at app start with a secret loaded
//! from `BOOGY_TOKENFEED_ID_SECRET` (or similar env-bound), then
//! encode/decode at the API boundary" is the right pattern.

use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 8-byte magic prefix. Encrypted into the first half of each block;
/// checked on decode to reject garbage / wrong-key inputs.
const MAGIC: &[u8; 8] = b"boogy_id";

/// Deterministic, reversible u64↔string codec via AES-128 single-block
/// encryption. See the module-level docs for the threat model.
#[derive(Clone)]
pub struct IdCodec {
    cipher: Aes128,
}

impl IdCodec {
    /// Construct a codec from a 16-byte secret key. The secret is the
    /// only thing protecting the mapping from inversion — anyone with
    /// the key can enumerate. Treat it like any other application
    /// secret: never log, never commit.
    pub fn new(secret: [u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(GenericArray::from_slice(&secret)),
        }
    }

    /// Encode a `u64` as an opaque 22-char URL-safe base64 string.
    /// Stable across calls for a given key+id pair.
    pub fn encode(&self, id: u64) -> String {
        let mut block = [0u8; 16];
        block[..8].copy_from_slice(MAGIC);
        block[8..].copy_from_slice(&id.to_be_bytes());
        self.cipher
            .encrypt_block(GenericArray::from_mut_slice(&mut block));
        URL_SAFE_NO_PAD.encode(block)
    }

    /// Decode an opaque string back to the underlying `u64`. Returns
    /// `None` if the input isn't valid base64, isn't the expected 16
    /// bytes after decoding, or if the decrypted block doesn't carry
    /// the magic prefix (i.e. wrong key or garbage input).
    pub fn decode(&self, opaque: &str) -> Option<u64> {
        let bytes = URL_SAFE_NO_PAD.decode(opaque.as_bytes()).ok()?;
        let mut block: [u8; 16] = bytes.as_slice().try_into().ok()?;
        self.cipher
            .decrypt_block(GenericArray::from_mut_slice(&mut block));
        if &block[..8] != MAGIC {
            return None;
        }
        let id_bytes: [u8; 8] = block[8..].try_into().ok()?;
        Some(u64::from_be_bytes(id_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> [u8; 16] {
        *b"test-secret-16!!"
    }

    #[test]
    fn round_trip_basic() {
        let c = IdCodec::new(test_secret());
        for id in [0u64, 1, 42, 9999, u64::MAX] {
            let s = c.encode(id);
            assert_eq!(c.decode(&s), Some(id), "round-trip failed for {id}");
        }
    }

    #[test]
    fn encoded_string_is_22_chars_url_safe_base64() {
        let c = IdCodec::new(test_secret());
        let s = c.encode(42);
        assert_eq!(s.len(), 22);
        assert!(
            s.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
            "expected URL-safe base64 alphabet, got: {s}",
        );
    }

    #[test]
    fn same_key_same_id_produces_same_string() {
        let c = IdCodec::new(test_secret());
        let a = c.encode(42);
        let b = c.encode(42);
        assert_eq!(a, b);
    }

    #[test]
    fn different_ids_produce_unrelated_strings() {
        let c = IdCodec::new(test_secret());
        let a = c.encode(1);
        let b = c.encode(2);
        // AES is a pseudorandom permutation — sequential ids should
        // produce strings with no visible common prefix.
        assert_ne!(a, b);
        let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
        assert!(common < 4, "ids 1 and 2 produced too-similar strings: {a} vs {b}");
    }

    #[test]
    fn wrong_key_returns_none() {
        let c1 = IdCodec::new(test_secret());
        let c2 = IdCodec::new(*b"wrong-secret-16!");
        let s = c1.encode(42);
        // Decoding with the wrong key produces a block that doesn't
        // start with MAGIC, so decode returns None.
        assert!(c2.decode(&s).is_none());
    }

    #[test]
    fn garbage_input_returns_none() {
        let c = IdCodec::new(test_secret());
        assert!(c.decode("not-base64-at-all!!").is_none());
        assert!(c.decode("").is_none());
        assert!(c.decode("YWJj").is_none()); // base64 of "abc" — wrong length
        assert!(c.decode(&"A".repeat(22)).is_none()); // valid base64, valid length, but won't decrypt to MAGIC
    }

    #[test]
    fn clone_produces_equivalent_codec() {
        let c1 = IdCodec::new(test_secret());
        let c2 = c1.clone();
        let s = c1.encode(42);
        assert_eq!(c2.decode(&s), Some(42));
    }
}
