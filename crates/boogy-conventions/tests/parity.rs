//! retired-spelling-file: the coverage table names the internal gate's section
//! titles verbatim, one of which is the retired `#[counter]` field form the
//! check exists to reject — now `#[model(counter(name = "..."))]` plus
//! `#[derive(Counter)]`.
//! The shipped checks must cover what the internal gate enforces.
//!
//! `scripts/check-service-conventions.sh` and `boogy check` drifted in THREE
//! directions before 2026-08-25: five internal checks were never shipped, two
//! shipped checks were absent internally, and one shipped check (`raw-schema`)
//! was STRICTER than its internal twin — it refused the `// dynamic-schema:`
//! exception the internal gate had always honoured, so a developer with a
//! genuinely runtime-known table was told to do something impossible.
//!
//! A developer got seven of the ten rules we hold ourselves to, and one of the
//! seven was wrong. This asserts the mapping stays complete, in both
//! directions, so it cannot happen again silently.

/// Internal gate section title → the `boogy check` id that covers it.
///
/// `None` means "deliberately not shipped, with a reason stated here". Both
/// remaining entries need the same CROSS-REFERENCE analysis — matching a
/// declaration against its uses elsewhere in the crate — which a line-oriented
/// lint cannot express.
const COVERAGE: &[(&str, Option<&str>, &str)] = &[
    ("raw table schema", Some("raw-schema"), ""),
    ("raw store CRUD", Some("raw-store-crud"), ""),
    ("un-annotated routes", Some("unannotated-routes"), ""),
    ("multi-write handler without tx", Some("multi-write-no-tx"), ""),
    ("hardcoded index name", Some("hardcoded-index-name"), ""),
    ("untyped HTTP response body", Some("untyped-response"), ""),
    (
        "untyped HTTP request DTO",
        None,
        "needs the internal gate's `Json<Name>` / `parse_body::<Name>` matcher: it flags a \
         struct only where it is USED as a request body. A line check flagged 18 of 31 crates, \
         because job payloads and stored rows also derive Deserialize.",
    ),
    (
        "reads no declared index can serve",
        None,
        "`scripts/check-keyset-indexes.py`, 1,561 lines: parses each model's declared access \
         patterns and matches them against the reads in the same crate. Porting it would \
         create a second implementation of the analysis, which is the drift this file exists \
         to prevent.",
    ),
    ("legacy init_tables", Some("legacy-init-tables"), ""),
    ("#[counter] as a struct field", Some("counter-field"), ""),
];

#[test]
fn every_internal_check_is_shipped_or_explicitly_deferred() {
    for (title, id, _why) in COVERAGE {
        if let Some(id) = id {
            assert!(
                boogy_conventions::CHECKS.contains(id),
                "internal check '{title}' maps to `{id}`, which boogy check does not ship",
            );
        }
    }
}

#[test]
fn a_deferred_check_must_carry_a_reason() {
    // A `None` with no reason is a gap wearing a decision's clothes.
    for (title, id, why) in COVERAGE {
        if id.is_none() {
            assert!(
                !why.trim().is_empty(),
                "internal check '{title}' is deferred with no reason recorded",
            );
        }
    }
}

#[test]
fn the_internal_gate_has_no_section_this_table_does_not_know_about() {
    // Without this, adding a check to the bash gate and forgetting to ship it
    // is invisible — which is exactly how five of them accumulated.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/check-service-conventions.sh"
    ))
    .expect("the internal gate must be readable from the workspace");

    let mut unknown = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("\"Check ") else { continue };
        let Some((_, title)) = rest.split_once("— ") else { continue };
        let title = title.trim_end_matches(['"', ' ', '\\']);
        if !COVERAGE.iter().any(|(t, _, _)| title.contains(t) || t.contains(title)) {
            unknown.push(title.to_string());
        }
    }
    assert!(
        unknown.is_empty(),
        "the internal gate has check(s) this parity table does not cover: {unknown:?} — \
         ship them in boogy-conventions, or add them to COVERAGE with None and a reason",
    );
}
