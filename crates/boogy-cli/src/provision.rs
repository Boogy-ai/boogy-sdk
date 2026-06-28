use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reqwest::multipart;
use serde_json::json;

use crate::frontend::tar_gz_dir;

/// What a manifest carries for publishing: the raw TOML, plus the optional
/// wasm and optional frontend bundle. A deployment must carry at least one of
/// the two (a wasm service, a static frontend, or both).
struct PublishArtifacts {
    manifest_content: String,
    /// `(full_path, bytes)` of the wasm, when `service.wasm` is present.
    wasm: Option<(PathBuf, Vec<u8>)>,
    /// Gzipped tarball of `[frontend].root`, when a `[frontend]` section is
    /// present.
    frontend_tar_gz: Option<Vec<u8>>,
}

/// Read a manifest toml + resolve and read the wasm bytes relative to the
/// manifest directory. Returns (manifest_content, wasm_full_path, wasm_bytes).
///
/// Accepts both `[service]` and the legacy `[api]` section name (the server
/// accepts either via `#[serde(alias = "api")]`).
///
/// The wasm is **required** here — this is the back-compat entry point for
/// callers that always ship a wasm. Deployments that may omit the wasm (a
/// static-frontend deployment) go through `read_publish_artifacts`.
pub fn read_manifest_and_wasm(manifest_path: &str) -> Result<(String, std::path::PathBuf, Vec<u8>)> {
    let arts = read_publish_artifacts(manifest_path)?;
    let (path, bytes) = arts
        .wasm
        .context("manifest missing service.wasm field")?;
    Ok((arts.manifest_content, path, bytes))
}

/// Read a manifest and resolve every artifact it references: the optional wasm
/// (`service.wasm`) and the optional frontend bundle (`[frontend].root`, tarred
/// + gzipped). Paths resolve relative to the manifest's directory.
///
/// A `[frontend]` section means a frontend ships; without `service.wasm` that
/// is a static (frontend-only) deployment. A manifest with neither a wasm nor a
/// frontend is rejected — there would be nothing to deploy.
fn read_publish_artifacts(manifest_path: &str) -> Result<PublishArtifacts> {
    let manifest_content =
        std::fs::read_to_string(manifest_path).context("failed to read manifest")?;

    let manifest: toml::Value =
        toml::from_str(&manifest_content).context("failed to parse manifest")?;

    let manifest_dir = Path::new(manifest_path).parent().unwrap_or(Path::new("."));

    // Accept [service] (canonical) or legacy [api] alias.
    let service_section = manifest.get("service").or_else(|| manifest.get("api"));

    // wasm is optional: a static-frontend deployment has no [service].wasm.
    let wasm = match service_section.and_then(|s| s.get("wasm")).and_then(|w| w.as_str()) {
        Some(wasm_path) => {
            let wasm_full_path = manifest_dir.join(wasm_path);
            let wasm_bytes = std::fs::read(&wasm_full_path)
                .with_context(|| format!("failed to read wasm at: {}", wasm_full_path.display()))?;
            Some((wasm_full_path, wasm_bytes))
        }
        None => None,
    };

    // Optional [frontend] section: tar+gzip the `root` directory.
    let frontend_tar_gz = match manifest.get("frontend") {
        Some(frontend) => {
            let root = frontend
                .get("root")
                .and_then(|r| r.as_str())
                .context("[frontend] section present but missing `root` (the source directory)")?;
            let root_path = manifest_dir.join(root);
            let tar_gz = tar_gz_dir(&root_path)
                .with_context(|| format!("failed to package frontend root: {}", root_path.display()))?;
            Some(tar_gz)
        }
        None => None,
    };

    if wasm.is_none() && frontend_tar_gz.is_none() {
        anyhow::bail!(
            "manifest has neither a [service] wasm nor a [frontend] section — \
             nothing to deploy. Add `service.wasm` or a `[frontend]` section."
        );
    }

    Ok(PublishArtifacts {
        manifest_content,
        wasm,
        frontend_tar_gz,
    })
}

/// Extract `(module_name, version)` = `([service].id, [service].version)` from a
/// manifest's TOML (accepts the legacy `[api]` alias). The module name in the
/// `boogy://owner/modules/<name>@<version>` URI is `[service].id`, not the
/// display `name`. Used by `--replace`.
fn module_name_version(manifest_content: &str) -> Result<(String, String)> {
    let manifest: toml::Value =
        toml::from_str(manifest_content).context("failed to parse manifest")?;
    let svc = manifest
        .get("service")
        .or_else(|| manifest.get("api"))
        .context("manifest has no [service] section")?;
    let id = svc
        .get("id")
        .and_then(|v| v.as_str())
        .context("manifest [service].id missing")?;
    let version = svc
        .get("version")
        .and_then(|v| v.as_str())
        .context("manifest [service].version missing")?;
    Ok((id.to_string(), version.to_string()))
}

/// Delete one unreferenced module version — the `--replace` pre-step.
/// 404 (nothing to delete: first publish of this version) is fine; 409 (a live
/// service references it) is a hard error with a clear next step; success is
/// reported. Hits the owner-scoped `DELETE /v1/modules/{name}/{version}`.
async fn delete_module_version(host: &str, token: &str, name: &str, version: &str) -> Result<()> {
    let url = format!("{host}/v1/modules/{name}/{version}");
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .context("failed to reach host")?;
    match resp.status() {
        s if s.is_success() => {
            println!("  Replaced: deleted existing {name}@{version}");
            Ok(())
        }
        reqwest::StatusCode::NOT_FOUND => Ok(()), // nothing to replace — fine
        reqwest::StatusCode::CONFLICT => {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "--replace: {name}@{version} is still referenced by a live service, so it \
                 can't be replaced in place. Bump the version, or upgrade/delete the \
                 service first.\n{body}"
            )
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("--replace: delete {name}@{version} failed ({s}): {body}")
        }
    }
}

/// Publish a module: an immutable, versioned wasm+manifest artifact.
///
/// When `provision` is true the host also provisions the publisher's own
/// service from the manifest in the same call. When `replace` is true, an
/// existing unreferenced copy of this version is deleted first (dev loop).
pub async fn publish(
    host: &str,
    token: &str,
    manifest_path: &str,
    provision: bool,
    replace: bool,
    smoke: crate::smoke::SmokeOptions,
) -> Result<()> {
    let PublishArtifacts {
        manifest_content,
        wasm,
        frontend_tar_gz,
    } = read_publish_artifacts(manifest_path)?;
    // Capture before the bundle is consumed into the multipart form — the smoke
    // step only applies to deployments that actually serve a frontend.
    let has_frontend = frontend_tar_gz.is_some();

    // --replace: GC this module version first (if it exists + is unreferenced)
    // so the same version can be re-published without a bump. A version a live
    // service still references is refused by the host (409) — surfaced here.
    if replace {
        let (name, version) = module_name_version(&manifest_content)
            .context("--replace needs [service].name and [service].version in the manifest")?;
        delete_module_version(host, token, &name, &version).await?;
    }

    println!("Publishing module...");
    println!("  Manifest: {manifest_path}");
    match &wasm {
        Some((path, bytes)) => println!("  Wasm: {} ({} bytes)", path.display(), bytes.len()),
        None => println!("  Wasm: none (static frontend deployment)"),
    }
    if let Some(tar_gz) = &frontend_tar_gz {
        println!("  Frontend: bundled ({} bytes gzipped)", tar_gz.len());
    }
    if provision {
        println!("  Provision: true (publisher's own service)");
    }

    let mut form = multipart::Form::new().text("manifest", manifest_content);
    if let Some((_, bytes)) = wasm {
        form = form.part(
            "wasm",
            multipart::Part::bytes(bytes).file_name("component.wasm"),
        );
    }
    if let Some(tar_gz) = frontend_tar_gz {
        form = form.part(
            "frontend",
            multipart::Part::bytes(tar_gz)
                .file_name("frontend.tar.gz")
                .mime_str("application/gzip")
                .context("failed to set frontend part mime")?,
        );
    }
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

    // Surface non-fatal frontend build warnings (e.g. an auto-rewritten `.ts`
    // reference) so the author learns to fix the source even though the deploy
    // succeeded.
    if let Some(warnings) = body.get("frontend_warnings").and_then(|w| w.as_array()) {
        for w in warnings {
            if let Some(msg) = w.as_str() {
                println!("  ⚠ frontend: {msg}");
            }
        }
    }

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
    // The host returns the canonical tenant URL (`https://<handle>.<base>/<id>`);
    // it is both what we print and what the smoke must load.
    let service_url = body.get("service_url").and_then(|u| u.as_str());
    if let Some(url) = service_url {
        println!("  URL: {url}");
    }

    // Opt-in post-deploy smoke. Only meaningful once a service is actually
    // serving — i.e. provision succeeded. Best-effort otherwise.
    if smoke.enabled {
        if provision && provisioned {
            let service_uri = crate::smoke::service_uri_from_module(module);
            crate::smoke::run_post_deploy_smoke(
                &smoke,
                host,
                service_url,
                service_uri.as_deref(),
                has_frontend,
            )
            .await?;
        } else {
            println!(
                "  Smoke: skipped (requires a provisioned service — \
                 use `deploy` or `publish --provision`)"
            );
        }
    }

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
    if let Some(url) = body.get("service_url").and_then(|u| u.as_str()) {
        println!("  URL: {url}");
    }

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

    // --replace: extract (module_name, version) = ([service].id, [service].version).
    #[test]
    fn module_name_version_reads_id_and_version() {
        let toml = "[service]\nid = \"notes\"\nname = \"Notes API\"\nversion = \"0.5.1\"\n";
        let (name, ver) = module_name_version(toml).expect("parse");
        // The URI slug is [service].id, NOT the display name.
        assert_eq!(name, "notes");
        assert_eq!(ver, "0.5.1");
    }

    #[test]
    fn module_name_version_accepts_legacy_api_alias() {
        let toml = "[api]\nid = \"legacy\"\nversion = \"1.0.0\"\n";
        let (name, ver) = module_name_version(toml).expect("parse alias");
        assert_eq!(name, "legacy");
        assert_eq!(ver, "1.0.0");
    }

    #[test]
    fn module_name_version_errors_without_id_or_version() {
        // Missing version.
        assert!(module_name_version("[service]\nid = \"x\"\n").is_err());
        // Missing id.
        assert!(module_name_version("[service]\nversion = \"1.0.0\"\n").is_err());
        // No service section at all.
        assert!(module_name_version("[frontend]\nroot = \"web\"\n").is_err());
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
