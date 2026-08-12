//! `boogy secret set` / `boogy secret rm` — bind a per-service secret.
//!
//! The value is **sealed client-side** before it leaves this machine: the CLI
//! fetches the host's public seal key, encrypts the value to it, and sends only
//! the opaque envelope. The host forwards that blob across the credential
//! boundary without ever seeing the plaintext.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;

/// Shape of `GET /v1/seal-pubkey`.
#[derive(Debug, Deserialize)]
struct SealPubkey {
    public_der_b64: String,
    fingerprint: String,
    #[allow(dead_code)]
    scheme_version: u8,
}

/// Path of the trust-on-first-use fingerprint pin (`~/.config/boogy/seal-fingerprint`).
///
/// We use the platform config dir (XDG on Linux, the equivalent elsewhere) and
/// fall back to `$HOME/.config` so the pin lands somewhere stable.
fn pin_path() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("could not determine a config directory for the seal fingerprint pin")?;
    Ok(base.join("boogy").join("seal-fingerprint"))
}

/// TOFU check. Returns Ok if this fingerprint matches the stored pin or is the
/// first one seen (in which case it is stored). A mismatch is a hard error: we
/// NEVER fall back to sending plaintext.
///
/// Residual: TOFU only protects against substitution *after* the first contact.
/// The very first fetch is trusted blindly; an attacker present on first use can
/// pin their own key. Out-of-band verification of the printed fingerprint closes
/// that gap.
fn check_or_store_pin(fingerprint: &str) -> Result<()> {
    let path = pin_path()?;
    match std::fs::read_to_string(&path) {
        Ok(stored) => {
            let stored = stored.trim();
            if stored != fingerprint {
                anyhow::bail!(
                    "seal key fingerprint changed — refusing to send.\n\
                     pinned:  {stored}\n\
                     current: {fingerprint}\n\
                     If this rotation is intentional, delete the pin file and retry:\n\
                       {}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, fingerprint)
                .with_context(|| format!("failed to write seal fingerprint pin {}", path.display()))?;
            println!("Pinned seal key fingerprint (first use): {fingerprint}");
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Fetch + validate the host's public seal key. Returns the DER bytes.
async fn fetch_seal_key(host: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{host}/v1/seal-pubkey"))
        .send()
        .await
        .context("failed to reach host for the seal key")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("could not fetch the seal key ({status}): {body}");
    }

    let pk: SealPubkey = resp
        .json()
        .await
        .context("failed to parse the seal key response")?;

    let der = base64::engine::general_purpose::STANDARD
        .decode(pk.public_der_b64.as_bytes())
        .context("seal key public_der_b64 was not valid base64")?;

    // Defensively recompute the fingerprint over the DER we actually decoded and
    // compare it to the server-reported one — this catches a mangled response
    // where the bytes and the advertised fingerprint disagree.
    let computed = boogy_seal::fingerprint(&der);
    if computed != pk.fingerprint {
        anyhow::bail!(
            "seal key response is inconsistent — refusing to send.\n\
             server-reported fingerprint: {}\n\
             computed over the DER bytes:  {computed}",
            pk.fingerprint
        );
    }

    // Trust-on-first-use pin: store on first contact, hard-fail on a change.
    check_or_store_pin(&pk.fingerprint)?;

    Ok(der)
}

/// Bind a sealed secret value to a service.
pub async fn set(
    host: &str,
    token: &str,
    service: &str,
    name: &str,
    value: &str,
) -> Result<()> {
    let der = fetch_seal_key(host).await?;

    let envelope = boogy_seal::seal(&der, value.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to seal the secret value: {e}"))?;
    let sealed = envelope.to_json_bytes();

    let url = format!("{host}/v1/services/{service}/secrets/{name}");
    let client = reqwest::Client::new();
    let resp = client
        .put(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(sealed)
        .send()
        .await
        .context("failed to reach host")?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "set failed (401 Unauthorized): token invalid or missing scope — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }
    if status == reqwest::StatusCode::BAD_REQUEST {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "set failed (400 Bad Request): the host rejected the sealed value. \
             This should not happen when sealing is correct — host said: {body}"
        );
    }
    if status.is_success() {
        println!("Bound secret: {service}/{name}");
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("set failed ({status}): {body}");
    }
}

/// Remove a secret binding from a service.
pub async fn rm(host: &str, token: &str, service: &str, name: &str) -> Result<()> {
    let url = format!("{host}/v1/services/{service}/secrets/{name}");
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!(
            "rm failed (401 Unauthorized): token invalid or missing scope — \
             set --token or BOOGY_TOKEN to a valid token"
        );
    }

    if resp.status().is_success() {
        println!("Removed secret: {service}/{name}");
        Ok(())
    } else {
        anyhow::bail!("rm failed ({})", resp.status());
    }
}

/// Read a secret value from stdin (so it never lands in shell history).
pub fn read_value_from_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read secret value from stdin")?;
    // Drop a single trailing newline (echo / heredoc convenience) but keep the
    // rest of the value byte-for-byte.
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    Ok(buf)
}
