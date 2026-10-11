//! The lexical layer every rule stands on: line numbers, comment ranges,
//! comment blanking (so a rule never sees text inside a comment, while byte
//! offsets and line numbers stay those of the original), and waivers.

use crate::Lang;

/// Line numbers for the byte offsets of one file.
pub(crate) struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    pub(crate) fn new(src: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(src.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
        Lines { starts }
    }

    /// The 1-based line holding byte `at`.
    pub(crate) fn line_of(&self, at: usize) -> usize {
        self.starts.partition_point(|&s| s <= at)
    }
}

pub(crate) fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The byte ranges of every comment in `src`, whole (delimiters included).
/// A comment inside a string is not a comment. An unterminated comment runs
/// to the end of the file.
pub(crate) fn comments(src: &str, lang: Lang) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let script = matches!(lang, Lang::Script { .. });
    let mut out = Vec::new();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            // A quote never spans a line, except a template literal's.
            if c == q || (c == b'\n' && q != b'`') {
                quote = None;
            }
            i += 1;
            continue;
        }
        let (open, close): (&[u8], &[u8]) = match lang {
            Lang::Html => (b"<!--", b"-->"),
            _ => (b"/*", b"*/"),
        };
        if b[i..].starts_with(open) {
            let end = find(b, i + open.len(), close).map(|e| e + close.len()).unwrap_or(b.len());
            out.push((i, end));
            i = end;
            continue;
        }
        if script && b[i..].starts_with(b"//") {
            let end = find(b, i, b"\n").unwrap_or(b.len());
            out.push((i, end));
            i = end;
            continue;
        }
        let quotes: &[u8] = match lang {
            Lang::Html => b"",
            Lang::Css => b"\"'",
            Lang::Script { .. } => b"\"'`",
        };
        if quotes.contains(&c) {
            quote = Some(c);
        }
        i += 1;
    }
    out
}

/// `src` with every comment byte but a newline replaced by a space. Comment
/// delimiters are ASCII, so whole characters are replaced and the result is
/// valid UTF-8 with the same byte offsets.
pub(crate) fn blank_comments(src: &str, ranges: &[(usize, usize)]) -> String {
    let mut out = src.as_bytes().to_vec();
    for &(s, e) in ranges {
        for byte in &mut out[s..e] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(out).expect("comment ranges start and end on ASCII delimiters")
}

pub(crate) struct ParsedWaiver {
    pub rule: String,
    pub line: usize,
    pub reason: String,
}

pub(crate) struct BadWaiver {
    pub line: usize,
    pub message: String,
}

const MARK: &str = "boogy-allow";

/// Every waiver in the file's comments: `boogy-allow <rule>: <reason>`, the
/// reason running to the end of its line or comment. A malformed one is
/// returned apart, as a finding to report.
pub(crate) fn waivers(src: &str, ranges: &[(usize, usize)], lines: &Lines) -> (Vec<ParsedWaiver>, Vec<BadWaiver>) {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for &(s, e) in ranges {
        let text = &src[s..e];
        let mut from = 0;
        while let Some(p) = text[from..].find(MARK) {
            let start = from + p;
            from = start + MARK.len();
            let line = lines.line_of(s + start);
            let rest = text[from..].split('\n').next().unwrap_or("").trim();
            let rest = rest.strip_suffix("*/").or_else(|| rest.strip_suffix("-->")).unwrap_or(rest).trim();
            let Some((rule, reason)) = rest.split_once(':') else {
                bad.push(BadWaiver { line, message: "write a waiver as `boogy-allow <rule>: <reason>`".into() });
                continue;
            };
            let (rule, reason) = (rule.trim(), reason.trim());
            if !crate::is_rule(rule) {
                bad.push(BadWaiver { line, message: format!("`{rule}` is not a rule; the rules are {}", crate::rule_list()) });
            } else if reason.is_empty() {
                bad.push(BadWaiver { line, message: format!("the waiver for `{rule}` gives no reason") });
            } else {
                good.push(ParsedWaiver { rule: rule.to_string(), line, reason: reason.to_string() });
            }
        }
    }
    (good, bad)
}
