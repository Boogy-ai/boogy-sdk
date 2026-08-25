//! retired-spelling-file: this crate now HOSTS the `counter-field` check, which
//! exists to reject the retired `#[counter]` struct-field form — so the dead
//! spelling appears here in the detector, its message and its tests. What is
//! true now: `#[model(counter(name = "..."))]` on the struct plus a
//! `#[derive(Counter)]` marker type.
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
//!   6. counter-read-in-tx — a snapshot `<Counter>::get(` AND a `<Counter>::add(`
//!                          for the SAME counter in one `tx(` body, without
//!                          `// counter-read-display-only:`

/// Every `Finding::check` id this crate can emit.
///
/// The one producer. `boogy check` groups findings under human-readable titles
/// from its own ORDER list, and a check missing from that list was COUNTED and
/// never PRINTED — the run said "1 issue" and then said nothing about it, which
/// is how `counter-read-in-tx` shipped invisible for its first hour. The CLI
/// asserts its list covers this one exactly.
pub const CHECKS: &[&str] = &[
    "raw-schema",
    "raw-store-crud",
    "untyped-response",
    "unannotated-routes",
    "router-no-info",
    "multi-write-no-tx",
    "counter-read-in-tx",
    "counter-field",
    "legacy-init-tables",
    "hardcoded-index-name",
];

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

/// Whether `lines[i]` or the line above carries `marker` FOLLOWED BY a reason.
///
/// Distinct from `lint_file`'s `marked()`, which only asks whether the marker is
/// present. A bare `// reconcile-exempt:` suppresses nothing: a marker is the
/// one place an author can write anything and be believed, so an empty one is a
/// suppression with no argument behind it.
fn marked_with_reason(lines: &[&str], i: usize, marker: &str) -> bool {
    let has = |l: &str| {
        l.split(marker).nth(1).map(|tail| !tail.trim().is_empty()).unwrap_or(false)
    };
    has(lines[i]) || (i > 0 && has(lines[i - 1]))
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
        // Run violation checks on the CODE part only — the text before a `//`
        // line comment — so a doc comment mentioning `Table::new(...)` isn't
        // flagged. Markers (`// escape-hatch:` etc.) are still matched on the
        // full line by `marked()`. (Heuristic: doesn't account for `//` inside
        // a string literal — rare for these tokens.)
        let code = line.split("//").next().unwrap_or(line);

        // 1. raw table schema
        //
        // `// dynamic-schema: <reason>` is the DECLARED exception, for a table
        // whose name or shape is only known at runtime — there is no
        // `#[derive(Model)]` that can express one. The internal gate has
        // honoured this marker all along; `boogy check` did not, so a developer
        // with a legitimate dynamic table was told to do something impossible
        // and had no way out. Found 2026-08-25 while converging the two gates.
        if (code.contains("Table::new(") || code.contains("create_table_from("))
            && !marked_with_reason(&lines, i, "// dynamic-schema:")
        {
            out.push(Finding {
                check: "raw-schema",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: "raw table schema — define the table with #[derive(Model)]".into(),
                hint: "Model the table as a `#[derive(Model)]` struct so indexes/access patterns are derived (boogy:boogy-data-modeling).",
            });
        }

        // 3. untyped HTTP response body
        let untyped_resp = code.contains("Json<serde_json::Value>")
            || code.contains("Json<json::Value>")
            || code.contains("Created<serde_json::Value>")
            || code.contains("Created<json::Value>");
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
        if line_has_raw_crud(code) && !marked(i, "// escape-hatch:") {
            out.push(Finding {
                check: "raw-store-crud",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: "raw store CRUD — prefer the Model API / declared access patterns".into(),
                hint: "Use the `#[derive(Model)]` query methods / access patterns, or mark `// escape-hatch: <reason>` (boogy:boogy-access-patterns).",
            });
        }
        // 6. retired `#[counter]` field attribute (HARD — the derive rejects it)
        if code.trim() == "#[counter]" {
            out.push(Finding {
                check: "counter-field",
                severity: Severity::Hard,
                file: file.into(),
                line: ln,
                message: "`#[counter]` on a field is a retired form".into(),
                hint: "Declare the counter on the STRUCT — `#[model(counter(name = \"<column>\"))]` — and read or add it through a `#[derive(Counter)]` marker type. There is no backing field (boogy:boogy-counters).",
            });
        }

        // 8. hardcoded index-name literal in a low-level cursor call
        let cursor_call = code.contains("for_each_batch(") || code.contains("open_cursor(");
        let index_literal = code.contains("\"ix_") || code.contains("\"idx_");
        if cursor_call && index_literal && !marked_with_reason(&lines, i, "// index-name-ok:") {
            out.push(Finding {
                check: "hardcoded-index-name",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: "hardcoded index name — index names are schema-canonical and a literal drifts silently".into(),
                hint: "Use the query DSL and let the planner choose the index by COLUMNS; if the low-level cursor is genuinely required, mark `// index-name-ok: <reason>` (boogy:boogy-access-patterns).",
            });
        }

        // 7. legacy init_tables / out-of-model index
        let legacy_init = code.contains("fn init_tables");
        let hand_index =
            code.contains("create_index(") && (code.contains("\"ix_") || code.contains("\"idx_"));
        if (legacy_init || hand_index) && !marked_with_reason(&lines, i, "// reconcile-exempt:") {
            out.push(Finding {
                check: "legacy-init-tables",
                severity: Severity::Fail,
                file: file.into(),
                line: ln,
                message: if legacy_init {
                    "`init_tables` is replaced by a declared schema + migrate + bootstrap".into()
                } else {
                    "hand-created index — declare the access pattern on the model instead".into()
                },
                hint: "Declare indexes through the model's access-pattern verbs (`list_by` / `ranked_by` / `lookup_by` / `tagged_by`) so the planner and the schema cannot disagree. Mark `// reconcile-exempt: <reason>` only for a migration-time backfill (boogy:boogy-migrations).",
            });
        }
    }

    // 5. multi-write handlers without a transaction.
    out.extend(multi_write_findings(file, src));
    out
}

/// Counter handles a crate declares: `#[counter(...)]` immediately above a
/// `struct <Name>`.
///
/// Discovered rather than pattern-matched on the call site, because the call
/// site is `RoomPostCount::get(store, id)` and a bare `::get(` matches most of
/// a Rust program. The derive is routinely aliased
/// (`use boogy_sdk::Counter as CounterDerive`), so the ATTRIBUTE is the stable
/// marker, not the derive name.
fn counter_handles(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (_, src) in files {
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[counter(") {
                continue;
            }
            // The struct may not be the very next line (further attributes, or
            // a doc comment between). Look ahead a few lines for the decl.
            for probe in lines.iter().skip(i + 1).take(4) {
                if let Some(name) = struct_name(probe) {
                    out.push(name);
                    break;
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether `body` reads counter `h` at SNAPSHOT isolation.
///
/// Two call forms, because the SDK has two and a service may use either:
///
///  * the typed handle — `LinkClicks::get(..)`, snapshot by definition
///    (`get_for_update` is the serializable one and cannot match, since `(` must
///    follow `get` immediately);
///  * the store verb — `counter_get(LinkClicks::NAME, &[..], true)` / the
///    `max_get` twin, where the trailing bool IS the isolation. This is the form
///    the examples actually write, so a check that matched only the typed handle
///    would have been decorative.
///
/// The bool is read from the call text up to the next `;`. A call passing
/// `false` has paid for the read-conflict range and is the documented escape, so
/// it never matches.
fn snapshot_reads(body: &str, handle: &str) -> bool {
    if body.contains(&format!("{handle}::get(")) {
        return true;
    }
    let name_const = format!("{handle}::NAME");
    for verb in ["counter_get(", "max_get("] {
        let mut from = 0;
        while let Some(rel) = body[from..].find(verb) {
            let at = from + rel;
            let end = body[at..].find(';').map(|e| at + e).unwrap_or(body.len());
            let call = &body[at..end];
            if call.contains(&name_const) && !call.contains("false") {
                return true;
            }
            from = at + verb.len();
        }
    }
    false
}

/// Whether `body` writes counter `h` — the typed handle or the store verb.
fn writes_counter(body: &str, handle: &str) -> bool {
    let name_const = format!("{handle}::NAME");
    body.contains(&format!("{handle}::add("))
        || body.contains(&format!("{handle}::observe("))
        || [("counter_add(", ()), ("max_observe(", ())].iter().any(|(verb, _)| {
            let mut from = 0;
            while let Some(rel) = body[from..].find(verb) {
                let at = from + rel;
                let end = body[at..].find(';').map(|e| at + e).unwrap_or(body.len());
                if body[at..end].contains(&name_const) {
                    return true;
                }
                from = at + verb.len();
            }
            false
        })
}

/// Extract `<Name>` from a line declaring `struct <Name>`, with a word boundary
/// before `struct` so `pub struct` matches and `my_struct` does not.
fn struct_name(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find("struct ") {
        let at = from + rel;
        if at == 0 || !is_ident_char(bytes[at - 1]) {
            let rest = line[at + 7..].trim_start();
            let name: String = rest.chars().take_while(|&c| is_ident_char(c as u8)).collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
        from = at + 7;
    }
    None
}

/// Check 6: reading a counter at SNAPSHOT and writing that same counter inside
/// one transaction.
///
/// **This predicts a runtime refusal.** Since 2026-08-24 the store refuses the
/// write outright (`ERR_COUNTER_WRITE_AFTER_READ`, guarantee-audit §1ap): a
/// snapshot read takes no read-conflict range, so anything decided from it is
/// serialized against nobody — two transactions read a counter one below its
/// limit, both decide there is room, both commit, and the limit is breached
/// with no error anywhere. The check exists so an author meets that in their
/// own loop, before a deploy, rather than on a live request.
///
/// It fires only when BOTH halves are present for the SAME counter, which is
/// exactly the store's condition. A read alone is fine and a write alone is the
/// normal case; neither is flagged.
///
/// Run-level, not per-file, for the same reason `route_findings` is: the handle
/// is declared in `models.rs` and used in `lib.rs`.
///
/// `get_for_update(` cannot match `::get(` — the `(` is required immediately
/// after `get` — so the documented remedy never trips the check that names it.
pub fn counter_findings(files: &[(String, String)]) -> Vec<Finding> {
    let handles = counter_handles(files);
    if handles.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (file, src) in files {
        for (name, fn_line, body) in fn_bodies(src) {
            if !body.contains("tx(") && !body.contains("tx::<") {
                continue;
            }
            if body.contains("// counter-read-display-only:") {
                continue;
            }
            for h in &handles {
                if snapshot_reads(&body, h) && writes_counter(&body, h) {
                    out.push(Finding {
                        check: "counter-read-in-tx",
                        severity: Severity::Fail,
                        file: file.clone(),
                        line: fn_line,
                        message: format!(
                            "fn `{name}` reads counter `{h}` at snapshot and writes it in the \
                             same transaction — the store REFUSES this at runtime"
                        ),
                        hint: "A snapshot read takes no read-conflict range, so a decision made from it is serialized against nobody: two callers read the same number, both decide there is room for one more, and both commit. Use `::get_for_update(..)` if you are deciding with the value, read it outside the transaction if you only wanted to report it, or mark `// counter-read-display-only: <reason>` (boogy:boogy-counters).",
                    });
                    break;
                }
            }
        }
    }
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
    let mut out = Vec::new();
    for (name, fn_line, body) in fn_bodies(src) {
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
    }
    out
}

/// Split a source file into `(fn name, 1-based fn line, body)` triples by brace
/// depth. The one producer — checks 5 and 6 both scan handler bodies, and a
/// second copy of this loop is a second place for the two to disagree about
/// what a body is.
fn fn_bodies(src: &str) -> Vec<(String, usize, String)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(name) = fn_name(lines[i]) else {
            i += 1;
            continue;
        };
        let fn_line = i + 1;
        // Accumulate from the `fn` line until brace depth returns to 0.
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
        out.push((name, fn_line, body));
        i = j + 1;
    }
    out
}

/// Count store-write calls in a handler body.
///
/// The vocabulary must cover EVERY way a handler can write, not just the
/// generated `db_*` helpers. It previously listed only `db_insert`/`db_update`/
/// `db_delete`, so a handler doing one `db_insert` plus one `upsert_increment`
/// — the shape the transactions guidance uses as its own worked example for a
/// dependent counter — counted as ONE write and passed the multi-write check
/// clean. The raw `store::*` forms were invisible for the same reason.
///
/// Reads (`find*`, `get*`, `scan*`, `count*`) must never appear here: counting
/// a read as a write would demand a transaction around read-only handlers.
fn count_writes(body: &str) -> usize {
    const WRITES: &[&str] = &[
        // generated per-model helpers
        "db_insert(",
        "db_update(",
        "db_delete(",
        // dependent-counter helper — a write, and the one this check missed
        "upsert_increment(",
        // raw store API
        "store::insert(",
        "store::insert_many(",
        "store::update(",
        "store::update_many(",
        "store::update_where(",
        "store::delete(",
        "store::delete_many(",
        "store::delete_where(",
    ];
    WRITES.iter().map(|p| body.matches(p).count()).sum()
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

    // -- check 6: counter-read-in-tx (guarantee-audit §1ap) ----------------

    /// The two files a real service splits this across.
    fn counter_crate(handler: &str) -> Vec<(String, String)> {
        vec![
            (
                "models.rs".to_string(),
                "#[derive(CounterDerive)]\n#[counter(of = Room, name = \"taken\")]\npub struct RoomTaken;\n"
                    .to_string(),
            ),
            ("lib.rs".to_string(), handler.to_string()),
        ]
    }

    #[test]
    fn a_dynamic_schema_marker_permits_a_runtime_table() {
        // A table whose name is a request parameter has no `#[derive(Model)]`
        // that could express it. The internal gate has always allowed this with
        // a stated reason; the shipped check refused it outright.
        let f = lint_file(
            "lib.rs",
            "    // dynamic-schema: table name is a request parameter, known only now.\n    create_table_from(&Table::new(&t).text(\"label\"));\n",
        );
        assert!(f.is_empty(), "the declared exception must be honoured: {f:?}");
    }

    #[test]
    fn raw_schema_with_no_marker_is_still_flagged() {
        // The control — the escape must not blunt the check.
        let f = lint_file("lib.rs", "    create_table_from(&Table::new(\"rooms\").text(\"slug\"));\n");
        assert_eq!(f.len(), 1, "an unmarked raw table must still be flagged: {f:?}");
        assert_eq!(f[0].check, "raw-schema");
    }

    #[test]
    fn a_dynamic_schema_marker_with_no_reason_does_not_suppress() {
        let f = lint_file("lib.rs", "    // dynamic-schema:\n    create_table_from(&Table::new(&t));\n");
        assert_eq!(f.len(), 1, "an empty marker must not suppress: {f:?}");
    }

    #[test]
    fn a_hardcoded_index_name_in_a_cursor_call_is_flagged() {
        // Index names are schema-canonical. A literal drifts the moment the
        // model's declared access patterns change, and nothing objects.
        let f = lint_file("lib.rs", "    for_each_batch(\"rooms\", \"ix_rooms_slug\", 100, |b| Ok(()))?;\n");
        assert_eq!(f.len(), 1, "a literal index name must be flagged: {f:?}");
        assert_eq!(f[0].check, "hardcoded-index-name");
    }

    #[test]
    fn open_cursor_is_covered_too() {
        let f = lint_file("lib.rs", "    open_cursor(\"rooms\", \"idx_rooms_created\", 50)?;\n");
        assert_eq!(f.len(), 1, "open_cursor is the same hazard: {f:?}");
    }

    #[test]
    fn a_cursor_call_with_no_index_literal_is_not_flagged() {
        // The control: it must fire on the LITERAL, not on the call. A variable
        // is at least a single source of truth.
        let f = lint_file("lib.rs", "    for_each_batch(Room::TABLE, idx, 100, |b| Ok(()))?;\n");
        assert!(f.is_empty(), "a non-literal index argument is fine: {f:?}");
    }

    #[test]
    fn the_index_name_ok_marker_suppresses_it() {
        let f = lint_file(
            "lib.rs",
            "    // index-name-ok: low-level cursor over a migration-only index\n    open_cursor(\"rooms\", \"ix_rooms_slug\", 50)?;\n",
        );
        assert!(f.is_empty(), "the documented escape must work: {f:?}");
    }





    #[test]
    fn a_bare_counter_field_attribute_is_flagged() {
        // `#[counter]` on a FIELD is a retired form — the derive rejects it with
        // a diagnostic naming the struct-level declaration. The internal gate has
        // caught this since the counters-outside-structs port; a developer
        // running `boogy check` never saw it.
        let f = lint_file("models.rs", "pub struct Room {\n    #[counter]\n    pub hits: i64,\n}\n");
        assert_eq!(f.len(), 1, "the bare field attribute must be flagged: {f:?}");
        assert_eq!(f[0].check, "counter-field");
        assert_eq!(f[0].severity, Severity::Hard, "no escape — the derive rejects it outright");
    }

    #[test]
    fn the_struct_level_counter_declaration_is_not_flagged() {
        // The control. The struct-level form is CURRENT and must never be
        // flagged, or the check fires on every correct model.
        let f = lint_file(
            "models.rs",
            "#[model(table = \"rooms\", counter(name = \"hits\"))]\npub struct Room {}\n",
        );
        assert!(f.is_empty(), "the struct-level form is correct: {f:?}");
    }

    #[test]
    fn legacy_init_tables_and_out_of_model_indexes_are_flagged() {
        // `init_tables` was replaced by schema + migrate + bootstrap. A
        // hand-created index bypasses the model's declared access patterns, so
        // the planner and the schema disagree.
        let a = lint_file("lib.rs", "fn init_tables() {}\n");
        assert_eq!(a.len(), 1, "legacy init_tables must be flagged: {a:?}");
        assert_eq!(a[0].check, "legacy-init-tables");

        let b = lint_file("lib.rs", "    create_index(\"ix_rooms_slug\", &[\"slug\"])?;\n");
        assert_eq!(b.len(), 1, "a hand-rolled index name must be flagged: {b:?}");
        assert_eq!(b[0].check, "legacy-init-tables");
    }

    #[test]
    fn the_reconcile_exempt_marker_suppresses_a_hand_created_index() {
        let f = lint_file(
            "lib.rs",
            "    // reconcile-exempt: migration backfill, dropped next release\n    create_index(\"ix_rooms_slug\", &[\"slug\"])?;\n",
        );
        assert!(f.is_empty(), "the documented escape must work: {f:?}");
    }

    #[test]
    fn a_reconcile_exempt_marker_with_no_reason_does_not_suppress() {
        // A marker is the one place an author can write anything and be
        // believed. An empty one is a suppression with no argument behind it.
        let f = lint_file(
            "lib.rs",
            "    // reconcile-exempt:\n    create_index(\"ix_rooms_slug\", &[\"slug\"])?;\n",
        );
        assert_eq!(f.len(), 1, "an empty marker must not suppress: {f:?}");
    }

    #[test]
    fn a_snapshot_counter_read_and_a_write_in_one_tx_is_flagged() {
        let f = counter_findings(&counter_crate(
            r#"
            fn take(req: &mut Req) -> Result<(), ApiError> {
                tx::<_, _, ApiError>(|| {
                    let n = RoomTaken::get(store, id)?;
                    if n < LIMIT { RoomTaken::add(store, id, 1)?; }
                    Ok(())
                })
            }
        "#,
        ));
        assert_eq!(f.len(), 1, "the oversell shape must be flagged: {f:?}");
        assert_eq!(f[0].check, "counter-read-in-tx");
        assert_eq!(f[0].file, "lib.rs", "flagged where it is USED, not where declared");
        assert!(
            f[0].message.contains("RoomTaken") && f[0].message.contains("take"),
            "the finding must name the counter and the handler: {}",
            f[0].message
        );
    }

    #[test]
    fn get_for_update_is_never_flagged() {
        // The documented remedy. A check that flags its own fix teaches the
        // author to suppress it instead.
        let f = counter_findings(&counter_crate(
            r#"
            fn take(req: &mut Req) -> Result<(), ApiError> {
                tx::<_, _, ApiError>(|| {
                    let n = RoomTaken::get_for_update(store, id)?;
                    if n < LIMIT { RoomTaken::add(store, id, 1)?; }
                    Ok(())
                })
            }
        "#,
        ));
        assert!(f.is_empty(), "get_for_update is the FIX, not the defect: {f:?}");
    }

    #[test]
    fn each_half_alone_is_not_flagged() {
        // A read alone is fine; a write alone is the normal case. Flagging
        // either would make the check fire on most services that use a counter
        // at all, which is how a check gets globally suppressed.
        let read_only = counter_findings(&counter_crate(
            "fn show() { tx::<_,_,E>(|| { let n = RoomTaken::get(store, id)?; Ok(n) }) }",
        ));
        assert!(read_only.is_empty(), "a read alone is not the defect: {read_only:?}");
        let write_only = counter_findings(&counter_crate(
            "fn bump() { tx::<_,_,E>(|| { RoomTaken::add(store, id, 1) }) }",
        ));
        assert!(write_only.is_empty(), "a write alone is the normal case: {write_only:?}");
    }

    #[test]
    fn outside_a_transaction_is_not_flagged() {
        // Each autocommit op is its own transaction, so the read cannot precede
        // the write inside one. The store does not refuse it and neither does this.
        let f = counter_findings(&counter_crate(
            "fn take() { let n = RoomTaken::get(store, id)?; RoomTaken::add(store, id, 1)?; }",
        ));
        assert!(f.is_empty(), "no tx, no hazard: {f:?}");
    }

    #[test]
    fn the_display_only_marker_suppresses_it() {
        let f = counter_findings(&counter_crate(
            r#"
            fn take() {
                tx::<_, _, ApiError>(|| {
                    // counter-read-display-only: echoed into the response, never branched on
                    let n = RoomTaken::get(store, id)?;
                    RoomTaken::add(store, id, 1)?;
                    Ok(n)
                })
            }
        "#,
        ));
        assert!(f.is_empty(), "the documented escape must work: {f:?}");
    }

    #[test]
    fn a_crate_that_declares_no_counter_is_never_scanned() {
        // The handle set is DISCOVERED. Without a declaration there is nothing
        // to match, and `::get(`/`::add(` must not be matched bare — they
        // appear all over ordinary Rust.
        let f = counter_findings(&[(
            "lib.rs".to_string(),
            "fn h() { tx::<_,_,E>(|| { let v = Map::get(k)?; List::add(v)?; Ok(()) }) }"
                .to_string(),
        )]);
        assert!(f.is_empty(), "bare ::get(/::add( must never match: {f:?}");
    }

    #[test]
    fn the_handle_is_found_through_an_aliased_derive_and_a_doc_comment() {
        // `use boogy_sdk::Counter as CounterDerive` is what the examples
        // actually write, so keying on the derive NAME would find nothing. The
        // attribute is the stable marker, and the struct may sit a few lines
        // below it.
        let files = vec![
            (
                "models.rs".to_string(),
                "#[counter(of = Room, name = \"taken\")]\n/// Typed handle.\n#[allow(dead_code)]\npub struct RoomTaken;\n".to_string(),
            ),
            (
                "lib.rs".to_string(),
                "fn take() { tx::<_,_,E>(|| { let n = RoomTaken::get(s,i)?; RoomTaken::add(s,i,1) }) }".to_string(),
            ),
        ];
        assert_eq!(counter_findings(&files).len(), 1, "the handle must still be found");
    }

    #[test]
    fn the_raw_store_verb_form_is_flagged_too() {
        // The form the examples actually write. Matching only the typed handle
        // made the check decorative — `wit_glue!` emits no counter verbs, so
        // `LinkClicks::get(..)` is not reachable from a deployed service at all
        // and every real read goes through `st::counter_get(NAME, .., true)`.
        let f = counter_findings(&counter_crate(
            r#"
            fn take() -> Result<(), ApiError> {
                tx::<_, _, ApiError>(|| {
                    let n = st::counter_get(RoomTaken::NAME, &[st::Value::Integer(id)], true)?;
                    if n < LIMIT {
                        st::counter_add(RoomTaken::NAME, &[st::Value::Integer(id)], 1)?;
                    }
                    Ok(())
                })
            }
        "#,
        ));
        assert_eq!(f.len(), 1, "the raw verb form must be flagged: {f:?}");
    }

    #[test]
    fn the_raw_serializable_read_is_the_escape() {
        // `false` is the trailing snapshot flag — the caller paid for the
        // read-conflict range. Same program, one argument apart from the test
        // above, and it must not be flagged.
        let f = counter_findings(&counter_crate(
            r#"
            fn take() -> Result<(), ApiError> {
                tx::<_, _, ApiError>(|| {
                    let n = st::counter_get(RoomTaken::NAME, &[st::Value::Integer(id)], false)?;
                    if n < LIMIT {
                        st::counter_add(RoomTaken::NAME, &[st::Value::Integer(id)], 1)?;
                    }
                    Ok(())
                })
            }
        "#,
        ));
        assert!(f.is_empty(), "snapshot=false is the documented escape: {f:?}");
    }

    #[test]
    fn a_raw_read_of_a_DIFFERENT_counter_is_not_flagged() {
        // The store's condition is per-CELL, and the check mirrors it: reading
        // one counter and writing another is not the oversell shape.
        let files = vec![
            (
                "models.rs".to_string(),
                "#[counter(of = Room, name = \"taken\")]\npub struct RoomTaken;\n                 #[counter(of = Room, name = \"views\")]\npub struct RoomViews;\n".to_string(),
            ),
            (
                "lib.rs".to_string(),
                "fn h() { tx::<_,_,E>(|| { let n = st::counter_get(RoomViews::NAME, &k, true)?;                  st::counter_add(RoomTaken::NAME, &k, 1)?; Ok(n) }) }".to_string(),
            ),
        ];
        assert!(counter_findings(&files).is_empty(), "different cells do not interact");
    }

    #[test]
    fn max_observe_after_a_snapshot_max_get_is_flagged() {
        let f = counter_findings(&counter_crate(
            "fn h() { tx::<_,_,E>(|| { let v = st::max_get(RoomTaken::NAME, &k, true)?;              st::max_observe(RoomTaken::NAME, &k, v.unwrap_or(0) + 1)?; Ok(()) }) }",
        ));
        assert_eq!(f.len(), 1, "the max accumulator carries the same hazard: {f:?}");
    }

    #[test]
    fn count_writes_sees_upsert_increment_and_raw_store_writes() {
        // The exact shape the transactions guidance teaches as its worked
        // example, minus the tx wrapper: an insert plus a dependent counter.
        // Before this fix it counted as ONE write and passed the gate.
        let body = r#"
            db_insert(&Message { .. })?;
            upsert_increment(Conversation::TABLE, id, Conversation::MSG_COUNT, 1)?;
        "#;
        assert_eq!(
            count_writes(body), 2,
            "an insert plus a dependent counter is TWO writes; counting it as one is what \
             let an untransacted multi-write handler pass the gate"
        );

        let raw = r#"
            store::insert(t, row)?;
            store::delete_where(t, pred)?;
        "#;
        assert_eq!(count_writes(raw), 2, "raw store::* writes must count too");
    }

    #[test]
    fn count_writes_does_not_count_reads() {
        // Counting a read as a write would demand a transaction around
        // read-only handlers — a false positive on the noisiest possible surface.
        let body = r#"
            store::find_many(t, pred)?;
            db_find_by(x)?;
            store::count(t)?;
        "#;
        assert_eq!(count_writes(body), 0, "reads must never count as writes");
    }
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
    fn flags_raw_schema_with_a_declared_exception() {
        // Severity is Fail, not Hard, since 2026-08-25: `// dynamic-schema:`
        // is a real declared exception the internal gate has always honoured,
        // and `Hard` in this crate means "no escape hatch exists".
        let f = lint_file("models.rs", "    let t = Table::new(\"rooms\");\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "raw-schema");
        assert_eq!(f[0].severity, Severity::Fail);
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

    #[test]
    fn comment_mentions_are_not_flagged() {
        // A doc comment mentioning the tokens must NOT be flagged.
        assert!(lint_file("lib.rs", "/// Avoid Table::new(...) — use #[derive(Model)].").is_empty());
        assert!(lint_file("lib.rs", "// returns Json<serde_json::Value> historically").is_empty());
        // But real code still flags.
        assert!(!lint_file("lib.rs", "let t = Table::new(\"x\");").is_empty());
    }

}
