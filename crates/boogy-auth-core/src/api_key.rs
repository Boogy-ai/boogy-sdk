//! API key format, generation, hashing, and verification.
//!
//! Format: `sk_<env>_<26 base62>_<4 hex crc32c>`.
//!
//! - The `sk_` prefix is the universally recognised "secret key" marker.
//! - `env` is operator-chosen — typically `live` or `test`. The format
//!   doesn't enforce a particular set; it just has to be ASCII
//!   alphanumeric.
//! - The 26 base62 random characters carry ~155 bits of entropy.
//! - The 4-char CRC32C suffix is a self-check: secret-scanner regexes
//!   (GitHub, GitLab, etc.) use the structure to detect leaked tokens
//!   in source dumps.
//!
//! ## Hashing
//!
//! API keys are high-entropy random tokens, not user-chosen passwords.
//! Brute force isn't a credible threat at 155 bits, so we use plain
//! SHA-256 + constant-time compare rather than a password-hashing
//! function (Argon2, bcrypt). This is the standard pattern across
//! Stripe / GitHub / Linear / Vercel and is many orders of magnitude
//! faster than Argon2id without losing security.
//!
//! Defense-in-depth via a server-side pepper (HMAC-SHA256 with a key
//! held outside the DB) is supported via [`hash_with_pepper`] —
//! deployers who want it can configure their store accordingly.

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{AuthError, AuthResult};

/// Length of the random body portion of an API key, in base62 characters.
const BODY_LEN: usize = 26;

/// Length of the CRC32C suffix in lowercase hex characters.
const CRC_LEN: usize = 4;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A freshly generated key. The `secret` is shown to the operator
/// **once**; everything else is what gets persisted.
///
/// The `Debug` impl deliberately redacts `secret` and `hash` —
/// `tracing::debug!(?key, ...)` is a common pattern and we don't
/// want it to spill secret material into log files. Programmatic
/// access via the field accessors is unaffected.
#[derive(Clone)]
pub struct ApiKey {
    /// The full `sk_<env>_<body>_<crc>` string. Treat as a secret —
    /// store nowhere except the operator's secrets manager.
    pub secret: String,
    /// First 11 characters of the secret — `sk_`, the env, and the
    /// leading body characters. Stored in the clear; used as a fast
    /// index for lookup.
    pub prefix: String,
    /// SHA-256 hex digest of `secret`. This is what gets persisted.
    pub hash: String,
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("prefix", &self.prefix)
            .field("secret", &"<redacted>")
            .field("hash", &"<redacted>")
            .finish()
    }
}

/// A parsed but unverified API key (i.e. we recognised the format and
/// the CRC checks out, but we haven't compared it against any stored
/// hash yet).
///
/// Same redaction applies as [`ApiKey`] — the hash is sensitive
/// because it was computed from the secret in the same request.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedKey {
    pub env: String,
    pub prefix: String,
    /// SHA-256 hex digest of the raw key string.
    pub hash: String,
}

impl std::fmt::Debug for ParsedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedKey")
            .field("env", &self.env)
            .field("prefix", &self.prefix)
            .field("hash", &"<redacted>")
            .finish()
    }
}

/// Generate a fresh API key for `env` (e.g. `"live"`, `"test"`).
///
/// The `env` segment must be ASCII alphanumeric, length 1..=16. Returns
/// `AuthError::Key` if not.
pub fn generate(env: &str) -> AuthResult<ApiKey> {
    validate_env(env)?;

    let body = random_base62(BODY_LEN);
    let core = format!("sk_{env}_{body}");
    let crc = crc_suffix(&core);
    let secret = format!("{core}_{crc}");
    let prefix = compute_prefix(&secret);
    let hash = sha256_hex(&secret);
    Ok(ApiKey { secret, prefix, hash })
}

/// Parse an `sk_<env>_<body>_<crc>` string. Validates the structural
/// shape and the CRC suffix; does **not** compare against a stored
/// hash (use [`verify`] for that).
pub fn parse(s: &str) -> AuthResult<ParsedKey> {
    let mut parts = s.splitn(4, '_');
    let p0 = parts.next();
    let env = parts.next();
    let body = parts.next();
    let crc = parts.next();

    let (Some("sk"), Some(env), Some(body), Some(crc)) = (p0, env, body, crc) else {
        return Err(AuthError::Malformed("api key: not in sk_<env>_<body>_<crc> form".into()));
    };

    if body.len() != BODY_LEN || !body.bytes().all(is_base62) {
        return Err(AuthError::Malformed(format!(
            "api key: body must be {BODY_LEN} base62 chars"
        )));
    }
    if crc.len() != CRC_LEN || !crc.bytes().all(is_hex_lower) {
        return Err(AuthError::Malformed(format!(
            "api key: crc must be {CRC_LEN} lowercase hex chars"
        )));
    }
    validate_env(env)?;

    let core = format!("sk_{env}_{body}");
    if crc_suffix(&core) != crc {
        return Err(AuthError::Malformed("api key: crc mismatch (likely typo or truncation)".into()));
    }

    Ok(ParsedKey {
        env: env.to_string(),
        prefix: compute_prefix(s),
        hash: sha256_hex(s),
    })
}

/// SHA-256 hex digest of `s`. Use for storage.
pub fn hash(s: &str) -> String {
    sha256_hex(s)
}

/// HMAC-SHA256 hex digest of `s` keyed by `pepper`. Use this for
/// hash storage when you want to keep the DB-resident hash useless
/// in isolation: an attacker who steals only the DB cannot brute-force
/// keys against the hashes without also stealing the pepper.
pub fn hash_with_pepper(s: &str, pepper: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(pepper)
        .expect("HMAC accepts any key length");
    mac.update(s.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

/// Constant-time comparison of `candidate` against `stored_hash`.
/// `candidate` is the raw key string (the user's input); we re-hash it
/// here so the caller doesn't have to remember to hash first.
pub fn verify(candidate: &str, stored_hash: &str) -> bool {
    let computed = sha256_hex(candidate);
    bool::from(computed.as_bytes().ct_eq(stored_hash.as_bytes()))
}

/// Constant-time pepper-aware variant of [`verify`].
pub fn verify_with_pepper(candidate: &str, stored_hash: &str, pepper: &[u8]) -> bool {
    let computed = hash_with_pepper(candidate, pepper);
    bool::from(computed.as_bytes().ct_eq(stored_hash.as_bytes()))
}

/// First 11 characters of the secret — `sk_`, the env, and the
/// leading body characters. Used as the indexed lookup key in storage.
pub fn compute_prefix(secret: &str) -> String {
    secret.chars().take(11).collect()
}

// -----------------------------------------------------------------------------
// Internals
// -----------------------------------------------------------------------------

fn validate_env(env: &str) -> AuthResult<()> {
    if env.is_empty() || env.len() > 16 {
        return Err(AuthError::Key(format!(
            "api key env must be 1..=16 chars, got {}",
            env.len()
        )));
    }
    if !env.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AuthError::Key(format!(
            "api key env must be ASCII alphanumeric: {env:?}"
        )));
    }
    Ok(())
}

fn random_base62(n: usize) -> String {
    // Rejection sampling: any byte ≥ 248 is dropped and re-rolled.
    // 248 = 4 × 62 is the largest multiple of 62 ≤ 256, so accepting
    // values [0, 248) and taking mod 62 gives a uniform distribution
    // across the 62 buckets — no modulo bias. The expected reroll
    // rate is 8/256 ≈ 3.1%, which is negligible compared to the
    // OsRng cost we'd pay anyway.
    const ACCEPT_THRESHOLD: u8 = 248;
    let mut out = String::with_capacity(n);
    let mut buf = [0u8; 64];
    let mut idx = buf.len();
    while out.len() < n {
        if idx >= buf.len() {
            OsRng.fill_bytes(&mut buf);
            idx = 0;
        }
        let b = buf[idx];
        idx += 1;
        if b >= ACCEPT_THRESHOLD {
            continue;
        }
        out.push(BASE62[(b as usize) % 62] as char);
    }
    out
}

fn crc_suffix(core: &str) -> String {
    let crc = crc32c::crc32c(core.as_bytes());
    // Take the low 16 bits → 4 hex chars. Identical inputs always
    // produce identical suffixes; that's the self-check property
    // secret scanners exploit.
    let truncated = (crc & 0xFFFF) as u16;
    format!("{truncated:04x}")
}

fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn is_base62(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_hex_lower(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_well_formed_key() {
        let key = generate("live").unwrap();
        // Shape: sk_live_<26 base62>_<4 hex>
        assert!(key.secret.starts_with("sk_live_"));
        let parts: Vec<&str> = key.secret.splitn(4, '_').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "sk");
        assert_eq!(parts[1], "live");
        assert_eq!(parts[2].len(), BODY_LEN);
        assert_eq!(parts[3].len(), CRC_LEN);

        // sk_live_ (8 chars) + 3 body chars = 11.
        assert_eq!(key.prefix.len(), 11);
        assert!(key.secret.starts_with(&key.prefix));

        // Hash is 64 hex chars (SHA-256).
        assert_eq!(key.hash.len(), 64);
    }

    #[test]
    fn each_generation_is_distinct() {
        let a = generate("test").unwrap();
        let b = generate("test").unwrap();
        assert_ne!(a.secret, b.secret);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn parse_round_trips_a_generated_key() {
        let key = generate("live").unwrap();
        let parsed = parse(&key.secret).unwrap();
        assert_eq!(parsed.env, "live");
        assert_eq!(parsed.prefix, key.prefix);
        assert_eq!(parsed.hash, key.hash);
    }

    #[test]
    fn parse_rejects_bad_crc() {
        let mut key = generate("live").unwrap();
        // Flip one char of the CRC suffix.
        let last = key.secret.pop().unwrap();
        let flipped = if last == 'a' { 'b' } else { 'a' };
        key.secret.push(flipped);
        let err = parse(&key.secret).unwrap_err();
        assert!(matches!(err, AuthError::Malformed(_)));
    }

    #[test]
    fn parse_rejects_wrong_body_length() {
        let err = parse("sk_live_short_abcd").unwrap_err();
        assert!(matches!(err, AuthError::Malformed(_)));
    }

    #[test]
    fn parse_rejects_wrong_shape() {
        assert!(parse("not-a-key").is_err());
        assert!(parse("sk_live").is_err());
        assert!(parse("xx_live_xxxxxxxxxxxxxxxxxxxxxxxxxx_abcd").is_err());
    }

    #[test]
    fn verify_accepts_correct_secret() {
        let key = generate("live").unwrap();
        assert!(verify(&key.secret, &key.hash));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let key = generate("live").unwrap();
        let other = generate("live").unwrap();
        assert!(!verify(&other.secret, &key.hash));
    }

    #[test]
    fn verify_with_pepper_round_trips() {
        let key = generate("live").unwrap();
        let pepper = b"super-secret-deployment-pepper";
        let stored = hash_with_pepper(&key.secret, pepper);
        assert!(verify_with_pepper(&key.secret, &stored, pepper));
        // Same hash without the pepper does not match.
        assert!(!verify(&key.secret, &stored));
        // Wrong pepper does not verify.
        assert!(!verify_with_pepper(&key.secret, &stored, b"wrong-pepper"));
    }

    #[test]
    fn env_validation_rejects_bad_input() {
        assert!(generate("").is_err());
        assert!(generate(&"a".repeat(17)).is_err());
        assert!(generate("hello!").is_err());
        assert!(generate("with space").is_err());
        assert!(generate("a-b").is_err()); // hyphens not in alphanumeric
    }

    #[test]
    fn env_validation_accepts_typical_inputs() {
        for env in ["live", "test", "dev", "prod", "stage123", "v2"] {
            generate(env).unwrap();
        }
    }

    #[test]
    fn prefix_is_first_11_chars_of_secret() {
        let key = generate("live").unwrap();
        let manual: String = key.secret.chars().take(11).collect();
        assert_eq!(key.prefix, manual);
    }

    #[test]
    fn parse_recognises_sk_test_envs() {
        let key = generate("test").unwrap();
        let parsed = parse(&key.secret).unwrap();
        assert_eq!(parsed.env, "test");
    }

    #[test]
    fn debug_impl_redacts_secret_and_hash() {
        let key = generate("live").unwrap();
        let formatted = format!("{key:?}");
        // Prefix is non-secret and shows for debugging.
        assert!(formatted.contains(&key.prefix));
        // Secret and hash MUST NOT appear in the rendered string.
        // (`<redacted>` is the marker the impl writes.)
        assert!(!formatted.contains(&key.secret));
        assert!(!formatted.contains(&key.hash));
        assert!(formatted.contains("<redacted>"));
    }

    #[test]
    fn parsed_key_debug_redacts_hash() {
        let key = generate("live").unwrap();
        let parsed = parse(&key.secret).unwrap();
        let formatted = format!("{parsed:?}");
        assert!(formatted.contains("live")); // env shows
        assert!(formatted.contains(&parsed.prefix));
        assert!(!formatted.contains(&parsed.hash));
        assert!(formatted.contains("<redacted>"));
    }

    #[test]
    fn hash_is_deterministic() {
        // Same input → same hash. The store layer treats `hash()` as
        // a pure function of the secret; if this regresses, every
        // verify-against-stored-hash call silently fails.
        let secret = "sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd";
        assert_eq!(hash(secret), hash(secret));
    }

    #[test]
    fn hash_matches_generate_output() {
        // `generate()` populates `hash` by calling the same routine
        // we expose publicly. Lock that invariant — otherwise stored
        // hashes would diverge from what callers compute on lookup.
        let key = generate("live").unwrap();
        assert_eq!(key.hash, hash(&key.secret));
    }

    #[test]
    fn hash_differs_per_input() {
        // Sanity floor: two unrelated keys must not collide. (SHA-256
        // makes this overwhelmingly likely; the test catches the case
        // where someone hard-codes a constant or hashes only a
        // prefix.)
        let a = generate("live").unwrap();
        let b = generate("live").unwrap();
        assert_ne!(a.hash, b.hash);
        assert_ne!(hash(&a.secret), hash(&b.secret));
    }

    #[test]
    fn hash_with_pepper_is_deterministic() {
        let pepper = b"pepper-bytes";
        let secret = "sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd";
        assert_eq!(
            hash_with_pepper(secret, pepper),
            hash_with_pepper(secret, pepper)
        );
    }

    #[test]
    fn hash_with_pepper_differs_from_unpeppered() {
        let secret = "sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd";
        assert_ne!(hash(secret), hash_with_pepper(secret, b"pepper"));
    }

    #[test]
    fn hash_with_pepper_differs_per_pepper() {
        // Distinct peppers → distinct hashes for the same secret.
        let secret = "sk_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_abcd";
        assert_ne!(
            hash_with_pepper(secret, b"pepper-a"),
            hash_with_pepper(secret, b"pepper-b")
        );
    }
}
