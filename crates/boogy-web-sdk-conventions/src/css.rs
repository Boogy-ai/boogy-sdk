//! Stylesheets, declaration lists and single declaration values. The input
//! has its comments blanked; `base` is its byte offset in the file, so every
//! finding is located in the file, not in the fragment.

use crate::Raw;

pub(crate) struct Ctx {
    /// The file is `theme.css`, whose job is to hold fixed values: there a
    /// custom-property definition may be a fixed size.
    pub theme: bool,
}

impl Ctx {
    pub(crate) fn for_path(path: &str) -> Self {
        Ctx { theme: path.rsplit('/').next() == Some("theme.css") }
    }
}

pub(crate) fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes().get(..prefix.len()).is_some_and(|h| h.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// The index just past the string opening at `i` (a quote byte).
pub(crate) fn skip_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == q {
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// A stylesheet: rules, at-rules and declarations, nested or not.
pub(crate) fn check_stylesheet(css: &str, base: usize, ctx: &Ctx, out: &mut Vec<Raw>) {
    let b = css.as_bytes();
    // One entry per open block: what its declarations are.
    let mut blocks: Vec<Block> = Vec::new();
    let mut start = 0;
    let mut paren = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                i = skip_string(b, i);
                continue;
            }
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'{' if paren == 0 => {
                let prelude = &css[start..i];
                let lead = start + (prelude.len() - prelude.trim_start().len());
                let p = prelude.trim();
                if starts_with_ci(p, "@media") && on_window_size(p) {
                    let shown: String = p.split_whitespace().collect::<Vec<_>>().join(" ");
                    out.push(Raw { rule: "layout-media-query", at: base + lead, message: format!("`{shown}` adapts the layout to the window's size") });
                }
                blocks.push(if starts_with_ci(p, "@property") {
                    Block::Property
                } else if p.split(',').any(|sel| matches!(sel.trim(), ":root" | "html")) {
                    Block::Root
                } else {
                    Block::Rule
                });
                start = i + 1;
            }
            c @ (b';' | b'}') if paren == 0 => {
                declaration(css, start, i, base, blocks.last().copied().unwrap_or(Block::Rule), ctx, out);
                if c == b'}' {
                    blocks.pop();
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < b.len() {
        declaration(css, start, b.len(), base, blocks.last().copied().unwrap_or(Block::Rule), ctx, out);
    }
}

/// A declaration list with no braces: an HTML `style` attribute, or the
/// string a script assigns to `cssText`.
pub(crate) fn check_declarations(decls: &str, base: usize, ctx: &Ctx, out: &mut Vec<Raw>) {
    check_stylesheet(decls, base, ctx, out);
}

fn on_window_size(prelude: &str) -> bool {
    let p = prelude.to_ascii_lowercase();
    ["width", "height", "aspect-ratio", "orientation"].iter().any(|f| p.contains(f))
}

/// What a block's declarations are.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Block {
    /// An ordinary rule.
    Rule,
    /// A `:root` (or `html`) rule: a custom property defined here is a named,
    /// global token.
    Root,
    /// `@property`: every declaration defines.
    Property,
}

fn declaration(css: &str, from: usize, to: usize, base: usize, block: Block, ctx: &Ctx, out: &mut Vec<Raw>) {
    let text = &css[from..to];
    let Some(colon) = text.find(':') else { return };
    let prop = text[..colon].trim();
    if prop.is_empty() || !prop.bytes().all(is_ident) {
        return;
    }
    check_value(prop, &text[colon + 1..], base + from + colon + 1, block, ctx, out);
}

/// The units of a fixed length. `em` and `ch` are not here: they are relative
/// to the element's own type size, which the page scale and the board's zoom
/// move with every token.
const UNITS: &[&str] = &["px", "rem", "pt", "vw", "vh", "vmin", "vmax", "dvw", "dvh", "svw", "svh", "lvw", "lvh"];

/// The viewport units, of which exactly 100 is the whole viewport: in a board,
/// the whole pane, which is what a layout that fills it means.
const VIEWPORT_UNITS: &[&str] = &["vw", "vh", "vmin", "vmax", "dvw", "dvh", "svw", "svh", "lvw", "lvh"];

/// One declaration's value. `prop` is its property in CSS or camelCase form;
/// `block` is the kind of block it is declared in.
pub(crate) fn check_value(prop: &str, value: &str, at: usize, block: Block, ctx: &Ctx, out: &mut Vec<Raw>) {
    let custom = block == Block::Property || prop.starts_with("--");
    // A named token: a custom property defined in theme.css, or on the root.
    let named_token = custom && (ctx.theme || block != Block::Rule);
    // A size multiplied by `--u-zoom` follows the board's Smaller/Larger:
    // the fluid form the SDK's guidance gives for a size no token fits.
    let follows_zoom = value.contains("var(--u-zoom)");
    if !named_token && !follows_zoom {
        for (off, len) in lengths(value) {
            out.push(Raw { rule: "fixed-size", at: at + off, message: format!("`{len}` is a fixed size") });
        }
    }
    if !custom {
        if let Some(off) = value.find("var(--u)") {
            out.push(Raw { rule: "unit-multiple", at: at + off, message: "a one-off multiple of `--u` in a rule".into() });
        }
        // A colour in a mask says only "opaque": it never shows.
        let mask = prop.to_ascii_lowercase().trim_start_matches("-webkit-").starts_with("mask");
        for (off, colour) in if mask { Vec::new() } else { colours(value, colour_property(prop)) } {
            out.push(Raw { rule: "colour-literal", at: at + off, message: format!("`{colour}` is a literal colour") });
        }
    }
}

/// Each fixed length in a value: its offset and its text. Skips strings and
/// `url(...)`; a zero of any unit, and `1px` or less, are fine.
pub(crate) fn lengths(value: &str) -> Vec<(usize, String)> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            i = skip_string(b, i);
            continue;
        }
        if b.get(i..i + 4).is_some_and(|w| w.eq_ignore_ascii_case(b"url(")) && (i == 0 || !is_ident(b[i - 1])) {
            i = b[i..].iter().position(|&x| x == b')').map_or(b.len(), |p| i + p + 1);
            continue;
        }
        let number = c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit));
        if !number {
            i += 1;
            continue;
        }
        let mut s = i;
        if s > 0 && matches!(b[s - 1], b'-' | b'+') {
            s -= 1;
        }
        // `col-12px`, `#1e3`, `a.5px`: part of a name, a colour or a word.
        let in_word = s > 0 && (is_ident(b[s - 1]) || matches!(b[s - 1], b'#' | b'.'));
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
            j += 1;
        }
        let unit_at = j;
        while j < b.len() && b[j].is_ascii_alphabetic() {
            j += 1;
        }
        let run_on = j < b.len() && is_ident(b[j]);
        let unit = value[unit_at..j].to_ascii_lowercase();
        if !in_word && !run_on && UNITS.contains(&unit.as_str()) {
            let n: f64 = value[i..unit_at].parse().unwrap_or(0.0);
            let whole_viewport = n == 100.0 && VIEWPORT_UNITS.contains(&unit.as_str());
            if n != 0.0 && !(unit == "px" && n <= 1.0) && !whole_viewport {
                out.push((s, value[s..j].to_string()));
            }
        }
        i = j.max(i + 1);
    }
    out
}

const COLOUR_FUNCTIONS: &[&str] = &["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color"];

/// The CSS named colours. `transparent` and `currentColor` are keywords, not
/// literals, and are not here.
const NAMED: &[&str] = &[
    "aliceblue", "antiquewhite", "aqua", "aquamarine", "azure", "beige", "bisque", "black", "blanchedalmond", "blue",
    "blueviolet", "brown", "burlywood", "cadetblue", "chartreuse", "chocolate", "coral", "cornflowerblue", "cornsilk",
    "crimson", "cyan", "darkblue", "darkcyan", "darkgoldenrod", "darkgray", "darkgreen", "darkgrey", "darkkhaki",
    "darkmagenta", "darkolivegreen", "darkorange", "darkorchid", "darkred", "darksalmon", "darkseagreen",
    "darkslateblue", "darkslategray", "darkslategrey", "darkturquoise", "darkviolet", "deeppink", "deepskyblue",
    "dimgray", "dimgrey", "dodgerblue", "firebrick", "floralwhite", "forestgreen", "fuchsia", "gainsboro",
    "ghostwhite", "gold", "goldenrod", "gray", "green", "greenyellow", "grey", "honeydew", "hotpink", "indianred",
    "indigo", "ivory", "khaki", "lavender", "lavenderblush", "lawngreen", "lemonchiffon", "lightblue", "lightcoral",
    "lightcyan", "lightgoldenrodyellow", "lightgray", "lightgreen", "lightgrey", "lightpink", "lightsalmon",
    "lightseagreen", "lightskyblue", "lightslategray", "lightslategrey", "lightsteelblue", "lightyellow", "lime",
    "limegreen", "linen", "magenta", "maroon", "mediumaquamarine", "mediumblue", "mediumorchid", "mediumpurple",
    "mediumseagreen", "mediumslateblue", "mediumspringgreen", "mediumturquoise", "mediumvioletred", "midnightblue",
    "mintcream", "mistyrose", "moccasin", "navajowhite", "navy", "oldlace", "olive", "olivedrab", "orange",
    "orangered", "orchid", "palegoldenrod", "palegreen", "paleturquoise", "palevioletred", "papayawhip", "peachpuff",
    "peru", "pink", "plum", "powderblue", "purple", "rebeccapurple", "red", "rosybrown", "royalblue", "saddlebrown",
    "salmon", "sandybrown", "seagreen", "seashell", "sienna", "silver", "skyblue", "slateblue", "slategray",
    "slategrey", "snow", "springgreen", "steelblue", "tan", "teal", "thistle", "tomato", "turquoise", "violet",
    "wheat", "white", "whitesmoke", "yellow", "yellowgreen",
];

/// Whether a property takes a colour, so a bare word in it can be a named
/// colour. `backgroundColor` and `background-color` both qualify.
pub(crate) fn colour_property(prop: &str) -> bool {
    let p = prop.to_ascii_lowercase().replace('-', "");
    p.contains("color")
        || p.starts_with("border")
        || p.starts_with("outline")
        || p.starts_with("columnrule")
        || p.starts_with("textdecoration")
        || p.starts_with("textemphasis")
        || matches!(p.as_str(), "background" | "fill" | "stroke" | "boxshadow" | "textshadow")
}

/// Each literal colour in a value: hex, a colour function, or (when `named`)
/// a named colour. Skips strings and `url(...)`.
pub(crate) fn colours(value: &str, named: bool) -> Vec<(usize, String)> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            i = skip_string(b, i);
            continue;
        }
        if c == b'#' && (i == 0 || !is_ident(b[i - 1])) {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if matches!(j - i - 1, 3 | 4 | 6 | 8) && !(j < b.len() && is_ident(b[j])) {
                out.push((i, value[i..j].to_string()));
            }
            i = j.max(i + 1);
            continue;
        }
        if is_ident(c) && (i == 0 || !is_ident(b[i - 1])) {
            let mut j = i;
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
            let word = value[i..j].to_ascii_lowercase();
            if b.get(j) == Some(&b'(') {
                if word == "url" {
                    i = b[j..].iter().position(|&x| x == b')').map_or(b.len(), |p| j + p + 1);
                    continue;
                }
                if COLOUR_FUNCTIONS.contains(&word.as_str()) {
                    let close = b[j..].iter().position(|&x| x == b')').map_or(b.len(), |p| j + p + 1);
                    out.push((i, value[i..close].to_string()));
                    i = close;
                    continue;
                }
            } else if named && NAMED.contains(&word.as_str()) {
                out.push((i, value[i..j].to_string()));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}
