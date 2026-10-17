//! The conventions of an app built on `@boogy/web`, checked over its authored
//! source. Pure: no I/O. The CLI (`boogy check`, `boogy deploy`), the builder
//! MCP and the platform's publish step all run this one copy, so an app gets
//! the same answer from each.
//!
//! A finding names its rule, its file and line, and the fix. An exception is a
//! waiver next to the code it excuses — `boogy-allow <rule>: <reason>` in a
//! comment on the finding's line or the line before. A rule about the whole
//! app is waived anywhere in the app's index file.

mod css;
mod html;
mod lex;
mod script;

/// Every rule, and the fix its findings name.
pub const RULES: &[(&str, &str)] = &[
    ("uses-sdk", "Build the page on @boogy/web: its components, its tokens and connectPane."),
    ("page-scale", "Call installFoundation({ dev: import.meta.env.DEV }) once, without `scale: false`: the page scale is on by default."),
    ("board-ready", "Declare [boards] background = \"board\" and call connectPane({ service: \"<your service id>\" }); or, when the page is only ever opened on its own, set [frontend] frame_options = \"deny\"."),
    ("fixed-size", "Use an SDK size token (--space-*, --control-*, --icon-*, --fs-*, --radius-*), or define a named token in theme.css."),
    ("layout-media-query", "Size with the page scale and container units, and adapt with the SDK's layouts (FillGrid, CardGrid, Stack, Sheet)."),
    ("unit-multiple", "Use a size token (--space-*, --control-*, --icon-*, --fs-*), or define a named one: `--name: calc(var(--u) * n)`."),
    ("colour-literal", "Use an SDK colour token (--text-*, --accent, --edge*, --ground*) or a custom property you define."),
    ("sdk-control", "Use the SDK component (Button, TextArea, TextField, SearchField, Switch, ChoiceGroup), or write its shape: `<button {...button()}>`, or a text control with `data-slot=\"control\"` inside `<div {...field()}>`, directly or in its `<div data-slot=\"group\">`."),
    ("board-navigation", "Navigate with connectPane({ … }).navigate(path): in a board it moves the pane, not the board's history."),
    ("waiver", "Write a waiver as `boogy-allow <rule>: <reason>`, naming a rule and giving a reason."),
];

/// The fix a finding of `rule` names.
pub fn fix_for(rule: &str) -> &'static str {
    RULES.iter().find(|(id, _)| *id == rule).map_or("", |(_, fix)| fix)
}

pub(crate) fn is_rule(id: &str) -> bool {
    id != "waiver" && RULES.iter().any(|(r, _)| *r == id)
}

pub(crate) fn rule_list() -> String {
    RULES.iter().map(|(r, _)| *r).filter(|r| *r != "waiver").collect::<Vec<_>>().join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub path: String,
    pub line: usize,
    pub message: String,
}

impl Finding {
    pub fn fix(&self) -> &'static str {
        fix_for(self.rule)
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{} [{}] {}\n    fix: {}", self.path, self.line, self.rule, self.message, self.fix())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiver {
    pub rule: String,
    pub path: String,
    pub line: usize,
    pub reason: String,
}

/// What the manifest says about the app as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFacts {
    /// `[boards] background = "board"`.
    pub board_background: bool,
    /// `[frontend] frame_options = "deny"`: the page is never framed.
    pub standalone_only: bool,
    /// `[frontend] index`, relative to the checked tree.
    pub index: String,
}

/// `app` is `None` when no manifest is known: then only the rules about a
/// single file run.
#[derive(Debug, Clone, Default)]
pub struct FrontendFacts {
    pub app: Option<AppFacts>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontendReport {
    pub findings: Vec<Finding>,
    pub waivers: Vec<Waiver>,
}

impl FrontendReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Css,
    Html,
    Script { jsx: bool },
}

/// A finding before waivers: its rule, its byte offset in its file, its message.
pub(crate) struct Raw {
    pub rule: &'static str,
    pub at: usize,
    pub message: String,
}

/// What the app's scripts say about the app as a whole.
#[derive(Default)]
pub(crate) struct AppSignals {
    pub imports_sdk: bool,
    pub installs: usize,
    pub connects_pane: bool,
}

/// The language of an authored file, or `None` for a file that is not the
/// author's to answer for: vendored, installed, built, minified, a type
/// declaration or a test.
pub fn is_authored(path: &str) -> Option<Lang> {
    let p = path.trim_start_matches("./");
    if p.split('/').any(|seg| matches!(seg, "vendor" | "node_modules" | "dist")) {
        return None;
    }
    let name = p.rsplit('/').next().unwrap_or(p);
    if name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".d.ts") || name.contains(".test.") || name.contains(".spec.") {
        return None;
    }
    match name.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
        "css" => Some(Lang::Css),
        "html" | "htm" => Some(Lang::Html),
        "ts" | "js" | "mjs" => Some(Lang::Script { jsx: false }),
        "tsx" | "jsx" => Some(Lang::Script { jsx: true }),
        _ => None,
    }
}

pub fn check_frontend(facts: &FrontendFacts, files: &[(String, String)]) -> FrontendReport {
    let mut report = FrontendReport::default();
    let mut signals = AppSignals::default();
    let index = facts.app.as_ref().map(|a| resolve_index(&a.index, files));
    let mut index_waivers: Vec<lex::ParsedWaiver> = Vec::new();
    for (path, src) in files {
        let Some(lang) = is_authored(path) else { continue };
        let lines = lex::Lines::new(src);
        let ranges = lex::comments(src, lang);
        let code = lex::blank_comments(src, &ranges);
        let mut raws = Vec::new();
        match lang {
            Lang::Css => css::check_stylesheet(&code, 0, &css::Ctx::for_path(path), &mut raws),
            Lang::Html => html::check(&code, path, &mut raws),
            Lang::Script { jsx } => script::check(&code, path, jsx, &mut raws, &mut signals),
        }
        let (waivers, bad) = lex::waivers(src, &ranges, &lines);
        for b in bad {
            report.findings.push(Finding { rule: "waiver", path: path.clone(), line: b.line, message: b.message });
        }
        for raw in raws {
            let line = lines.line_of(raw.at);
            match waivers.iter().find(|w| w.rule == raw.rule && (w.line == line || w.line + 1 == line)) {
                Some(w) => report.waivers.push(Waiver { rule: w.rule.clone(), path: path.clone(), line: w.line, reason: w.reason.clone() }),
                None => report.findings.push(Finding { rule: raw.rule, path: path.clone(), line, message: raw.message }),
            }
        }
        if index.as_deref() == Some(path.trim_start_matches("./")) {
            index_waivers.extend(waivers);
        }
    }
    if let (Some(app), Some(index)) = (&facts.app, &index) {
        app_rules(app, index, &signals, &index_waivers, &mut report);
    }
    report.findings.sort_by(|a, b| (&a.path, a.line, a.rule).cmp(&(&b.path, b.line, b.rule)));
    report.waivers.sort_by(|a, b| (&a.path, a.line, &a.rule).cmp(&(&b.path, b.line, &b.rule)));
    report.waivers.dedup();
    report
}

/// The app's index among `files`: the file the manifest names, or, when the
/// source is a directory beneath the manifest (a Vite app's `web/`), the
/// shallowest file of that name under it.
fn resolve_index(index: &str, files: &[(String, String)]) -> String {
    let index = index.trim_start_matches("./");
    if files.iter().any(|(p, _)| p.trim_start_matches("./") == index) {
        return index.to_string();
    }
    let suffix = format!("/{index}");
    files
        .iter()
        .map(|(p, _)| p.trim_start_matches("./"))
        .filter(|p| p.ends_with(&suffix))
        .min_by_key(|p| (p.matches('/').count(), p.len()))
        .map_or_else(|| index.to_string(), str::to_string)
}

/// The rules about the app as a whole, located at its index file and waived
/// anywhere in it.
fn app_rules(app: &AppFacts, index: &str, s: &AppSignals, index_waivers: &[lex::ParsedWaiver], report: &mut FrontendReport) {
    let mut found: Vec<(&'static str, String)> = Vec::new();
    if !s.imports_sdk {
        found.push(("uses-sdk", "no file imports `@boogy/web`".into()));
    } else if s.installs == 0 {
        found.push(("page-scale", "the app never calls `installFoundation`, so it has no foundation, tokens or page scale".into()));
    }
    if !app.standalone_only {
        let mut missing = Vec::new();
        if !app.board_background {
            missing.push("`[boards] background = \"board\"` in the manifest");
        }
        if !s.connects_pane {
            missing.push("a `connectPane` call");
        }
        if !missing.is_empty() {
            found.push(("board-ready", format!("the app opens in boards but lacks {}", missing.join(" and "))));
        }
    }
    let mut sdk_waived = false;
    for (rule, message) in found {
        match index_waivers.iter().find(|w| w.rule == rule) {
            Some(w) => {
                sdk_waived |= rule == "uses-sdk";
                report.waivers.push(Waiver { rule: rule.to_string(), path: index.to_string(), line: w.line, reason: w.reason.clone() });
            }
            None => report.findings.push(Finding { rule, path: index.to_string(), line: 1, message }),
        }
    }
    // A page never framed owns its history.
    if app.standalone_only {
        report.findings.retain(|f| f.rule != "board-navigation");
    }
    // Without the SDK its tokens and components do not exist, so the rules
    // built on them do not apply to a page that says why it does without.
    if sdk_waived {
        report.findings.retain(|f| !PRESUPPOSE_THE_SDK.contains(&f.rule));
    }
}

/// The rules a page can only meet with the SDK's tokens and components.
const PRESUPPOSE_THE_SDK: &[&str] = &["fixed-size", "colour-literal", "unit-multiple", "layout-media-query", "sdk-control", "page-scale"];
