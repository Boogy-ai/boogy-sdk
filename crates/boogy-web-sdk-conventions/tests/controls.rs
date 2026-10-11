use boogy_web_sdk_conventions::{check_frontend, FrontendFacts, FrontendReport};

fn check(path: &str, src: &str) -> FrontendReport {
    check_frontend(&FrontendFacts::default(), &[(path.to_string(), src.to_string())])
}

fn found(r: &FrontendReport) -> Vec<(&str, usize, String)> {
    r.findings.iter().map(|f| (f.rule, f.line, f.message.clone())).collect()
}

#[test]
fn raw_controls_the_sdk_has_a_component_for_are_findings_naming_it() {
    let r = check(
        "web/app.tsx",
        "<button onClick={go}>Go</button>\n<textarea />\n<input />\n<input type=\"checkbox\" />\n<input type='search' />\n<input type={\"radio\"} />\n<input type=\"email\" />",
    );
    let got: Vec<(usize, bool)> = found(&r).iter().map(|(rule, line, m)| (*line, *rule == "sdk-control" && !m.is_empty())).collect();
    assert_eq!(got, (1..=7).map(|l| (l, true)).collect::<Vec<_>>(), "{r:?}");
    let m: Vec<String> = found(&r).into_iter().map(|(_, _, m)| m).collect();
    assert!(m[0].contains("Button") && m[1].contains("TextArea") && m[2].contains("TextField") && m[3].contains("Switch") && m[4].contains("SearchField") && m[5].contains("ChoiceGroup"), "{m:?}");
}

#[test]
fn an_element_spreading_the_sdks_attrs_is_the_sdks_own_way() {
    let r = check(
        "web/app.tsx",
        "import { button, field as fieldAttrs } from '@boogy/web';\nconst a = <button {...button({ variant: 'solid' })}>Go</button>;\nconst b = <input {...fieldAttrs()} />;",
    );
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn a_spread_of_something_else_is_not_the_sdk() {
    let r = check("web/app.tsx", "import { Button } from '@boogy/web/preact';\nconst a = <button {...props}>Go</button>;\nconst b = <button {...button()}>Go</button>;");
    assert_eq!(r.findings.len(), 2, "{r:?}");
}

#[test]
fn controls_the_sdk_has_no_component_for_and_types_set_at_run_time_are_fine() {
    let r = check(
        "web/app.tsx",
        "<input type=\"range\" />\n<input type=\"file\" />\n<input type=\"color\" />\n<input type=\"hidden\" />\n<input type={kind} />\n<select />\n<dialog />\n<ButtonGroup />\n<buttons />",
    );
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn raw_elements_built_in_plain_script_are_the_vanilla_api() {
    let r = check("web/main.ts", "const b = el('button', button({ variant: 'solid' }));\nconst t = '<button>';\ndocument.createElement('input');");
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn history_push_and_replace_are_findings_and_pane_navigate_is_not() {
    let r = check(
        "web/router.ts",
        "history.pushState({}, '', '/a');\nwindow.history.replaceState(null, '', '/b');\npane.navigate('/c');\n// history.pushState(x)\nconst h = history.length;",
    );
    let got: Vec<(&str, usize)> = r.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(got, vec![("board-navigation", 1), ("board-navigation", 2)], "{r:?}");
}

#[test]
fn a_file_of_endless_button_openings_finishes() {
    let src = "<button ".repeat(100_000);
    let _ = check("web/app.tsx", &src);
}

#[test]
fn a_button_given_as_an_items_title_is_the_sdks_own_title_control() {
    let r = check(
        "web/row.tsx",
        "const a = <ListItem title={<button type=\"button\" onClick={open}>{name}</button>} />;\nconst b = <Card\n  title={\n    <button type=\"button\" onClick={open}>{name}</button>\n  } />;\nconst c = <Card footer={<button onClick={go}>Go</button>} />;",
    );
    let got: Vec<(&str, usize)> = r.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(got, vec![("sdk-control", 6)], "{r:?}");
}
