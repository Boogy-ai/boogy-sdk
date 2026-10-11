use boogy_web_sdk_conventions::{check_frontend, FrontendFacts, FrontendReport};

fn check(files: &[(&str, &str)]) -> FrontendReport {
    let files: Vec<(String, String)> = files.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect();
    check_frontend(&FrontendFacts::default(), &files)
}

/// (rule, path, line) of every finding, in report order.
fn found(r: &FrontendReport) -> Vec<(&str, &str, usize)> {
    r.findings.iter().map(|f| (f.rule, f.path.as_str(), f.line)).collect()
}

#[test]
fn fixed_sizes_are_findings_and_zero_and_hairlines_are_not() {
    let r = check(&[("web/app.css", "a { padding: 12px; margin: 0px; border: 1px solid; width: 2rem; height: 50vh; gap: .5pt; inset: 0rem; }")]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 1); 4], "{r:?}");
    assert!(r.findings[0].message.contains("12px"), "{r:?}");
}

#[test]
fn the_dynamic_viewport_units_are_fixed_sizes_too() {
    let r = check(&[("web/app.css", "a { min-height: 90dvh; width: 50svw; }")]);
    assert_eq!(r.findings.len(), 2, "{r:?}");
}

#[test]
fn theme_css_may_define_a_fixed_size_as_a_custom_property_and_nothing_else() {
    let r = check(&[
        ("web/theme.css", ":root { --gutter: 12px; }\nbody { padding: 12px; }"),
        ("web/app.css", ".panel { --gutter: 12px; }"),
        ("web/sizes.css", "@property --w { syntax: '<length>'; inherits: false; initial-value: 0px; }"),
    ]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 1), ("fixed-size", "web/theme.css", 2)], "{r:?}");
}

#[test]
fn text_that_only_looks_like_a_size_is_not_one() {
    let r = check(&[(
        "web/app.css",
        "a { content: \"12px\"; background: url(/img/12px.png); grid-area: col-12px; }\n.w-12px { gap: 0; }\n/* 24px */\n.x { padding: var(--space-3); scale: var(--u-zoom); }",
    )]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn a_query_on_the_window_size_is_a_finding_and_one_on_the_person_or_device_is_not() {
    let r = check(&[(
        "web/app.css",
        "@media (min-width: 40rem) {}\n@media (prefers-reduced-motion: reduce) {}\n@media (pointer: coarse) and (hover: hover) {}\n@media print {}\n@media (width >= 30rem) {}\n@media (orientation: portrait) {}\n@container (min-width: 30rem) {}",
    )]);
    assert_eq!(
        found(&r),
        vec![("layout-media-query", "web/app.css", 1), ("layout-media-query", "web/app.css", 5), ("layout-media-query", "web/app.css", 6)],
        "{r:?}"
    );
}

#[test]
fn multiplying_the_unit_is_for_token_definitions_only() {
    let r = check(&[(
        "web/app.css",
        ":root {\n  --panel-w: calc(var(--u) * 28);\n}\n.x { padding: calc(var(--u) * 0.6); gap: var(--space-2); }",
    )]);
    assert_eq!(found(&r), vec![("unit-multiple", "web/app.css", 4)], "{r:?}");
    assert!(r.findings[0].fix().contains("--space"), "{r:?}");
}

#[test]
fn style_blocks_and_style_attributes_in_html_are_checked_and_html_comments_are_not() {
    let r = check(&[(
        "index.html",
        "<html><head>\n<style>a { padding: 12px }</style>\n<!-- 24px -->\n</head><body>\n<div style=\"width: 20px\"></div>\n<div data-style=\"width: 20px\"></div>\n</body></html>",
    )]);
    assert_eq!(found(&r), vec![("fixed-size", "index.html", 2), ("fixed-size", "index.html", 5)], "{r:?}");
}

#[test]
fn a_waiver_on_the_line_or_the_line_before_excuses_the_finding_and_is_listed() {
    let r = check(&[(
        "web/app.css",
        "a { padding: 12px; } /* boogy-allow fixed-size: the slider's track is drawn in pixels */\n/* boogy-allow fixed-size: the frame is a fixed print size */\nb { width: 300px; }\n/* boogy-allow fixed-size: too far above */\n\nc { width: 300px; }",
    )]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 6)], "{r:?}");
    let waived: Vec<(&str, usize, &str)> = r.waivers.iter().map(|w| (w.rule.as_str(), w.line, w.reason.as_str())).collect();
    assert_eq!(
        waived,
        vec![("fixed-size", 1, "the slider's track is drawn in pixels"), ("fixed-size", 2, "the frame is a fixed print size")]
    );
}

#[test]
fn an_html_comment_holds_a_waiver_too() {
    let r = check(&[("index.html", "<!-- boogy-allow fixed-size: a fixed-width email preview -->\n<div style=\"width: 600px\"></div>")]);
    assert!(r.is_clean(), "{r:?}");
    assert_eq!(r.waivers.len(), 1);
}

#[test]
fn a_waiver_without_a_reason_or_with_an_unknown_rule_or_no_colon_is_itself_a_finding() {
    let r = check(&[(
        "web/app.css",
        "/* boogy-allow fixed-size: */\na { padding: 12px; }\n/* boogy-allow fixed-sise: typo */\n/* boogy-allow fixed-size because */",
    )]);
    let rules: Vec<(&str, usize)> = r.findings.iter().map(|f| (f.rule, f.line)).collect();
    assert_eq!(rules, vec![("waiver", 1), ("fixed-size", 2), ("waiver", 3), ("waiver", 4)], "{r:?}");
}

#[test]
fn files_that_are_not_the_authors_are_never_scanned() {
    let r = check(&[
        ("web/vendor/lib.css", "a { padding: 12px }"),
        ("node_modules/x/a.css", "a { padding: 12px }"),
        ("dist/assets/a.css", "a { padding: 12px }"),
        ("web/app.min.css", "a { padding: 12px }"),
        ("web/types.d.ts", "export const x = 1;"),
        ("web/app.test.tsx", "<div style={{ width: 12 }} />"),
        ("web/app.spec.ts", "x"),
        ("web/logo.svg", "<svg width=\"120px\"/>"),
    ]);
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn malformed_source_never_panics() {
    let long = "a{padding:0}".repeat(90_000);
    let r = check(&[
        ("web/a.css", "a { padding: 12px; /* never closed"),
        ("web/b.css", "a { content: \"never closed; padding: 12px; }"),
        ("web/c.css", "a { content: 'é'; padding: 12px }"),
        ("web/d.css", "}}}{{{ a { padding: 12px"),
        ("web/e.css", &long),
        ("index.html", "<style>a { padding: 12px"),
    ]);
    let paths: Vec<&str> = r.findings.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"web/a.css") && paths.contains(&"web/c.css") && paths.contains(&"web/d.css") && paths.contains(&"index.html"), "{r:?}");
}

#[test]
fn a_size_multiplied_by_the_board_zoom_follows_it_and_is_not_fixed() {
    let r = check(&[("web/app.css", "a { font-size: calc(max(1.125rem, 3.5cqmin) * var(--u-zoom)); }\nb { font-size: 1rem; }")]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 2)], "{r:?}");
}

#[test]
fn the_whole_viewport_is_the_whole_pane_and_not_a_fixed_size() {
    let r = check(&[("web/app.css", "a { block-size: 100dvh; inline-size: 100vw; min-height: 100svh; }\nb { height: 50vh; }")]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 2)], "{r:?}");
}

#[test]
fn em_and_ch_follow_the_type_they_belong_to_and_are_not_fixed() {
    let r = check(&[("web/app.css", "a { padding: 0.15em 0.55em; max-width: 40ch; letter-spacing: 0.04em; width: 1.2em; }\nb { padding: 1rem; }")]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 2)], "{r:?}");
}

#[test]
fn a_token_defined_on_the_root_is_a_named_token_and_may_hold_a_fixed_size() {
    let r = check(&[("web/app.css", ":root { --gutter: 12px; }\n:root, .app { --blur: 8px; }\n.x { --w: 320px; }\n:root { padding: 12px; }")]);
    assert_eq!(found(&r), vec![("fixed-size", "web/app.css", 3), ("fixed-size", "web/app.css", 4)], "{r:?}");
}
