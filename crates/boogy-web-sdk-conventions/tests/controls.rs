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

// The SDK's own components, copied as they are: the spread is on the
// `field()` wrapper, and the control inside it carries `data-slot="control"`,
// directly or inside a `data-slot="group"`.
const TEXT_FIELD: &str = include_str!("fixtures/text-field.tsx");
const TEXT_AREA: &str = include_str!("fixtures/text-area.tsx");
const COMPOSER: &str = include_str!("fixtures/composer.tsx");

#[test]
fn the_sdks_own_text_field_text_area_and_composer_pass() {
    for (path, src) in [("web/text-field.tsx", TEXT_FIELD), ("web/text-area.tsx", TEXT_AREA), ("web/composer.tsx", COMPOSER)] {
        let r = check(path, src);
        assert!(r.is_clean(), "{path}: {r:?}");
    }
}

#[test]
fn the_field_function_imported_under_another_name_counts() {
    let aliased = TEXT_AREA.replace("import { field } from '@boogy/web';", "import { field as f } from '@boogy/web';").replace("{...field(", "{...f(");
    assert!(aliased.contains("{...f({"), "the fixture changed shape");
    assert!(check("web/text-area.tsx", &aliased).is_clean());
    let group = "import { field as f } from '@boogy/web';\nconst c = <form {...f()}>\n  <div data-slot=\"group\">\n    <input type=\"search\" data-slot='control' />\n  </div>\n</form>;";
    assert!(check("web/search.tsx", group).is_clean());
}

#[test]
fn a_field_shaped_control_inside_an_expression_or_a_fragment_keeps_its_parent() {
    let r = check(
        "web/a.tsx",
        "import { field } from '@boogy/web';\nconst a = <div {...field()}>{open && <textarea data-slot=\"control\" />}</div>;\nconst b = <div {...field()}><><input data-slot=\"control\" /></></div>;",
    );
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn a_control_slot_with_no_field_parent_is_a_finding() {
    let r = check(
        "web/a.tsx",
        concat!(
            "import { field, stack } from '@boogy/web';\n",
            "const a = <textarea data-slot=\"control\" />;\n",
            "const b = <div {...stack()}><input data-slot=\"control\" /></div>;\n",
            "const c = <div {...field()}><div><textarea data-slot=\"control\" /></div></div>;\n",
            "const d = <div {...field()}><textarea /></div>;\n",
            "const e = <div {...field()}><div data-slot=\"group\"><div data-slot=\"group\"><input data-slot=\"control\" /></div></div></div>;\n",
            "const f = <div {...field()}><div data-slot={slot}><input data-slot=\"control\" /></div></div>;\n",
            "const g = <div {...field()} />;\nconst h = <input data-slot=\"control\" />;\n",
            "const i = <div {...field()}><span>{x}</span><input data-slot={kind} /></div>;",
        ),
    );
    let got: Vec<(&str, usize)> = r.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(got, [2, 3, 4, 5, 6, 7, 9, 10].map(|l| ("sdk-control", l)).to_vec(), "{r:?}");
}

#[test]
fn spreading_some_other_sdk_function_onto_a_control_is_not_the_sdks_shape() {
    let r = check(
        "web/a.tsx",
        "import { sheet, field, button } from '@boogy/web';\nconst a = <textarea {...sheet()} />;\nconst b = <input {...field()} />;\nconst c = <textarea {...button()} data-slot=\"control\" />;",
    );
    let got: Vec<(&str, usize)> = r.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(got, vec![("sdk-control", 2), ("sdk-control", 3), ("sdk-control", 4)], "{r:?}");
}

#[test]
fn a_button_passes_when_it_spreads_the_sdks_button_and_only_then() {
    let ok = check("web/a.tsx", "import { button } from '@boogy/web';\nconst a = <button {...button({ variant: 'solid' })}>Go</button>;");
    assert!(ok.is_clean(), "{ok:?}");
    let aliased = check("web/a.tsx", "import { button as btn } from '@boogy/web';\nconst a = <button type=\"button\" {...btn()}>Go</button>;");
    assert!(aliased.is_clean(), "{aliased:?}");
    let bad = check("web/a.tsx", "import { button, field } from '@boogy/web';\nconst a = <button {...field()}>Go</button>;\nconst b = <div {...field()}><button data-slot=\"control\">Go</button></div>;");
    let got: Vec<(&str, usize)> = bad.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(got, vec![("sdk-control", 2), ("sdk-control", 3)], "{bad:?}");
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

#[test]
fn deep_unclosed_nesting_finishes() {
    let n = 50_000;
    for src in [
        format!("{}{}", "<a>".repeat(n), "</b>".repeat(n)),
        format!("const x = {}{}", "<>".repeat(n), "<input />".repeat(n)),
        format!("const x = <div>{}{}", "{".repeat(n), "<input />".repeat(n)),
    ] {
        let started = std::time::Instant::now();
        let _ = check("web/app.tsx", &src);
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "took {:?}", started.elapsed());
    }
}
