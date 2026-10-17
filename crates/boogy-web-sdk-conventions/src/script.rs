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

/// What this file imports from `@boogy/web` itself (its attrs functions), as
/// (exported name, local name); sets `imports_sdk` for any import of the SDK
/// or a sub-path.
pub(crate) fn sdk_imports(code: &str, app: &mut AppSignals) -> Vec<(String, String)> {
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
            let (export, local) = item.split_once(" as ").unwrap_or((item, item));
            names.push((export.trim().to_string(), local.trim().to_string()));
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

enum AttrValue {
    Missing,
    Static(String),
    Dynamic,
}

/// An open JSX element, as a child below it sees it.
struct Element {
    name: (usize, usize),
    /// It spreads the SDK's `field()`: a field's wrapper.
    field: bool,
    /// It carries a static `data-slot="group"`.
    group: bool,
}

impl Element {
    /// A fragment renders no element of its own, so its children are its
    /// parent's.
    fn transparent(&self, code: &str) -> bool {
        matches!(&code[self.name.0..self.name.1], "" | "Fragment" | "React.Fragment")
    }
}

/// Where the scan is: in script, in an element's children, or in a tag's
/// attributes.
enum Frame {
    /// Script, with its open braces; `attr` when it is an attribute's
    /// `{…}`, whose elements have no parent.
    Code { braces: usize, attr: bool },
    Children(Element),
    Tag { at: usize, name: (usize, usize) },
}

/// The local names bound to the SDK's `export` attrs function.
fn locals<'a>(sdk: &'a [(String, String)], export: &str) -> Vec<&'a str> {
    sdk.iter().filter(|(e, _)| e == export).map(|(_, l)| l.as_str()).collect()
}

/// Raw controls in JSX. The SDK's own shape passes: a `<button>` spreading
/// `button()`, and a text control with `data-slot="control"` whose parent
/// spreads `field()`, or whose parent is a `data-slot="group"` whose parent
/// does. One pass keeps a stack of the open elements: a `{…}` in children is
/// script whose elements are children of the element around it, and a
/// `{…}` in a tag's attributes is script whose elements have no parent.
fn controls(code: &str, sdk: &[(String, String)], out: &mut Vec<Raw>) {
    let b = code.as_bytes();
    let (button_fns, field_fns) = (locals(sdk, "button"), locals(sdk, "field"));
    let mut frames = vec![Frame::Code { braces: 0, attr: false }];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match frames.last_mut().expect("the script frame is never popped") {
            Frame::Code { braces, .. } => match c {
                q if is_quote(q) => {
                    i = skip_string(b, i);
                    continue;
                }
                b'{' => *braces += 1,
                b'}' if *braces > 0 => *braces -= 1,
                b'}' if frames.len() > 1 => {
                    frames.pop();
                }
                b'<' if opens_element(b, i) => {
                    i = open_tag(b, i, &mut frames);
                    continue;
                }
                _ => {}
            },
            Frame::Children(_) => match c {
                b'{' => frames.push(Frame::Code { braces: 0, attr: false }),
                b'<' if b.get(i + 1) == Some(&b'/') => {
                    let (name, gt) = tag_name(b, i + 2);
                    close(code, &code[name.0..name.1], &mut frames);
                    i = (gt + 1).max(i + 2);
                    continue;
                }
                b'<' if b.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic() || *n == b'>' || *n == b'_') => {
                    i = open_tag(b, i, &mut frames);
                    continue;
                }
                _ => {}
            },
            Frame::Tag { at, name } => match c {
                q if is_quote(q) => {
                    i = skip_string(b, i);
                    continue;
                }
                b'{' => frames.push(Frame::Code { braces: 0, attr: true }),
                b'>' => {
                    let (at, name) = (*at, *name);
                    frames.pop();
                    let attrs = &code[name.1..i];
                    let tag = &code[name.0..name.1];
                    if matches!(tag, "button" | "textarea" | "input") {
                        let (parent, grandparent) = parents(code, &frames);
                        let in_field = parent.is_some_and(|p| p.field) || (parent.is_some_and(|p| p.group) && grandparent.is_some_and(|g| g.field));
                        if let Some(suggestion) = control_finding(code, at, tag, attrs, &button_fns, in_field) {
                            out.push(Raw { rule: "sdk-control", at, message: format!("a raw `<{tag}>`; the SDK has `{suggestion}`") });
                        }
                    }
                    if !attrs.trim_end().ends_with('/') {
                        let group = matches!(attr_value(attrs, "data-slot"), AttrValue::Static(v) if v == "group");
                        frames.push(Frame::Children(Element { name, field: spreads(attrs, &field_fns), group }));
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
}

/// In script, a `<` opens an element only where an expression may start:
/// after `(`, `,`, `=`, `=>`, `?`, `:`, `{`, `[`, `&&`, `||`, `;`, `return`
/// or the start of the file. After a name it is a comparison or a type's
/// arguments (`useRef<HTMLInputElement>`).
fn opens_element(b: &[u8], i: usize) -> bool {
    if !b.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic() || *n == b'>' || *n == b'_') {
        return false;
    }
    let head = &b[..i];
    let end = head.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(0, |p| p + 1);
    let head = &head[..end];
    match head.last() {
        None => true,
        Some(b'(' | b',' | b'=' | b'?' | b':' | b'{' | b'[' | b'&' | b'|' | b';' | b'>') => true,
        Some(_) => ["return", "yield", "default"].iter().any(|k| {
            head.ends_with(k.as_bytes()) && (head.len() == k.len() || !is_ident(head[head.len() - k.len() - 1]))
        }),
    }
}

/// The name of the tag whose name starts at `from`, and the index of the
/// first byte past it (for a closing tag, its `>`).
fn tag_name(b: &[u8], from: usize) -> ((usize, usize), usize) {
    let s = skip_ws(b, from);
    let mut e = s;
    while e < b.len() && (is_ident(b[e]) || matches!(b[e], b'.' | b':' | b'$')) {
        e += 1;
    }
    let gt = skip_ws(b, e);
    ((s, e), if b.get(gt) == Some(&b'>') { gt } else { e.max(from) })
}

fn open_tag(b: &[u8], lt: usize, frames: &mut Vec<Frame>) -> usize {
    let (name, _) = tag_name(b, lt + 1);
    frames.push(Frame::Tag { at: lt, name });
    name.1.max(lt + 1)
}

/// How far up the open elements a closing tag or a parent is looked for:
/// deeper than any page nests, and it keeps a file of unclosed tags linear.
const REACH: usize = 64;

/// A closing tag ends the innermost open element of its name in the
/// current run of children, and every element left open inside it; one
/// that names none of them is ignored.
fn close(code: &str, name: &str, frames: &mut Vec<Frame>) {
    for k in (frames.len().saturating_sub(REACH)..frames.len()).rev() {
        match &frames[k] {
            Frame::Children(el) if &code[el.name.0..el.name.1] == name => {
                frames.truncate(k);
                return;
            }
            Frame::Children(_) => {}
            _ => return,
        }
    }
}

/// The element a tag closing now is a child of, and that element's parent,
/// seen through fragments and children's `{…}`.
fn parents<'a>(code: &str, frames: &'a [Frame]) -> (Option<&'a Element>, Option<&'a Element>) {
    let mut found = frames.iter().rev().take(REACH).map_while(|f| match f {
        Frame::Code { attr: false, .. } => Some(None),
        Frame::Children(el) => Some(Some(el)),
        _ => None,
    });
    let mut next = || found.by_ref().flatten().find(|el| !el.transparent(code));
    // `map_while` stops at the script around the outermost element and at
    // an attribute's `{…}`: nothing there is a parent.
    let parent = next();
    (parent, next())
}

/// The component a raw control should be, or `None` when it is the SDK's
/// own shape or not a control the SDK has one for.
fn control_finding(code: &str, at: usize, tag: &str, attrs: &str, button_fns: &[&str], in_field: bool) -> Option<&'static str> {
    if is_item_title(code, at) {
        return None;
    }
    let slotted = in_field && matches!(attr_value(attrs, "data-slot"), AttrValue::Static(v) if v == "control");
    match tag {
        "button" => (!spreads(attrs, button_fns)).then_some("Button"),
        "textarea" => (!slotted).then_some("TextArea"),
        _ => {
            let suggestion = match attr_value(attrs, "type") {
                AttrValue::Missing => "TextField",
                AttrValue::Static(t) => FLAGGED_INPUTS.iter().find(|(k, _)| *k == t)?.1,
                AttrValue::Dynamic => return None,
            };
            let text = matches!(suggestion, "TextField" | "SearchField");
            (!(text && slotted)).then_some(suggestion)
        }
    }
}

/// A control given as an item's `title` (`<ListItem title={<button …>}>`):
/// the SDK's own pattern, whose press area covers the whole item.
fn is_item_title(code: &str, at: usize) -> bool {
    code[..at].trim_end().strip_suffix('{').is_some_and(|h| h.trim_end().strip_suffix('=').is_some_and(|h| h.trim_end().ends_with("title")))
}

/// `{...f(` in a tag's attributes, for one of the names `fns`.
fn spreads(attrs: &str, fns: &[&str]) -> bool {
    let b = attrs.as_bytes();
    let mut from = 0;
    while let Some(p) = attrs[from..].find("{...") {
        let s = skip_ws(b, from + p + 4);
        from = s.max(from + p + 4);
        let mut j = s;
        while j < b.len() && (is_ident(b[j]) || b[j] == b'$') {
            j += 1;
        }
        if b.get(skip_ws(b, j)) == Some(&b'(') && fns.contains(&&attrs[s..j]) {
            return true;
        }
    }
    false
}

/// The value of attribute `name`: a string, written as one or as `{'…'}`.
fn attr_value(attrs: &str, name: &str) -> AttrValue {
    let b = attrs.as_bytes();
    let pat = format!("{name}=");
    let mut from = 0;
    while let Some(p) = attrs[from..].find(&pat) {
        let at = from + p;
        from = at + pat.len();
        if at > 0 && !b[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut v = from;
        if b.get(v) == Some(&b'{') {
            v = skip_ws(b, v + 1);
        }
        return match string_at(b, v) {
            Some((s, e, _)) => AttrValue::Static(attrs[s..e].to_ascii_lowercase()),
            None => AttrValue::Dynamic,
        };
    }
    AttrValue::Missing
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
