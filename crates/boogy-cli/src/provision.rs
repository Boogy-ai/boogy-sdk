use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use reqwest::multipart;
use serde_json::json;

use crate::frontend::tar_gz_dir;

/// Where the compiled protobuf descriptor lands, relative to the manifest's
/// directory. Must agree with `boogy_proto_build::DESCRIPTOR_REL_PATH`, which
/// is what actually writes the file: that crate is a **build-dependency** of a
/// service crate, and taking it as an ordinary dependency here would pull the
/// whole protobuf codegen toolchain into the CLI to share one path literal.
/// So the literal is duplicated — keep the two in step.
const DESCRIPTOR_REL_PATH: &str = ".boogy/descriptor.bin";

/// What a manifest carries for publishing: the raw TOML, plus the optional
/// wasm, optional frontend bundle, and optional protobuf descriptor. A
/// deployment must carry at least one of the wasm/frontend two (a wasm
/// service, a static frontend, or both).
#[derive(Debug)]
struct PublishArtifacts {
    manifest_content: String,
    /// `(full_path, bytes)` of the wasm, when `service.wasm` is present.
    wasm: Option<(PathBuf, Vec<u8>)>,
    /// Gzipped tarball of `[frontend].root`, when a `[frontend]` section is
    /// present.
    frontend_tar_gz: Option<Vec<u8>>,
    /// Gzipped tarball of a pre-built frontend's `[frontend] source` paths,
    /// sent as the check-only `frontend_source` part. `None` for a source
    /// build, and when the source is the root itself.
    frontend_source_tar_gz: Option<Vec<u8>>,
    /// Compiled protobuf `FileDescriptorSet` bytes
    /// (`DESCRIPTOR_REL_PATH`, written by `boogy_proto_build::compile` at the
    /// author's build time), when `[grpc]` is declared with a non-empty
    /// `services` list. `None` when `[grpc]` is absent/disabled — never an
    /// error in that case.
    descriptor: Option<Vec<u8>>,
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

    // A pre-built frontend's authored source, for the publish check only. A
    // source equal to the root is the root itself, already uploaded.
    let frontend_source_tar_gz = match manifest.get("frontend") {
        Some(fe) if matches!(fe.get("build").and_then(|b| b.as_str()), Some("dist" | "none")) => {
            let root = fe.get("root").and_then(|r| r.as_str()).unwrap_or_default().to_string();
            let source: Vec<String> = fe
                .get("source")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            if source.is_empty() || source == [root] {
                None
            } else {
                Some(crate::frontend::tar_gz_paths(manifest_dir, &source)?)
            }
        }
        _ => None,
    };

    if wasm.is_none() && frontend_tar_gz.is_none() {
        anyhow::bail!(
            "manifest has neither a [service] wasm nor a [frontend] section — \
             nothing to deploy. Add `service.wasm` or a `[frontend]` section."
        );
    }

    // Optional [grpc] section: ship the compiled descriptor beside the wasm.
    // The CLI carries no protobuf toolchain of its own (same precedent as the
    // frontend pipeline transpiling TS in Rust rather than shelling out) — the
    // descriptor is produced once, at the author's `cargo build` time, by
    // `boogy_proto_build::compile` (invoked from the service crate's
    // `build.rs`), which writes it to `DESCRIPTOR_REL_PATH` at the crate
    // root. This just reads that file back.
    //
    // "Enabled" here means the same thing the manifest schema means by it:
    // a non-empty `[grpc] services` list. The CLI parses the manifest as raw
    // TOML rather than deserializing the typed schema, so that one rule is
    // restated here — keep the two in step. A `[grpc]` block with no
    // `services` (or no block at all) needs no descriptor.
    let grpc_enabled = manifest
        .get("grpc")
        .and_then(|g| g.get("services"))
        .and_then(|s| s.as_array())
        .is_some_and(|services| !services.is_empty());
    let descriptor = if grpc_enabled {
        let descriptor_path = manifest_dir.join(DESCRIPTOR_REL_PATH);
        let bytes = std::fs::read(&descriptor_path).with_context(|| {
            format!(
                "[grpc] is declared but {} is missing.\n\
                 Run `cargo build --target wasm32-wasip2 --release` first — the build \
                 script writes the descriptor there.",
                descriptor_path.display()
            )
        })?;
        Some(bytes)
    } else {
        None
    };

    Ok(PublishArtifacts {
        manifest_content,
        wasm,
        frontend_tar_gz,
        frontend_source_tar_gz,
        descriptor,
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
    // The @boogy/web conventions, before anything is uploaded: the same rules
    // `boogy check` reports and publish enforces. No flag skips them.
    if let Some(report) = crate::check::frontend_report(Path::new(manifest_path))? {
        crate::check::print_frontend_report(&report);
        anyhow::ensure!(
            report.is_clean(),
            "{} frontend finding(s): fix each, or waive it next to the code with `boogy-allow <rule>: <reason>`. Nothing was uploaded.",
            report.findings.len()
        );
    }
    let PublishArtifacts {
        manifest_content,
        wasm,
        frontend_tar_gz,
        frontend_source_tar_gz,
        descriptor,
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
    if let Some(bytes) = &descriptor {
        println!("  Protobuf: descriptor bundled ({} bytes)", bytes.len());
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
    if let Some(src) = frontend_source_tar_gz {
        form = form.part(
            "frontend_source",
            multipart::Part::bytes(src)
                .file_name("frontend-source.tar.gz")
                .mime_str("application/gzip")
                .context("failed to set frontend_source part mime")?,
        );
    }
    if let Some(bytes) = descriptor {
        form = form.part(
            "descriptor",
            multipart::Part::bytes(bytes).file_name("descriptor.bin"),
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
    // The host returns the canonical URL — the root of the service's own
    // address, `https://<slug>-<suffix>.<base>` — which is both what we print
    // and what the smoke must load.
    let service_url = body.get("service_url").and_then(|u| u.as_str());
    if let Some(url) = service_url {
        println!("  URL: {url}");
        if let Some(note) = label_note_line(&body) {
            println!("{note}");
        }
        if let Some(line) = connections_callback_line(&body) {
            println!("{line}");
        }
        // Printing a URL is a claim, so check it. A provisioned service whose
        // hostname has no route at the edge answers from the ingress default
        // backend — indistinguishable, from the author's side, from their own
        // router being wrong. One probe turns hours of looking in the wrong
        // place into a line of output.
        let decision = crate::smoke::pin_plan(host, url);
        let pins: Vec<crate::smoke::Pin> = match &decision {
            crate::smoke::PinDecision::Pinned(p) => p.clone(),
            _ => Vec::new(),
        };
        if !pins.is_empty() {
            println!("{}", crate::smoke::pin_note(&pins));
        }
        if let crate::smoke::PinDecision::Refuse(why) = &decision {
            println!("  Skipped the reachability check: {why}");
        } else if provisioned {
            match crate::smoke::probe_reachable(url, &pins).await {
                crate::smoke::Reachability::PlatformAnswered => {}
                crate::smoke::Reachability::AnsweredByNonPlatform { status } => {
                    println!("{}", crate::smoke::unreachable_advice(url));
                    println!("\x20   (answered {status}, with no platform response header)");
                }
                crate::smoke::Reachability::Unreachable { detail } => {
                    println!("{}", crate::smoke::unreachable_advice(url));
                    println!("\x20   (connection failed: {detail})");
                }
            }
        }
    }

    // Opt-in post-deploy smoke. Only meaningful once a service is actually
    // serving — i.e. provision succeeded. Best-effort otherwise.
    if smoke.enabled {
        if provision && provisioned {
            let pins = match service_url.map(|u| crate::smoke::pin_plan(host, u)) {
                Some(crate::smoke::PinDecision::Pinned(p)) => p,
                Some(crate::smoke::PinDecision::Refuse(why)) => {
                    anyhow::bail!("smoke not run: {why}");
                }
                _ => Vec::new(),
            };
            crate::smoke::run_post_deploy_smoke(&smoke, service_url, has_frontend, &pins).await?;
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
        if let Some(note) = label_note_line(&body) {
            println!("{note}");
        }
        if let Some(line) = connections_callback_line(&body) {
            println!("{line}");
        }
    }

    Ok(())
}

/// The line `boogy deploy` prints for a service that declares OAuth
/// connections: the redirect URI to register with each provider, from the
/// response's `connections_callback_url`.
fn connections_callback_line(body: &serde_json::Value) -> Option<String> {
    let url = body.get("connections_callback_url").and_then(|u| u.as_str())?;
    Some(format!("  OAuth redirect URI: {url}  (register it with every provider this service connects to)"))
}

/// The line `boogy deploy` prints beside the URL when the service's address
/// does not carry its name, from the response's `label_note`.
fn label_note_line(body: &serde_json::Value) -> Option<String> {
    let note = body.get("label_note").and_then(|n| n.as_str())?;
    Some(match note {
        "denied" => "  Note: the service's name is reserved for the platform, so its address uses `svc` instead.".to_string(),
        "empty" => "  Note: the service's name has no characters an address can use, so its address uses `svc` instead.".to_string(),
        other => format!("  Note: the service's address does not use its name ({other})."),
    })
}

/// The upgrade request: `overrides` only when given, so its absence carries
/// the instance's own overrides forward.
fn upgrade_body(to: &str, overrides: Option<String>) -> serde_json::Value {
    let mut body = json!({ "to_version": to });
    if let Some(o) = overrides {
        body["overrides"] = serde_json::Value::String(o);
    }
    body
}

/// `text` with every control character except a newline and a tab replaced
/// by U+FFFD, so text a module author wrote reaches the terminal as text: an
/// escape sequence in it cannot recolour, hide or overwrite the lines around it.
fn terminal_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { '\u{fffd}' } else { c })
        .collect()
}

/// The line printed when `--overrides` replaces the instance's overrides: the
/// platform computed the preview from the ones it has now.
const OVERRIDES_PREVIEW_LINE: &str =
    "This preview uses the instance's current overrides; the new ones apply when it upgrades.\n";

/// Render what an upgrade changes, from the platform's update body. Pure.
/// Every string in the body is printed through [`terminal_safe`].
/// `overrides_replaced` adds a line saying the preview does not reflect new
/// overrides.
pub fn render_plan(service_id: &str, body: &serde_json::Value, overrides_replaced: bool) -> String {
    let text = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).map(terminal_safe);
    let current = text(body.get("current")).unwrap_or_else(|| "?".into());
    let mut out = String::new();
    match body.get("update").filter(|u| !u.is_null()) {
        None => out.push_str("No version change; this applies the overrides only.\n"),
        Some(update) => {
            let version = text(update.get("version")).unwrap_or_else(|| "?".into());
            out.push_str(&format!("Update {service_id}: {current} \u{2192} {version}\n"));
            if let Some(notes) = text(update.get("notes")).filter(|n| !n.is_empty()) {
                out.push('\n');
                out.push_str(&notes);
                out.push('\n');
            }
            out.push('\n');
            let items = update.get("items").and_then(|i| i.as_array()).map(Vec::as_slice).unwrap_or_default();
            if items.is_empty() {
                out.push_str("Nothing else changes.\n");
            }
            for item in items {
                let label = text(item.get("label")).unwrap_or_default();
                let marker = if item.get("tone").and_then(|v| v.as_str()) == Some("attention") { "!" } else { "\u{b7}" };
                match text(item.get("detail")).filter(|d| !d.is_empty()) {
                    Some(detail) => out.push_str(&format!("{marker} {label}: {detail}\n")),
                    None => out.push_str(&format!("{marker} {label}\n")),
                }
            }
        }
    }
    if overrides_replaced {
        out.push_str(OVERRIDES_PREVIEW_LINE);
    }
    out
}

/// Whether the upgrade may go ahead: `--yes` always; otherwise only a
/// terminal user answering `y`/`yes`. Without a terminal and without `--yes`
/// there is nobody to ask, so it refuses.
pub fn may_proceed(yes: bool, is_tty: bool, answer: Option<&str>) -> Result<bool, &'static str> {
    if yes {
        return Ok(true);
    }
    if !is_tty {
        return Err("pass --yes to upgrade without a prompt");
    }
    let a = answer.unwrap_or("").trim().to_ascii_lowercase();
    Ok(a == "y" || a == "yes")
}

/// Upgrade a provisioned service to a module version (the current one, to
/// change only its overrides). `overrides_path`, when given, REPLACES the
/// instance's overrides; otherwise they carry forward.
pub async fn upgrade(host: &str, token: &str, service_id: &str, to: &str, overrides_path: Option<&str>, yes: bool) -> Result<()> {
    let overrides = match overrides_path {
        Some(p) => Some(std::fs::read_to_string(p).context("failed to read overrides file")?),
        None => None,
    };
    let client = reqwest::Client::new();
    match client
        .get(format!("{host}/v1/registry/provisioned/{service_id}/update"))
        .query(&[("to", to)])
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(plan) => print!("{}", render_plan(service_id, &plan, overrides.is_some())),
            Err(_) => println!("Could not load what changes (unreadable answer)."),
        },
        Ok(r) => println!("Could not load what changes ({}).", r.status()),
        Err(_) => println!("Could not load what changes (host unreachable)."),
    }
    let is_tty = std::io::stdin().is_terminal();
    let answer = if !yes && is_tty {
        use std::io::Write;
        print!("Upgrade? [y/N] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        Some(line)
    } else {
        None
    };
    if !may_proceed(yes, is_tty, answer.as_deref()).map_err(|m| anyhow::anyhow!(m))? {
        println!("Not upgraded.");
        return Ok(());
    }
    println!("Upgrading service...");
    println!("  Service: {service_id}");
    println!("  To version: {to}");
    if overrides.is_some() {
        println!("  Overrides: replaced");
    }

    let resp = client
        .post(format!("{host}/v1/services/{service_id}/upgrade"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&upgrade_body(to, overrides))
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
    if let Some(line) = connections_callback_line(&body) {
        println!("{line}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn plan_body(items: serde_json::Value) -> serde_json::Value {
        json!({"service_id":"polls","current":"0.1.0","update":{"version":"0.2.0","bump":"minor",
            "published_at":"2026-10-01T00:00:00Z","notes":"Faster polls.","items":items}})
    }

    #[test]
    fn the_plan_names_the_versions_notes_and_each_change() {
        let out = render_plan("polls", &plan_body(json!([
            {"kind":"capability","label":"New permission","detail":"web requests to other sites","tone":"attention"},
            {"kind":"capability","label":"No longer uses","detail":"background jobs","tone":"info"}
        ])), false);
        assert!(out.contains("Update polls: 0.1.0 \u{2192} 0.2.0"), "{out}");
        assert!(out.contains("Faster polls."), "{out}");
        assert!(out.contains("! New permission: web requests to other sites"), "{out}");
        assert!(out.contains("\u{b7} No longer uses: background jobs"), "{out}");
        assert!(!out.contains("Nothing else changes."), "{out}");
    }

    #[test]
    fn a_plan_with_no_items_says_nothing_else_changes() {
        let out = render_plan("polls", &plan_body(json!([])), false);
        assert!(out.contains("Nothing else changes."), "{out}");
    }

    #[test]
    fn a_wire_shaped_item_has_its_label_alone_and_a_null_detail_is_not_printed() {
        let out = render_plan("polls", &plan_body(json!([
            {"kind":"capability","label":"New permission: web requests to other sites","detail":null,"tone":"attention"}
        ])), false);
        assert!(out.contains("! New permission: web requests to other sites\n"), "{out}");
        assert!(!out.contains("null"), "{out}");
    }

    #[test]
    fn no_update_means_overrides_only() {
        let out = render_plan("polls", &json!({"service_id":"polls","current":"0.1.0","update":null}), false);
        assert!(out.contains("No version change; this applies the overrides only."), "{out}");
    }

    /// The preview is computed from the overrides the instance has now; new
    /// ones passed with `--overrides` replace them only when the upgrade runs.
    #[test]
    fn new_overrides_are_said_to_be_absent_from_the_preview() {
        let line = "This preview uses the instance's current overrides; the new ones apply when it upgrades.";
        let with = render_plan("polls", &plan_body(json!([])), true);
        assert!(with.contains(line), "{with}");
        let overrides_only = render_plan("polls", &json!({"service_id":"polls","current":"0.1.0","update":null}), true);
        assert!(overrides_only.contains(line), "{overrides_only}");
        assert!(!render_plan("polls", &plan_body(json!([])), false).contains(line));
    }

    /// The notes and every item are the module author's text: a terminal
    /// escape in them must not reach the terminal, where it could hide the
    /// lines that ask for attention.
    #[test]
    fn author_text_cannot_carry_terminal_controls() {
        let body = json!({"service_id":"polls","current":"0.1.0","update":{"version":"0.2.0","bump":"minor",
            "published_at":"2026-10-01T00:00:00Z","notes":"Faster.\u{1b}[8m\nline two\twith a tab\u{7f}\u{9b}31m\r",
            "items":[{"kind":"capability","label":"New permission\u{1b}[8m","detail":"web\u{0}requests\u{85}","tone":"attention"}]}});
        let out = render_plan("polls", &body, false);
        for c in out.chars() {
            assert!(!c.is_control() || c == '\n' || c == '\t', "a control character {c:?} reached the terminal: {out:?}");
        }
        assert!(out.contains("Faster.\u{fffd}[8m\nline two\twith a tab\u{fffd}\u{fffd}31m\u{fffd}"), "{out:?}");
        assert!(out.contains("! New permission\u{fffd}[8m: web\u{fffd}requests\u{fffd}\n"), "{out:?}");
    }

    #[test]
    fn upgrade_asks_unless_told_not_to() {
        assert_eq!(may_proceed(true, false, None), Ok(true));
        assert_eq!(may_proceed(false, false, None), Err("pass --yes to upgrade without a prompt"));
        assert_eq!(may_proceed(false, true, Some("y")), Ok(true));
        assert_eq!(may_proceed(false, true, Some("Y\n")), Ok(true));
        assert_eq!(may_proceed(false, true, Some("")), Ok(false));
        assert_eq!(may_proceed(false, true, Some("n")), Ok(false));
        assert_eq!(may_proceed(false, true, None), Ok(false));
    }

    /// The deploy output says why a service's address does not carry its
    /// name: `denied` (the name would impersonate the platform) or `empty`
    /// (nothing usable was left). Nothing when the name was kept.
    #[test]
    fn a_replaced_name_is_explained_beside_the_url() {
        let denied = label_note_line(&json!({ "service_url": "https://svc-k3v9.boogy.app", "label_note": "denied" }));
        assert!(denied.as_deref().is_some_and(|l| l.contains("svc") && l.contains("reserved")), "{denied:?}");
        let empty = label_note_line(&json!({ "service_url": "https://svc-k3v9.boogy.app", "label_note": "empty" }));
        assert!(empty.as_deref().is_some_and(|l| l.contains("svc")), "{empty:?}");
        assert_ne!(denied, empty, "two different reasons");
        assert_eq!(label_note_line(&json!({ "service_url": "https://notes-k3v9.boogy.app" })), None);
        assert_eq!(label_note_line(&json!({ "label_note": "something-new" })).is_some(), true, "an unknown reason is still shown");
    }

    /// A service that declares OAuth connections is told the redirect URI to
    /// register with each provider; one that declares none is told nothing.
    #[test]
    fn the_oauth_redirect_uri_is_printed_when_the_service_has_one() {
        let url = "https://yt-k3v9.boogy.app/boogy/connections/callback";
        let line = connections_callback_line(&json!({ "service_url": "https://yt-k3v9.boogy.app", "connections_callback_url": url }));
        assert!(line.as_deref().is_some_and(|l| l.contains(url)), "{line:?}");
        assert_eq!(connections_callback_line(&json!({ "service_url": "https://notes-k3v9.boogy.app" })), None);
    }

    #[test]
    fn an_upgrade_carries_overrides_forward_unless_given_a_file() {
        // No file: the key is absent, so the host carries the instance's own
        // overrides forward. A file: its text replaces them ("" clears them).
        assert_eq!(upgrade_body("1.2.0", None), json!({ "to_version": "1.2.0" }));
        assert_eq!(
            upgrade_body("1.2.0", Some("[discovery]\nlisted = false\n".into())),
            json!({ "to_version": "1.2.0", "overrides": "[discovery]\nlisted = false\n" })
        );
    }

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

    // ── The descriptor rides beside the wasm ───────────────────────────

    const GRPC_MANIFEST: &str = "\
[service]\nname = \"echo\"\nwasm = \"echo.wasm\"\n\n\
[grpc]\nproto = \"proto/echo.proto\"\nservices = [\"echo.v1.EchoService\"]\n";

    /// The author declared `[grpc]` but never ran the build that writes
    /// `.boogy/descriptor.bin` — the CLI must refuse LOCALLY (no network
    /// call) and say exactly what to run, not surface a confusing 400 from
    /// the host or silently publish a `[grpc]` service with no descriptor.
    #[test]
    fn grpc_manifest_without_descriptor_names_the_build_step() {
        let (_dir, manifest) =
            setup_test_dir("grpc-no-descriptor", GRPC_MANIFEST, Some("echo.wasm"));
        // Deliberately no `.boogy/descriptor.bin` written.
        let err = read_publish_artifacts(manifest.to_str().unwrap()).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("cargo build"), "must name the fix, got: {msg}");
        assert!(
            msg.contains(".boogy/descriptor.bin"),
            "must name the missing file, got: {msg}"
        );
    }

    /// The happy path: `[grpc]` declared, the build ran, `.boogy/descriptor.bin`
    /// exists — the CLI reads and bundles those exact bytes.
    #[test]
    fn grpc_manifest_with_descriptor_bundles_it() {
        let (dir, manifest) =
            setup_test_dir("grpc-with-descriptor", GRPC_MANIFEST, Some("echo.wasm"));
        let boogy_dir = dir.join(".boogy");
        std::fs::create_dir_all(&boogy_dir).expect("mkdir .boogy");
        let descriptor_bytes = b"\x0a\x05hello-descriptor-bytes".to_vec();
        std::fs::write(boogy_dir.join("descriptor.bin"), &descriptor_bytes)
            .expect("write descriptor.bin");

        let arts = read_publish_artifacts(manifest.to_str().unwrap()).expect("must succeed");
        assert_eq!(
            arts.descriptor.as_deref(),
            Some(descriptor_bytes.as_slice()),
            "must read the exact descriptor bytes from .boogy/descriptor.bin"
        );
    }

    /// A manifest with no `[grpc]` section never looks for a descriptor —
    /// no error, and no `descriptor` part to bundle, even if a stray
    /// `.boogy/descriptor.bin` happens to sit next to it (e.g. leftover from
    /// a different manifest in the same dir during local iteration).
    #[test]
    fn non_grpc_manifest_has_no_descriptor() {
        let toml = "[service]\nname = \"hello\"\nwasm = \"hello.wasm\"\n";
        let (dir, manifest) = setup_test_dir("non-grpc-no-descriptor", toml, Some("hello.wasm"));
        let boogy_dir = dir.join(".boogy");
        std::fs::create_dir_all(&boogy_dir).expect("mkdir .boogy");
        std::fs::write(boogy_dir.join("descriptor.bin"), b"unrelated").expect("write stray file");

        let arts = read_publish_artifacts(manifest.to_str().unwrap()).expect("must succeed");
        assert!(
            arts.descriptor.is_none(),
            "a non-grpc manifest must never bundle a descriptor"
        );
    }

    /// A `[grpc]` block with an empty `services` list is the same as absent —
    /// no descriptor required, none bundled. This is the manifest schema's own
    /// rule (a `[grpc]` block is enabled iff `services` is non-empty), restated
    /// here because the CLI reads the manifest as raw TOML rather than
    /// deserializing the typed schema — see the comment on
    /// `read_publish_artifacts`.
    #[test]
    fn grpc_block_with_empty_services_is_treated_as_disabled() {
        let toml = "[service]\nname = \"hello\"\nwasm = \"hello.wasm\"\n\n[grpc]\nservices = []\n";
        let (_dir, manifest) = setup_test_dir("grpc-empty-services", toml, Some("hello.wasm"));
        // No .boogy/descriptor.bin written — if this manifest were (wrongly)
        // treated as grpc-enabled, this would fail with the missing-descriptor error.
        let arts = read_publish_artifacts(manifest.to_str().unwrap()).expect("must succeed");
        assert!(arts.descriptor.is_none());
    }

    #[test]
    fn a_dist_build_uploads_its_source_as_a_check_only_part_and_a_source_build_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let w = |p: &str, s: &str| { let p = dir.path().join(p); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, s).unwrap(); };
        w("boogy.toml", "[service]\nid = \"a\"\nname = \"a\"\nversion = \"0.1.0\"\n[frontend]\nroot = \"dist\"\nbuild = \"dist\"\nsource = [\"index.html\", \"web\"]\n");
        w("dist/index.html", "<p>built</p>");
        w("index.html", "<p>source</p>");
        w("web/main.ts", "export {};");
        let a = read_publish_artifacts(dir.path().join("boogy.toml").to_str().unwrap()).unwrap();
        assert!(a.frontend_source_tar_gz.is_some());
        w("boogy.toml", "[service]\nid = \"a\"\nname = \"a\"\nversion = \"0.1.0\"\n[frontend]\nroot = \"dist\"\nbuild = \"dist\"\nsource = [\"dist\"]\n");
        assert!(read_publish_artifacts(dir.path().join("boogy.toml").to_str().unwrap()).unwrap().frontend_source_tar_gz.is_none(), "source == [root]: the root is the source");
    }

    #[tokio::test]
    async fn publish_stops_before_uploading_a_frontend_that_breaks_the_conventions() {
        let dir = tempfile::tempdir().unwrap();
        let w = |p: &str, s: &str| { let p = dir.path().join(p); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(p, s).unwrap(); };
        w("boogy.toml", "[service]\nid = \"a\"\nname = \"a\"\nversion = \"0.1.0\"\n[routing]\npath = \"/\"\nmethods = [\"GET\"]\n[ingress]\nmode = \"public\"\n[frontend]\nroot = \"web\"\nbuild = \"source\"\nframe_options = \"deny\"\n");
        w("web/index.html", "<p>x</p>");
        w("web/main.ts", "import { installFoundation } from '@boogy/web';\ninstallFoundation();");
        w("web/app.css", "a { padding: 12px }");
        let smoke = crate::smoke::SmokeOptions { enabled: false, selector: String::new(), path: "/".into(), timeout_ms: 1 };
        // An address nothing listens on: the refusal must come before any connection.
        let err = publish("http://127.0.0.1:9", "t", dir.path().join("boogy.toml").to_str().unwrap(), false, false, smoke)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Nothing was uploaded"), "{err}");
    }
}
