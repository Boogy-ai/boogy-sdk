//! Script files: their style contexts, controls, navigation, and the imports
//! and calls the whole-app rules read. `code` has its comments blanked.

use crate::css::{self, is_ident, skip_string};
use crate::{AppSignals, Raw};

pub(crate) fn check(code: &str, path: &str, jsx: bool, out: &mut Vec<Raw>, app: &mut AppSignals) {
    let ctx = css::Ctx::for_path(path);
    let sdk = sdk_imports(code, app);
    style_attributes(code, &ctx, out);
    style_assignments(code, &ctx, out);
    navigation(code, out);
    calls(code, out, app);
    if jsx {
        controls(code, &sdk, out);
    }
}

pub(crate) fn is_quote(b: u8) -> bool {
    matches!(b, b'"' | b'\'' | b'`')
}

/// The string literal opening at `i`: (content start, content end, index past it).
pub(crate) fn string_at(b: &[u8], i: usize) -> Option<(usize, usize, usize)> {
    if !b.get(i).copied().is_some_and(is_quote) {
        return None;
    }
    let after = skip_string(b, i);
    let end = if after <= b.len() && after > i + 1 && b.get(after - 1) == Some(&b[i]) { after - 1 } else { b.len() };
    Some((i + 1, end.max(i + 1), after))
}

pub(crate) fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The index of the bracket closing the one at `open`, past strings and
/// nested brackets of every kind.
pub(crate) fn matching(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            q if is_quote(q) => {
                i = skip_string(b, i);
                continue;
            }
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The next `sep` at bracket depth 0 from `from`, or `limit`.
pub(crate) fn next_top_level(b: &[u8], from: usize, limit: usize, sep: u8) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < limit {
        match b[i] {
            q if is_quote(q) => {
                i = skip_string(b, i);
                continue;
            }
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    limit
}

/// `backgroundColor` → `background-color`; a custom property stays as it is.
pub(crate) fn kebab(key: &str) -> String {
    if key.starts_with("--") {
        return key.to_string();
    }
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The style properties Preact writes a bare number to as it is, with no
/// `px`: the same test Preact applies, plus custom properties.
pub(crate) fn unitless(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.starts_with('-')
        || k.starts_with("ord")
        || ["acit", "rph", "grid", "ows", "mnc", "ntw", "inec", "ineh", "zoo", "itera"].iter().any(|s| k.contains(s))
        || k.match_indices("ex").any(|(p, _)| matches!(k.as_bytes().get(p + 2), None | Some(b's' | b'g' | b'n' | b'p')))
}

/// `style="…"`, `style={'…'}` and `style={{ … }}` attributes.
fn style_attributes(code: &str, ctx: &css::Ctx, out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(p) = code[from..].find("style=") {
        let at = from + p;
        from = at + "style=".len();
        if at == 0 || !b[at - 1].is_ascii_whitespace() {
            continue;
        }
        let v = from;
        match b.get(v) {
            Some(&q) if is_quote(q) => {
                if let Some((s, e, _)) = string_at(b, v) {
                    css::check_declarations(&code[s..e], s, ctx, out);
                }
            }
            Some(b'{') => {
                let Some(end) = matching(b, v) else { continue };
                let inner = skip_ws(b, v + 1);
                match b.get(inner) {
                    Some(b'{') => object_literal(code, inner, ctx, out),
                    Some(&q) if is_quote(q) => {
                        if let Some((s, e, _)) = string_at(b, inner) {
                            css::check_declarations(&code[s..e], s, ctx, out);
                        }
                    }
                    _ => {}
                }
                from = end;
            }
            _ => {}
        }
    }
}

/// The properties of a style object literal: each string value as that
/// property's value, and each bare number as Preact writes it.
fn object_literal(code: &str, open: usize, ctx: &css::Ctx, out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    let Some(close) = matching(b, open) else { return };
    let mut i = open + 1;
    while i < close {
        i = skip_ws(b, i);
        if i >= close {
            break;
        }
        if b[i] == b'.' {
            i = next_top_level(b, i, close, b',') + 1;
            continue;
        }
        let (key, after_key) = if is_quote(b[i]) {
            match string_at(b, i) {
                Some((s, e, after)) => (&code[s..e], after),
                None => return,
            }
        } else {
            let s = i;
            while i < close && (is_ident(b[i]) || b[i] == b'$') {
                i += 1;
            }
            (&code[s..i], i)
        };
        let colon = skip_ws(b, after_key);
        if key.is_empty() || b.get(colon) != Some(&b':') {
            i = next_top_level(b, after_key.max(i + 1), close, b',') + 1;
            continue;
        }
        let v = skip_ws(b, colon + 1);
        let end = next_top_level(b, v, close, b',');
        style_value(key, code[v..end].trim_end(), v, ctx, out);
        i = end + 1;
    }
}

fn style_value(key: &str, value: &str, at: usize, ctx: &css::Ctx, out: &mut Vec<Raw>) {
    let b = value.as_bytes();
    if b.first().copied().is_some_and(is_quote) {
        if let Some((s, e, after)) = string_at(b, 0) {
            if value[after.min(value.len())..].trim().is_empty() {
                css::check_value(&kebab(key), &value[s..e], at + s, css::Block::Rule, ctx, out);
            }
        }
        return;
    }
    if let Ok(n) = value.parse::<f64>() {
        if n.abs() > 1.0 && !unitless(key) {
            out.push(Raw { rule: "fixed-size", at, message: format!("`{key}: {value}` is {value} pixels") });
        }
    }
}

/// `el.style.x = '…'`, `el.style.cssText = '…'` and `el.style.setProperty('x', '…')`.
fn style_assignments(code: &str, ctx: &css::Ctx, out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(p) = code[from..].find(".style.") {
        let s = from + p + ".style.".len();
        from = s;
        let mut i = s;
        while i < b.len() && is_ident(b[i]) {
            i += 1;
        }
        let name = &code[s..i];
        if name == "setProperty" {
            let open = skip_ws(b, i);
            if b.get(open) != Some(&b'(') {
                continue;
            }
            let Some((s0, e0, after0)) = string_at(b, skip_ws(b, open + 1)) else { continue };
            let comma = skip_ws(b, after0);
            if b.get(comma) != Some(&b',') {
                continue;
            }
            let Some((s1, e1, _)) = string_at(b, skip_ws(b, comma + 1)) else { continue };
            css::check_value(&code[s0..e0], &code[s1..e1], s1, css::Block::Rule, ctx, out);
            continue;
        }
        let eq = skip_ws(b, i);
        if b.get(eq) != Some(&b'=') || b.get(eq + 1) == Some(&b'=') {
            continue;
        }
        let Some((s1, e1, _)) = string_at(b, skip_ws(b, eq + 1)) else { continue };
        if name == "cssText" {
            css::check_declarations(&code[s1..e1], s1, ctx, out);
        } else {
            css::check_value(&kebab(name), &code[s1..e1], s1, css::Block::Rule, ctx, out);
        }
    }
}

/// The local names this file imports from `@boogy/web` itself (its attrs
/// functions); sets `imports_sdk` for any import of the SDK or a sub-path.
pub(crate) fn sdk_imports(code: &str, app: &mut AppSignals) -> Vec<String> {
    let b = code.as_bytes();
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(p) = code[from..].find("@boogy/web") {
        let at = from + p;
        from = at + "@boogy/web".len();
        let quoted = at > 0 && is_quote(b[at - 1]);
        let next = b.get(from).copied();
        if !quoted || !next.is_some_and(|c| c == b'/' || is_quote(c)) {
            continue;
        }
        app.imports_sdk = true;
        if next != Some(b[at - 1]) {
            continue; // a sub-path (`@boogy/web/preact`): components, not attrs
        }
        let head = code[..at - 1].trim_end();
        let Some(head) = head.strip_suffix("from") else { continue };
        let Some(body) = head.trim_end().strip_suffix('}').and_then(|h| h.rfind('{').map(|o| &h[o + 1..])) else { continue };
        for item in body.split(',') {
            let item = item.trim();
            if item.is_empty() || item.starts_with("type ") {
                continue;
            }
            names.push(item.rsplit(" as ").next().unwrap_or(item).trim().to_string());
        }
    }
    names
}

fn navigation(code: &str, out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    for name in ["pushState", "replaceState"] {
        let pat = format!("history.{name}");
        let mut from = 0;
        while let Some(p) = code[from..].find(&pat) {
            let at = from + p;
            from = at + pat.len();
            if b.get(skip_ws(b, from)) == Some(&b'(') {
                out.push(Raw { rule: "board-navigation", at, message: format!("`history.{name}` moves a board's history, not the pane's") });
            }
        }
    }
}

const FLAGGED_INPUTS: &[(&str, &str)] = &[
    ("text", "TextField"), ("email", "TextField"), ("password", "TextField"), ("number", "TextField"),
    ("url", "TextField"), ("tel", "TextField"), ("search", "SearchField"), ("checkbox", "Switch"), ("radio", "ChoiceGroup"),
];

enum InputType {
    Missing,
    Static(String),
    Dynamic,
}

fn controls(code: &str, sdk: &[String], out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    for (tag, component) in [("button", "Button"), ("textarea", "TextArea"), ("input", "")] {
        let open = format!("<{tag}");
        let mut from = 0;
        while let Some(p) = code[from..].find(&open) {
            let at = from + p;
            let name_end = at + open.len();
            from = name_end;
            if !b.get(name_end).is_some_and(|c| c.is_ascii_whitespace() || *c == b'/' || *c == b'>') {
                continue;
            }
            let end = tag_end(b, name_end);
            from = end.max(name_end);
            let attrs = &code[name_end..end];
            if spreads_sdk(attrs, sdk) || is_item_title(code, at) {
                continue;
            }
            let suggestion = if tag == "input" {
                match input_type(attrs) {
                    InputType::Missing => "TextField",
                    InputType::Static(t) => match FLAGGED_INPUTS.iter().find(|(k, _)| *k == t) {
                        Some((_, c)) => c,
                        None => continue,
                    },
                    InputType::Dynamic => continue,
                }
            } else {
                component
            };
            out.push(Raw { rule: "sdk-control", at, message: format!("a raw `<{tag}>`; the SDK has `{suggestion}`") });
        }
    }
}

/// A control given as an item's `title` (`<ListItem title={<button …>}>`):
/// the SDK's own pattern, whose press area covers the whole item.
fn is_item_title(code: &str, at: usize) -> bool {
    code[..at].trim_end().strip_suffix('{').is_some_and(|h| h.trim_end().strip_suffix('=').is_some_and(|h| h.trim_end().ends_with("title")))
}

/// The index of the `>` that closes a tag whose attributes start at `from`,
/// past strings and `{…}` expressions; the end of the file if none does.
fn tag_end(b: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q if is_quote(q) => {
                i = skip_string(b, i);
                continue;
            }
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn spreads_sdk(attrs: &str, sdk: &[String]) -> bool {
    let b = attrs.as_bytes();
    let mut from = 0;
    while let Some(p) = attrs[from..].find("{...") {
        let s = skip_ws(b, from + p + 4);
        from = s.max(from + p + 4);
        let mut j = s;
        while j < b.len() && (is_ident(b[j]) || b[j] == b'$') {
            j += 1;
        }
        if b.get(skip_ws(b, j)) == Some(&b'(') && sdk.iter().any(|n| n == &attrs[s..j]) {
            return true;
        }
    }
    false
}

fn input_type(attrs: &str) -> InputType {
    let b = attrs.as_bytes();
    let mut from = 0;
    while let Some(p) = attrs[from..].find("type=") {
        let at = from + p;
        from = at + 5;
        if at > 0 && !b[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut v = from;
        if b.get(v) == Some(&b'{') {
            v = skip_ws(b, v + 1);
        }
        return match string_at(b, v) {
            Some((s, e, _)) => InputType::Static(attrs[s..e].to_ascii_lowercase()),
            None => InputType::Dynamic,
        };
    }
    InputType::Missing
}

/// Byte offsets of each call to `name`: the whole word, then `(`.
fn call_sites(code: &str, name: &str) -> Vec<usize> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = code[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        if (at > 0 && is_ident(b[at - 1])) || b.get(from).is_some_and(|c| is_ident(*c)) {
            continue;
        }
        if b.get(skip_ws(b, from)) == Some(&b'(') {
            out.push(at);
        }
    }
    out
}

fn calls(code: &str, out: &mut Vec<Raw>, app: &mut AppSignals) {
    let b = code.as_bytes();
    for at in call_sites(code, "installFoundation") {
        app.installs += 1;
        let open = skip_ws(b, at + "installFoundation".len());
        if let Some(close) = matching(b, open) {
            let args: String = code[open + 1..close].chars().filter(|c| !c.is_whitespace()).collect();
            if args.contains("scale:false") {
                out.push(Raw { rule: "page-scale", at, message: "`installFoundation` turns the page scale off".into() });
            }
        }
    }
    if !call_sites(code, "connectPane").is_empty() {
        app.connects_pane = true;
    }
}
