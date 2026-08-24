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
//! - **No deep-page cliff.** `OFFSET 10000` scans 10001 rows before
//!   returning the first one. A cursor names where to resume, so page
//!   *k* never pays for the *k-1* pages before it.
//!
//! ## Cost: the two cursor shapes are not equivalent
//!
//! **Only the composite shape is O(page).** A cursor carrying a sort
//! value ([`Cursor::keyset`]) resumes on `(sort_col, _id)`, and when the
//! table declares a matching access pattern — `list_by(filter = "…",
//! newest = "…")` on the model, which derives the covering
//! `[filter_col, sort_col]` index — the store recognises that boundary as
//! a POSITION in the index and seeks straight to it. Cost is the page,
//! whatever the table holds.
//!
//! **The id-only shape ([`Cursor::id_only`], `last_value` null) is not.**
//! `_id` is the row's auto-assigned primary key, not a declared column, so
//! it cannot be named as the sort column of an access pattern — which
//! means `_id > <last>` reaches the store as an ordinary predicate rather
//! than as a resume position. It is evaluated per row, so the walk starts
//! where it started the first time and discards everything the earlier
//! pages already returned. Measured on a 40,000-row table, one page cost
//! ~40,000 key reads; with an equality filter over a composite index it
//! cost ~80,000. The answers are correct, which is why only a cost
//! measurement finds this.
//!
//! So: **declare an access pattern and page with [`Cursor::keyset`].**
//! `auth::find_owned` enforces this — without a declared order column it
//! returns a named error instead of serving any page. That check runs
//! after the underlying read has already happened, so it stops a wrong
//! page from reaching the caller; it does not bound the cost of getting
//! there. Reach for [`Cursor::id_only`] only for sets that are small by
//! construction.
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
//!             in_values: None,
//!         });
//!     }
//!
//!     // Overfetch by 1 to detect "is there another page?" without
//!     // a separate count query.
//!     let result = store::find("items", &store::FindOptions {
//!         filters,
//!         order_by: vec![store::OrderTerm::Column(store::SortBy {
//!             column: "_id".into(),
//!             dir: store::SortDir::Asc,
//!         })],
//!         page: Some(store::Page { limit: (q.limit + 1) as u32, offset: 0 }),
//!         or_groups: vec![],
//!         skip_total: true,
//!         group_cursor: None,
//!         counters: vec![],              // no counter merge in this example
//!     })
//!     .map_err(ApiError::internal)?;
//!
//!     let rows: Vec<json::Value> = result.rows.iter()
//!         .map(|r| to_sdk_row(r).to_json(&["title"])).collect();
//!     let page = CursorPage::from_overfetched(rows, q.limit, |row| {
//!         Cursor::id_only(row.get("id").and_then(|v| v.as_str()).unwrap_or(""))
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

/// Confirm that a keyset page resumed strictly PAST the one before it.
///
/// A keyset loop that gathers every matching row terminates only when a page
/// comes back empty, so it depends on each page starting after the last row of
/// the previous one. `_id` is unique per row, so a page whose last row repeats
/// the previous boundary is proof the resume boundary had no effect — and
/// continuing would read that same page forever.
///
/// Returns `Err` with a message naming what to change, rather than spinning or
/// stopping. Stopping would be worse than the spin: it hands back a partial
/// listing that looks complete.
///
/// `previous` is `None` for the first page, which is always progress.
pub fn keyset_advanced(
    table: &str,
    order_col: &str,
    previous: Option<&Cursor>,
    next: &Cursor,
) -> Result<(), String> {
    let Some(prev) = previous else { return Ok(()) };
    if prev.last_id != next.last_id {
        return Ok(());
    }
    Err(format!(
        "listing '{table}' cannot make progress: a page ended on the same row (id {}) as the page \
         before it, so paging by '{order_col}' would repeat that page forever. Give '{order_col}' \
         a value that is set on every row and never changes after the row is written — a column \
         left empty on some rows cannot order them.",
        next.last_id,
    ))
}

/// The largest page any bounded listing helper will serve in one call.
///
/// A ceiling, not a default: [`PageRequest::new`] CLAMPS to it rather than
/// erroring, because a caller asking for more has not made a mistake it can
/// correct — it has asked for something the platform will not do, and the
/// answer is a page plus a cursor, not a failure.
pub const MAX_PAGE_LIMIT: usize = 200;

/// The page size a listing serves when the caller states none.
pub const DEFAULT_PAGE_LIMIT: usize = 20;

/// How much of a listing one call may read, and where it resumes.
///
/// **This type is why an unbounded listing has no representation.** `limit` is
/// private and every constructor clamps it to [`MAX_PAGE_LIMIT`], so there is
/// no value a caller can pass — no `0`, no `usize::MAX`, no "all" — that asks a
/// helper for a whole table. The listing helpers take this instead of a limit
/// number for exactly that reason: a check that a caller can be talked out of
/// is a convention, and the conventions here were already written down (in a
/// retired-spelling: the "small bounded sets" label is obsolete —
/// listings return a bounded `RowPage`. Quoted because the label is the
/// counter-example this type exists to answer.
/// doc comment saying "for small bounded sets") when a listing exhausted a
/// 32 MiB guest heap and trapped on `handle_alloc_error`.
///
/// The resume token is kept VERBATIM alongside its decoded form. A token that
/// cannot be decoded is refused by the helper rather than dropped: dropping it
/// silently restarts a listing the caller believes it is continuing, which
/// re-serves page one forever.
#[derive(Debug, Clone)]
pub struct PageRequest {
    limit: usize,
    token: Option<String>,
    cursor: Option<Cursor>,
}

impl PageRequest {
    /// A page of `limit` rows resuming from `token`.
    ///
    /// `limit` is clamped into `1..=`[`MAX_PAGE_LIMIT`]. Zero clamps UP: a
    /// page of nothing carries no cursor to continue from, so it would end a
    /// walk that had not started.
    pub fn new(limit: usize, token: Option<String>) -> Self {
        let limit = limit.clamp(1, MAX_PAGE_LIMIT);
        let cursor = token.as_deref().and_then(decode);
        Self { limit, token, cursor }
    }

    /// The first page at the default size — the listing a handler serves when
    /// its caller asked for no page in particular.
    pub fn first() -> Self {
        Self::new(DEFAULT_PAGE_LIMIT, None)
    }

    /// Rows this page may hold, already clamped.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The decoded resume position, if this page continues a listing.
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    /// True when a token was supplied but could not be decoded — the case a
    /// helper must refuse rather than treat as "start over".
    pub fn has_unreadable_token(&self) -> bool {
        self.token.is_some() && self.cursor.is_none()
    }
}

/// A bounded page of store rows, plus where the next page resumes.
///
/// Returned by the principal-scoped listing helper instead of a `Vec<Row>`.
/// The rows stay rows — a handler that must batch-load children off the page
/// (the eager-load shape) needs them before it can build its response — but
/// they arrive attached to the cursor that continues the listing, so a handler
/// cannot serve a page without having been handed the means to expose the rest.
pub struct RowPage {
    pub rows: Vec<crate::store::Row>,
    /// The token the next page resumes from. `None` means this was the last
    /// page — the only end-of-listing marker.
    pub next_cursor: Option<String>,
}

/// A bounded page of typed model rows, plus where the next page resumes.
///
/// What [`RowPage`] is to `Vec<Row>`, this is to `Vec<M>`: the return type of
/// the typed listing verb, carrying the cursor that continues it so a handler
/// cannot serve a page without having been handed the means to expose the rest.
pub struct ModelPage<M> {
    pub items: Vec<M>,
    /// The token the next page resumes from. `None` means this was the last
    /// page — the only end-of-listing marker.
    pub next_cursor: Option<String>,
}

impl<M> ModelPage<M> {
    /// Items on this page.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when this page carries no items at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// True when no page follows this one.
    pub fn is_last(&self) -> bool {
        self.next_cursor.is_none()
    }
}

impl RowPage {
    /// Rows on this page.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// True when no page follows this one.
    pub fn is_last(&self) -> bool {
        self.next_cursor.is_none()
    }

    /// Render this page as the SDK's wire envelope, carrying the cursor
    /// through. One call, so a handler cannot map the rows and forget the
    /// cursor — which would turn a bounded page back into a truncation.
    pub fn map<T, F>(&self, row_to_item: F) -> CursorPage<T>
    where
        T: Serialize,
        F: Fn(&crate::store::Row) -> T,
    {
        CursorPage {
            items: self.rows.iter().map(row_to_item).collect(),
            next_cursor: self.next_cursor.clone(),
        }
    }
}

/// Pagination response envelope. Serializes as
/// `{"items": [...], "next_cursor": "..."}` with `next_cursor`
/// omitted when there is no next page (last-page marker).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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

    // -- keyset_advanced tests --

    use serde_json::json;

    fn cur(id: &str, v: serde_json::Value) -> Cursor {
        Cursor { last_id: id.to_string(), last_value: v }
    }

    #[test]
    fn first_page_always_advances() {
        assert!(keyset_advanced("notes", "created_at", None, &cur("7", json!(1))).is_ok());
    }

    #[test]
    fn a_page_ending_on_a_new_row_advances() {
        let prev = cur("7", json!(1));
        assert!(keyset_advanced("notes", "created_at", Some(&prev), &cur("8", json!(1))).is_ok());
    }

    #[test]
    fn a_page_ending_on_the_same_row_is_refused_by_name() {
        let prev = cur("7", json!(1));
        let err = keyset_advanced("notes", "created_at", Some(&prev), &cur("7", json!(1)))
            .expect_err("repeating the boundary row is not progress");
        assert!(err.contains("cannot make progress"), "{err}");
        assert!(err.contains("created_at"), "the message must name the order column: {err}");
        assert!(err.contains("notes"), "the message must name the table: {err}");
    }

    /// The sort VALUE moving is not progress on its own — the row is the
    /// boundary, and a repeated row means the same page comes back.
    #[test]
    fn a_moving_sort_value_on_the_same_row_is_still_refused() {
        let prev = cur("7", json!(1));
        assert!(keyset_advanced("notes", "created_at", Some(&prev), &cur("7", json!(99))).is_err());
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

    // -- PageRequest: the type that makes an unbounded ask unrepresentable --

    #[test]
    fn page_request_clamps_an_enormous_limit() {
        // The shape this whole type exists to refuse. There is no `limit`
        // value — and no absence of one — that yields a whole-table read.
        assert_eq!(PageRequest::new(usize::MAX, None).limit(), MAX_PAGE_LIMIT);
        assert_eq!(PageRequest::new(MAX_PAGE_LIMIT + 1, None).limit(), MAX_PAGE_LIMIT);
        assert_eq!(PageRequest::new(1_000_000, None).limit(), MAX_PAGE_LIMIT);
    }

    #[test]
    fn page_request_clamps_zero_upward() {
        // A page of nothing carries no row to build a cursor from, so it would
        // end a walk that never started — a silent empty listing.
        assert_eq!(PageRequest::new(0, None).limit(), 1);
    }

    #[test]
    fn page_request_keeps_a_limit_it_can_honour() {
        assert_eq!(PageRequest::new(37, None).limit(), 37);
        assert_eq!(PageRequest::new(MAX_PAGE_LIMIT, None).limit(), MAX_PAGE_LIMIT);
        assert_eq!(PageRequest::first().limit(), DEFAULT_PAGE_LIMIT);
    }

    #[test]
    fn page_request_decodes_a_token_it_issued() {
        let token = encode(&Cursor::keyset("42", serde_json::json!(1_700)));
        let req = PageRequest::new(10, Some(token));
        assert!(!req.has_unreadable_token());
        let c = req.cursor().expect("a round-tripped token decodes");
        assert_eq!(c.last_id, "42");
        assert_eq!(c.last_value, serde_json::json!(1_700));
    }

    #[test]
    fn page_request_flags_a_token_it_cannot_read() {
        // Reported rather than dropped: a dropped token silently restarts a
        // listing the caller believes it is continuing, so the walk re-serves
        // page one forever and never terminates.
        let req = PageRequest::new(10, Some("!!!not-a-cursor!!!".to_string()));
        assert!(req.cursor().is_none());
        assert!(req.has_unreadable_token());
    }

    #[test]
    fn no_token_is_not_an_unreadable_token() {
        assert!(!PageRequest::first().has_unreadable_token());
    }

    // -- RowPage --

    fn row(id: i64, title: &str) -> crate::store::Row {
        crate::store::Row {
            columns: vec![
                ("_id".to_string(), Val::Integer(id)),
                ("title".to_string(), Val::Text(title.to_string())),
            ],
        }
    }

    #[test]
    fn row_page_map_carries_the_cursor_with_the_items() {
        // One call, so a handler cannot render the rows and drop the cursor —
        // which is how a bounded page turns back into a silent truncation.
        let page = RowPage {
            rows: vec![row(1, "a"), row(2, "b")],
            next_cursor: Some("tok".to_string()),
        };
        let out = page.map(|r| r.text("title"));
        assert_eq!(out.items, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(out.next_cursor.as_deref(), Some("tok"));
        assert!(!page.is_last());
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn row_page_without_a_cursor_is_the_last_page() {
        let page = RowPage { rows: vec![], next_cursor: None };
        assert!(page.is_last());
        assert!(page.is_empty());
        let out = page.map(|r| r.id());
        assert!(out.items.is_empty());
        assert!(out.next_cursor.is_none());
    }
}
