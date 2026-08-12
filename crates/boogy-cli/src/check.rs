//! `boogy check` — lint a Boogy service crate for conventions before deploy.
//! The lint logic lives in `boogy-conventions` (shared with the builder MCP
//! server); this is the filesystem walk + CLI reporting around it.

use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

use boogy_conventions::{lint_file, route_findings, Finding, Severity};

/// Recursively collect `*.rs` files under `root`, skipping build/output dirs.
fn collect_rs_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_into(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_into(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !dir.is_dir() {
        if dir.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(dir.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "node_modules" | "wit") {
                continue;
            }
            collect_into(&path, out)?;
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

/// Recursively collect frontend source files (`*.html`/`*.ts`/`*.js`/`*.css`)
/// under `root`, skipping build/output/vendor dirs. Returns `(relative_path,
/// contents)` pairs for [`check_frontend_refs`].
fn collect_frontend_files(root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let mut paths = Vec::new();
    collect_fe_into(root, &mut paths)?;
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for p in &paths {
        let src = fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
        out.push((rel, src));
    }
    Ok(out)
}

fn collect_fe_into(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let is_fe = |p: &Path| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("html" | "ts" | "js" | "css")
        )
    };
    if !dir.is_dir() {
        if is_fe(dir) {
            out.push(dir.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "node_modules" | "vendor") {
                continue;
            }
            collect_fe_into(&path, out)?;
        } else if is_fe(&path) {
            out.push(path);
        }
    }
    Ok(())
}

pub fn run(root: Option<&str>) -> anyhow::Result<()> {
    let root = PathBuf::from(root.unwrap_or("."));
    let files = collect_rs_files(&root)?;

    // Frontend reference check (when the project ships a frontend). Runs
    // independently of the Rust lint so a frontend-only (Static) deployment is
    // still checked even with no `.rs` files.
    let fe_files = collect_frontend_files(&root)?;
    let mut fe_errors: Vec<String> = Vec::new();
    if !fe_files.is_empty() {
        let (fe_err, fe_warn) = check_frontend_refs(&fe_files);
        for w in &fe_warn {
            println!("  [warn] frontend: {w}");
        }
        for e in &fe_err {
            println!("  [FAIL] frontend: {e}");
        }
        fe_errors = fe_err;
    }

    if files.is_empty() {
        if !fe_files.is_empty() {
            println!(
                "boogy check: no .rs files found under {} (frontend checked)",
                root.display()
            );
        } else {
            println!("boogy check: no .rs files found under {}", root.display());
        }
        // A frontend-only project can still fail on a dangling reference.
        if fe_errors.is_empty() {
            return Ok(());
        } else {
            std::process::exit(1);
        }
    }

    let mut sources = Vec::with_capacity(files.len());
    for f in &files {
        let src = fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        sources.push((rel, src));
    }

    let mut findings = Vec::new();
    for (rel, src) in &sources {
        findings.extend(lint_file(rel, src));
    }
    findings.extend(route_findings(&sources));

    report(&findings, sources.len());
    if findings.is_empty() && fe_errors.is_empty() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Source-level frontend reference check (no transpile — avoids pulling swc into
/// the CLI). Resolve each HTML resource ref / relative JS import against the
/// SOURCE files, treating `.ts` and `.js` as equivalent. A `.ts` reference is a
/// warning (the platform serves `.js`); a reference with no matching source is
/// an error. The authoritative full-bundle check runs at publish.
pub fn check_frontend_refs(files: &[(String, String)]) -> (Vec<String>, Vec<String>) {
    use std::collections::BTreeSet;
    // Source file set, normalized: strip a leading "./", index by stem-equivalence
    // so app.ts and app.js are interchangeable.
    let exists = |target: &str, from: &str| -> bool {
        let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let raw = target.trim_start_matches("./");
        // normalize ../ against dir
        let mut segs: Vec<&str> = Vec::new();
        for p in dir.split('/').chain(raw.split('/')) {
            match p {
                "" | "." => {}
                ".." => {
                    segs.pop();
                }
                s => segs.push(s),
            }
        }
        let path = segs.join("/");
        let stem = path
            .strip_suffix(".ts")
            .or_else(|| path.strip_suffix(".js"))
            .map(|s| s.to_string());
        files.iter().any(|(p, _)| {
            let p = p.trim_start_matches("./");
            p == path
                || stem
                    .as_deref()
                    .map_or(false, |st| p == format!("{st}.ts") || p == format!("{st}.js"))
        })
    };
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, body) in files {
        if name.ends_with(".html") {
            for attr in ["src=\"", "href=\""] {
                let mut from = 0;
                while let Some(rel) = body[from..].find(attr) {
                    let start = from + rel + attr.len();
                    if let Some(end) = body[start..].find('"') {
                        let val = &body[start..start + end];
                        from = start + end + 1;
                        if val.is_empty()
                            || val.starts_with("http")
                            || val.starts_with("//")
                            || val.starts_with("data:")
                            || val.starts_with('#')
                        {
                            continue;
                        }
                        if !seen.insert((name.clone(), val.to_string())) {
                            continue;
                        }
                        if !exists(val, name) {
                            errors.push(format!("{name}: {val} — no matching source file"));
                        } else if val.ends_with(".ts") {
                            warnings.push(format!(
                                "{name}: {val} — the platform serves the transpiled .js; reference the .js output"));
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }
    (errors, warnings)
}

fn report(findings: &[Finding], scanned: usize) {
    if findings.is_empty() {
        println!("boogy check: {scanned} file(s) scanned — no issues. ✓");
        return;
    }
    const ORDER: &[(&str, &str)] = &[
        ("raw-schema", "Raw table schema (use #[derive(Model)])"),
        ("unannotated-routes", "Routes without a summary"),
        ("untyped-response", "Untyped response body"),
        ("raw-store-crud", "Raw store CRUD"),
        ("multi-write-no-tx", "Multi-write handler without a transaction"),
    ];
    println!("boogy check: {} issue(s) across {scanned} file(s)\n", findings.len());
    for (id, title) in ORDER {
        let group: Vec<&Finding> = findings.iter().filter(|f| f.check == *id).collect();
        if group.is_empty() {
            continue;
        }
        let tag = if group[0].severity == Severity::Hard { "HARD" } else { "FAIL" };
        println!("  [{tag}] {title}");
        for f in &group {
            if f.line == 0 {
                println!("    {} — {}", f.file, f.message);
            } else {
                println!("    {}:{} — {}", f.file, f.line, f.message);
            }
        }
        println!("    ↳ {}\n", group[0].hint);
    }
}

#[cfg(test)]
mod fe_tests {
    use super::*;
    #[test]
    fn check_frontend_refs_source_level() {
        // index references ./app.ts (source exists as app.ts) → warning, not error.
        // index references ./missing.css (no source) → error.
        let files = vec![
            (
                "index.html".to_string(),
                "<script src=\"./app.ts\"></script><link href=\"./missing.css\">".to_string(),
            ),
            ("app.ts".to_string(), "export const x = 1;".to_string()),
        ];
        let (errors, warnings) = check_frontend_refs(&files);
        assert_eq!(errors.len(), 1, "missing.css dangles");
        assert!(errors[0].contains("missing.css"));
        assert_eq!(warnings.len(), 1, "app.ts ref → warn (serves .js)");
    }
}
