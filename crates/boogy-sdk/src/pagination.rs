//! Cursor-based pagination on top of the WIT store's offset/limit
//! `Page` primitive.
//!
//! Why cursors over `?offset=10000`:
//!
//! - **Stable under writes.** Offset pagination shifts when rows are
//!   inserted mid-paginate; the second page silently re-includes
//!   what the first page just showed. Cursors anchor on the last
//!   row's id, so concurrent inserts don't smear into the page
//!   boundary.
//! - **Constant-cost.** `WHERE id > <last>` is an indexed lookup.
//!   `OFFSET 10000` scans 10001 rows before returning the first
//!   one — a scaling cliff that production APIs eventually hit.
//!
//! ## Shape
//!
//! A [`Cursor`] is a `(last_id, last_sort_value)` pair the SDK
//! encodes as URL-safe base64 of compact JSON. The cursor is
//! deliberately **opaque to clients** — they round-trip it as a
//! string, never inspect it. SDK consumers can extend the schema
//! later without breaking older clients.
//!
//! ## Typical handler
//!
//! Use `Req::parse_query::<T>()` to decode `?cursor=…&limit=…` into a
//! typed struct with `garde`-checked bounds, then return
//! `Json<CursorPage<T>>` so the framework serializes the page.
//!
//! ```ignore
//! use boogy_sdk::pagination::{Cursor, CursorPage, decode};
//!
//! #[derive(Deserialize, garde::Validate)]
//! struct ListQuery {
//!     #[garde(range(min = 1, max = 100))]
//!     #[serde(default = "default_limit")]
//!     limit: usize,
//!     #[garde(skip)]
//!     cursor: Option<String>,
//! }
//! fn default_limit() -> usize { 20 }
//!
//! fn list_items(req: &mut Req<'_>) -> Result<Json<CursorPage<json::Value>>, ApiError> {
//!     let q: ListQuery = req.parse_query()?;
//!
//!     // Build the WHERE clause from the inbound cursor (or no
//!     // filter on the first page).
//!     let mut filters = vec![];
//!     if let Some(c) = q.cursor.as_deref().and_then(decode) {
//!         filters.push(store::Filter {
//!             column: "_id".into(),
//!             op: store::FilterOp::Gt,
//!             val: store::Value::Text(c.last_id),
//!         });
//!     }
//!
//!     // Overfetch by 1 to detect "is there another page?" without
//!     // a separate count query.
//!     let result = store::find("items", &store::FindOptions {
//!         filters,
//!         sort: vec![store::SortBy {
//!             column: "_id".into(),
//!             dir: store::SortDir::Asc,
//!         }],
//!         page: Some(store::Page { limit: (q.limit + 1) as u32, offset: 0 }),
//!     })
//!     .map_err(ApiError::internal)?;
//!
//!     let rows: Vec<json::Value> = result.rows.iter()
//!         .map(|r| to_sdk_row(r).to_json(&["title"])).collect();
//!     let page = CursorPage::from_overfetched(rows, q.limit, |row| {
//!         Cursor::id_only(row.get("id").and_then(|v| v.as_str()).unwrap_or("").into())
//!     });
//!     Ok(Json(page))
//! }
//! ```

use crate::store::{Filter, FilterOp, SortDir, Val};
use serde::{Deserialize, Serialize};

/// Opaque pagination state. Encoded as URL-safe base64 of compact
/// JSON; clients round-trip it as a single string. Schema extensions
/// (e.g. an extra `last_sort_value` field for keyset queries on a
/// non-id column) ride additively — older cursors keep decoding
/// because absent fields default sensibly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cursor {
    /// Row id of the last item on the page just served. The next
    /// page's filter is `WHERE _id > last_id` (asc) / `< last_id`
    /// (desc). For id-only sorting this is the whole story.
    pub last_id: String,

    /// Sort-column value of the last row, when paginating by a
    /// non-id column. Required for keyset pagination on (sort_value,
    /// _id) ordering — the WHERE clause becomes
    /// `(sort_col, _id) > (last_value, last_id)`. `Null` for the
    /// id-only case (the default).
    #[serde(default, skip_serializing_if = "is_null")]
    pub last_value: serde_json::Value,
}

fn is_null(v: &serde_json::Value) -> bool {
    v.is_null()
}

impl Cursor {
    /// Cursor for the common id-only sort case (the default for
    /// most list endpoints — sort by `_id`, no secondary key).
    pub fn id_only(last_id: impl Into<String>) -> Self {
        Self { last_id: last_id.into(), last_value: serde_json::Value::Null }
    }

    /// Cursor for keyset pagination on `(sort_value, _id)`. Use when
    /// the list is sorted by a non-id column and rows can share that
    /// column's value — the row id is the tiebreak.
    pub fn keyset(last_id: impl Into<String>, last_value: serde_json::Value) -> Self {
        Self { last_id: last_id.into(), last_value }
    }
}

/// Encode a cursor for inclusion in a `?cursor=...` query parameter
/// or `next_cursor` JSON field. Output is URL-safe base64 (RFC 4648
/// §5) with no padding — embeds cleanly in query strings without
/// the `+`/`/`/`=` escaping that standard base64 forces clients
/// through.
pub fn encode(cursor: &Cursor) -> String {
    // serde_json never panics on a Cursor (all fields are
    // serializable concrete types), so unwrap is safe.
    let json = serde_json::to_vec(cursor).expect("cursor serializes");
    base64_url_encode(&json)
}

/// Decode a cursor produced by [`encode`]. Returns `None` for any
/// failure — invalid base64, invalid UTF-8, invalid JSON, or
/// missing required fields. Treat `None` as "no cursor" so a
/// malformed query parameter just resets pagination instead of
/// throwing the request out.
pub fn decode(s: impl AsRef<str>) -> Option<Cursor> {
    let bytes = base64_url_decode(s.as_ref())?;
    serde_json::from_slice(&bytes).ok()
}

/// Convert a `serde_json::Value` from [`Cursor::last_value`] into a
/// store [`Val`]. Used internally by [`keyset_resume_filter`].
///
/// Mapping:
/// - `Null` → `Val::Null`
/// - `Bool` → `Val::Boolean`
/// - `Number` (integer-representable) → `Val::Integer`
/// - `Number` (float-only) → `Val::Real`
/// - `String` → `Val::Text`
/// - Arrays / objects → `Val::Text` (JSON-serialized) — callers should
///   not paginate on structured types; this is a safe fallback.
fn json_to_val(v: &serde_json::Value) -> Val {
    match v {
        serde_json::Value::Null => Val::Null,
        serde_json::Value::Bool(b) => Val::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Val::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Val::Real(f)
            } else {
                // Unreachable in practice (serde_json Numbers are always i64 or f64
                // representable), but safe fallback preserves the raw text.
                Val::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => Val::Text(s.clone()),
        // Arrays / objects: not valid sort columns, but produce a
        // safe (if surprising) text value rather than panicking.
        other => Val::Text(other.to_string()),
    }
}

/// Build the resume filter for keyset pagination on `(sort_col, _id)`.
///
/// Returns `(extra_filters, or_groups)` to splice into `FindOptions`:
/// - `extra_filters` are ANDed with the caller's base filters.
/// - `or_groups` is an OR-of-AND-groups expanding the lexicographic
///   tuple comparison `(sort_col, _id) CMP (last_value, last_id)`,
///   where `CMP` is `>` for [`SortDir::Asc`] and `<` for
///   [`SortDir::Desc`].
///
/// Concretely the or-group expands to:
/// ```text
/// (sort_col CMP last_value)
/// OR (sort_col = last_value AND _id CMP last_id)
/// ```
///
/// This is the correct fix for the tuple-ordering compromise that
/// single-column keyset pagination suffers — it includes all tied rows
/// on subsequent pages instead of silently skipping them.
///
/// # Returns
/// - `(vec![], vec![])` when `cursor == None` (initial page, no filter).
/// - `(vec![Filter { _id CMP last_id }], vec![])` for id-only cursors
///   (`cursor.last_value` is `Null`) — no or-group needed.
/// - `(vec![], or_groups)` with `or_groups` of exactly 2 AND-groups
///   for composite `(sort_col, _id)` cursors.
///
/// The `_id` tie-break value is emitted as `Val::Integer`: `_id` is an
/// integer rowid in the store, and a cross-type Integer-vs-Text comparison
/// never orders — so a `Val::Text` arm would evaluate false and silently
/// drop every tied row on page 2+. `last_id` is always a numeric rowid
/// stringified ([`Cursor`] stores it as a [`String`]), so parsing it back
/// is safe.
fn id_val(last_id: &str) -> Val {
    match last_id.parse::<i64>() {
        Ok(n) => Val::Integer(n),
        // Defensive: a non-numeric id falls back to Text (shouldn't happen
        // for the `_id` rowid column).
        Err(_) => Val::Text(last_id.to_string()),
    }
}

pub fn keyset_resume_filter(
    cursor: Option<&Cursor>,
    sort_col: &str,
    dir: SortDir,
) -> (Vec<Filter>, Vec<Vec<Filter>>) {
    let Some(c) = cursor else {
        return (vec![], vec![]);
    };

    let cmp_op = match dir {
        SortDir::Asc => FilterOp::Gt,
        SortDir::Desc => FilterOp::Lt,
    };

    // Id-only fast path: no secondary sort column (last_value is Null).
    if c.last_value.is_null() {
        return (
            vec![Filter {
                column: "_id".to_string(),
                op: cmp_op,
                val: id_val(&c.last_id),
                in_values: None,
            }],
            vec![],
        );
    }

    // Composite (sort_col, _id) keyset — expand the tuple comparison
    // into an OR of two AND-groups.
    let last_val = json_to_val(&c.last_value);
    let or_groups = vec![
        // Group 1: sort_col CMP last_val  (strictly ahead on sort column)
        vec![Filter {
            column: sort_col.to_string(),
            op: cmp_op.clone(),
            val: last_val.clone(),
            in_values: None,
        }],
        // Group 2: sort_col = last_val AND _id CMP last_id  (tied on sort column, ahead on id)
        vec![
            Filter {
                column: sort_col.to_string(),
                op: FilterOp::Eq,
                val: last_val,
                in_values: None,
            },
            Filter {
                column: "_id".to_string(),
                op: cmp_op,
                val: id_val(&c.last_id),
                in_values: None,
            },
        ],
    ];
    (vec![], or_groups)
}

/// Pagination response envelope. Serializes as
/// `{"items": [...], "next_cursor": "..."}` with `next_cursor`
/// omitted when there is no next page (last-page marker).
#[derive(Debug, Clone, Serialize)]
pub struct CursorPage<T: Serialize> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T: Serialize> CursorPage<T> {
    /// Build a page from rows fetched with `limit + 1` capacity.
    ///
    /// The "+1 trick": if the query returned more than `limit`
    /// rows, the extra row proves there's a next page. Drop it and
    /// derive a cursor from the *last kept* row. If not, this is
    /// the last page — no cursor.
    ///
    /// `cursor_for` receives the last kept row and produces the
    /// [`Cursor`] for it. For the typical id-only case use
    /// `Cursor::id_only(row.id())`.
    pub fn from_overfetched<F>(rows: Vec<T>, limit: usize, cursor_for: F) -> Self
    where
        F: FnOnce(&T) -> Cursor,
    {
        if rows.len() > limit && limit > 0 {
            let kept: Vec<T> = rows.into_iter().take(limit).collect();
            // limit > 0 + len > limit ⇒ kept has at least one entry.
            let last = kept.last().expect("kept page is non-empty");
            let next = encode(&cursor_for(last));
            Self { items: kept, next_cursor: Some(next) }
        } else {
            Self { items: rows, next_cursor: None }
        }
    }
}

// -- URL-safe base64 (RFC 4648 §5, no padding) --
//
// Standalone implementation to avoid pulling in the `base64` crate
// just for this. `store.rs` already has a standard-alphabet variant
// for api_keys; the cursor variant uses the URL-safe alphabet
// (`-`/`_` instead of `+`/`/`) and no padding so the output drops
// straight into a query string without escaping.

const URL_SAFE_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64_url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(URL_SAFE_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(URL_SAFE_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(URL_SAFE_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(URL_SAFE_CHARS[(triple & 0x3F) as usize] as char);
        }
    }
    out
}

fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    // Reverse-lookup table: ascii char → 0..=63 or sentinel 0xFF.
    let mut table = [0xFFu8; 256];
    for (i, &c) in URL_SAFE_CHARS.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n % 4 == 1 {
        // 4n+1 input bytes can never decode — base64 quanta are
        // {2,3,4} chars per 1/2/3 output bytes.
        return None;
    }
    let mut out = Vec::with_capacity(n * 3 / 4 + 2);
    let mut i = 0;
    while i + 4 <= n {
        let v0 = table[bytes[i] as usize];
        let v1 = table[bytes[i + 1] as usize];
        let v2 = table[bytes[i + 2] as usize];
        let v3 = table[bytes[i + 3] as usize];
        if v0 == 0xFF || v1 == 0xFF || v2 == 0xFF || v3 == 0xFF {
            return None;
        }
        let triple = ((v0 as u32) << 18)
            | ((v1 as u32) << 12)
            | ((v2 as u32) << 6)
            | (v3 as u32);
        out.push((triple >> 16) as u8);
        out.push((triple >> 8) as u8);
        out.push(triple as u8);
        i += 4;
    }
    // Trailing 2 or 3 chars (no-padding mode).
    match n - i {
        0 => {}
        2 => {
            let v0 = table[bytes[i] as usize];
            let v1 = table[bytes[i + 1] as usize];
            if v0 == 0xFF || v1 == 0xFF {
                return None;
            }
            let triple = ((v0 as u32) << 18) | ((v1 as u32) << 12);
            out.push((triple >> 16) as u8);
        }
        3 => {
            let v0 = table[bytes[i] as usize];
            let v1 = table[bytes[i + 1] as usize];
            let v2 = table[bytes[i + 2] as usize];
            if v0 == 0xFF || v1 == 0xFF || v2 == 0xFF {
                return None;
            }
            let triple =
                ((v0 as u32) << 18) | ((v1 as u32) << 12) | ((v2 as u32) << 6);
            out.push((triple >> 16) as u8);
            out.push((triple >> 8) as u8);
        }
        _ => unreachable!(), // % 4 != 0/2/3 was rejected above
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{FilterOp, SortDir, Val};

    #[test]
    fn cursor_round_trips_id_only() {
        let c = Cursor::id_only("row-abc-123");
        let encoded = encode(&c);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, c);
    }

    #[test]
    fn cursor_round_trips_keyset() {
        let c = Cursor::keyset("row-abc", serde_json::json!("alice"));
        let encoded = encode(&c);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, c);

        let c2 = Cursor::keyset("row-xyz", serde_json::json!(42));
        let decoded2 = decode(encode(&c2)).expect("decodes");
        assert_eq!(decoded2, c2);
    }

    #[test]
    fn cursor_encoding_is_url_safe() {
        // Encode a value chosen to force `+`/`/` in standard base64.
        // The URL-safe alphabet should produce `-` / `_` instead.
        let c = Cursor::keyset("id", serde_json::json!("\u{FFFF}\u{FFFE}"));
        let encoded = encode(&c);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn decode_returns_none_on_garbage() {
        // Invalid base64 alphabet (contains `!` / `@`).
        assert!(decode("not!base64@@@").is_none());
        // Valid base64 ("YQ" → "a") but not valid JSON.
        assert!(decode("YQ").is_none());
        // Empty input → empty bytes → JSON parse fails.
        assert!(decode("").is_none());
        // Valid JSON but missing the required `last_id` field.
        let bad = base64_url_encode(br#"{"last_value":null}"#);
        assert!(decode(&bad).is_none());
    }

    #[test]
    fn from_overfetched_no_extra_emits_no_cursor() {
        let rows: Vec<u32> = vec![1, 2, 3];
        let page = CursorPage::from_overfetched(rows, 5, |_| {
            Cursor::id_only("never-called")
        });
        assert_eq!(page.items, vec![1, 2, 3]);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn from_overfetched_with_extra_emits_cursor_from_last_kept() {
        let rows: Vec<&str> = vec!["a", "b", "c", "d"];
        // limit=3, fetched 4 ⇒ drop "d", emit cursor for "c".
        let page = CursorPage::from_overfetched(rows, 3, |s| {
            Cursor::id_only(*s)
        });
        assert_eq!(page.items, vec!["a", "b", "c"]);
        let next = page.next_cursor.expect("has cursor");
        let decoded = decode(&next).expect("decodes");
        assert_eq!(decoded.last_id, "c");
    }

    #[test]
    fn from_overfetched_exact_limit_match_emits_no_cursor() {
        // Edge case: limit=3, fetched exactly 3 ⇒ no extra ⇒ last page.
        let rows: Vec<u32> = vec![10, 20, 30];
        let page = CursorPage::from_overfetched(rows, 3, |_| {
            Cursor::id_only("nope")
        });
        assert_eq!(page.items.len(), 3);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn cursor_page_serializes_to_json() {
        let page = CursorPage {
            items: vec!["x", "y"],
            next_cursor: Some("abc".into()),
        };
        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("\"items\":[\"x\",\"y\"]"));
        assert!(json.contains("\"next_cursor\":\"abc\""));

        // Last-page case omits next_cursor entirely.
        let last = CursorPage::<&str> {
            items: vec!["z"],
            next_cursor: None,
        };
        let json = serde_json::to_string(&last).unwrap();
        assert!(!json.contains("next_cursor"), "got: {json}");
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        for case in [
            &[][..],
            &[0x00],
            &[0xFF],
            b"hello",
            b"hello!",
            b"hello!!",
            b"hello!!!",
            &[0xDE, 0xAD, 0xBE, 0xEF],
        ] {
            let encoded = base64_url_encode(case);
            let decoded = base64_url_decode(&encoded).expect("decodes");
            assert_eq!(&decoded[..], case, "mismatch for {case:?}");
        }
    }

    #[test]
    fn base64_decode_rejects_invalid_alphabet() {
        // Standard-base64 chars `+` / `/` are NOT in the URL-safe alphabet.
        assert!(base64_url_decode("ab+d").is_none());
        assert!(base64_url_decode("ab/d").is_none());
        assert!(base64_url_decode("ab=d").is_none());
    }

    // -- keyset_resume_filter tests --

    #[test]
    fn keyset_resume_empty_cursor() {
        let (f, og) = keyset_resume_filter(None, "score", SortDir::Asc);
        assert!(f.is_empty());
        assert!(og.is_empty());
    }

    #[test]
    fn keyset_resume_id_only_asc() {
        let c = Cursor::id_only("42");
        let (f, og) = keyset_resume_filter(Some(&c), "score", SortDir::Asc);
        assert!(og.is_empty());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].column, "_id");
        assert!(matches!(f[0].op, FilterOp::Gt));
        // `_id` must be Integer — a Text arm never orders against the
        // Integer rowid column and silently drops rows.
        assert!(matches!(&f[0].val, Val::Integer(42)));
    }

    #[test]
    fn keyset_resume_id_only_desc() {
        let c = Cursor::id_only("42");
        let (f, og) = keyset_resume_filter(Some(&c), "score", SortDir::Desc);
        assert!(og.is_empty());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].column, "_id");
        assert!(matches!(f[0].op, FilterOp::Lt));
        assert!(matches!(&f[0].val, Val::Integer(42)));
    }

    #[test]
    fn keyset_resume_composite_asc() {
        let c = Cursor::keyset("7", serde_json::json!(10));
        let (f, og) = keyset_resume_filter(Some(&c), "score", SortDir::Asc);
        // No extra AND filters — all logic is in or_groups.
        assert!(f.is_empty());
        assert_eq!(og.len(), 2);
        // Group 1: [score > 10]
        assert_eq!(og[0].len(), 1);
        assert_eq!(og[0][0].column, "score");
        assert!(matches!(og[0][0].op, FilterOp::Gt));
        assert!(matches!(&og[0][0].val, Val::Integer(10)));
        // Group 2: [score = 10, _id > 7]
        assert_eq!(og[1].len(), 2);
        assert_eq!(og[1][0].column, "score");
        assert!(matches!(og[1][0].op, FilterOp::Eq));
        assert!(matches!(&og[1][0].val, Val::Integer(10)));
        assert_eq!(og[1][1].column, "_id");
        assert!(matches!(og[1][1].op, FilterOp::Gt));
        // `_id` tie-break must be Integer (see id_val).
        assert!(matches!(&og[1][1].val, Val::Integer(7)));
    }

    #[test]
    fn keyset_resume_composite_desc() {
        let c = Cursor::keyset("abc", serde_json::json!(10));
        let (f, og) = keyset_resume_filter(Some(&c), "score", SortDir::Desc);
        assert!(f.is_empty());
        assert_eq!(og.len(), 2);
        // Both comparison ops must be Lt for Desc.
        assert!(matches!(og[0][0].op, FilterOp::Lt));
        assert!(matches!(og[1][1].op, FilterOp::Lt));
        // Equality arm unchanged.
        assert!(matches!(og[1][0].op, FilterOp::Eq));
    }

    #[test]
    fn keyset_resume_value_types() {
        // Integer, Real, Text last_value each map to the correct Val variant.
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (serde_json::json!(42), "integer"),
            (serde_json::json!(std::f64::consts::PI), "real"),
            (serde_json::json!("hello"), "text"),
        ];
        for (jv, label) in cases {
            let c = Cursor::keyset("id1", jv.clone());
            let (_, og) = keyset_resume_filter(Some(&c), "col", SortDir::Asc);
            assert_eq!(og.len(), 2, "expected 2 or_groups for case {label}");
            let val = &og[0][0].val;
            match (label, val) {
                ("integer", Val::Integer(42)) => {}
                ("real", Val::Real(f)) => {
                    assert!((f - std::f64::consts::PI).abs() < 1e-9, "real mismatch: {f}")
                }
                ("text", Val::Text(s)) => assert_eq!(s, "hello"),
                _ => panic!("val type mismatch for case {label}: got {val:?}"),
            }
        }
    }

    #[test]
    fn id_val_parses_numeric_else_text() {
        // Numeric rowids → Integer (so they order against the Integer
        // `_id` column); non-numeric → defensive Text fallback.
        assert!(matches!(id_val("42"), Val::Integer(42)));
        assert!(matches!(id_val("0"), Val::Integer(0)));
        assert!(matches!(id_val("not-a-number"), Val::Text(s) if s == "not-a-number"));
    }
}
