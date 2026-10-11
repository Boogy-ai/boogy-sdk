use boogy_web_sdk_conventions::{check_frontend, FrontendFacts, FrontendReport};

fn check(path: &str, src: &str) -> FrontendReport {
    check_frontend(&FrontendFacts::default(), &[(path.to_string(), src.to_string())])
}

fn found(r: &FrontendReport) -> Vec<(&str, usize)> {
    r.findings.iter().map(|f| (f.rule, f.line)).collect()
}

#[test]
fn a_jsx_style_object_is_checked_and_a_bare_number_is_pixels_where_preact_says_so() {
    let r = check(
        "web/app.tsx",
        "const x = <div style={{\n  width: 320,\n  padding: '12px',\n  opacity: 0.5, flexGrow: 2, zIndex: 3, lineHeight: 1.4, order: 2,\n  color: '#fff',\n  gap: 'var(--space-2)',\n  '--w': 'var(--space-4)',\n  ...rest,\n}} />;",
    );
    assert_eq!(found(&r), vec![("fixed-size", 2), ("fixed-size", 3), ("colour-literal", 5)], "{r:?}");
}

#[test]
fn a_style_string_in_jsx_and_the_dom_style_api_are_checked() {
    let r = check(
        "web/app.tsx",
        "const a = <p style=\"width: 12px\" />;\nel.style.width = '24px';\nel.style.setProperty('margin', '2rem');\nel.style.setProperty('--panel', 'var(--space-6)');\nel.style.cssText = 'gap: 8px; color: red';\nconst b = <p style={'height: 10vh'} />;",
    );
    assert_eq!(
        found(&r),
        // Within a line, findings sort by rule.
        vec![("fixed-size", 1), ("fixed-size", 2), ("fixed-size", 3), ("colour-literal", 5), ("fixed-size", 5), ("fixed-size", 6)],
        "{r:?}"
    );
}

#[test]
fn a_string_that_is_not_a_style_is_not_checked() {
    let r = check(
        "web/app.tsx",
        "const HEADER = scale({ axis: 'min', floor: '0.75rem', cap: '1.125rem' });\nconst label = 'a 12px gap';\nel.style.width = width;\nif (a.style == b) {}\nconst sel = document.querySelector('#add');",
    );
    assert!(r.is_clean(), "{r:?}");
}

#[test]
fn a_script_comment_holds_a_waiver_and_hides_a_size() {
    let r = check(
        "web/app.tsx",
        "// boogy-allow fixed-size: the canvas is drawn at its intrinsic pixel width\nconst c = <canvas style={{ width: 640 }} />;\n/* style={{ width: 9 }} */\nconst d = <div style={{ width: 2 }} />; // boogy-allow fixed-size: a two-pixel rule line",
    );
    assert!(r.is_clean(), "{r:?}");
    assert_eq!(r.waivers.len(), 2, "{r:?}");
}

#[test]
fn an_unterminated_style_never_panics() {
    for src in ["<div style={{ width: 320", "<div style={{ width: '12px", "el.style.setProperty('margin',", "<p style=\"width: 12px"] {
        let _ = check("web/app.tsx", src);
    }
}
