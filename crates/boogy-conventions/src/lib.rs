//! Heuristic conventions lint for Boogy service source. Pure (no I/O):
//! `lint_file` scans one file, `route_findings` aggregates route/summary
//! annotation across a whole crate. Shared by `boogy check` (CLI) and the
//! builder MCP server's `check_service` tool. A lint, not a compiler — it
//! catches the egregious cases an agent would otherwise ship.
//!
//! Checks (all gate — any finding exits non-zero):
//!   1. raw-schema        — `Table::new(` / `create_table_from(`              (no escape)
//!   2. raw-store-crud    — `store::{insert,find,update,delete,get}` / `FindOptions`
//!                          without `// escape-hatch:`
//!   3. untyped-response  — `Json<serde_json::Value>` / `Created<…Value>`
//!                          without `// untyped-response:`
//!   4. unannotated-routes— more route registrations than `.summary(...)` calls  (no escape)
//!   5. multi-write-no-tx — ≥2 `db_{insert,update,delete}(` in one fn body without
//!                          `tx(`/`tx::<` and without `// independent-writes:`

/// `Hard` findings have no escape hatch; `Fail` findings can be suppressed with
/// a documented marker. Both gate the check (any finding → non-zero exit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Hard,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    pub file: String,
    /// 1-based line, or 0 for a file-level finding.
    pub line: usize,
    pub message: String,
    pub hint: &'static str,
}

/// Whether a line makes a raw single-row store CRUD call: `store::<m>(` for an
/// exact method name (mirrors the CI gate's `store::(insert|find|update|delete|
/// get)\s*\(`), or uses `FindOptions`. Crucially the method name must be
/// FOLLOWED by `(` — so the legitimate batch helpers `store::update_many(`,
/// `store::delete_where(`, `store::find_owned(` are NOT flagged.
fn line_has_raw_crud(line: &str) -> bool {
    if line.contains("FindOptions") {
        return true;
    }
    for m in ["insert", "find", "update", "delete", "get"] {
        let pat = format!("store::{m}");
        let mut from = 0;
        while let Some(rel) = line[from..].find(&pat) {
            let after = &line[from + rel + pat.len()..];
            if after.trim_start().starts_with('(') && !starts_with_ident_char(after) {
                return true;
            }
            from += rel + pat.len();
        }
    }
    false
}

/// True if the first char is part of an identifier (so `store::update_many`'s
/// `_` disqualifies the `update` match).
fn starts_with_ident_char(s: &str) -> bool {
    s.bytes().next().map(is_ident_char).unwrap_or(false)
}

/// Lint one source file. Pure (no I/O) so it is unit-testable.
pub fn lint_file(file: &str, src: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    // A `// marker:` counts whether it's on this line (trailing) or the line above.
    let marked = |i: usize, marker: &str| -> bool {
        lines[i].contains(marker) || (i > 0 && lines[i - 1].contains(marker))
    };

    for (i, line) in lines.iter().enumerate() {
        let ln = i + 1;

        // 1. raw table schema (HARD)
        if line.contains("Table::new(") || line.contains("create_table_from(") {
            out.push(Finding {
                check: "raw-schema",
                severity: Severity::Hard,
                file: file.into(),
                line: ln,
                message: "raw table schema — define the table with #[derive(Model)]".into(),
                hint: "Model the table as a `#[derive(Model)]` struct so indexes/access patterns are derived (boogy:boogy-data-modeling).",
            });
        }

        // 3. untyped HTTP response body
        let untyped_resp = line.contains("Json<serde_json::Value>")
            || line.contains("Json<json::Value>")
            || line.contains("Created<serde_json::Value>")
            || line.contains("Created<json::Value>");
        if untyped_resp && !marked(i, "// untyped-response:") {
            out.push(Finding {
                check: "untyped-response",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: "untyped response body — return a typed DTO, not a raw JSON value".into(),
                hint: "Return `Json<MyDto>` where MyDto derives Serialize + schemars::JsonSchema (so it appears in openapi.json), or mark `// untyped-response: <reason>` (boogy:boogy-rest-apis).",
            });
        }

        // 2. raw store CRUD without an escape hatch
        if line_has_raw_crud(line) && !marked(i, "// escape-hatch:") {
            out.push(Finding {
                check: "raw-store-crud",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: "raw store CRUD — prefer the Model API / declared access patterns".into(),
                hint: "Use the `#[derive(Model)]` query methods / access patterns, or mark `// escape-hatch: <reason>` (boogy:boogy-access-patterns).",
            });
        }
    }

    // 5. multi-write handlers without a transaction.
    out.extend(multi_write_findings(file, src));
    out
}

/// Aggregate route findings across ALL scanned files (a router can be split
/// across modules, so this is run-level, not per-file — matching the CI gate's
/// per-crate aggregation). Two findings are possible: more routes than
/// `.summary(...)` calls, and a router that never calls `Router::info(...)`.
pub fn route_findings(files: &[(String, String)]) -> Vec<Finding> {
    let mut routes = 0usize;
    let mut summaries = 0usize;
    let mut has_info = false;
    for (_, src) in files {
        routes += src.lines().filter(|l| line_registers_route(l)).count();
        summaries += src.lines().filter(|l| l.contains(".summary(")).count();
        if src.contains("Router::info(") || src.contains(".info(") {
            has_info = true;
        }
    }
    let mut out = Vec::new();
    // Only services that actually register HTTP routes owe summaries + info; a
    // service that mounts only MCP/RPC surfaces legitimately has neither.
    if routes == 0 {
        return out;
    }
    if routes > summaries {
        out.push(Finding {
            check: "unannotated-routes",
            severity: Severity::Hard,
            file: String::new(),
            line: 0,
            message: format!(
                "{routes} route(s) but {summaries} .summary(...) — {} route(s) un-annotated",
                routes - summaries
            ),
            hint: "Add `.summary(\"…\")` to each route so the service self-documents in openapi.json (boogy:boogy-api-specs).",
        });
    }
    if !has_info {
        out.push(Finding {
            check: "unannotated-routes",
            severity: Severity::Hard,
            file: String::new(),
            line: 0,
            message: "router never calls Router::info(...) — set the doc identity once".into(),
            hint: "Call `Router::info(name, version)` once so the generated openapi.json has an identity (boogy:boogy-api-specs).",
        });
    }
    out
}

/// Whether a line registers a route: a `.method(` call whose first argument is
/// a path string literal (`"/…`). Mirrors the CI gate's `\.(get|post|put|delete|
/// patch)\(\s*"/` so a map `.get(key)` or a non-path `.get(x)` isn't counted.
fn line_registers_route(line: &str) -> bool {
    for m in ["get", "post", "put", "delete", "patch"] {
        let pat = format!(".{m}(");
        let mut from = 0;
        while let Some(rel) = line[from..].find(&pat) {
            let after = line[from + rel + pat.len()..].trim_start();
            if after.starts_with("\"/") {
                return true;
            }
            from += rel + pat.len();
        }
    }
    false
}

/// Find functions that write ≥2 rows without an enclosing transaction. Mirrors
/// the CI gate: split into `fn` bodies by brace depth, count
/// `db_{insert,update,delete}(`, and require `tx(`/`tx::<` (atomicity) or
/// `// independent-writes:` (an explicit opt-out) when there are ≥2.
fn multi_write_findings(file: &str, src: &str) -> Vec<Finding> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(name) = fn_name(lines[i]) else {
            i += 1;
            continue;
        };
        let fn_line = i + 1;
        // Accumulate the body from the `fn` line until brace depth returns to 0.
        let mut depth: i32 = 0;
        let mut started = false;
        let mut body = String::new();
        let mut j = i;
        while j < lines.len() {
            for ch in lines[j].chars() {
                if ch == '{' {
                    depth += 1;
                    started = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            body.push_str(lines[j]);
            body.push('\n');
            if started && depth <= 0 {
                break;
            }
            j += 1;
        }

        let writes = count_writes(&body);
        if writes >= 2
            && !body.contains("tx(")
            && !body.contains("tx::<")
            && !body.contains("// independent-writes:")
        {
            out.push(Finding {
                check: "multi-write-no-tx",
                severity: Severity::Fail,
                file: file.into(),
                line: fn_line,
                message: format!("fn `{name}` writes {writes} rows without a transaction"),
                hint: "Treat the handler as one unit of work: wrap its writes in `tx(|| { … })` so ANY later error rolls back ALL of them — no partial state. Mark `// independent-writes: <reason>` only if the writes are genuinely unrelated (boogy:boogy-transactions).",
            });
        }
        i = j + 1;
    }
    out
}

/// Count `db_insert(` / `db_update(` / `db_delete(` write calls in a body.
fn count_writes(body: &str) -> usize {
    ["db_insert(", "db_update(", "db_delete("]
        .iter()
        .map(|p| body.matches(p).count())
        .sum()
}

/// Extract the function name from a line declaring `fn <name>`, requiring a word
/// boundary before `fn` so identifiers like `transform` don't match.
fn fn_name(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find("fn ") {
        let at = from + rel;
        let boundary = at == 0 || !is_ident_char(bytes[at - 1]);
        if boundary {
            let rest = line[at + 3..].trim_start();
            let name: String = rest.chars().take_while(|&c| is_ident_char(c as u8)).collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
        from = at + 3;
    }
    None
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(findings: &[Finding]) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = findings.iter().map(|f| f.check).collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn clean_service_has_no_findings() {
        let src = r#"
            impl Api for S {
                fn build_router() -> Router {
                    Router::new()
                        .info("svc", "1.0")
                        .get("/items", list).summary("list items")
                        .post("/items", create).summary("create item")
                }
            }
            fn create(req: &mut Req) -> Json<ItemDto> {
                tx(|| { item.db_insert()?; tag.db_insert()?; Ok(()) })?;
                Json(dto)
            }
        "#;
        assert!(lint_file("lib.rs", src).is_empty());
    }

    #[test]
    fn flags_raw_schema_hard() {
        let f = lint_file("lib.rs", "let t = Table::new(\"items\");");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "raw-schema");
        assert_eq!(f[0].severity, Severity::Hard);
        assert!(lint_file("lib.rs", "create_table_from(&schema);")
            .iter()
            .any(|x| x.check == "raw-schema"));
    }

    #[test]
    fn raw_store_crud_suppressed_by_escape_hatch() {
        let bad = "let r = store::find(opts);";
        assert!(checks(&lint_file("lib.rs", bad)).contains(&"raw-store-crud"));
        // Marker on the line above suppresses it.
        let ok = "// escape-hatch: legacy migration scan\nlet r = store::find(opts);";
        assert!(!checks(&lint_file("lib.rs", ok)).contains(&"raw-store-crud"));
        // Trailing marker on the same line also suppresses.
        let ok2 = "let r = store::find(opts); // escape-hatch: one-off";
        assert!(!checks(&lint_file("lib.rs", ok2)).contains(&"raw-store-crud"));
    }

    #[test]
    fn batch_store_helpers_are_not_raw_crud() {
        // These are legitimate distinct helpers, not the single-row CRUD the
        // check targets — the method name is a prefix, not the whole call.
        for ok in [
            "store::update_many(t, &rows)?;",
            "store::delete_many(t, &keys)?;",
            "store::delete_where(Edge::TABLE, pred)?;",
            "store::find_owned::<T>(p)?;",
            "store::get_or_init(x);",
        ] {
            assert!(
                !checks(&lint_file("lib.rs", ok)).contains(&"raw-store-crud"),
                "false positive on: {ok}",
            );
        }
        // The exact single-row calls still flag.
        assert!(checks(&lint_file("lib.rs", "store::insert(row)?;")).contains(&"raw-store-crud"));
        assert!(checks(&lint_file("lib.rs", "store::delete(t, k)?;")).contains(&"raw-store-crud"));
    }

    #[test]
    fn flags_untyped_response_unless_marked() {
        let bad = "fn h() -> Json<serde_json::Value> { Json(json!({})) }";
        assert!(checks(&lint_file("lib.rs", bad)).contains(&"untyped-response"));
        let ok = "// untyped-response: proxying upstream shape\nfn h() -> Json<serde_json::Value> { x }";
        assert!(!checks(&lint_file("lib.rs", ok)).contains(&"untyped-response"));
    }

    fn files(src: &str) -> Vec<(String, String)> {
        vec![("lib.rs".to_string(), src.to_string())]
    }

    #[test]
    fn flags_more_routes_than_summaries() {
        let two_routes_one_summary = r#"
            Router::new()
                .info("svc", "1.0")
                .get("/a", a).summary("a")
                .get("/b", b)
        "#;
        let f = route_findings(&files(two_routes_one_summary));
        assert_eq!(f.len(), 1, "exactly one un-annotated route");
        assert_eq!(f[0].check, "unannotated-routes");
        assert!(f[0].message.contains("1 route(s) un-annotated"));
        // Fully annotated + info → no finding.
        let ok = r#"Router::new().info("s","1").get("/a", a).summary("a").get("/b", b).summary("b")"#;
        assert!(route_findings(&files(ok)).is_empty());
    }

    #[test]
    fn flags_router_without_info() {
        let no_info = r#"Router::new().get("/a", a).summary("a")"#;
        let f = route_findings(&files(no_info));
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("Router::info"));
    }

    #[test]
    fn route_check_split_across_files_is_not_a_false_positive() {
        // Routes in one module, Router::info + summaries in another — the
        // run-level aggregation must see them together.
        let routes = ("routes.rs".to_string(), "g.get(\"/a\", a).summary(\"a\")\n".to_string());
        let info = ("app.rs".to_string(), "Router::info(\"svc\", \"1.0\")\n".to_string());
        assert!(route_findings(&[routes, info]).is_empty());
    }

    #[test]
    fn mcp_only_service_owes_no_route_annotations() {
        // No `.method("/...)` registrations → no summary/info requirement.
        let mcp = ("lib.rs".to_string(), "Router::new().mcp(\"/mcp\", handler)\n".to_string());
        assert!(route_findings(&[mcp]).is_empty());
    }

    #[test]
    fn flags_multi_write_without_tx() {
        let bad = r#"
            fn transfer(req: &mut Req) -> Json<Ok> {
                from.db_update()?;
                to.db_update()?;
                Json(ok)
            }
        "#;
        let findings = lint_file("lib.rs", bad);
        let mw: Vec<&Finding> = findings.iter().filter(|x| x.check == "multi-write-no-tx").collect();
        assert_eq!(mw.len(), 1, "two db writes with no tx must flag");
        assert!(mw[0].message.contains("transfer"));
    }

    #[test]
    fn multi_write_satisfied_by_tx_or_marker() {
        let with_tx = r#"
            fn f() {
                tx(|| { a.db_insert()?; b.db_insert()?; Ok(()) })?;
            }
        "#;
        assert!(!checks(&lint_file("lib.rs", with_tx)).contains(&"multi-write-no-tx"));

        let with_marker = r#"
            fn f() {
                // independent-writes: two unrelated audit logs
                a.db_insert()?;
                b.db_insert()?;
            }
        "#;
        assert!(!checks(&lint_file("lib.rs", with_marker)).contains(&"multi-write-no-tx"));

        // A single write is fine.
        let single = "fn f() { a.db_insert()?; }";
        assert!(!checks(&lint_file("lib.rs", single)).contains(&"multi-write-no-tx"));
    }

    #[test]
    fn fn_name_requires_word_boundary() {
        assert_eq!(fn_name("fn create(x: i32) {"), Some("create".to_string()));
        assert_eq!(fn_name("    pub async fn handle() {"), Some("handle".to_string()));
        // `transform` contains "fn" but not as the `fn ` keyword.
        assert_eq!(fn_name("let transform = 1;"), None);
    }
}
