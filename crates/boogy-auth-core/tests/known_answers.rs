//! Known answers for the API-key hashes: published vectors, so a change to
//! the hash or MAC underneath them (a dependency upgrade included) cannot
//! silently change what a stored key hashes to.

use boogy_auth_core::api_key::{hash, hash_with_pepper, verify, verify_with_pepper};

/// FIPS 180-2, appendix B.1: SHA-256 of "abc".
#[test]
fn the_key_hash_is_sha256_hex() {
    let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(hash("abc"), want);
    assert!(verify("abc", want));
    assert!(!verify("abd", want));
}

/// RFC 4231, test case 2: HMAC-SHA256 with key "Jefe".
#[test]
fn the_peppered_hash_is_hmac_sha256_hex() {
    let want = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
    assert_eq!(hash_with_pepper("what do ya want for nothing?", b"Jefe"), want);
    assert!(verify_with_pepper("what do ya want for nothing?", want, b"Jefe"));
    assert!(!verify_with_pepper("what do ya want for nothing?", want, b"jefe"));
}
