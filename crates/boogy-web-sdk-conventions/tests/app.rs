use boogy_web_sdk_conventions::{check_frontend, AppFacts, FrontendFacts, FrontendReport};

fn app(board_background: bool, standalone_only: bool) -> FrontendFacts {
    FrontendFacts { app: Some(AppFacts { board_background, standalone_only, index: "index.html".into() }) }
}

fn check(facts: &FrontendFacts, files: &[(&str, &str)]) -> FrontendReport {
    let files: Vec<(String, String)> = files.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect();
    check_frontend(facts, &files)
}

fn found(r: &FrontendReport) -> Vec<(&str, &str, usize)> {
    r.findings.iter().map(|f| (f.rule, f.path.as_str(), f.line)).collect()
}

const INDEX: &str = "<!doctype html>\n<html><head><title>x</title></head><body><div id=\"app\"></div><script type=\"module\" src=\"./main.js\"></script></body></html>";
const GOOD_MAIN: &str = "import { installFoundation, connectPane } from '@boogy/web';\ninstallFoundation({ dev: import.meta.env.DEV });\nconnectPane({ service: 'x' });";

#[test]
fn a_board_ready_app_on_the_sdk_is_clean() {
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", GOOD_MAIN)]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn an_app_not_built_on_the_sdk_is_told_so_at_its_index() {
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", "document.body.textContent = 'hi';")]);
    assert_eq!(found(&r), vec![("board-ready", "index.html", 1), ("uses-sdk", "index.html", 1)], "{r:?}");
}

#[test]
fn an_app_that_never_installs_the_foundation_has_no_page_scale() {
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", "import { connectPane } from '@boogy/web';\nconnectPane({ service: 'x' });")]);
    assert_eq!(found(&r), vec![("page-scale", "index.html", 1)], "{r:?}");
}

#[test]
fn turning_the_page_scale_off_is_a_finding_where_it_is_done_and_can_be_waived_there() {
    let off = "import { installFoundation, connectPane } from '@boogy/web';\ninstallFoundation({ scale: false });\nconnectPane({ service: 'x' });";
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", off)]);
    assert_eq!(found(&r), vec![("page-scale", "web/main.ts", 2)], "{r:?}");
    let waived = off.replace("installFoundation({ scale: false });", "// boogy-allow page-scale: the board shell sizes itself from its own zoom\ninstallFoundation({ scale : false });");
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", &waived)]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn board_ready_names_what_is_missing() {
    let main = "import { installFoundation } from '@boogy/web';\ninstallFoundation();";
    let r = check(&app(false, false), &[("index.html", INDEX), ("web/main.ts", main)]);
    assert_eq!(found(&r), vec![("board-ready", "index.html", 1)], "{r:?}");
    assert!(r.findings[0].message.contains("background") && r.findings[0].message.contains("connectPane"), "{r:?}");
}

#[test]
fn a_standalone_only_app_owes_the_board_nothing() {
    let main = "import { installFoundation } from '@boogy/web';\ninstallFoundation();";
    let r = check(&app(false, true), &[("index.html", INDEX), ("web/main.ts", main)]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn a_whole_app_rule_is_waived_anywhere_in_the_index_and_the_waiver_is_listed() {
    let index = INDEX.replace("<title>", "<!-- boogy-allow board-ready: this app is the board itself -->\n<title>");
    let main = "import { installFoundation } from '@boogy/web';\ninstallFoundation();";
    let r = check(&app(false, false), &[("index.html", &index), ("web/main.ts", main)]);
    assert!(r.is_clean(), "{r:?}");
    assert_eq!(r.waivers.iter().map(|w| (w.rule.as_str(), w.path.as_str(), w.line)).collect::<Vec<_>>(), vec![("board-ready", "index.html", 2)]);
}

#[test]
fn a_waiver_for_a_whole_app_rule_outside_the_index_does_not_count() {
    let main = "// boogy-allow uses-sdk: a test page\ndocument.body.textContent = 'hi';";
    let r = check(&app(false, true), &[("index.html", INDEX), ("web/main.ts", main)]);
    assert_eq!(found(&r), vec![("uses-sdk", "index.html", 1)], "{r:?}");
}

#[test]
fn without_a_manifest_only_the_file_rules_run() {
    let r = check(&FrontendFacts::default(), &[("web/main.ts", "document.body.textContent = 'hi';")]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn an_index_under_a_source_directory_is_the_apps_index() {
    // A Vite app whose root is `web/`: the source is `["web"]`, so its index
    // arrives as `web/index.html` while the manifest names the served `index.html`.
    let index = INDEX.replace("<title>", "<!-- boogy-allow board-ready: this app is the board itself -->\n<title>");
    let main = "import { installFoundation } from '@boogy/web';\ninstallFoundation();";
    let r = check(&app(false, false), &[("web/index.html", &index), ("web/about/index.html", INDEX), ("web/main.ts", main)]);
    assert!(r.is_clean(), "{r:?}");
    assert_eq!(r.waivers.iter().map(|w| (w.rule.as_str(), w.path.as_str())).collect::<Vec<_>>(), vec![("board-ready", "web/index.html")]);
    let r = check(&app(false, false), &[("web/index.html", INDEX), ("web/main.ts", main)]);
    assert_eq!(found(&r), vec![("board-ready", "web/index.html", 1)], "{r:?}");
}

#[test]
fn a_standalone_only_app_owns_its_history() {
    let main = "import { installFoundation, connectPane } from '@boogy/web';\ninstallFoundation();\nconnectPane({ service: 'x' });\nhistory.pushState({}, '', '/a');";
    let r = check(&app(true, true), &[("index.html", INDEX), ("web/main.ts", main)]);
    assert!(r.is_clean(), "{r:?}");
    let r = check(&app(true, false), &[("index.html", INDEX), ("web/main.ts", main)]);
    assert_eq!(found(&r), vec![("board-navigation", "web/main.ts", 4)], "{r:?}");
}

#[test]
fn waiving_uses_sdk_lifts_the_rules_that_presuppose_the_sdk() {
    let index = INDEX.replace("<title>", "<!-- boogy-allow uses-sdk: a demo that predates @boogy/web -->\n<!-- boogy-allow board-ready: a demo that predates @boogy/web -->\n<title>");
    let files = [
        ("index.html", index.as_str()),
        ("web/app.css", "a { padding: 12px; color: #fff; }\n@media (min-width: 40rem) {}\n.x { gap: calc(var(--u) * 2); }"),
        ("web/main.tsx", "const b = <button>Go</button>;\nhistory.pushState({}, '', '/a');"),
    ];
    let r = check(&app(false, false), &files);
    assert_eq!(found(&r), vec![("board-navigation", "web/main.tsx", 2)], "{r:?}");
}
