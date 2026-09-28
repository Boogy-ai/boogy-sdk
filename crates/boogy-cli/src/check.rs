//! `boogy check` — lint a Boogy service crate for conventions before deploy.
//! The lint logic lives in `boogy-conventions` (shared with the builder MCP
//! server); this is the filesystem walk + CLI reporting around it.

use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

use boogy_conventions::{
    counter_findings, key_route_findings, lint_file, route_findings, Finding, Severity,
};

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

/// Recursively collect frontend source files
/// (`*.html`/`*.ts`/`*.tsx`/`*.js`/`*.jsx`/`*.css`) under `root`, skipping
/// build/output/vendor dirs. Returns `(relative_path, contents)` pairs for
/// [`check_frontend_refs`].
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
        // `tsx`/`jsx` are here because the platform compiles them
        // (`[frontend] jsx_import_source`, `boogy_frontend`), and without them a
        // `.tsx` entry point never entered the set at all — so `index.html`'s
        // correct `src="./app.js"` resolved against nothing and this gate printed
        // a dangling-reference FAIL, in output byte-identical to a genuinely
        // missing file, on every correct `.tsx` service in the repo.
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("html" | "ts" | "tsx" | "js" | "jsx" | "css")
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

pub fn run(root: Option<&str>, layout: bool) -> anyhow::Result<()> {
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
        if layout {
            // Build output is not source: skip a local `dist/` (a Vite build).
            let sources: Vec<(String, String)> =
                fe_files.iter().filter(|(p, _)| !p.starts_with("dist/")).cloned().collect();
            for l in layout_findings(&sources) {
                println!("  [FAIL] layout: {l}");
                fe_errors.push(l);
            }
        }
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
    findings.extend(counter_findings(&sources));
    // The mount comes from the manifest, not from the shape of the routes:
    // a root-mounted service is free to group its own routes under `/api`,
    // and inferring a mount from that flags correct code.
    findings.extend(key_route_findings(&sources, routing_path(&root).as_deref()));

    // `fe_errors.len()` is passed, not just the Rust findings. `report` used to
    // see the Rust half alone and print its summary from that, so a frontend
    // failure produced `[FAIL] frontend: …` immediately followed by
    // `no issues. ✓` — two lines flatly contradicting each other, with the exit
    // code (correctly 1) agreeing with neither. That contradiction is why the
    // gate has twice been read as "prints FAIL and exits 0"; it does not, but a
    // summary claiming success is indistinguishable from one.
    report(&findings, sources.len(), fe_errors.len());
    if findings.is_empty() && fe_errors.is_empty() {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// Source-level frontend reference check (no transpile — avoids pulling swc into
/// the CLI). Resolve each HTML resource ref / relative JS import against the
/// SOURCE files, treating `.ts`, `.tsx`, `.js` and `.jsx` as equivalent — the
/// platform compiles all four to `.js`, so a reference naming `.js` beside a
/// `.tsx` source is correct and must not be reported. A `.ts`/`.tsx` reference is
/// a warning (the platform serves `.js`); a reference with no matching source of
/// any of those extensions is an error. The authoritative full-bundle check runs
/// at publish.
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
        // Every extension the platform compiles to `.js`. Both halves matter and
        // each was incomplete: the STRIP decides whether a `./app.js` reference
        // has a stem to compare at all, and the CANDIDATE list decides which
        // sources that stem may match. With `.tsx` missing from either, a
        // `.tsx` entry point could not satisfy the `.js` reference that names it.
        const COMPILED_TO_JS: &[&str] = &[".ts", ".tsx", ".js", ".jsx"];
        let stem = COMPILED_TO_JS
            .iter()
            .find_map(|ext| path.strip_suffix(ext))
            .map(|s| s.to_string());
        files.iter().any(|(p, _)| {
            let p = p.trim_start_matches("./");
            p == path
                || stem.as_deref().map_or(false, |st| {
                    COMPILED_TO_JS.iter().any(|ext| p == format!("{st}{ext}"))
                })
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

/// Report groups, in print order: `(check id, human title)`.
///
/// Module-scope so `every_check_the_engine_emits_has_a_report_group` can assert
/// it against `boogy_conventions::CHECKS`. It was a local const inside `report`
/// until 2026-08-24, which is why nothing noticed a check with no group.
const REPORT_GROUPS: &[(&str, &str)] = &[
    ("raw-schema", "Raw table schema (use #[derive(Model)])"),
    ("unannotated-routes", "Routes without a summary"),
    ("router-no-info", "Router without Router::info(...)"),
    ("untyped-response", "Untyped response body"),
    ("raw-store-crud", "Raw store CRUD"),
    ("multi-write-no-tx", "Multi-write handler without a transaction"),
    ("counter-read-in-tx", "Counter read at snapshot and written in one transaction"),
    // retired-spelling: the group TITLE names the form the check rejects — now
    // `#[model(counter(name = "..."))]` plus `#[derive(Counter)]`.
    ("counter-field", "Retired `#[counter]` field attribute"),
    ("legacy-init-tables", "Legacy init_tables / hand-created index"),
    ("hardcoded-index-name", "Hardcoded index name in a cursor call"),
    ("unmounted-key-routes", "API-key routes registered without the service's mount prefix"),
];

/// Print the summary.
///
/// `frontend_errors` is a COUNT rather than the messages: `run` has already
/// printed each one above this, so reprinting them would duplicate. What this
/// needs them for is the verdict — a summary that says "no issues" while a
/// frontend failure is on screen is worse than no summary at all.
fn report(findings: &[Finding], scanned: usize, frontend_errors: usize) {
    if findings.is_empty() {
        if frontend_errors == 0 {
            println!("boogy check: {scanned} file(s) scanned — no issues. ✓");
        } else {
            let s = if frontend_errors == 1 { "" } else { "s" };
            println!(
                "boogy check: {scanned} file(s) scanned —                  {frontend_errors} frontend issue{s} above, no Rust issues."
            );
        }
        return;
    }
    const ORDER: &[(&str, &str)] = REPORT_GROUPS;
    let total = findings.len() + frontend_errors;
    if frontend_errors == 0 {
        println!("boogy check: {total} issue(s) across {scanned} file(s)\n");
    } else {
        println!(
            "boogy check: {total} issue(s) across {scanned} file(s)              ({frontend_errors} frontend, listed above)\n"
        );
    }
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
    // Anything ORDER does not name still prints. The unit test below keeps the
    // two lists in step, but a finding must never be counted and then silently
    // dropped — that failure reads as "1 issue" followed by nothing.
    let unlisted: Vec<&Finding> =
        findings.iter().filter(|f| !ORDER.iter().any(|(id, _)| *id == f.check)).collect();
    for f in &unlisted {
        println!("  [FAIL] {}", f.check);
        println!("    {}:{} — {}", f.file, f.line, f.message);
        println!("    ↳ {}\n", f.hint);
    }
}

/// Layout conventions for a project built on the `@boogy/web` foundation
/// (`boogy check --layout`). Opt-in, because a project written before the
/// foundation legitimately uses media queries and pixel sizes. Checks
/// `.css`/`.ts`/`.tsx` sources and returns `path:line — message` findings:
///   * no media queries — size from the container units instead;
///   * no `sqrt(pow(...))` — unsupported in CSS today and silently dropped;
///     `hypot()` is the supported spelling;
///   * no pixel sizes above 1px outside `theme.css`, the one file whose job
///     is to hold fixed values;
///   * no one-off `var(--u)` multiples outside a custom-property definition:
///     sizes come from the named scale.
pub fn layout_findings(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, src) in files {
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "css" | "ts" | "tsx") {
            continue;
        }
        let is_theme = Path::new(path).file_name().and_then(|n| n.to_str()) == Some("theme.css");
        let code = strip_comments(src, ext != "css");
        for (i, line) in code.lines().enumerate() {
            let at = format!("{path}:{}", i + 1);
            // `prefers-*` (reduced motion, colour scheme, contrast) and the
            // pointer/hover queries describe the USER and the DEVICE, not the
            // layout, and have no container-query equivalent.
            let about_device = ["prefers-", "pointer", "hover"].iter().any(|k| line.contains(k));
            if line.contains("@media") && !about_device {
                out.push(format!("{at} — media query; size with scale()/surface() and container units instead"));
            }
            let squashed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if squashed.contains("sqrt(pow(") {
                out.push(format!("{at} — sqrt(pow(...)) is dropped by browsers; use hypot()"));
            }
            // Sizes come from the named scale (--space-*, --control-*, --fs-*…).
            // Multiplying the unit directly is how a token is DEFINED, and
            // nowhere else: a one-off multiplier in a rule is a magic number.
            let defines_token = line.trim_start().starts_with("--");
            if line.contains("var(--u)") && !defines_token {
                out.push(format!("{at} — a one-off multiple of --u; use a size token (--space-*, --control-*, --icon-*, --fs-*), or define a named token for it"));
            }
            if !is_theme && has_px_size_above_one(line) {
                out.push(format!("{at} — pixel size; use a multiple of var(--u) or a token, or move it to theme.css"));
            }
        }
    }
    out
}

/// `src` with comments replaced by spaces, newlines kept so line numbers
/// still match. Block comments always; `//` line comments only in script
/// (`line_comments`), and never inside a string literal.
fn strip_comments(src: &str, line_comments: bool) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(n) = next {
                    out.push(n);
                    i += 1;
                }
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            i += 2;
            continue;
        }
        if line_comments && c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if line_comments && matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        }
        out.push(c);
        i += 1;
    }
    out
}

/// True when `line` holds a `<number>px` whose value is above 1 (0px and 1px
/// hairlines are allowed everywhere).
fn has_px_size_above_one(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(off) = line[i..].find("px") {
        let end = i + off;
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        // A letter, `-`, `_` or `/` before the number means it is part of a
        // name or a path (`h12px`, `/assets/12px.svg`), not a size.
        let preceded_by_word = start > 0
            && (bytes[start - 1].is_ascii_alphabetic() || matches!(bytes[start - 1], b'-' | b'_' | b'/'));
        if start < end && !preceded_by_word {
            if let Ok(v) = line[start..end].parse::<f64>() {
                if v > 1.0 {
                    return true;
                }
            }
        }
        i = end + 2;
    }
    false
}

#[cfg(test)]
mod fe_tests {
    use super::*;

    #[test]
    fn every_check_the_engine_emits_has_a_report_group() {
        // ORDER is hand-maintained and the reporter iterates IT, not the
        // findings — so a check missing from it was counted and never printed.
        // Measured: `counter-read-in-tx` produced "boogy check: 1 issue(s)"
        // followed by an empty report and a non-zero exit, which reads as a
        // crash rather than a finding.
        let listed: Vec<&str> = REPORT_GROUPS.iter().map(|(id, _)| *id).collect();
        for id in boogy_conventions::CHECKS {
            assert!(
                listed.contains(id),
                "check `{id}` has no group in `boogy check`'s ORDER — it would be \
                 counted in the total and never shown to the author",
            );
        }
        for id in &listed {
            assert!(
                boogy_conventions::CHECKS.contains(id),
                "ORDER names `{id}`, which the lint engine never emits — a group \
                 that can never render is a claim the check exists",
            );
        }
    }

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

    /// **A `.tsx` entry point is a correct entry point.** The platform transpiles
    /// `.tsx` → `.js` at deploy (`boogy_frontend`, `[frontend] jsx_import_source`),
    /// so `src="./app.js"` beside an `app.tsx` source is exactly right — it is what
    /// `apps/squad`, `apps/boards` and `crates/examples/tsx-probe` all ship.
    ///
    /// This gate reported it as a dangling reference, in output byte-identical to a
    /// genuinely missing file, and exited 1. So the gate was RED on every correct
    /// `.tsx` service in the repo, which trains a person to ignore it — and then a
    /// real dangling reference prints the same line.
    ///
    /// Two separate omissions had to be fixed for this to pass, and either alone
    /// leaves it failing: `collect_fe_into`'s extension filter did not admit `.tsx`
    /// (so the file never entered the set at all), and the stem-equivalence below
    /// knew only `.ts`/`.js` (so a `./app.js` reference would not have matched an
    /// `app.tsx` even once it was there).
    #[test]
    fn a_tsx_entry_point_resolves_a_dot_js_reference() {
        let files = vec![
            (
                "index.html".to_string(),
                "<script type=\"module\" src=\"./app.js\"></script>".to_string(),
            ),
            ("app.tsx".to_string(), "export const App = () => null;".to_string()),
        ];
        let (errors, warnings) = check_frontend_refs(&files);
        assert!(
            errors.is_empty(),
            "a .tsx source satisfies a .js reference — the platform transpiles it; got {errors:?}"
        );
        assert!(
            warnings.is_empty(),
            "and it is not even a warning: the reference already names .js, which \
             is what will be served; got {warnings:?}"
        );
    }

    /// The same for `.jsx`, and the same for a relative import inside a module
    /// rather than an HTML attribute.
    #[test]
    fn a_jsx_source_and_a_module_relative_import_both_resolve() {
        let files = vec![
            (
                "index.html".to_string(),
                "<script type=\"module\" src=\"./main.js\"></script>".to_string(),
            ),
            ("main.jsx".to_string(), "import { ui } from \"./ui.js\";".to_string()),
            ("ui.tsx".to_string(), "export const ui = 1;".to_string()),
        ];
        let (errors, warnings) = check_frontend_refs(&files);
        assert!(errors.is_empty(), "got {errors:?}");
        assert!(warnings.is_empty(), "got {warnings:?}");
    }

    /// **The falsifier for the two arms above.** They would both pass if the fix
    /// had been "treat every reference as resolved" — which is the shape a gate
    /// takes when someone silences it rather than fixing it. A reference with no
    /// source of ANY extension must still be an error.
    #[test]
    fn a_genuinely_dangling_reference_is_still_an_error_after_the_tsx_fix() {
        let files = vec![
            (
                "index.html".to_string(),
                "<script type=\"module\" src=\"./app.js\"></script>".to_string(),
            ),
            // Deliberately NOT app.tsx/app.ts/app.js — a neighbour, so the stem
            // widening cannot accidentally match it.
            ("other.tsx".to_string(), "export const x = 1;".to_string()),
        ];
        let (errors, _) = check_frontend_refs(&files);
        assert_eq!(errors.len(), 1, "app.js still dangles; got {errors:?}");
        assert!(errors[0].contains("app.js"), "got {errors:?}");
    }

    /// `collect_fe_into`'s filter is the OTHER half, and a unit test on
    /// `check_frontend_refs` alone cannot see it: that function is handed a file
    /// set, so it passes regardless of what the walker chose to put in it. This
    /// drives the walker over a real directory.
    #[test]
    fn the_file_walker_collects_tsx_and_jsx() {
        let dir = std::env::temp_dir().join(format!("boogy-check-fe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("nested")).expect("create fixture dir");
        for name in ["index.html", "app.tsx", "ui.jsx", "styles.css", "helper.ts", "notes.md"] {
            fs::write(dir.join(name), "x").expect("write fixture");
        }
        fs::write(dir.join("nested/deep.tsx"), "x").expect("write nested fixture");

        let mut out = Vec::new();
        collect_fe_into(&dir, &mut out).expect("walk");
        let names: Vec<String> = out
            .iter()
            .map(|p| p.strip_prefix(&dir).unwrap_or(p).display().to_string())
            .collect();

        for want in ["app.tsx", "ui.jsx", "nested/deep.tsx", "index.html", "styles.css", "helper.ts"]
        {
            assert!(names.iter().any(|n| n == want), "{want} should be collected; got {names:?}");
        }
        // And the filter still filters — otherwise "collects .tsx" would be
        // satisfied by collecting everything, which would make every unrelated
        // file in a bundle a candidate reference target.
        assert!(
            !names.iter().any(|n| n == "notes.md"),
            "a .md file is not a frontend source; got {names:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

/// The manifest's `[routing] path`, when this crate has a `boogy.toml`.
///
/// Deliberately tolerant: a missing, unreadable or malformed manifest yields
/// `None`, and the checks that consult it then abstain. A conventions lint
/// that started failing because a manifest could not be parsed would be
/// reporting on the wrong thing.
fn routing_path(root: &std::path::Path) -> Option<String> {
    let raw = fs::read_to_string(root.join("boogy.toml")).ok()?;
    let doc: toml::Value = raw.parse().ok()?;
    doc.get("routing")?.get("path")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn f(path: &str, src: &str) -> (String, String) {
        (path.to_string(), src.to_string())
    }

    #[test]
    fn a_media_query_is_a_finding() {
        let out = layout_findings(&[f("web/app.css", "@media (min-width: 40rem) { a { color: red } }")]);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("web/app.css:1"), "{out:?}");
        assert!(out[0].contains("media"), "{out:?}");
    }

    #[test]
    fn sqrt_of_pow_is_a_finding() {
        let out = layout_findings(&[f("web/app.css", "a { width: calc(sqrt(pow(1cqi, 2))); }")]);
        assert!(out.iter().any(|m| m.contains("hypot")), "{out:?}");
    }

    #[test]
    fn a_px_size_outside_theme_css_is_a_finding_but_hairlines_are_not() {
        let out = layout_findings(&[
            f("web/app.css", "a { padding: 12px; border: 1px solid; margin: 0px; }"),
            f("web/app.tsx", "const s = { width: '24px' };"),
            f("web/theme.css", "a { padding: 12px; }"),
        ]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.iter().any(|m| m.starts_with("web/app.css:1")));
        assert!(out.iter().any(|m| m.starts_with("web/app.tsx:1")));
    }

    #[test]
    fn a_clean_project_has_no_findings() {
        let out = layout_findings(&[f("web/app.css", ".x { padding: var(--space-3); border: 1px solid; }")]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn html_and_js_are_not_linted() {
        let out = layout_findings(&[f("web/index.html", "<style>@media print {}</style>"), f("web/vendor.js", "x='12px'")]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn sizes_and_media_inside_comments_are_not_findings() {
        let out = layout_findings(&[
            f("web/app.css", "/* a 12px seam,\n   and @media (min-width: 40rem) was here */\n.x { gap: 0; } /* 24px */"),
            f("web/app.tsx", "// a 12px seam\n/** floor minmax(400px, 1fr) */\nconst u = 'https://example.com/12px';"),
        ]);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_user_preference_media_query_is_allowed_but_a_layout_one_is_not() {
        let out = layout_findings(&[f(
            "web/app.css",
            "@media (prefers-reduced-motion: reduce) { .x { transition: none; } }\n@media (prefers-color-scheme: dark) {}\n@media (min-width: 48rem) {}",
        )]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("web/app.css:3"), "{out:?}");
    }

    #[test]
    fn a_device_capability_query_is_allowed() {
        let out = layout_findings(&[f(
            "web/app.css",
            "@media (pointer: coarse) {}\n@media (hover: hover) {}\n@media (any-pointer: fine) {}\n@media (max-width: 30rem) {}",
        )]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("web/app.css:4"), "{out:?}");
    }

    #[test]
    fn multiplying_the_unit_is_for_token_definitions_only() {
        let out = layout_findings(&[f(
            "web/app.css",
            ":root, [data-u-policy] {\n  --panel-w: calc(var(--u) * 28);\n}\n.x { padding: calc(var(--u) * 0.6); gap: var(--space-2); }",
        )]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("web/app.css:4"), "{out:?}");
        assert!(out[0].contains("--space"), "the finding should point at the scale: {out:?}");
    }
}
