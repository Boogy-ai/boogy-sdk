//! Procedural macros for boogy-sdk. `#[derive(Model)]` + `#[job(...)]`.

// `PayloadKind::Typed` carries a `syn::Type`, which is large; the enum is a
// short-lived per-fn classification during macro expansion, so the variant
// size spread is immaterial and boxing would only add noise.
#![allow(clippy::large_enum_variant)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr};

/// Derive `boogy_sdk::model::Model` for a struct with named fields.
///
/// Attributes:
/// - `#[model(table = "name")]` (struct) — table name; defaults to the
///   snake_case of the struct identifier.
/// - `#[model(index(name = "...", cols = ["a", "b"]))]` /
///   `#[model(unique_index(name = "...", cols = [...]))]` /
///   `#[model(covering_index(name = "...", cols = [...]))]` (struct, repeatable)
///   — composite indexes. `covering_index` stores a copy of the row in the
///   index entry so an index walk skips the per-row fetch (read-fast-path;
///   costs write amplification on row updates).
/// - `#[pk]` (field) — maps to the store auto-PK `_id`; excluded from
///   `to_columns`; read from `_id`.
/// - `#[index]` (field) — single-column index named `idx_<table>_<col>`.
/// - `#[covering_index]` (field) — single-column covering index (see above),
///   same name `idx_<table>_<col>`.
/// - `#[model(column = "name")]` (field) — override the column name.
/// - `#[model(list_by(filter = "...", newest = "..." | oldest = "..."))]`
///   (struct, repeatable) — declares a filtered-and-ordered list access
///   pattern; resolves to a covering composite index.
/// - `#[model(ranked_by(highest = "..." | lowest = "..."))]` (struct,
///   repeatable) — declares a global ranked feed; resolves to a single-column
///   covering index.
/// - `#[model(tagged_by(tag = "...", refs = "..."))]` (struct, repeatable) —
///   declares a junction/side-table membership pattern; resolves to a covering
///   `[tag, refs]` index.
/// - `#[lookup_by]` (field) — declares a unique point-lookup access pattern on
///   the field's column; resolves to a UNIQUE single-column index.
/// - `#[counter]` (field) — conflict-free counter column. The value is kept in
///   its own cell rather than inside the row, and an increment is an atomic add
///   that takes no read-conflict range, so concurrent increments compose instead
///   of conflicting.
///
///   **The field is read-only.** Reads merge the real value in, but writes go
///   ONLY through the increment path: the field is excluded from `to_columns`,
///   so `db_insert` starts it at zero and `db_update` does not mention the
///   column at all. That exclusion is the point — a counter field still
///   round-trips as an ordinary struct field, so
///   `db_update(id, &Row { title, ..row })` to change something else would
///   otherwise write back the counter value read earlier and discard every
///   increment made since.
///
///   The column is a **64-bit signed integer** and the delta must be an integer
///   too — a fractional delta is rejected by the store, because the atomic add
///   operates on the integer cell directly. The add **wraps** on overflow rather
///   than erroring or saturating: pushing the value past `i64::MAX` rolls it
///   over to `i64::MIN`. That is 9.2 quintillion increments away for a counting
///   workload, but it is a real edge for a counter accumulating large deltas
///   (byte totals, currency in minor units) — clamp the delta on the way in if
///   your values can approach the limit.
///
///   **A counter read is not serialized against concurrent increments.** Reads
///   merge the real value in, but they deliberately take no read-conflict range
///   on the counter's cell — that is what keeps reading a counter from
///   re-introducing the very conflict the atomic add removes. The consequence is
///   a rule, and breaking it fails silently:
///
///   > Never gate a write on a counter — or on anything derived from one — read
///   > in the same transaction.
///
///   "Derived from one" is the part that is easy to miss: a `count_rows` whose
///   filter names a counter, a `find` that filters or sorts on one, "how many
///   rows are below the threshold" — none of those is a *value*, and all of them
///   are a snapshot reading you can branch on.
///
///   ```ignore
///   tx(|| {
///       let post = db_get::<Post>(id)?;      // may already be stale
///       if post.vote_score < -10 { db_delete::<Post>(id)?; }   // WRONG
///       Ok(())
///   })
///
///   tx(|| {
///       let n = count_rows(Post::TABLE, vec![filter_lt(Post::VOTE_SCORE, v)])?;
///       if n > 5 { db_insert(&alert)?; }     // WRONG — a count is derived too
///       Ok(())
///   })
///   ```
///
///   An increment that commits between that read and the commit does not make
///   the transaction retry — it is simply discarded. When the decision must
///   hold, express it as a predicate instead (`store::delete_where` /
///   `store::update_where` with the counter in the filters): those serialize the
///   rows they actually MATCH against concurrent increments, so an increment
///   that lifts a matched row out of the predicate becomes a retryable conflict
///   rather than a row acted on with a value that no longer matches.
///
///   That serialization stops at the matched rows, by design: an increment on a
///   row the sweep did NOT match does not conflict with it (the outcome equals
///   running the sweep first and the increment after). Conflict-checking every
///   row the sweep scanned instead would make a full-table sweep lose to any
///   increment anywhere in the table — on the workload `#[counter]` exists for,
///   a sweep that never lands.
///
///   Reads that only report a value (get, list, count) need none of this — they
///   hand the reading to the caller and stop. It is the branch-then-write that
///   the rule is about.
///
///   Increments themselves stay conflict-free only with an EMPTY `always` on
///   the UPDATE arm. Three ways to carry a companion column on
///   `upsert_increment`, and they are not equivalent:
///
///   - **hot and standalone** — `UpsertColumns::none()`: neither arm writes
///     anything but the counter cell, so concurrent increments compose;
///   - **needed once, at creation** — `UpsertColumns::on_insert_only(&[...])`:
///     a value computed at call time (a timestamp, a derived id), so it has
///     nowhere to live as a static `default`. Written only by the row-creating
///     call and never touched again, so it costs a later increment nothing;
///   - **must change on every call** — `UpsertColumns::always(&[...])`: the
///     right choice only for a value that genuinely needs to keep changing.
///     An `upsert_increment` carrying a non-empty `always` rewrites the whole
///     row on every call, an ordinary read-modify-write that conflicts like
///     any other.
///
///   `on_insert` does not buy conflict-freedom everywhere, though: on the
///   read-modify-write arm — incrementing a plain, non-`#[counter]` column —
///   the counter's own new value is written through the ordinary row update
///   regardless of `on_insert`. Only a `#[counter]` column's atomic add is
///   conflict-free; `on_insert` only narrows which OTHER columns ride along.
///
///   A counter column **cannot back an index** (the derive rejects `#[index]`,
///   `#[lookup_by]`, `#[covering_index]`, struct-level index `cols`, and any
///   `list_by`/`ranked_by`/`tagged_by` column at compile time): index
///   maintenance needs the previous value to clear the stale entry, and an
///   atomic add never reads one.
///
///   `#[counter]` takes **no arguments** — in particular there is no
///   `index` option, and `#[counter(index = true)]` is a compile error rather
///   than a silently-discarded one. Every field marker below is bare for the
///   same reason: `attr.path().is_ident(..)` matches `#[x]`, `#[x(..)]` and
///   `#[x = ..]` alike, so an unchecked argument would vanish without a
///   diagnostic and leave the author believing a storage attribute was
///   configured when it never was.
///
/// - `#[default = <literal>]` — the column's default, the equivalent of
///   `status TEXT DEFAULT 'pending'`:
///
///   ```ignore
///   #[default = "pending"] pub status: String,
///   #[default = 0]         pub retries: i64,
///   ```
///
///   A **negative** number uses the parenthesized spelling — `#[default(-1)]` —
///   because rustc rejects a non-literal after `=` in any attribute before the
///   derive can see it.
///
///   Unlike every other field marker this one *takes* a value, so it is checked
///   from the opposite side: a bare `#[default]` is a compile error, as is a
///   non-literal value and a literal whose kind does not match the field's
///   column type. Literal values only — there are no expression defaults.
///
///   Declaring or changing a default never rewrites stored rows: a write that
///   omits the column records the default in force at that moment, while a row
///   that predates the column resolves against the current default on every
///   read. A defaulted column also satisfies the not-null requirement, which
///   makes this the way to add a required field.
///
/// There is no field-level `#[unique]`. It existed, compiled, and enforced
/// nothing — it set a column flag no write path reads, while emitting no index,
/// so duplicate values were accepted silently by a model that had declared they
/// could not be. Uniqueness is enforced by an INDEX: use `#[lookup_by]` for a
/// single column, or `#[model(unique_index(cols = [..]))]` for a composite. The
/// attribute is still *registered* below purely so the derive can reject it with
/// that explanation rather than leaving rustc to say "cannot find attribute".
#[proc_macro_derive(Model, attributes(model, pk, unique, index, covering_index, lookup_by, counter, default))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_ident = input.ident.clone();

    // --- struct-level #[model(...)] ---
    let mut table_name = to_snake_case(&struct_ident.to_string());
    // (index name, cols, unique, covering)
    let mut struct_indexes: Vec<(String, Vec<String>, bool, bool)> = Vec::new();
    // (column, pattern name) for every column an access pattern sorts or
    // filters on. Collected in parallel because the patterns below are quoted
    // into token streams immediately, after which their column names cannot be
    // recovered — and the `#[counter]` rejection needs them.
    let mut pattern_cols: Vec<(String, &'static str)> = Vec::new();
    // Accumulated `__t.access_patterns.push(...)` token streams from the
    // struct-level access-pattern verbs (list_by / ranked_by).
    let mut access_patterns: Vec<TokenStream2> = Vec::new();
    for attr in &input.attrs {
        if !attr.path().is_ident("model") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let s: LitStr = meta.value()?.parse()?;
                table_name = s.value();
                Ok(())
            } else if meta.path.is_ident("list_by") {
                // list_by(filter = "...", newest = "..." | oldest = "...")
                let (mut filter, mut newest, mut oldest) =
                    (String::new(), String::new(), String::new());
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("filter") {
                        filter = m.value()?.parse::<LitStr>()?.value();
                    } else if m.path.is_ident("newest") {
                        newest = m.value()?.parse::<LitStr>()?.value();
                    } else if m.path.is_ident("oldest") {
                        oldest = m.value()?.parse::<LitStr>()?.value();
                    } else {
                        return Err(m.error("list_by expects filter + newest|oldest"));
                    }
                    Ok(())
                })?;
                if filter.is_empty() {
                    return Err(meta.error("list_by requires a `filter = \"...\"`"));
                }
                if newest.is_empty() == oldest.is_empty() {
                    return Err(meta.error("list_by requires exactly one of `newest`/`oldest`"));
                }
                let desc = !newest.is_empty();
                let order_col = if desc { newest } else { oldest };
                pattern_cols.push((filter.clone(), "list_by(filter)"));
                pattern_cols.push((order_col.clone(), "list_by sort column"));
                access_patterns.push(quote! {
                    __t.access_patterns.push(::boogy_sdk::store::AccessPattern::ListBy {
                        filter: #filter.into(),
                        order: ::boogy_sdk::store::Order { column: #order_col.into(), desc: #desc },
                    });
                });
                Ok(())
            } else if meta.path.is_ident("ranked_by") {
                // ranked_by(highest = "..." | lowest = "...")
                let (mut highest, mut lowest) = (String::new(), String::new());
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("highest") {
                        highest = m.value()?.parse::<LitStr>()?.value();
                    } else if m.path.is_ident("lowest") {
                        lowest = m.value()?.parse::<LitStr>()?.value();
                    } else {
                        return Err(m.error("ranked_by expects highest|lowest"));
                    }
                    Ok(())
                })?;
                if highest.is_empty() == lowest.is_empty() {
                    return Err(meta.error("ranked_by requires exactly one of `highest`/`lowest`"));
                }
                let desc = !highest.is_empty();
                let order_col = if desc { highest } else { lowest };
                pattern_cols.push((order_col.clone(), "ranked_by"));
                access_patterns.push(quote! {
                    __t.access_patterns.push(::boogy_sdk::store::AccessPattern::RankedBy {
                        order: ::boogy_sdk::store::Order { column: #order_col.into(), desc: #desc },
                    });
                });
                Ok(())
            } else if meta.path.is_ident("tagged_by") {
                // tagged_by(tag = "...", refs = "...")
                let (mut tag, mut refs) = (String::new(), String::new());
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("tag") {
                        tag = m.value()?.parse::<LitStr>()?.value();
                    } else if m.path.is_ident("refs") {
                        refs = m.value()?.parse::<LitStr>()?.value();
                    } else {
                        return Err(m.error("tagged_by expects tag + refs"));
                    }
                    Ok(())
                })?;
                if tag.is_empty() || refs.is_empty() {
                    return Err(meta.error("tagged_by requires both `tag = \"...\"` and `refs = \"...\"`"));
                }
                // `tagged_by` resolves to a COVERING two-column index [tag, refs]
                // (see schema_resolve), so `refs` is an index key column just as
                // much as `tag` is. Checking only `tag` left a counter able to
                // back a covering index — worse than a plain one, because a
                // covering index serves the row payload from the index itself,
                // so stale entries are never corrected by a row fetch.
                pattern_cols.push((tag.clone(), "tagged_by(tag)"));
                pattern_cols.push((refs.clone(), "tagged_by(refs)"));
                access_patterns.push(quote! {
                    __t.access_patterns.push(::boogy_sdk::store::AccessPattern::TaggedBy {
                        tag: #tag.into(),
                        refs: #refs.into(),
                    });
                });
                Ok(())
            } else if meta.path.is_ident("index")
                || meta.path.is_ident("unique_index")
                || meta.path.is_ident("covering_index")
            {
                let unique = meta.path.is_ident("unique_index");
                let covering = meta.path.is_ident("covering_index");
                let mut name = String::new();
                let mut cols: Vec<String> = Vec::new();
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("name") {
                        name = m.value()?.parse::<LitStr>()?.value();
                    } else if m.path.is_ident("cols") {
                        let arr: syn::ExprArray = m.value()?.parse()?;
                        for e in arr.elems {
                            if let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = e {
                                cols.push(s.value());
                            }
                        }
                    } else {
                        return Err(m.error("unknown index attribute key"));
                    }
                    Ok(())
                })?;
                struct_indexes.push((name, cols, unique, covering));
                Ok(())
            } else {
                Err(meta.error("unknown model attribute"))
            }
        })?;
    }

    // --- fields ---
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    "#[derive(Model)] requires named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                &struct_ident,
                "#[derive(Model)] can only be applied to structs",
            ))
        }
    };

    /// Reject arguments on a field marker that takes none.
    ///
    /// `attr.path().is_ident("x")` matches `#[x]`, `#[x(..)]` and `#[x = ..]`
    /// alike, so without this check an argument is **silently discarded**: the
    /// author writes `#[counter(index = true)]` or a field-level
    /// `#[index(cols = [..])]`, gets a clean build, and believes the derive
    /// saw something it never did. For storage attributes that surfaces later
    /// as wrong data or a missing index, with nothing to trace it back to —
    /// so refusing to build is strictly better, the same reasoning as the
    /// counter/index rejections below.
    ///
    /// `hint` adds a marker-specific sentence when the wrong argument is a
    /// predictable one worth naming outright.
    fn deny_marker_args(attr: &syn::Attribute, name: &str, hint: &str) -> syn::Result<()> {
        if matches!(attr.meta, syn::Meta::Path(_)) {
            return Ok(());
        }
        Err(syn::Error::new_spanned(
            attr,
            format!("#[{name}] takes no arguments — write a bare `#[{name}]`.{hint}"),
        ))
    }

    /// Parse `#[default = <literal>]` into the `Val` expression the schema will
    /// carry, plus a human name for the literal's kind (used by the field-type
    /// check below).
    ///
    /// This is `deny_marker_args`' mirror image, and it exists for the same
    /// reason. `attr.path().is_ident("default")` matches `#[default]`,
    /// `#[default(..)]` and `#[default = ..]` alike, so without an explicit
    /// rejection the two malformed spellings would be **silently discarded**:
    /// the author declares a column default, the build is clean, and the column
    /// reads back null forever with nothing to trace it to. Every one of these
    /// arms is a compile error rather than a shrug.
    fn parse_default_attr(
        attr: &syn::Attribute,
    ) -> syn::Result<(proc_macro2::TokenStream, &'static str)> {
        const FORMS: &str = "Supported literals: string (`= \"pending\"`), integer \
                             (`= 0`), float (`= 1.5`), boolean (`= true`), byte string \
                             (`= b\"\\x00\"`). A NEGATIVE number must use the \
                             parenthesized form — `#[default(-1)]` — because rustc \
                             rejects a non-literal on the right of `=` in any attribute.";
        // Two accepted spellings. `#[default = <lit>]` is the normal one;
        // `#[default(<expr>)]` exists ONLY because rustc refuses `-1` after `=`
        // ("attribute value must be a literal") before the derive ever sees it,
        // which would otherwise leave negative defaults inexpressible.
        let value: syn::Expr = match &attr.meta {
            syn::Meta::NameValue(nv) => nv.value.clone(),
            syn::Meta::List(l) => l.parse_args::<syn::Expr>().map_err(|_| {
                syn::Error::new_spanned(
                    attr,
                    format!("#[default(...)] takes a single literal value. {FORMS}"),
                )
            })?,
            syn::Meta::Path(_) => {
                return Err(syn::Error::new_spanned(
                    attr,
                    format!("#[default] needs a value — write `#[default = <literal>]`. {FORMS}"),
                ))
            }
        };

        // `-1` / `-1.5` parse as a unary-negation expression, not a literal.
        let (lit, negated) = match &value {
            syn::Expr::Lit(l) => (&l.lit, false),
            syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => match &*u.expr {
                syn::Expr::Lit(l) => (&l.lit, true),
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        format!("#[default] needs a literal value. {FORMS}"),
                    ))
                }
            },
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    format!(
                        "#[default] needs a LITERAL value — expressions, consts and \
                         function calls are not evaluated by the derive and there are no \
                         expression defaults in the store. {FORMS}"
                    ),
                ))
            }
        };

        let neg_err = |l: &syn::Lit| {
            syn::Error::new_spanned(l, format!("#[default]: this literal cannot be negated. {FORMS}"))
        };
        Ok(match lit {
            syn::Lit::Str(s) if !negated => {
                let v = s.value();
                (quote! { ::boogy_sdk::store::Val::Text(#v.to_string()) }, "string")
            }
            syn::Lit::Int(i) => {
                let mut v: i64 = i.base10_parse()?;
                if negated {
                    v = v.checked_neg().ok_or_else(|| {
                        syn::Error::new_spanned(i, "#[default]: integer out of range for i64")
                    })?;
                }
                (quote! { ::boogy_sdk::store::Val::Integer(#v) }, "integer")
            }
            syn::Lit::Float(f) => {
                let v: f64 = f.base10_parse()?;
                let v = if negated { -v } else { v };
                (quote! { ::boogy_sdk::store::Val::Real(#v) }, "float")
            }
            syn::Lit::Bool(b) if !negated => {
                let v = b.value();
                (quote! { ::boogy_sdk::store::Val::Boolean(#v) }, "boolean")
            }
            syn::Lit::ByteStr(b) if !negated => {
                let bytes = b.value();
                (
                    quote! { ::boogy_sdk::store::Val::Blob(::std::vec![ #(#bytes),* ]) },
                    "byte string",
                )
            }
            other if negated => return Err(neg_err(other)),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    format!("#[default]: unsupported literal type. {FORMS}"),
                ))
            }
        })
    }

    /// The literal kind a field type's column can hold, for the types the derive
    /// knows. `None` = a custom `Field` impl the derive cannot reason about, so
    /// no check is applied.
    ///
    /// This catches the realistic mistake — `#[default = "0"]` on an `i64`, or
    /// `#[default = 0]` on a `String` — which would otherwise store a default of
    /// the wrong storage class and surface as a wrong value on read.
    fn expected_default_kind(ty: &syn::Type) -> Option<&'static str> {
        let path = match ty {
            syn::Type::Path(p) => &p.path,
            _ => return None,
        };
        let seg = path.segments.last()?;
        if seg.ident == "Option" {
            // `Option<Inner>` stores Inner's column type.
            if let syn::PathArguments::AngleBracketed(a) = &seg.arguments {
                if let Some(syn::GenericArgument::Type(inner)) = a.args.first() {
                    return expected_default_kind(inner);
                }
            }
            return None;
        }
        match seg.ident.to_string().as_str() {
            // `Decimal` is stored as fixed-precision TEXT, so its default is a
            // string like "1.500000" — not a float literal.
            "String" | "Decimal" => Some("string"),
            "i64" | "u64" | "Timestamp" | "Id" => Some("integer"),
            "f64" => Some("float"),
            "bool" => Some("boolean"),
            _ => None,
        }
    }

    struct FieldInfo {
        ident: syn::Ident,
        ty: syn::Type,
        column: String,
        is_pk: bool,
        index: bool,
        covering: bool,
        counter: bool,
        default: Option<proc_macro2::TokenStream>,
    }

    let mut field_infos: Vec<FieldInfo> = Vec::new();
    // Field-level `#[lookup_by]` columns (resolved to LookupBy access patterns
    // after the column name override is known).
    let mut lookup_by_cols: Vec<String> = Vec::new();
    for f in fields {
        let ident = f.ident.clone().unwrap();
        let mut column = ident.to_string();
        let mut is_pk = false;
        let mut index = false;
        let mut covering = false;
        let mut lookup_by = false;
        let mut counter = false;
        let mut default: Option<proc_macro2::TokenStream> = None;
        for attr in &f.attrs {
            if attr.path().is_ident("pk") {
                deny_marker_args(attr, "pk", "")?;
                is_pk = true;
            } else if attr.path().is_ident("unique") {
                // --- #[unique] enforces nothing, so the derive refuses it -----
                //
                // It used to compile and set a column flag (`ColDef.unique` →
                // `ColSpec.unique`) that no write path reads. The store's only
                // uniqueness probe is driven by the table's INDEX list, and
                // `#[unique]` emitted no index — so a duplicate insert into a
                // column the model declared unique succeeded silently.
                //
                // Rejecting rather than quietly emitting an index is the same
                // call as `deny_marker_args` above and the #[counter]/index
                // rejection below: a storage declaration that is silently
                // discarded surfaces later as wrong data, with nothing to trace
                // it back to. And emitting one would change the storage layout
                // of every model carrying the attribute for a guarantee nobody
                // can currently rely on — while still being ineffective on a
                // table that already holds duplicates.
                return Err(syn::Error::new_spanned(
                    attr,
                    "#[unique] is not supported: it enforces nothing. The derive set a \
                     column flag the store never reads, so duplicate values were accepted \
                     silently by a model that declared they could not be. A uniqueness \
                     constraint is enforced by an INDEX, so declare one:\n  \
                     - single column: replace #[unique] with #[lookup_by] on this field \
                     (a UNIQUE single-column index, and the point lookup that goes with it)\n  \
                     - composite: drop #[unique] and declare \
                     #[model(unique_index(name = \"...\", cols = [\"a\", \"b\"]))] on the struct",
                ));
            } else if attr.path().is_ident("index") {
                deny_marker_args(
                    attr,
                    "index",
                    " A multi-column index is declared on the STRUCT, as \
                     #[model(index(cols = [\"a\", \"b\"]))] — prefer an access-pattern \
                     verb (list_by / ranked_by / lookup_by), which derives it for you.",
                )?;
                index = true;
            } else if attr.path().is_ident("covering_index") {
                deny_marker_args(
                    attr,
                    "covering_index",
                    " A multi-column covering index is declared on the STRUCT, as \
                     #[model(covering_index(cols = [\"a\", \"b\"]))].",
                )?;
                index = true;
                covering = true;
            } else if attr.path().is_ident("lookup_by") {
                deny_marker_args(
                    attr,
                    "lookup_by",
                    " It is always a UNIQUE single-column point lookup; there is \
                     nothing to configure.",
                )?;
                lookup_by = true;
            } else if attr.path().is_ident("counter") {
                deny_marker_args(
                    attr,
                    "counter",
                    " In particular there is no `index` option: a counter column \
                     cannot back an index at all, because an atomic add never reads \
                     the previous value and the stale entry could never be removed. \
                     To rank or filter on this value, scope the ranking to a bounded \
                     sub-range and sort in memory, or materialize it into a separate \
                     plain column refreshed by a background job.",
                )?;
                counter = true;
            } else if attr.path().is_ident("default") {
                let (val, kind) = parse_default_attr(attr)?;
                if let Some(expected) = expected_default_kind(&f.ty) {
                    if expected != kind {
                        return Err(syn::Error::new_spanned(
                            attr,
                            format!(
                                "#[default] is a {kind} literal, but this field's column \
                                 stores a value of kind `{expected}`. The default would be \
                                 written with the wrong storage class and read back as the \
                                 wrong value, so the derive refuses it rather than \
                                 emitting it."
                            ),
                        ));
                    }
                }
                default = Some(val);
            } else if attr.path().is_ident("model") {
                attr.parse_nested_meta(|m| {
                    if m.path.is_ident("column") {
                        column = m.value()?.parse::<LitStr>()?.value();
                        Ok(())
                    } else {
                        Err(m.error("unknown field model attribute"))
                    }
                })?;
            }
        }
        if lookup_by {
            if is_pk {
                return Err(syn::Error::new_spanned(
                    &ident,
                    "#[lookup_by] cannot be applied to the #[pk] field (the PK is already a point lookup)",
                ));
            }
            lookup_by_cols.push(column.clone());
        }
        // A `#[pk]` field maps to the store's auto-assigned `_id`, which the
        // derive never emits a ColDef for — so a default on it would be dropped
        // on the floor. Refuse rather than discard.
        if is_pk && default.is_some() {
            return Err(syn::Error::new_spanned(
                &ident,
                "#[default] cannot be applied to the #[pk] field — the primary key is \
                 assigned by the store, so a default for it would never be used",
            ));
        }
        // A counter column's value is read from its own cell and merged over the
        // decoded row AFTER defaults are resolved, so a default on one could
        // never be observed. (An absent counter already reads as 0.) The store
        // rejects this too; the derive catches it at compile time.
        if counter && default.is_some() {
            return Err(syn::Error::new_spanned(
                &ident,
                "#[counter] cannot be combined with #[default]: a counter's value is read \
                 from its own cell, so the default would never be observed. An absent \
                 counter already reads as 0.",
            ));
        }
        field_infos.push(FieldInfo { ident, ty: f.ty.clone(), column, is_pk, index, covering, counter, default });
    }

    // Emit a LookupBy access pattern per `#[lookup_by]` field.
    for col in &lookup_by_cols {
        access_patterns.push(quote! {
            __t.access_patterns.push(::boogy_sdk::store::AccessPattern::LookupBy {
                column: #col.into(),
            });
        });
    }

    let pk_count = field_infos.iter().filter(|f| f.is_pk).count();
    if pk_count > 1 {
        return Err(syn::Error::new_spanned(
            &struct_ident,
            "#[derive(Model)] allows at most one #[pk] field",
        ));
    }

    // --- every access-pattern / index column must name a declared column ----
    //
    // Without this the derive happily emits an index over a column that does
    // not exist — and worse, it makes the #[counter] rejection below bypassable:
    // the patterns carry AUTHOR-written names while a field's stored name can be
    // changed with #[model(column = "...")], so a renamed counter never matches
    // and silently ends up backing an index.
    {
        let declared: Vec<&str> = field_infos
            .iter()
            .map(|f| if f.is_pk { "_id" } else { f.column.as_str() })
            .collect();
        let known = |c: &String| c == "_id" || declared.iter().any(|d| d == c);
        let near = |c: &String| {
            field_infos
                .iter()
                .find(|f| f.ident == *c)
                .map(|f| format!(" (did you mean the stored name `{}`?)", f.column))
                .unwrap_or_default()
        };
        for (col, pattern) in &pattern_cols {
            if !known(col) {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    format!(
                        "`{pattern}` names column `{col}`, which this model does not \
                         declare{}. An index would be built over a column that does not \
                         exist.",
                        near(col)
                    ),
                ));
            }
        }
        for (name, cols, _, _) in &struct_indexes {
            for col in cols {
                if !known(col) {
                    return Err(syn::Error::new_spanned(
                        &struct_ident,
                        format!(
                            "index `{name}` names column `{col}`, which this model does \
                             not declare{}.",
                            near(col)
                        ),
                    ));
                }
            }
        }
    }

    // --- #[counter] may not back an index -----------------------------------
    //
    // A counter column is mutated by an atomic add, which never reads the
    // previous value — and index maintenance needs exactly that value to remove
    // the stale entry. So an indexed counter cannot be kept correct.
    //
    // This is a COMPILE error rather than a runtime one on purpose. The runtime
    // alternative is an index that looks maintained and silently is not, which
    // returns wrong answers instead of failing; refusing to build is strictly
    // better than that.
    for f in field_infos.iter().filter(|f| f.counter) {
        if f.is_pk {
            return Err(syn::Error::new_spanned(
                &f.ident,
                "#[counter] cannot be applied to the #[pk] field — the primary key \
                 identifies the row and cannot be an atomically-added value",
            ));
        }
        // `#[unique]` is deliberately absent from this list. It used to appear
        // here on the premise that it backed an index; it never did, and it is
        // now rejected outright at the attribute-parsing site above, so a
        // `#[counter] #[unique]` field can no longer reach this check. Listing
        // it would be an arm that can never fire.
        let offender = if f.covering {
            Some("#[covering_index]")
        } else if f.index {
            Some("#[index]")
        } else if lookup_by_cols.contains(&f.column) {
            Some("#[lookup_by]")
        } else {
            None
        };
        if let Some(attr) = offender {
            return Err(syn::Error::new_spanned(
                &f.ident,
                format!(
                    "#[counter] cannot be combined with {attr}: a counter column cannot \
                     back an index. An atomic add never reads the previous value, so the \
                     old index entry could never be removed. To rank or filter on this \
                     value, either scope the ranking to a bounded sub-range and sort in \
                     memory, or materialize it into a separate plain column refreshed by \
                     a background job."
                ),
            ));
        }
        for (name, cols, _, _) in &struct_indexes {
            if cols.contains(&f.column) {
                return Err(syn::Error::new_spanned(
                    &f.ident,
                    format!(
                        "#[counter] column `{}` is used by index `{}`: a counter column \
                         cannot back an index, because an atomic add never reads the \
                         previous value and the old entry could never be removed.",
                        f.column, name
                    ),
                ));
            }
        }
        for (col, pattern) in &pattern_cols {
            if col == &f.column {
                return Err(syn::Error::new_spanned(
                    &f.ident,
                    format!(
                        "#[counter] column `{}` is used by `{}`, which is backed by an \
                         index, and a counter column cannot back one. Scope the ranking \
                         to a bounded sub-range and sort in memory, or materialize it \
                         into a plain column refreshed by a background job.",
                        f.column, pattern
                    ),
                ));
            }
        }
    }

    // --- column-name consts: `pub const FIELD: &str = "column";` ---
    // pk fields are excluded: their store column is `_id`, not the field name.
    let const_defs = field_infos.iter().filter(|f| !f.is_pk).map(|f| {
        let cname = format_ident!("{}", f.ident.to_string().to_uppercase());
        let col = &f.column;
        quote! { pub const #cname: &'static str = #col; }
    });

    // --- schema(): push a ColDef per non-pk field, plus indexes ---
    let col_pushes = field_infos.iter().filter(|f| !f.is_pk).map(|f| {
        let ty = &f.ty;
        let col = &f.column;
        let counter = f.counter;
        // `unique` is always false: the derive has no field marker that sets it
        // any more, because the flag is inert — nothing on any write path reads
        // it. A derived model states its uniqueness constraints as UNIQUE
        // indexes (`#[lookup_by]`, `#[model(unique_index(...))]`), which the
        // store does enforce.
        let default = match &f.default {
            Some(v) => quote! { ::core::option::Option::Some(#v) },
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            __t.columns.push(::boogy_sdk::model::col_def_for::<#ty>(#col, false, #counter, #default));
        }
    });
    let field_index_pushes = field_infos.iter().filter(|f| f.index && !f.is_pk).map(|f| {
        let col = &f.column;
        let idx_name = format!("idx_{}_{}", table_name, f.column);
        let covering = f.covering;
        quote! {
            __t.indices.push(::boogy_sdk::store::Index {
                name: #idx_name.to_string(),
                columns: vec![#col.to_string()],
                unique: false,
                covering: #covering,
            });
        }
    });
    let struct_index_pushes = struct_indexes.iter().map(|(name, cols, unique, covering)| {
        let cols_lit = cols.iter().map(|c| quote! { #c.to_string() });
        quote! {
            __t.indices.push(::boogy_sdk::store::Index {
                name: #name.to_string(),
                columns: vec![ #(#cols_lit),* ],
                unique: #unique,
                covering: #covering,
            });
        }
    });

    // --- from_row ---
    let from_row_fields = field_infos.iter().map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        let key = if f.is_pk { "_id".to_string() } else { f.column.clone() };
        quote! {
            #ident: <#ty as ::boogy_sdk::model::Field>::from_val(row.get(#key)),
        }
    });

    // --- to_columns (non-pk, non-counter) ---
    //
    // A `#[counter]` field is deliberately absent. `to_columns` feeds both
    // `db_insert` and `db_update`, and a counter column is written ONLY by the
    // increment path — `db_update(id, &Row { title, ..row })` would otherwise
    // carry the counter value the author read earlier and overwrite every atomic
    // add made since. Emitting nothing (rather than emitting a value the store
    // then ignores) keeps the wire honest: the update genuinely does not mention
    // the column. The field stays on the struct and still reads back its true
    // value; it is simply read-only.
    let to_col_pushes = field_infos.iter().filter(|f| !f.is_pk && !f.counter).map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        quote! {
            (#col.to_string(), ::boogy_sdk::model::Field::to_val(&self.#ident)),
        }
    });

    // --- id() ---
    let id_body = match field_infos.iter().find(|f| f.is_pk) {
        Some(pk) => {
            let ident = &pk.ident;
            // Both u64 and Id<T> need to yield u64. Encode via Field::to_val
            // -> Integer, which works uniformly for u64 and Id<T>.
            quote! {
                match ::boogy_sdk::model::Field::to_val(&self.#ident) {
                    ::boogy_sdk::store::Val::Integer(i) => ::core::option::Option::Some(i as u64),
                    _ => ::core::option::Option::None,
                }
            }
        }
        None => quote! { ::core::option::Option::None },
    };

    let expanded = quote! {
        impl #struct_ident {
            #(#const_defs)*
        }

        impl ::boogy_sdk::model::Model for #struct_ident {
            const TABLE: &'static str = #table_name;

            fn schema() -> ::boogy_sdk::store::Table {
                let mut __t = ::boogy_sdk::store::Table::new(#table_name);
                #(#col_pushes)*
                #(#field_index_pushes)*
                #(#struct_index_pushes)*
                #(#access_patterns)*
                __t
            }

            fn from_row(row: &::boogy_sdk::store::Row) -> Self {
                Self {
                    #(#from_row_fields)*
                }
            }

            fn to_columns(&self) -> ::std::vec::Vec<(::std::string::String, ::boogy_sdk::store::Val)> {
                ::std::vec![
                    #(#to_col_pushes)*
                ]
            }

            fn id(&self) -> ::core::option::Option<u64> {
                #id_body
            }
        }
    };

    Ok(expanded)
}

/// snake_case a PascalCase identifier (e.g. `UserAffinityEdge` -> `user_affinity_edge`).
fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// #[job(...)] attribute macro
// ---------------------------------------------------------------------------

/// `#[job("name")]` (exact) or `#[job(prefix = "name_")]` (prefix-matched).
///
/// Annotates a free function whose signature is one of (each may take an
/// optional leading `ctx: JobContext` to read `ctx.attempts` etc.):
///   - `fn() -> Result<(), E>`
///   - `fn() -> Result<R, E>`                     where R: Serialize
///   - `fn(payload: T) -> …`                      where T: DeserializeOwned
///   - `fn(payload: Vec<u8>) -> …`                (raw bytes; no deserialization)
///   - `fn(suffix: &str) -> …`                    (prefix form, no payload)
///   - `fn(suffix: &str, payload: T) -> …`        (prefix form + typed payload)
///   - `fn(suffix: &str, payload: Vec<u8>) -> …`  (prefix form + raw bytes)
///   - `fn(ctx: JobContext, payload: T) -> …`     (+ any of the above)
///
/// The error type `E` is either `String` (treated as retryable) or
/// `boogy_sdk::JobError` (explicit `Retry`/`Terminal` control).
///
/// The original function name is replaced by a `pub fn <name>() -> JobRegistration`
/// constructor. Register it via `JobRouter::new().exact(my_job)` or `.prefix(my_job)`.
/// The actual function body is renamed to `__job_<name>_inner` and called from inside
/// the handler closure — it is an implementation detail and should not be called directly.
#[proc_macro_attribute]
pub fn job(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as JobAttr);
    let user_fn = parse_macro_input!(item as syn::ItemFn);
    match expand_job(args, user_fn) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Parsed form of `#[job(...)]` arguments.
enum JobAttr {
    Exact(String),
    Prefix(String),
}

impl syn::parse::Parse for JobAttr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        if lookahead.peek(LitStr) {
            // `#[job("name")]`
            let name: LitStr = input.parse()?;
            Ok(JobAttr::Exact(name.value()))
        } else if lookahead.peek(syn::Ident) {
            // `#[job(prefix = "name_")]`
            let ident: syn::Ident = input.parse()?;
            if ident != "prefix" {
                return Err(syn::Error::new(
                    ident.span(),
                    "expected `prefix = \"…\"` or a string literal",
                ));
            }
            let _eq: syn::token::Eq = input.parse()?;
            let value: LitStr = input.parse()?;
            Ok(JobAttr::Prefix(value.value()))
        } else {
            Err(lookahead.error())
        }
    }
}

/// What kind of payload the user fn accepts (if any).
enum PayloadKind {
    /// No payload argument.
    None,
    /// Raw `Vec<u8>` — passed through without deserialization.
    Bytes,
    /// A typed `T: DeserializeOwned` — deserialized from JSON.
    Typed(syn::Type),
}

/// Inspect the fn signature and return `(takes_ctx, is_prefix_form, payload_kind)`.
///
/// An optional leading `ctx: JobContext` argument is stripped first. The
/// remaining 0–2 args follow the historical rules:
/// - `&str` — `Type::Reference` whose inner type is the path `str` → suffix arg
///   (sets `is_prefix_form = true`; must be the first non-ctx arg).
/// - `Vec<u8>` — `Type::Path` whose last segment is `Vec` with a single `u8` generic
///   arg → `PayloadKind::Bytes`.
/// - Anything else → `PayloadKind::Typed(ty)` (assumed `T: DeserializeOwned`).
fn inspect_signature(sig: &syn::Signature) -> syn::Result<(bool, bool, PayloadKind)> {
    let inputs: Vec<&syn::FnArg> = sig.inputs.iter().collect();

    if inputs.len() > 3 {
        return Err(syn::Error::new_spanned(
            &sig.inputs,
            "#[job] functions accept at most 3 arguments (optional ctx: JobContext, optional suffix: &str, optional payload)",
        ));
    }

    // Strip an optional leading `ctx: JobContext`.
    let mut takes_ctx = false;
    let mut rest: &[&syn::FnArg] = &inputs;
    if let Some(first) = inputs.first() {
        if is_job_context(fn_arg_type(first)?) {
            takes_ctx = true;
            rest = &inputs[1..];
        }
    }

    if rest.len() > 2 {
        return Err(syn::Error::new_spanned(
            &sig.inputs,
            "#[job] functions accept at most (ctx: JobContext, suffix: &str, payload) — too many arguments, or the first arg should be `ctx: JobContext`",
        ));
    }

    let mut is_prefix_form = false;
    let mut payload_kind = PayloadKind::None;

    match rest.len() {
        0 => {}
        1 => {
            let ty = fn_arg_type(rest[0])?;
            if is_str_ref(ty) {
                is_prefix_form = true;
            } else {
                payload_kind = classify_payload(ty);
            }
        }
        2 => {
            // Two non-ctx args are only valid as (suffix: &str, payload).
            let ty0 = fn_arg_type(rest[0])?;
            if !is_str_ref(ty0) {
                return Err(syn::Error::new_spanned(
                    &sig.inputs,
                    "#[job] functions: if the first non-ctx arg is not `&str` (suffix), only one arg (payload) is allowed",
                ));
            }
            is_prefix_form = true;
            payload_kind = classify_payload(fn_arg_type(rest[1])?);
        }
        _ => unreachable!("rest.len() <= 2 enforced above"),
    }

    Ok((takes_ctx, is_prefix_form, payload_kind))
}

/// Return true iff `ty` is (path ending in) `JobContext` — the optional leading
/// handler-context argument.
fn is_job_context(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if p.qself.is_none() {
            if let Some(last) = p.path.segments.last() {
                return last.ident == "JobContext";
            }
        }
    }
    false
}

/// Extract the `syn::Type` from a typed `FnArg`. Errors on `self` receivers.
fn fn_arg_type(arg: &syn::FnArg) -> syn::Result<&syn::Type> {
    match arg {
        syn::FnArg::Typed(pat_ty) => Ok(&pat_ty.ty),
        syn::FnArg::Receiver(r) => Err(syn::Error::new_spanned(
            r,
            "#[job] functions must be free functions (no `self`)",
        )),
    }
}

/// Return true iff `ty` is `&str` (a shared reference whose inner type is the path `str`).
fn is_str_ref(ty: &syn::Type) -> bool {
    if let syn::Type::Reference(r) = ty {
        if r.mutability.is_none() {
            if let syn::Type::Path(p) = r.elem.as_ref() {
                if p.qself.is_none() && p.path.segments.len() == 1 {
                    return p.path.segments[0].ident == "str";
                }
            }
        }
    }
    false
}

/// Return true iff `ty` is `Vec<u8>` (path ending in `Vec` with single generic arg `u8`).
fn is_vec_u8(ty: &syn::Type) -> bool {
    if let syn::Type::Path(p) = ty {
        if p.qself.is_none() {
            let segs = &p.path.segments;
            if let Some(last) = segs.last() {
                if last.ident == "Vec" {
                    if let syn::PathArguments::AngleBracketed(ab) = &last.arguments {
                        if ab.args.len() == 1 {
                            if let syn::GenericArgument::Type(syn::Type::Path(ip)) = &ab.args[0] {
                                if ip.qself.is_none()
                                    && ip.path.segments.len() == 1
                                    && ip.path.segments[0].ident == "u8"
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Classify a payload type into `Bytes` or `Typed(T)`.
fn classify_payload(ty: &syn::Type) -> PayloadKind {
    if is_vec_u8(ty) {
        PayloadKind::Bytes
    } else {
        PayloadKind::Typed(ty.clone())
    }
}

/// Inspect the return type and determine if the `Ok` variant is `()`.
///
/// Looks for `Result<(), …>` by checking the first generic argument of the
/// outermost `Result` path — if it is `Type::Tuple` with zero elements it
/// is the unit type.
fn return_is_unit(output: &syn::ReturnType) -> bool {
    let ty = match output {
        syn::ReturnType::Default => return false,
        syn::ReturnType::Type(_, ty) => ty.as_ref(),
    };
    // Must be a path ending in `Result`.
    if let syn::Type::Path(p) = ty {
        if let Some(last) = p.path.segments.last() {
            if last.ident == "Result" {
                if let syn::PathArguments::AngleBracketed(ab) = &last.arguments {
                    if let Some(syn::GenericArgument::Type(syn::Type::Tuple(t))) = ab.args.first() {
                        return t.elems.is_empty(); // `()` == empty tuple
                    }
                }
            }
        }
    }
    false
}

/// Emit the return-mapping tokens. The inner fn's error (`String` or
/// `JobError`) is normalized to `JobError` via `JobError::from` first, so both
/// handler error types compile; then `Ok` is mapped to bytes (`vec![]` for
/// unit, serde_json serialize otherwise — a serialize failure is `Terminal`).
fn build_return_mapping(output: &syn::ReturnType) -> TokenStream2 {
    if return_is_unit(output) {
        quote! {
            result.map_err(::boogy_sdk::JobError::from).map(|_| ::std::vec::Vec::new())
        }
    } else {
        quote! {
            result.map_err(::boogy_sdk::JobError::from).and_then(|r| {
                ::serde_json::to_vec(&r).map_err(|e| {
                    ::boogy_sdk::JobError::Terminal(::std::format!("result serialize: {e}"))
                })
            })
        }
    }
}

/// Build the handler closure body tokens.
fn build_handler_body(
    inner: &proc_macro2::Ident,
    takes_ctx: bool,
    is_prefix: bool,
    payload: &PayloadKind,
    output: &syn::ReturnType,
) -> TokenStream2 {
    // 0. The closure always receives `ctx: &JobContext`; discard it when the
    //    user fn does not take one.
    let ctx_discard = if takes_ctx {
        quote! {}
    } else {
        quote! { let _ = ctx; }
    };

    // 1. Extract the suffix (prefix jobs) or discard it (exact jobs). A missing
    //    suffix is a routing bug that never resolves → Terminal.
    let suffix_extraction = if is_prefix {
        quote! {
            let suffix: &str = suffix_opt.ok_or_else(|| {
                ::boogy_sdk::JobError::Terminal("missing suffix for prefix job".to_string())
            })?;
        }
    } else {
        quote! { let _ = suffix_opt; }
    };

    // 2. Prepare the payload variable. A bad payload never deserializes on
    //    retry → Terminal.
    let payload_let = match payload {
        PayloadKind::None => quote! { let _ = payload_bytes; },
        PayloadKind::Bytes => quote! {
            let payload: ::std::vec::Vec<u8> = payload_bytes.to_vec();
        },
        PayloadKind::Typed(ty) => quote! {
            let payload: #ty = ::serde_json::from_slice(payload_bytes)
                .map_err(|e| ::boogy_sdk::JobError::Terminal(::std::format!("payload deserialize: {e}")))?;
        },
    };

    // 3. Build the call expression.
    let ctx_arg = if takes_ctx {
        quote! { ctx.clone(), }
    } else {
        quote! {}
    };
    let suffix_arg = if is_prefix {
        quote! { suffix, }
    } else {
        quote! {}
    };
    let payload_arg = match payload {
        PayloadKind::None => quote! {},
        PayloadKind::Bytes | PayloadKind::Typed(_) => quote! { payload },
    };
    let call = quote! { #inner(#ctx_arg #suffix_arg #payload_arg) };

    // 4. Map the result to `Result<Vec<u8>, JobError>`.
    let return_mapping = build_return_mapping(output);

    quote! {
        #ctx_discard
        #suffix_extraction
        #payload_let
        let result = #call;
        #return_mapping
    }
}

/// Core expansion logic for `#[job]`.
fn expand_job(attr: JobAttr, user_fn: syn::ItemFn) -> syn::Result<TokenStream2> {
    let user_fn_ident = user_fn.sig.ident.clone();
    let inner_ident = format_ident!("__job_{}_inner", user_fn_ident);

    // Rename the user fn body to the hidden inner ident.
    let mut renamed = user_fn.clone();
    renamed.sig.ident = inner_ident.clone();
    // Strip outer attributes from the renamed inner fn (they've been consumed).
    renamed.attrs.clear();

    // Inspect the signature to learn (takes ctx?, prefix?, payload kind).
    let (takes_ctx, is_prefix_form, payload_kind) = inspect_signature(&user_fn.sig)?;

    // Cross-check: attr form must agree with signature form.
    let attr_is_prefix = matches!(attr, JobAttr::Prefix(_));
    if attr_is_prefix != is_prefix_form {
        return Err(syn::Error::new_spanned(
            &user_fn.sig.ident,
            format!(
                "#[job] mismatch: attribute says `{}` but fn {} a `&str` first arg",
                if attr_is_prefix { "prefix = \"…\"" } else { "\"exact_name\"" },
                if is_prefix_form { "has" } else { "does not have" },
            ),
        ));
    }

    let name_lit = match &attr {
        JobAttr::Exact(s) | JobAttr::Prefix(s) => s.as_str(),
    };
    let is_prefix_lit = is_prefix_form;

    let body = build_handler_body(
        &inner_ident,
        takes_ctx,
        is_prefix_form,
        &payload_kind,
        &user_fn.sig.output,
    );

    // Preserve the user fn's visibility on the registration ctor.
    let vis = &user_fn.vis;

    Ok(quote! {
        // Hidden renamed inner fn (the actual user logic).
        #[allow(non_snake_case)]
        #renamed

        /// `JobRegistration` constructor emitted by `#[job]`.
        /// Pass this function (by name) to `JobRouter::new().exact(…)` or `.prefix(…)`.
        #[allow(non_snake_case)]
        #vis fn #user_fn_ident() -> ::boogy_sdk::JobRegistration {
            ::boogy_sdk::JobRegistration {
                name: #name_lit,
                is_prefix: #is_prefix_lit,
                handler: |ctx: &::boogy_sdk::JobContext,
                          suffix_opt: ::core::option::Option<&str>,
                          payload_bytes: &[u8]|
                    -> ::core::result::Result<::std::vec::Vec<u8>, ::boogy_sdk::JobError>
                {
                    #body
                },
            }
        }
    })
}
