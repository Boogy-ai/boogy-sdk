//! The pinned client-side seal envelope: a client encrypts a secret value to a
//! recipient RSA public key so an intermediary can forward an opaque blob it
//! cannot decrypt — only the holder of the private key unseals.
//!
//! Scheme v1 (pinned; the `v` byte gates future rotation):
//!
//! - random AES-256 key K; AES-256-GCM(K, value) -> (nonce, ciphertext||tag)
//! - RSA-OAEP-SHA256(recipient_pub, K) -> wrapped_key
//!
//! Wire form: JSON `{ "v":1, "k":b64(wrapped_key), "n":b64(nonce), "c":b64(ct||tag) }`.
//! The same JSON is produced by a browser Web Crypto implementation, so the
//! cross-language round-trip is byte-compatible.
//!
//! Byte-interop invariants a Web Crypto implementation MUST match exactly:
//!
//! - **GCM AAD is empty.** Rust passes `Payload { aad: b"" }`; Web Crypto must
//!   omit `additionalData` (do not pass an empty-but-present buffer mismatch).
//! - **GCM tag length is 128 bits (16 bytes).** This is the default for both
//!   `aes-gcm` and Web Crypto; a Web Crypto author must NOT override `tagLength`
//!   (it accepts 32–128). The 16-byte tag is appended to the ciphertext, so
//!   `c = ciphertext || tag`.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

pub const SCHEME_V1: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealEnvelope {
    pub v: u8,
    #[serde(with = "b64")]
    pub k: Vec<u8>, // RSA-OAEP-wrapped AES key
    #[serde(with = "b64")]
    pub n: Vec<u8>, // 12-byte GCM nonce
    #[serde(with = "b64")]
    pub c: Vec<u8>, // AES-256-GCM ciphertext || 16-byte tag
}

mod b64 {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("unsupported scheme version {0}")]
    UnsupportedVersion(u8),
    #[error("malformed envelope: {0}")]
    Malformed(String),
    #[error("crypto failure")] // deliberately opaque — never leak plaintext/key detail
    Crypto,
}

impl SealEnvelope {
    /// Parse + structurally validate the JSON wire form WITHOUT decrypting — this
    /// is exactly what an intermediary runs to reject a plaintext/unsealed bind.
    pub fn parse_validated(bytes: &[u8]) -> Result<Self, SealError> {
        let env: SealEnvelope = serde_json::from_slice(bytes)
            .map_err(|e| SealError::Malformed(format!("json: {e}")))?;
        if env.v != SCHEME_V1 {
            return Err(SealError::UnsupportedVersion(env.v));
        }
        if env.n.len() != 12 {
            return Err(SealError::Malformed("nonce must be 12 bytes".into()));
        }
        if env.k.is_empty() || env.c.len() < 16 {
            return Err(SealError::Malformed("missing wrapped key / ciphertext".into()));
        }
        // Upper bounds: an untrusted intermediary must not accept a multi-MB blob
        // that passes the lower-bound checks. A v1 RSA-3072 wrapped key is exactly
        // 384 bytes (1024 leaves headroom); 256 KiB is a generous ciphertext backstop.
        if env.k.len() > 1024 {
            return Err(SealError::Malformed("wrapped key too large".into()));
        }
        if env.c.len() > 262144 {
            return Err(SealError::Malformed("ciphertext too large".into()));
        }
        Ok(env)
    }
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("SealEnvelope serializes")
    }
}

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use secrecy::{ExposeSecret, SecretBox};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Seal `plaintext` to `recipient_pub_der` (SPKI/DER RSA public key). Generates a
/// fresh AES-256 key + 12-byte nonce per call.
pub fn seal(recipient_pub_der: &[u8], plaintext: &[u8]) -> Result<SealEnvelope, SealError> {
    let pubkey = RsaPublicKey::from_public_key_der(recipient_pub_der).map_err(|_| SealError::Crypto)?;
    let mut rng = rand::thread_rng();
    // Raw AES-256 key material — zeroed on drop so it does not linger as a residual.
    let mut key = Zeroizing::new([0u8; 32]);
    let mut nonce = [0u8; 12];
    use rand::RngCore;
    rng.fill_bytes(&mut *key);
    rng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&*key).map_err(|_| SealError::Crypto)?;
    // Interop: AAD MUST be empty (Web Crypto: omit `additionalData`); the GCM tag
    // is 128 bits / 16 bytes (default — do not override) and appended: c = ct || tag.
    let c = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: b"" })
        .map_err(|_| SealError::Crypto)?;
    let wrapped = pubkey
        .encrypt(&mut rng, Oaep::new::<Sha256>(), &*key)
        .map_err(|_| SealError::Crypto)?;
    Ok(SealEnvelope { v: SCHEME_V1, k: wrapped, n: nonce.to_vec(), c })
}

/// Unseal an envelope with `recipient_priv_der` (PKCS#8 DER RSA private key).
/// Returns the plaintext in a zeroizing box. Opaque on any failure.
pub fn unseal(recipient_priv_der: &SecretBox<Vec<u8>>, env: &SealEnvelope) -> Result<SecretBox<Vec<u8>>, SealError> {
    if env.v != SCHEME_V1 {
        return Err(SealError::UnsupportedVersion(env.v));
    }
    let privkey = RsaPrivateKey::from_pkcs8_der(recipient_priv_der.expose_secret()).map_err(|_| SealError::Crypto)?;
    // Unwrapped raw AES-256 key material — zeroed on drop so it does not linger.
    let key = Zeroizing::new(privkey.decrypt(Oaep::new::<Sha256>(), &env.k).map_err(|_| SealError::Crypto)?);
    if key.len() != 32 || env.n.len() != 12 {
        return Err(SealError::Crypto);
    }
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SealError::Crypto)?;
    // Interop: AAD MUST be empty (Web Crypto: omit `additionalData`); the GCM tag
    // is 128 bits / 16 bytes (default) and is the trailing 16 bytes of c = ct || tag.
    let pt = cipher
        .decrypt(Nonce::from_slice(&env.n), Payload { msg: &env.c, aad: b"" })
        .map_err(|_| SealError::Crypto)?;
    Ok(SecretBox::new(Box::new(pt)))
}

/// SHA-256 fingerprint (lowercase hex) of a SPKI/DER public key — what clients
/// pin (TOFU) to detect substitution of the seal public key.
pub fn fingerprint(pub_der: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(pub_der);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_json_roundtrips_and_validates() {
        let env = SealEnvelope { v: 1, k: vec![1, 2, 3], n: vec![0u8; 12], c: vec![9u8; 32] };
        let bytes = env.to_json_bytes();
        let got = SealEnvelope::parse_validated(&bytes).unwrap();
        assert_eq!(got.n.len(), 12);
        assert_eq!(got.k, vec![1, 2, 3]);
    }
    #[test]
    fn rejects_plaintext_and_bad_version() {
        assert!(SealEnvelope::parse_validated(b"sk_live_not_json").is_err());
        let mut env = SealEnvelope { v: 2, k: vec![1], n: vec![0u8; 12], c: vec![0u8; 16] };
        assert!(matches!(SealEnvelope::parse_validated(&env.to_json_bytes()), Err(SealError::UnsupportedVersion(2))));
        env.v = 1; env.n = vec![0u8; 8];
        assert!(SealEnvelope::parse_validated(&env.to_json_bytes()).is_err());
    }
    #[test]
    fn rejects_oversized_key_and_ciphertext() {
        // Oversized wrapped key (> 1024 bytes) is rejected.
        let big_k = SealEnvelope { v: 1, k: vec![0u8; 1025], n: vec![0u8; 12], c: vec![0u8; 16] };
        assert!(matches!(
            SealEnvelope::parse_validated(&big_k.to_json_bytes()),
            Err(SealError::Malformed(_))
        ));
        // Oversized ciphertext (> 256 KiB) is rejected.
        let big_c = SealEnvelope { v: 1, k: vec![1u8; 384], n: vec![0u8; 12], c: vec![0u8; 262145] };
        assert!(matches!(
            SealEnvelope::parse_validated(&big_c.to_json_bytes()),
            Err(SealError::Malformed(_))
        ));
    }
}

#[cfg(test)]
mod crypto_tests {
    use super::*;
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};

    fn keypair() -> (Vec<u8>, SecretBox<Vec<u8>>) {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 3072).unwrap();
        let pub_der = RsaPublicKey::from(&priv_key).to_public_key_der().unwrap().as_bytes().to_vec();
        let priv_der = priv_key.to_pkcs8_der().unwrap().as_bytes().to_vec();
        (pub_der, SecretBox::new(Box::new(priv_der)))
    }

    #[test]
    fn seal_then_unseal_roundtrips() {
        let (pubk, privk) = keypair();
        let env = seal(&pubk, b"sk_live_secret_value").unwrap();
        let pt = unseal(&privk, &env).unwrap();
        assert_eq!(pt.expose_secret().as_slice(), b"sk_live_secret_value");
    }

    #[test]
    fn tampered_ciphertext_fails_closed() {
        let (pubk, privk) = keypair();
        let mut env = seal(&pubk, b"value").unwrap();
        env.c[0] ^= 0xff; // flip a byte -> GCM auth fail
        assert!(matches!(unseal(&privk, &env), Err(SealError::Crypto)));
    }

    #[test]
    fn wrong_recipient_key_fails_closed() {
        let (pubk, _) = keypair();
        let (_, other_priv) = keypair();
        let env = seal(&pubk, b"value").unwrap();
        assert!(matches!(unseal(&other_priv, &env), Err(SealError::Crypto)));
    }

    #[test]
    fn fingerprint_is_stable_and_keyed() {
        let (pubk, _) = keypair();
        assert_eq!(fingerprint(&pubk), fingerprint(&pubk));
        let (other, _) = keypair();
        assert_ne!(fingerprint(&pubk), fingerprint(&other));
    }
}
