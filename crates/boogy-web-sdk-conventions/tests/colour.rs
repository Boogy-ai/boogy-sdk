use boogy_web_sdk_conventions::{check_frontend, FrontendFacts, FrontendReport};

fn check(path: &str, src: &str) -> FrontendReport {
    check_frontend(&FrontendFacts::default(), &[(path.to_string(), src.to_string())])
}

fn messages(r: &FrontendReport) -> Vec<String> {
    r.findings.iter().filter(|f| f.rule == "colour-literal").map(|f| f.message.clone()).collect()
}

#[test]
fn hex_and_colour_functions_are_findings_anywhere_in_a_value() {
    let r = check("web/app.css", "a { color: #fff; background: rgb(0 0 0 / 50%); border: 1px solid hsl(200 50% 50%); box-shadow: 0 0 0 1px oklch(0.5 0.1 200); outline-color: #11223344; fill: color(display-p3 1 0 0); }");
    assert_eq!(messages(&r).len(), 6, "{r:?}");
}

#[test]
fn a_named_colour_is_a_finding_on_a_colour_property_only() {
    let r = check("web/app.css", "a { color: tomato; border-top: 2px solid red; animation-name: tomato; font-family: Navy Sans; }");
    let m = messages(&r);
    assert_eq!(m.len(), 2, "{r:?}");
    assert!(m[0].contains("tomato") && m[1].contains("red"), "{m:?}");
}

#[test]
fn keywords_tokens_and_mixes_of_tokens_are_not_literals() {
    let r = check("web/app.css", "a { color: currentColor; background: transparent; border-color: inherit; caret-color: var(--accent); background-color: color-mix(in oklch, var(--ground) 80%, transparent); --brand: #ff0066; }\n#add .x { color: var(--text-1); mask: url(#mask); }");
    assert!(messages(&r).is_empty(), "{r:?}");
}

#[test]
fn an_html_style_attribute_is_checked_for_colours() {
    let r = check("index.html", "<p style=\"color: #333\">x</p>");
    assert_eq!(messages(&r).len(), 1, "{r:?}");
}

#[test]
fn a_colour_in_a_mask_only_says_opaque_and_is_not_a_literal_colour() {
    let r = check("web/app.css", "a { mask: linear-gradient(#fff 0 0) content-box, linear-gradient(#fff 0 0); -webkit-mask: linear-gradient(#000 0 0); mask-image: linear-gradient(black, transparent); }\nb { background: linear-gradient(#fff 0 0); }");
    let lines: Vec<usize> = r.findings.iter().map(|f| f.line).collect();
    assert_eq!(lines, vec![2], "{r:?}");
}
