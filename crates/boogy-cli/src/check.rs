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

pub fn run(root: Option<&str>) -> anyhow::Result<()> {
    let root = PathBuf::from(root.unwrap_or("."));
    let files = collect_rs_files(&root)?;
    if files.is_empty() {
        println!("boogy check: no .rs files found under {}", root.display());
        return Ok(());
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
    if findings.is_empty() {
        Ok(())
    } else {
        std::process::exit(1);
    }
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
