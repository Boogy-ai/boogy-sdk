//! `boogy signing import` / `boogy signing list` — bring your own signing key.
//!
//! The private key is **sealed on this machine** to the host's public seal key
//! (pinned on first use, as for `boogy secret set`), so only the signing
//! service ever opens it. The file is sent as it is, PEM armour removed: an
//! Apple `.p8` works as downloaded, and so does an `openssl ecparam -genkey`
//! file (its `EC PARAMETERS` block is skipped for the private key's). A
//! secp256k1 key may also be a 64-hex-character text file. The CLI does not
//! parse the key; the platform does, and refuses a key for the wrong curve.

use anyhow::{Context, Result};
use base64::Engine;

/// The algorithms `--alg` accepts, as the import endpoint names them.
pub const ALGS: &[&str] = &["ecdsa-p256", "ed25519", "ecdsa-secp256k1"];

/// The key bytes to seal, for `alg`:
/// - a PEM file: the DER of the block whose label ends in `PRIVATE KEY`
///   (`PRIVATE KEY`, `EC PRIVATE KEY`), whatever blocks come before it;
/// - for `ecdsa-secp256k1`, a file holding 64 hex characters: the 32 bytes
///   they spell;
/// - anything else as it is (DER, or a raw 32-byte key).
///
/// Every buffer holding key material is zeroized when dropped.
pub fn key_bytes(file: &[u8], alg: &str) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    use zeroize::Zeroizing;
    let Ok(text) = std::str::from_utf8(file) else { return Ok(Zeroizing::new(file.to_vec())) };
    if alg == "ecdsa-secp256k1" {
        let hex = text.trim();
        if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let digit = |b: u8| (b as char).to_digit(16).expect("checked hex") as u8;
            let bytes = hex.as_bytes().chunks(2).map(|p| (digit(p[0]) << 4) | digit(p[1])).collect();
            return Ok(Zeroizing::new(bytes));
        }
    }
    if !text.trim_start().starts_with("-----BEGIN ") {
        return Ok(Zeroizing::new(file.to_vec()));
    }
    let mut lines = text.lines().map(str::trim);
    let body = loop {
        let Some(line) = lines.next() else {
            anyhow::bail!("the PEM file holds no PRIVATE KEY block");
        };
        let is_key = line.strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----")).is_some_and(|l| l.ends_with("PRIVATE KEY"));
        if is_key {
            break Zeroizing::new(lines.by_ref().take_while(|l| !l.starts_with("-----END ")).collect::<String>());
        }
    };
    let der = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(body.as_bytes())
            .context("the PEM file's private key is not base64")?,
    );
    anyhow::ensure!(!der.is_empty(), "the PEM file holds no key");
    Ok(der)
}

/// Import the private key in `key_file` under `service`/`label`.
pub async fn import(host: &str, token: &str, service: &str, label: &str, alg: &str, key_file: &std::path::Path) -> Result<()> {
    anyhow::ensure!(ALGS.contains(&alg), "--alg must be one of {}", ALGS.join(", "));
    let file = zeroize::Zeroizing::new(std::fs::read(key_file).with_context(|| format!("failed to read {}", key_file.display()))?);
    let key = key_bytes(&file, alg)?;
    let der = crate::secret::fetch_seal_key(host).await?;
    let envelope = boogy_seal::seal(&der, &key).map_err(|e| anyhow::anyhow!("failed to seal the key: {e}"))?;
    let body = serde_json::json!({
        "alg": alg,
        "sealed": base64::engine::general_purpose::STANDARD.encode(envelope.to_json_bytes()),
    });
    let resp = crate::http::client()
        .put(format!("{host}/v1/services/{service}/signing-keys/{label}"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .context("failed to reach host")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("import failed ({status}): {text}");
    }
    println!("Imported {alg} key: {service}/{label}");
    // The import answers no body; the public key is read back from the list.
    if let Some(pk) = list_keys(host, token, service).await?.into_iter().find(|k| k.0 == label) {
        println!("public key: {}", pk.2);
    }
    Ok(())
}

/// `(label, alg, public_key_hex)` for every key `service` holds.
async fn list_keys(host: &str, token: &str, service: &str) -> Result<Vec<(String, String, String)>> {
    let resp = crate::http::client()
        .get(format!("{host}/v1/services/{service}/signing-keys"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("list failed ({status}): {text}");
    }
    let v: serde_json::Value = resp.json().await.context("the key list is not JSON")?;
    Ok(v["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|k| {
                    let s = |f: &str| k[f].as_str().unwrap_or_default().to_string();
                    (s("label"), s("alg"), s("public_key_hex"))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Print every signing key `service` holds: label, algorithm, public key.
pub async fn list(host: &str, token: &str, service: &str) -> Result<()> {
    for (label, alg, pk) in list_keys(host, token, service).await? {
        println!("{label}\t{alg}\t{pk}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_p8_file_yields_the_der_inside_it() {
        let der = vec![0x30u8, 0x81, 0x87, 0x02, 0x01, 0x00];
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&der)
        );
        assert_eq!(*key_bytes(pem.as_bytes(), "ecdsa-p256").unwrap(), der);
    }

    /// `openssl ecparam -name prime256v1 -genkey` writes the curve's
    /// `EC PARAMETERS` block first; the key is the `EC PRIVATE KEY` block.
    #[test]
    fn an_ecparam_genkey_file_yields_its_private_key_not_its_parameters() {
        let der = vec![0x30u8, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
        let pem = format!(
            "-----BEGIN EC PARAMETERS-----\nBggqhkjOPQMBBw==\n-----END EC PARAMETERS-----\n\
             -----BEGIN EC PRIVATE KEY-----\n{}\n-----END EC PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(&der)
        );
        assert_eq!(*key_bytes(pem.as_bytes(), "ecdsa-p256").unwrap(), der);
        assert!(key_bytes(b"-----BEGIN EC PARAMETERS-----\nBggqhkjOPQMBBw==\n-----END EC PARAMETERS-----\n", "ecdsa-p256").is_err(), "no key block");
    }

    #[test]
    fn a_hex_secp256k1_file_is_its_32_bytes() {
        let hex = format!("{}\n", "0a".repeat(32));
        assert_eq!(*key_bytes(hex.as_bytes(), "ecdsa-secp256k1").unwrap(), vec![0x0au8; 32]);
        assert_eq!(*key_bytes(hex.as_bytes(), "ecdsa-p256").unwrap(), hex.as_bytes(), "only for secp256k1");
    }

    #[test]
    fn a_der_or_raw_file_is_sent_as_it_is() {
        let raw = vec![7u8; 32];
        assert_eq!(*key_bytes(&raw, "ecdsa-secp256k1").unwrap(), raw);
    }

    #[test]
    fn a_pem_file_with_a_broken_body_is_refused() {
        assert!(key_bytes(b"-----BEGIN PRIVATE KEY-----\n!!!\n-----END PRIVATE KEY-----\n", "ed25519").is_err());
        assert!(key_bytes(b"-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----\n", "ed25519").is_err());
    }
}
