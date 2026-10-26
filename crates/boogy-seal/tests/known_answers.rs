//! Known answers for the seal envelope (RSA-OAEP-SHA256 key wrap +
//! AES-256-GCM): an envelope recorded before a dependency upgrade must still
//! unseal to its plaintext, and a key's fingerprint must not move.
//!
//! The key in `fixtures/known_answer_rsa2048.pkcs8.hex` is a TEST-ONLY key,
//! generated for these tests and used nowhere else. RECORDED values were
//! produced by this code before the RustCrypto/rand upgrade and must never be
//! re-blessed to make an upgrade pass.

use boogy_seal::{fingerprint, seal, unseal, SealEnvelope};
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
use secrecy::{ExposeSecret, SecretBox};

fn key() -> (Vec<u8>, Vec<u8>) {
    let hex = include_str!("fixtures/known_answer_rsa2048.pkcs8.hex").trim();
    let der: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
    let private = rsa::RsaPrivateKey::from_pkcs8_der(&der).unwrap();
    let public = rsa::RsaPublicKey::from(&private).to_public_key_der().unwrap().as_bytes().to_vec();
    (der, public)
}

/// RECORDED: `seal(public, PLAINTEXT).to_json_bytes()` (a fresh key and nonce
/// each seal, so this one envelope is the record).
const RECORDED_ENVELOPE: &str = r#"{"v":1,"k":"M7ydWowI8oX+gk4ExsuKdh0OqgNHbnwdz3jm4xijOXqsFWcIPUEjJk7DxFAkwKnb9qq6l0EPQUneJOEUovc+nPKLw1IQp+A9pAVe3I7wJXbnNRJEofQhZasWr835VgKLcPVNzMm/mHCcHTrdvfI61RWXOpk8Kuny2Z5jO7GMFhDs+FLBrDPK7ms2v4znn7iTuEuJ4angyOAMLgAfBnotKyZuqE1ki/XmBLc91PUE5Hnk+PfI1E4PiPEUmp1vPmwojqzO/Nye9UcyALTNbSPoxIBwHSJYWdgbgp6cX90OGbrz1PL8eonhd+D7FrY72fDvbFzDn3xp8UnlFhODjlwkcw==","n":"dwhHDZTzYDY8Tu2T","c":"ky/foAfJDSR20X3d2wyo5Bpm0CgOcVlLU1v2FeFBUtZAWKzJuG64bwjsC6PadnzkaQ=="}"#;
const PLAINTEXT: &[u8] = b"sk_live_known-answer-secret-value";

#[test]
fn a_recorded_envelope_still_unseals() {
    let (private, public) = key();
    let private = SecretBox::new(Box::new(private));
    let fresh = seal(&public, PLAINTEXT).unwrap();
    let fresh_json = String::from_utf8(fresh.to_json_bytes()).unwrap();
    assert_eq!(unseal(&private, &fresh).unwrap().expose_secret().as_slice(), PLAINTEXT);
    let recorded = SealEnvelope::parse_validated(RECORDED_ENVELOPE.as_bytes())
        .unwrap_or_else(|e| panic!("{e:?}; a fresh envelope to record: {fresh_json}"));
    assert_eq!(unseal(&private, &recorded).unwrap().expose_secret().as_slice(), PLAINTEXT, "a fresh envelope to record: {fresh_json}");
}

/// RECORDED: the recipient key's fingerprint.
#[test]
fn the_key_fingerprint_is_as_recorded() {
    let (_, public) = key();
    assert_eq!(fingerprint(&public), "03ebd090d96157a6119b0df84a24ae4518aa54593d8e3bce9f1fb5d3640e0b22");
}
