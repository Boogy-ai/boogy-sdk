//! An HTML file: its `<style>` blocks as stylesheets, its `style` attributes
//! as declaration lists. `code` has its comments blanked.

use crate::{css, lex, Lang, Raw};

pub(crate) fn check(code: &str, path: &str, out: &mut Vec<Raw>) {
    let ctx = css::Ctx::for_path(path);
    let lower = code.to_ascii_lowercase();
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(p) = lower[from..].find("<style") {
        let open = from + p;
        let Some(gt) = lower[open..].find('>') else { break };
        let body = open + gt + 1;
        let end = lower[body..].find("</style").map_or(code.len(), |e| body + e);
        // A `<style>` block's own comments are CSS comments.
        let block = &code[body..end];
        let block = lex::blank_comments(block, &lex::comments(block, Lang::Css));
        css::check_stylesheet(&block, body, &ctx, out);
        from = end;
    }
    for open in ["style=\"", "style='"] {
        let mut from = 0;
        while let Some(p) = lower[from..].find(open) {
            let at = from + p;
            from = at + open.len();
            // An attribute, not `data-style=` or `[style="…"]`.
            if at == 0 || !b[at - 1].is_ascii_whitespace() {
                continue;
            }
            let q = b[at + open.len() - 1];
            let end = b[from..].iter().position(|&c| c == q).map_or(b.len(), |e| from + e);
            css::check_declarations(&code[from..end], from, &ctx, out);
            from = end;
        }
    }
}
