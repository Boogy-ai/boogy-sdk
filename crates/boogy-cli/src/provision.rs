use std::path::Path;

use anyhow::{Context, Result};
use reqwest::multipart;
use serde_json::json;

/// Read a manifest toml + resolve and read the wasm bytes relative to the
/// manifest directory. Returns (manifest_content, wasm_full_path, wasm_bytes).
///
/// Accepts both `[service]` and the legacy `[api]` section name (the server
/// accepts either via `#[serde(alias = "api")]`).
pub fn read_manifest_and_wasm(manifest_path: &str) -> Result<(String, std::path::PathBuf, Vec<u8>)> {
    let manifest_content =
        std::fs::read_to_string(manifest_path).context("failed to read manifest")?;

    let manifest: toml::Value =
        toml::from_str(&manifest_content).context("failed to parse manifest")?;

    // Accept [service] (canonical) or legacy [api] alias.
    let service_section = manifest
        .get("service")
        .or_else(|| manifest.get("api"))
        .context("manifest missing [service] (or [api]) section")?;

    let wasm_path = service_section
        .get("wasm")
        .and_then(|w| w.as_str())
        .context("manifest missing service.wasm field")?;

    // Resolve wasm path relative to manifest location
    let manifest_dir = Path::new(manifest_path).parent().unwrap_or(Path::new("."));
    let wasm_full_path = manifest_dir.join(wasm_path);

    let wasm_bytes = std::fs::read(&wasm_full_path)
        .with_context(|| format!("failed to read wasm at: {}", wasm_full_path.display()))?;

    Ok((manifest_content, wasm_full_path, wasm_bytes))
}

/// Publish a module: an immutable, versioned wasm+manifest artifact.
///
/// When `provision` is true the host also provisions the publisher's own
/// service from the manifest in the same call.
pub async fn publish(host: &str, token: &str, manifest_path: &str, provision: bool) -> Result<()> {
    let (manifest_content, wasm_full_path, wasm_bytes) = read_manifest_and_wasm(manifest_path)?;

    println!("Publishing module...");
    println!("  Manifest: {manifest_path}");
    println!(
        "  Wasm: {} ({} bytes)",
        wasm_full_path.display(),
        wasm_bytes.len()
    );
    if provision {
        println!("  Provision: true (publisher's own service)");
    }

    let mut form = multipart::Form::new().text("manifest", manifest_content).part(
        "wasm",
        multipart::Part::bytes(wasm_bytes).file_name("component.wasm"),
    );
    if provision {
        form = form.text("provision", "true");
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{host}/v1/modules"))
        .header("Authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .context("failed to reach host")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("publish failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("failed to parse response")?;
    let module = body
        .get("module")
        .and_then(|m| m.as_str())
        .unwrap_or("<unknown>");
    let provisioned = body
        .get("provisioned")
        .and_then(|p| p.as_bool())
        .unwrap_or(false);

    println!("Published: {module}");

    if provision && !provisioned {
        // The module was published successfully but the auto-provision step
        // failed. Surface the server-side error and exit non-zero so scripts
        // and CI catch it.
        let provision_error = body
            .get("provision_error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!(
            "module published but auto-provision failed: {provision_error}\n\
             The module is available — re-provision manually with `boogy provision`."
        );
    }

    println!("  Provisioned: {provisioned}");

    Ok(())
}

/// Provision a service instance from a published module.
pub async fn provision(
    host: &str,
    token: &str,
    module: &str,
    service_id: &str,
    overrides_path: Option<&str>,
) -> Result<()> {
    let overrides = match overrides_path {
        Some(p) => Some(std::fs::read_to_string(p).context("failed to read overrides file")?),
        None => None,
    };

    println!("Provisioning service...");
    println!("  Module: {module}");
    println!("  Service: {service_id}");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{host}/v1/services"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "module": module,
            "service_id": service_id,
            "overrides": overrides,
        }))
        .send()
        .await
        .context("failed to reach host")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("provision failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("failed to parse response")?;
    let service = body
        .get("service")
        .and_then(|s| s.as_str())
        .unwrap_or("<unknown>");
    let deployment_id = body
        .get("deployment_id")
        .map(|d| d.to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    println!("Provisioned: {service}");
    println!("  Deployment: {deployment_id}");

    Ok(())
}

/// Upgrade a provisioned service to a newer module version.
pub async fn upgrade(host: &str, token: &str, service_id: &str, to: &str) -> Result<()> {
    println!("Upgrading service...");
    println!("  Service: {service_id}");
    println!("  To version: {to}");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{host}/v1/services/{service_id}/upgrade"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({ "to_version": to }))
        .send()
        .await
        .context("failed to reach host")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("upgrade failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("failed to parse response")?;
    let service = body
        .get("service")
        .and_then(|s| s.as_str())
        .unwrap_or("<unknown>");
    let module_version = body
        .get("module_version")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let deployment_id = body
        .get("deployment_id")
        .map(|d| d.to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    println!("Upgraded: {service}");
    println!("  Module version: {module_version}");
    println!("  Deployment: {deployment_id}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Write a manifest file + optional wasm file into a unique temp subdir.
    /// Returns the manifest path. Caller is responsible for cleanup (dir is
    /// deterministic per test name via a uuid-like suffix derived from content).
    fn setup_test_dir(
        test_name: &str,
        manifest_content: &str,
        wasm_rel: Option<&str>,
    ) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("boogy-cli-tests-{test_name}"));
        std::fs::create_dir_all(&base).expect("create test dir");
        let manifest_path = base.join("boogy.toml");
        std::fs::write(&manifest_path, manifest_content).expect("write manifest");
        if let Some(rel) = wasm_rel {
            let wasm_path = base.join(rel);
            std::fs::write(&wasm_path, b"\0asm").expect("write wasm");
        }
        (base, manifest_path)
    }

    /// FIX 4: [service] section resolves wasm path correctly.
    #[test]
    fn service_section_resolves_wasm() {
        let toml = "[service]\nname = \"hello\"\nwasm = \"hello.wasm\"\n";
        let (_dir, manifest) = setup_test_dir("service-section", toml, Some("hello.wasm"));
        let (content, _path, bytes) =
            read_manifest_and_wasm(manifest.to_str().unwrap()).expect("[service] should work");
        assert!(!content.is_empty());
        assert_eq!(&bytes, b"\0asm");
    }

    /// FIX 4: legacy [api] section is accepted as a fallback (server alias).
    #[test]
    fn api_section_alias_resolves_wasm() {
        let toml = "[api]\nname = \"hello\"\nwasm = \"api.wasm\"\n";
        let (_dir, manifest) = setup_test_dir("api-alias", toml, Some("api.wasm"));
        let (content, _path, bytes) =
            read_manifest_and_wasm(manifest.to_str().unwrap()).expect("[api] alias should work");
        assert!(!content.is_empty());
        assert_eq!(&bytes, b"\0asm");
    }

    /// FIX 4: manifests with neither section get a clear, actionable error.
    #[test]
    fn missing_section_gives_clear_error() {
        let toml = "[ingress]\nmode = \"public\"\n";
        let (_dir, manifest) = setup_test_dir("missing-section", toml, None);
        let err = read_manifest_and_wasm(manifest.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("[service]") || err.to_string().contains("[api]"),
            "error should mention [service] or [api], got: {err}"
        );
    }
}
