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
/// - `#[model(counter(name = "<column>"))]` (struct, repeatable) — a
///   conflict-free counter column, with **no backing field at all**: the
///   value is kept in its own cell rather than inside the row, and an
///   increment is an atomic add that takes no read-conflict range, so
///   concurrent increments compose instead of conflicting. There is no
///   struct field to read it back through — a caller reads or adds it via
///   the companion `#[derive(Counter)] #[counter(of = <Self>, name =
///   "<column>")]` marker type instead (see that derive's own docs).
///
///   The column is a **64-bit signed integer** and the delta must be an
///   integer too. The add **wraps** on overflow rather than erroring or
///   saturating: pushing the value past `i64::MAX` rolls it over to
///   `i64::MIN`.
///
///   **A counter read is not serialized against concurrent increments** —
///   deliberately, since a read-conflict range on the cell would
///   re-introduce the very conflict the atomic add removes. Never gate a
///   write — or anything derived from a counter, like a `count_rows` whose
///   filter names one — on a counter value read in the same transaction; an
///   increment that commits between that read and the commit is simply
///   discarded, not a retry trigger. When the decision must hold, express it
///   as a predicate instead (`store::delete_where` / `store::update_where`
///   with the counter in the filters), which serializes against the rows it
///   actually matches.
///
///   Increments stay conflict-free only with an EMPTY `always` on
///   `upsert_increment`'s UPDATE arm — a non-empty `always` rewrites the
///   whole row on every call, an ordinary read-modify-write that conflicts
///   like any other. `on_insert_only` columns do not cost this: they are
///   written only by the row-creating call.
///
///   A counter column **cannot back an index** (the derive rejects it as an
///   `#[index]`, `#[lookup_by]`, `#[covering_index]`, struct-level index
///   column, or `list_by`/`ranked_by`/`tagged_by` column, at compile time):
///   index maintenance needs the previous value to clear the stale entry,
///   and an atomic add never reads one.
///
///   Always appended after every field column — in declaration order, when
///   more than one is declared. That is not a placeholder for "the position
///   it would have had as a field": the store assigns a table's column
///   ordinals once, when the table is first created, and never revisits an
///   already-created table's ordinals afterward (a later redeploy that adds
///   or reorders a `counter(...)` declaration, or reorders the struct's
///   fields, cannot move an existing table's ordinals no matter what it
///   says). There is therefore no prior position for a counter column to
///   reproduce, ever, and appending it is the only definition of "where"
///   that means anything.
///
///   ```ignore
///   #[derive(Model)]
///   #[model(table = "articles", counter(name = "reads"))]
///   pub struct Article {
///       #[pk] pub id: Id<Article>,
///       pub title: String,
///   }
///
///   #[derive(Counter)]
///   #[counter(of = Article, name = "reads")]
///   pub struct ArticleReads;
///   ```
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
/// - `#[renamed_from = "old_name"]` (field) — the column's previous name.
///   A declaration diff can never tell a rename from a drop-plus-add apart —
///   the two are textually identical and have opposite consequences for the
///   data — so the platform never infers one; this is the ONLY way a rename
///   is expressed. Lives on the field because the field still exists to
///   carry it. Consumed by `schema_resolve::plan_column_reconcile`, which
///   renames the live column in place instead of dropping the old name and
///   adding the new one as empty.
/// - `#[model(dropped("a", "b"))]` (struct) — columns this model has
///   deliberately removed, repeatable-by-list rather than repeatable-by-verb.
///   Lives on the model, not a field, because by the time a column is
///   dropped there is no field left to annotate — this doubles as the record
///   of what was removed, and an entry is deleted once that column has
///   actually been purged from the live table. Emitted as `M::ALLOW_DROPPED`,
///   the list `schema_resolve::plan_column_reconcile` checks before treating
///   a missing column as an undeclared, accidental drop. Naming a column a
///   field still declares is a compile error — if you meant a rename, use
///   `#[renamed_from]` on the new field instead.
///
/// There is no field-level `#[unique]`. It existed, compiled, and enforced
/// nothing — it set a column flag no write path reads, while emitting no index,
/// so duplicate values were accepted silently by a model that had declared they
/// could not be. Uniqueness is enforced by an INDEX: use `#[lookup_by]` for a
/// single column, or `#[model(unique_index(cols = [..]))]` for a composite. The
/// attribute is still *registered* below purely so the derive can reject it with
/// that explanation rather than leaving rustc to say "cannot find attribute".
///
/// retired-spelling: the field form was removed 2026-08-19;
/// `#[model(counter(name = "<column>"))]` replaces it. Named here because
/// this doc explains why the attribute is still registered.
/// There is no field-level `#[counter]` either. It used to declare a counter
/// column backed by a struct field; a counter column now has no field of its
/// own at all, declared instead with `#[model(counter(name = "<column>"))]`
/// above. `#[counter]` is still *registered* below purely so the derive can
/// reject it with that explanation rather than leaving rustc to say "cannot
/// find attribute".
#[proc_macro_derive(Model, attributes(model, pk, unique, index, covering_index, lookup_by, counter, default, belongs_to, renamed_from))]
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
    // retired-spelling: the field form is gone — this is the
    // `#[model(counter(name = ..))]` path that replaced it.
    // A counter column declared on the STRUCT rather than as a `#[counter]`
    // field — the companion `#[derive(Counter)] #[counter(of = Self, name =
    // "...")]` marker type is what a caller reads/adds it through. Always
    // appended after every field's column: the store assigns a table's
    // column ordinals once, at first creation, from whatever order this
    // list has THEN — a later redeploy that changes this declaration (or
    // the struct's field order) never revisits an already-created table's
    // ordinals, so there is no "correct" position to reproduce and no
    // reason to expose one. (column name)
    let mut struct_counters: Vec<String> = Vec::new();
    // Which of `struct_counters` are MAX accumulators. Kept as a marker set
    // rather than a second list so both kinds share ONE name namespace — the
    // duplicate-name check below then covers them without knowing they differ.
    let mut struct_max_names: std::collections::HashSet<String> = Default::default();
    // `#[model(dropped("a", "b"))]` — columns this model has deliberately
    // removed. Lives on the model, not a field, because by the time a column
    // is dropped there is no field left to hang the declaration on. Doubles
    // as the record of what was removed: the developer deletes an entry once
    // that column has actually been purged. Consumed by
    // `schema_resolve::plan_column_reconcile` via the emitted `ALLOW_DROPPED`
    // const, which is what lets that column vanish from `desired` without
    // being read back as an accidental, undeclared drop.
    let mut model_dropped: Vec<String> = Vec::new();
    // `list_by` order columns, resolved after the attribute loop so a
    // `counter`/`max` declared later in the same attribute still counts.
    let mut list_by_order_cols: Vec<String> = Vec::new();
    let mut access_patterns_deferred: Vec<(String, String, bool)> = Vec::new();
    // (column, pattern name) for every column an access pattern sorts or
    // filters on. Collected in parallel because the patterns below are quoted
    // into token streams immediately, after which their column names cannot be
    // retired-spelling: `#[counter]` on a field is refused, not honoured;
    // the live form is `#[model(counter(name = ..))]`.
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
                // The SORT column is checked against the counter list only when
                // it is not itself an accumulator. An accumulator ordering is
                // legal and is the whole point of `ListByRanked` — refusing it
                // here is what blocked the conflict-free `last_post_at` from
                // being adopted at all.
                //
                // Deferred to after the attribute loop because `max(..)` and
                // `counter(..)` may be declared AFTER `list_by(..)` in the
                // attribute, and reading `struct_counters` here would depend on
                // that order.
                list_by_order_cols.push(order_col.clone());
                access_patterns_deferred.push((filter.clone(), order_col.clone(), desc));
                Ok(())
            } else if meta.path.is_ident("rollup") {
                // rollup(group = "...", sum = "...", count = true)
                //
                // `sum` may be repeated to total more than one column. `count`
                // defaults to true because a rollup without it can serve no
                // query at all — a group whose rows have all gone still holds a
                // total of zero, and only the count separates that from a group
                // that genuinely sums to zero. Making the developer discover
                // that by declaring a rollup nothing ever uses would be a poor
                // trade for the one keystroke it saves.
                let (mut group, mut sums, mut count) = (Vec::<String>::new(), Vec::new(), true);
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("group") {
                        // `group = "customer"` and `group = ["room_id",
                        // "post_id"]` are both accepted: one grouping column is
                        // by far the common case and should not have to be
                        // written as a list, and the list is the same thing with
                        // more of it. Composite order is the composite-INDEX
                        // rule — bind a leading prefix, group by the rest.
                        let v = m.value()?;
                        if v.peek(syn::token::Bracket) {
                            let content;
                            syn::bracketed!(content in v);
                            let cols: syn::punctuated::Punctuated<LitStr, syn::Token![,]> =
                                content.parse_terminated(|p| p.parse::<LitStr>(), syn::Token![,])?;
                            group = cols.into_iter().map(|c| c.value()).collect();
                        } else {
                            group = vec![v.parse::<LitStr>()?.value()];
                        }
                    } else if m.path.is_ident("sum") {
                        sums.push(m.value()?.parse::<LitStr>()?.value());
                    } else if m.path.is_ident("count") {
                        count = m.value()?.parse::<syn::LitBool>()?.value();
                    } else {
                        return Err(m.error("rollup expects group + sum (repeatable) + count"));
                    }
                    Ok(())
                })?;
                if group.is_empty() {
                    return Err(meta.error(
                        "rollup requires a `group = \"...\"` (or `group = [\"a\", \"b\"]` \
                         to group by more than one column)",
                    ));
                }
                if sums.is_empty() && !count {
                    return Err(meta.error(
                        "rollup maintains nothing: give it at least one `sum = \"...\"` \
                         or leave `count` at its default of true",
                    ));
                }
                // Registered like every other pattern column, so naming a
                // column the struct does not have is a compile error rather
                // than a runtime refusal at deploy time.
                for c in &group {
                    pattern_cols.push((c.clone(), "rollup(group)"));
                }
                for c in &sums {
                    pattern_cols.push((c.clone(), "rollup(sum)"));
                }
                access_patterns.push(quote! {
                    __t.access_patterns.push(::boogy_sdk::store::AccessPattern::Rollup {
                        group: ::std::vec![#(#group.into()),*],
                        sum: ::std::vec![#(#sums.into()),*],
                        count: #count,
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
            } else if meta.path.is_ident("counter") {
                // counter(name = "post_count")
                //
                // Emits a counter column with no backing field, matching what
                // retired-spelling: the field form is gone;
                // `#[model(counter(name = ..))]` emits the same column
                // with no field.
                // a `#[counter]` field used to push into `__t.columns` — a
                // 64-bit integer column, not nullable, no default (a counter's
                // value lives in its own cell; a default would never be
                // observed, exactly as the field form's rejection explains).
                // Always appended after every field's column; see the comment
                // on `struct_counters` above for why there is no way to (and
                // no reason to want to) choose a different position.
                let mut name = String::new();
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("name") {
                        name = m.value()?.parse::<LitStr>()?.value();
                        Ok(())
                    } else {
                        Err(m.error("counter(...) takes only `name = \"...\"` (required)"))
                    }
                })?;
                if name.is_empty() {
                    return Err(meta.error(
                        "#[model(counter(...))] requires a `name = \"...\"` naming the \
                         counter column, e.g. `counter(name = \"post_count\")`",
                    ));
                }
                struct_counters.push(name);
                Ok(())
            } else if meta.path.is_ident("max") {
                // max(name = "last_post_at")
                //
                // A counter column maintained with MAX instead of ADD: the cell
                // keeps the LARGEST value ever observed for the row, and a
                // smaller write is a silent no-op.
                //
                // This is how "last activity" is declared without contention.
                // The ordinary way — a plain column stamped on the parent row
                // whenever a child is written — rewrites that row on every
                // write, so every writer conflicts with every other. Same
                // field-free shape as `counter(...)`, and the same reasons for
                // it: the value lives in its own cell, so there is no field to
                // clobber and no default that would ever be observed.
                let mut name = String::new();
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("name") {
                        name = m.value()?.parse::<LitStr>()?.value();
                        Ok(())
                    } else {
                        Err(m.error("max(...) takes only `name = \"...\"` (required)"))
                    }
                })?;
                if name.is_empty() {
                    return Err(meta.error(
                        "#[model(max(...))] requires a `name = \"...\"` naming the \
                         column, e.g. `max(name = \"last_post_at\")`",
                    ));
                }
                struct_max_names.insert(name.clone());
                struct_counters.push(name);
                Ok(())
            } else if meta.path.is_ident("dropped") {
                // dropped("legacy_note", "old_flag") — a bare, comma-separated
                // list of column-name literals, not `key = value` pairs, so it
                // is parsed off the raw token stream rather than through
                // `parse_nested_meta` (which expects each item to be a `Meta`).
                let content;
                syn::parenthesized!(content in meta.input);
                let cols: syn::punctuated::Punctuated<LitStr, syn::Token![,]> =
                    content.parse_terminated(|p| p.parse::<LitStr>(), syn::Token![,])?;
                if cols.is_empty() {
                    return Err(meta.error(
                        "dropped(...) requires at least one column-name string literal",
                    ));
                }
                model_dropped.extend(cols.into_iter().map(|c| c.value()));
                Ok(())
            } else {
                Err(meta.error("unknown model attribute"))
            }
        })?;
    }

    // Resolve the deferred `list_by` declarations now that every `counter(..)`
    // and `max(..)` on this struct is known.
    for (filter, order_col, desc) in &access_patterns_deferred {
        let is_accum = struct_counters.iter().any(|c| c == order_col);
        let (f, o, d) = (filter.clone(), order_col.clone(), *desc);
        if is_accum {
            // Ordering by an accumulator: the index is the filter alone and the
            // ordering comes from a projection over the cells.
            access_patterns.push(quote! {
                __t.access_patterns.push(::boogy_sdk::store::AccessPattern::ListByRanked {
                    filter: #f.into(),
                    order: ::boogy_sdk::store::Order { column: #o.into(), desc: #d },
                });
            });
        } else {
            pattern_cols.push((order_col.clone(), "list_by sort column"));
            access_patterns.push(quote! {
                __t.access_patterns.push(::boogy_sdk::store::AccessPattern::ListBy {
                    filter: #f.into(),
                    order: ::boogy_sdk::store::Order { column: #o.into(), desc: #d },
                });
            });
        }
    }
    let _ = &list_by_order_cols;

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
    /// author writes `#[pk(auto)]` or a field-level
    /// `#[index(cols = [..])]`, gets a clean build, and believes the derive
    /// saw something it never did. For storage attributes that surfaces later
    /// as wrong data or a missing index, with nothing to trace it back to —
    /// so refusing to build is strictly better, the same reasoning as the
    /// retired-spelling: both field forms are rejected outright; a
    /// counter column is `#[model(counter(name = ..))]`.
    /// unconditional `#[unique]`/`#[counter]` field rejections below.
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
        ty: &syn::Type,
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
                if is_decimal_type(ty) {
                    // `Decimal` stores as scaled `i64` minor units, so its
                    // string default is parsed EXACTLY here, at compile
                    // time — never through a float — and emitted as the
                    // `Val::Integer` the column actually holds. A malformed
                    // or over-precise literal is a compile error, same as
                    // every other `#[default]` mistake this fn catches.
                    let minor = parse_decimal_default(&v).map_err(|e| {
                        syn::Error::new_spanned(
                            s,
                            format!("#[default]: invalid Decimal literal: {e} (got {v:?})"),
                        )
                    })?;
                    (quote! { ::boogy_sdk::store::Val::Integer(#minor) }, "string")
                } else {
                    (quote! { ::boogy_sdk::store::Val::Text(#v.to_string()) }, "string")
                }
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

    /// True for `Decimal` or `Option<Decimal>` — the one field type whose
    /// `#[default]` string literal is parsed EXACTLY at compile time into
    /// scaled minor units, rather than stored as `Val::Text` verbatim.
    /// Mirrors `expected_default_kind`'s `Option` unwrap below.
    fn is_decimal_type(ty: &syn::Type) -> bool {
        let path = match ty {
            syn::Type::Path(p) => &p.path,
            _ => return false,
        };
        let Some(seg) = path.segments.last() else { return false };
        if seg.ident == "Option" {
            if let syn::PathArguments::AngleBracketed(a) = &seg.arguments {
                if let Some(syn::GenericArgument::Type(inner)) = a.args.first() {
                    return is_decimal_type(inner);
                }
            }
            return false;
        }
        seg.ident == "Decimal"
    }

    /// Compile-time twin of `boogy_sdk::model::Decimal`'s exact string
    /// parser. This crate cannot depend on `boogy-sdk` — the dependency
    /// runs the other way — so the algorithm is duplicated here; keep the
    /// two in lockstep (6 fractional digits, reject rather than round
    /// anything with more, `i64` minor units).
    fn parse_decimal_default(s: &str) -> Result<i64, String> {
        const SCALE: i64 = 1_000_000;
        const SCALE_DIGITS: usize = 6;
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("empty".to_string());
        }
        let (neg, rest) = match trimmed.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
        };
        let mut parts = rest.splitn(2, '.');
        let int_part = parts.next().unwrap_or("");
        let frac_part = parts.next().unwrap_or("");
        if int_part.is_empty() && frac_part.is_empty() {
            return Err("no digits".to_string());
        }
        if !int_part.chars().all(|c| c.is_ascii_digit()) {
            return Err("non-digit in integer part".to_string());
        }
        if !frac_part.chars().all(|c| c.is_ascii_digit()) {
            return Err("non-digit in fractional part (or more than one '.')".to_string());
        }
        if frac_part.len() > SCALE_DIGITS {
            return Err(format!(
                "more than {SCALE_DIGITS} fractional digits — Decimal is exact to \
                 {SCALE_DIGITS} decimal places; round explicitly before writing the literal"
            ));
        }
        let int_val: i64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| "integer part out of range for i64".to_string())?
        };
        let mut padded = frac_part.to_string();
        while padded.len() < SCALE_DIGITS {
            padded.push('0');
        }
        let frac_val: i64 =
            padded.parse().map_err(|_| "fractional part out of range".to_string())?;
        let magnitude = int_val
            .checked_mul(SCALE)
            .and_then(|m| m.checked_add(frac_val))
            .ok_or_else(|| "out of range for Decimal".to_string())?;
        Ok(if neg { -magnitude } else { magnitude })
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
            // `Decimal` ALSO takes a string literal — a plain decimal
            // string like `"19.99"`, parsed EXACTLY at compile time into
            // scaled minor units (never through a float; see
            // `parse_decimal_default` below and `boogy_sdk::model::Decimal`).
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
        default: Option<proc_macro2::TokenStream>,
        /// `#[belongs_to(Parent)]` — the parent type this column's value keys.
        belongs_to: Option<syn::Path>,
        /// `#[renamed_from = "old"]` — the column's previous name, so the
        /// reconciler can rename in place instead of reading a drop-plus-add.
        renamed_from: Option<String>,
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
        let mut default: Option<proc_macro2::TokenStream> = None;
        let mut belongs_to: Option<syn::Path> = None;
        let mut renamed_from: Option<String> = None;
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
                // retired-spelling: the `#[counter]` named here is the
                // rejected FIELD form; the live declaration is
                // `#[model(counter(name = ..))]`.
                // call as `deny_marker_args` above and the #[counter] rejection
                // below: a storage declaration that is silently discarded
                // surfaces later as wrong data, with nothing to trace it back
                // to. And emitting one would change the storage layout of every
                // model carrying the attribute for a guarantee nobody can
                // currently rely on — while still being ineffective on a table
                // that already holds duplicates.
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
            } else if attr.path().is_ident("belongs_to") {
                // `#[belongs_to(Post)]` — a TYPE, not a table name string. The
                // table comes from `<Post as Model>::TABLE`, so renaming the
                // parent's table cannot leave a relation pointing at a name
                // nothing answers to.
                let parent: syn::Path = attr.parse_args().map_err(|_| {
                    syn::Error::new_spanned(
                        attr,
                        "#[belongs_to] names the parent MODEL, e.g. \
                         `#[belongs_to(Post)]`. It takes a type, not a table \
                         name — the table is read from the model so a rename \
                         cannot leave the relation behind.",
                    )
                })?;
                belongs_to = Some(parent);
            } else if attr.path().is_ident("counter") {
                // retired-spelling: this whole arm exists to reject the
                // retired field form and name `#[model(counter(name =
                // ..))]` as the replacement.
                // --- #[counter] is not supported as a field -------------------
                //
                // A counter column has no field of its own any more: it is
                // declared on the STRUCT, as `#[model(counter(name =
                // "<column>"))]`, and read or added through the companion
                // `#[derive(Counter)] #[counter(of = <Self>, name =
                // "<column>")]` marker type. Rejecting outright (rather than
                // discarding the attribute or reinterpreting it) follows the
                // same reasoning as `#[unique]` above: a storage declaration
                // that is silently accepted-and-ignored surfaces later as
                // wrong data, with nothing to trace it back to.
                // retired-spelling: the message quotes the RETIRED field
                // form back at the author and names the replacement,
                // `#[model(counter(name = ..))]`. Both spellings must stay
                // literal here — the diagnostic is the routing.
                return Err(syn::Error::new_spanned(
                    attr,
                    "#[counter] is not supported as a field: a counter column has \
                     no field of its own. Declare it on the struct instead — \
                     #[model(counter(name = \"<column>\"))] — and read or add its \
                     value through a companion #[derive(Counter)] #[counter(of = \
                     Self, name = \"<column>\")] marker type.",
                ));
            } else if attr.path().is_ident("default") {
                let (val, kind) = parse_default_attr(attr, &f.ty)?;
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
            } else if attr.path().is_ident("renamed_from") {
                // #[renamed_from = "old_name"] — the ONLY way a rename is ever
                // expressed. A declaration diff cannot tell a rename from a
                // drop-plus-add (textually identical, opposite consequences
                // for the data), so the platform never infers one; this
                // annotation lives on the field because the field still
                // exists to carry it.
                let syn::Meta::NameValue(nv) = &attr.meta else {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "#[renamed_from = \"old_name\"] needs a string value, e.g. \
                         #[renamed_from = \"title\"]",
                    ));
                };
                let old = match &nv.value {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => s.value(),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "#[renamed_from] needs a string literal naming the old column",
                        ))
                    }
                };
                renamed_from = Some(old);
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
        field_infos.push(FieldInfo {
            ident, ty: f.ty.clone(), column, is_pk, index, covering, default, belongs_to,
            renamed_from,
        });
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

    // --- #[model(counter(...))] validation --------------------------------
    //
    // A struct-level counter has no field, so it cannot collide with a field's
    // NAME the way a #[belongs_to]/#[default] mistake would — but it can still
    // collide with another column's STORED name, or with itself. Both are
    // compile errors naming the fix, matching every other malformed-
    // declaration path in this derive.
    {
        let field_cols: Vec<&str> = field_infos
            .iter()
            .filter(|f| !f.is_pk)
            .map(|f| f.column.as_str())
            .collect();
        for (i, name) in struct_counters.iter().enumerate() {
            if field_cols.contains(&name.as_str()) {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    format!(
                        "#[model(counter(name = \"{name}\"))] collides with a field \
                         already declaring column `{name}` — a counter column has no \
                         field of its own, so its name must not be reused by one."
                    ),
                ));
            }
            if struct_counters[..i].iter().any(|n| n == name) {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    format!("counter column `{name}` is declared more than once"),
                ));
            }
            // The name becomes a Rust const identifier below (`Room::POST_COUNT`,
            // `Room::post_count`) — `format_ident!` panics on a string that is
            // not a valid identifier, and a raw macro panic is not this
            // derive's error style (every other malformed declaration here is
            // a spanned, named compile error). Catching it here, once, keeps
            // that true for this path too.
            if syn::parse_str::<syn::Ident>(name).is_err() {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    format!(
                        "#[model(counter(name = \"{name}\"))] is not a valid Rust \
                         identifier — it becomes a column-name const \
                         (`{}`) and a typed column handle (`{name}`), so it must \
                         parse as one, e.g. \"post_count\" rather than \"post-count\" \
                         or \"2fast\".",
                        name.to_uppercase()
                    ),
                ));
            }
        }
        // A counter column cannot back an index: an atomic add never reads
        // the previous value, so a stale index entry could never be removed.
        for name in &struct_counters {
            for (col, pattern) in &pattern_cols {
                if col == name {
                    return Err(syn::Error::new_spanned(
                        &struct_ident,
                        format!(
                            "counter column `{name}` is used by `{pattern}`, which is \
                             backed by an index, and a counter column cannot back one. \
                             Scope the ranking to a bounded sub-range and sort in \
                             memory, or materialize it into a plain column refreshed by \
                             a background job."
                        ),
                    ));
                }
            }
            for (idx_name, cols, _, _) in &struct_indexes {
                if cols.contains(name) {
                    return Err(syn::Error::new_spanned(
                        &struct_ident,
                        format!(
                            "counter column `{name}` is used by index `{idx_name}`: a \
                             counter column cannot back an index, because an atomic add \
                             never reads the previous value and the old entry could \
                             never be removed."
                        ),
                    ));
                }
            }
        }
    }

    // --- every access-pattern / index column must name a declared column ----
    //
    // Without this the derive happily emits an index over a column that does
    // not exist, from a typo or a stale rename in a `list_by`/`ranked_by`/
    // `tagged_by`/struct-level `index` declaration.
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
        // --- dropped(...) may never name a column a field still declares ----
        //
        // Declaring and dropping the same column is a contradiction: `dropped`
        // exists precisely because the field is GONE, so if a field is still
        // sitting right there, the developer meant something else (a real
        // rename, which is `#[renamed_from]`) or made a mistake. Either way
        // this is cheap to catch here and expensive to debug after a bad
        // reconcile plan runs against a live table.
        for name in &model_dropped {
            if known(name) {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    format!(
                        "#[model(dropped(\"{name}\"))] names column `{name}`, but a field \
                         still declares it. `dropped(...)` is for columns with NO field left \
                         — if you meant to rename it, use #[renamed_from = \"{name}\"] on the \
                         new field instead; if you meant to actually remove it, delete the \
                         field first."
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

    // --- typed column handles: `pub const field: Col<FieldType>` ---
    //
    // Lower-case on purpose: `Post::room_id.eq(5)` reads as the column it is,
    // and the SCREAMING form stays for the places a column genuinely is just a
    // name (row accessors, schema attributes). Same name, two shapes, so this
    // does not become a second vocabulary.
    let typed_cols = field_infos.iter().filter(|f| !f.is_pk).map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        let col = &f.column;
        quote! {
            #[allow(non_upper_case_globals)]
            pub const #ident: ::boogy_sdk::expr::Col<#ty> =
                ::boogy_sdk::expr::Col::new(#col);
        }
    });

    // A `#[model(counter(...))]` column has no field to hang `Post::room_id`
    // -style consts off, but the same two shapes are still what callers need:
    // the SCREAMING name for `upsert_increment`'s `counter` argument, and a
    // typed `Col<i64>` for the one thing sorting BY a counter's value is
    // still allowed to do — `.order(Link::clicks.desc())` (spec §9's
    // narrower exception: ranking by a counter's cells, not reading them).
    let struct_counter_const_defs = struct_counters.iter().map(|name| {
        let cname = format_ident!("{}", name.to_uppercase());
        quote! { pub const #cname: &'static str = #name; }
    });
    let struct_counter_typed_cols = struct_counters.iter().map(|name| {
        let ident = format_ident!("{}", name);
        quote! {
            #[allow(non_upper_case_globals)]
            pub const #ident: ::boogy_sdk::expr::Col<i64> =
                ::boogy_sdk::expr::Col::new(#name);
        }
    });

    // --- schema(): push a ColDef per non-pk field, then every
    // `#[model(counter(...))]` declaration, in declaration order ---
    //
    // The store assigns a table's column ordinals once, at `create_table`
    // time, from whatever order this pushes them in THEN — and never
    // revisits them afterward (`add_column` only appends; a later redeploy
    // that changes this declaration, or the struct's field order, cannot
    // touch an already-created table's ordinals at all, because
    // `create_table_from` skips creation outright when the table already
    // exists). So there is no historical position for a counter column to
    // reproduce, ever, and appending it after the real columns is not a
    // placeholder for something more precise — it is the only definition of
    // "where" that means anything here.
    let field_pushes = field_infos.iter().filter(|f| !f.is_pk).map(|f| {
        let ty = &f.ty;
        let col = &f.column;
        // `unique` is always false: the derive has no field marker that sets it
        // any more, because the flag is inert — nothing on any write path reads
        // it. A derived model states its uniqueness constraints as UNIQUE
        // indexes (`#[lookup_by]`, `#[model(unique_index(...))]`), which the
        // store does enforce. `counter` is always false too: a field can never
        // be a counter column any more — see `#[model(counter(name = ...))]`.
        let default = match &f.default {
            Some(v) => quote! { ::core::option::Option::Some(#v) },
            None => quote! { ::core::option::Option::None },
        };
        // The parent's table is read from the model, so a `#[model(table = ..)]`
        // rename on the parent moves the relation with it.
        let parent = match &f.belongs_to {
            Some(p) => quote! {
                ::core::option::Option::Some(::boogy_sdk::store::ForeignKey {
                    references_table:
                        <#p as ::boogy_sdk::model::Model>::TABLE.to_string(),
                    references_column: "_id".to_string(),
                    on_delete: ::boogy_sdk::store::CascadeAction::NoAction,
                    on_update: ::boogy_sdk::store::CascadeAction::NoAction,
                })
            },
            None => quote! { ::core::option::Option::None },
        };
        let renamed = match &f.renamed_from {
            Some(old) => quote! { ::core::option::Option::Some(#old.to_string()) },
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            {
                let mut __c =
                    ::boogy_sdk::model::col_def_for::<#ty>(#col, false, false, #default);
                __c.references = #parent;
                __c.renamed_from = #renamed;
                __t.columns.push(__c);
            }
        }
    });
    let struct_counter_pushes = struct_counters.iter().map(|name| {
        let is_max = struct_max_names.contains(name);
        quote! {
            __t.columns.push(
                ::boogy_sdk::model::col_def_for_accum::<i64>(
                    #name, false, true, #is_max, ::core::option::Option::None,
                )
            );
        }
    });
    let col_pushes = field_pushes.chain(struct_counter_pushes);
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

    // --- to_columns (non-pk) ---
    //
    // Every remaining field is an ordinary writable column. A counter column
    // is never among them: it has no field at all — `struct_counter_pushes`
    // (below) puts it in the schema, but `to_columns` only ever walks
    // `field_infos`, and a counter never appears there. That absence is what
    // keeps a counter's value out of `db_insert`/`db_update`: it is written
    // ONLY by the increment path, so `db_update(id, &Row { title, ..row })`
    // can never carry a stale read of it back over a concurrent atomic add.
    let to_col_pushes = field_infos.iter().filter(|f| !f.is_pk).map(|f| {
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

    let dropped_lits = model_dropped.iter().map(|s| quote! { #s });

    let expanded = quote! {
        impl #struct_ident {
            #(#const_defs)*
            #(#typed_cols)*
            #(#struct_counter_const_defs)*
            #(#struct_counter_typed_cols)*

            /// Columns this model has deliberately removed. Declared with
            /// `#[model(dropped("a", "b"))]` — named on the model rather than
            /// a field because the field is gone; there is nothing left to
            /// annotate. Consumed by
            /// `boogy_sdk::schema_resolve::plan_column_reconcile` as the
            /// `allow_dropped` list, so a missing column matching an entry
            /// here reconciles as an intentional drop rather than an
            /// undeclared one. Remove an entry once that column has actually
            /// been purged from the live table.
            pub const ALLOW_DROPPED: &'static [&'static str] = &[#(#dropped_lits),*];
        }

        impl ::boogy_sdk::model::Model for #struct_ident {
            const TABLE: &'static str = #table_name;

            fn schema() -> ::boogy_sdk::store::Table {
                let mut __t = ::boogy_sdk::store::Table::new(#table_name);
                // Read from the emitted const rather than re-expanding the
                // literals: `ALLOW_DROPPED` is the documented, user-visible
                // name for this list, and two expansions of one attribute can
                // disagree.
                __t.allow_dropped = #struct_ident::ALLOW_DROPPED;
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
// #[derive(Counter)]
// ---------------------------------------------------------------------------

/// Derive `boogy_sdk::model::Counter` for a marker type — either naming an
/// existing counter column on a `#[derive(Model)]` struct (declared with
/// `#[model(counter(name = "..."))]` — see that derive's docs), or standing
/// alone as a counter keyed by an arbitrary tuple attached to no model.
///
/// ```ignore
/// #[derive(Counter)]
/// #[counter(of = Room, name = "post_count")]
/// pub struct RoomPostCount;          // keyed by the room's row id
/// ```
///
/// ```ignore
/// #[derive(Counter)]
/// #[counter(key = (room_id, day))]
/// pub struct RoomDailyPosts;         // keyed by an arbitrary tuple, no model
/// ```
///
/// Exactly one of two shapes, never both and never neither — combining them
/// or giving neither is a compile error naming the fix:
///
/// - `#[counter(of = <Model>, name = "<column>")]` — a counter attached to
///   a model's row.
///   - `of` — the parent model type (a path, not a string — same reasoning
///     as `#[belongs_to]`: the table name is read from `<of as Model>::TABLE`,
///     so a `#[model(table = "...")]` rename on the parent cannot leave this
///     pointing at a name nothing answers to).
///   - `name` — the counter column's name on that model, as a string
///     literal. The column itself is declared on `<of>`'s own
///     `#[derive(Model)]`, with `#[model(counter(name = "<column>"))]`.
///     This derive does not emit a column of its own, and does not inspect
///     `<of>`'s declarations to check that `name` really is one — it only
///     gives whichever cell that name already addresses a freestanding,
///     typed handle.
///   - Emits `Counter::Key = Id<of>`.
///
/// - `#[counter(key = (col_a, col_b, ...))]` — a counter keyed by an
///   arbitrary tuple, attached to no model at all. `name = "..."` is an
///   optional override for the counter's own name; the default is the
///   struct's name in snake_case. Emits `Counter::Key = [Val; N]`, one
///   value per listed column, in the listed order, plus a
///   `Self::KEY_COLS: &'static [&'static str]` inherent const naming those
///   columns in the same order.
///
///   **Unbounded key cardinality is a new way to fill a keyspace.** This
///   derive allocates a cell for every DISTINCT key tuple ever added to and
///   never reclaims one — keying by something with no natural bound (a
///   request id, a raw event id) grows storage without limit, the same risk
///   the store's `rollup-def` documents for an unbounded `group`. Keep
///   `key` columns bounded (a customer id, a day, an IP) the way you would
///   size an index.
#[proc_macro_derive(Counter, attributes(counter))]
pub fn derive_counter(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_counter(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_counter(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_ident = input.ident.clone();

    let mut of: Option<syn::Path> = None;
    let mut name: Option<LitStr> = None;
    let mut key: Option<Vec<String>> = None;

    for attr in &input.attrs {
        if !attr.path().is_ident("counter") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("of") {
                of = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("key") {
                let value = meta.value()?;
                let content;
                syn::parenthesized!(content in value);
                let cols: syn::punctuated::Punctuated<syn::Ident, syn::Token![,]> =
                    content.parse_terminated(|p| p.parse::<syn::Ident>(), syn::Token![,])?;
                if cols.is_empty() {
                    return Err(meta.error(
                        "#[counter(key = (...))] needs at least one column name — \
                         e.g. `#[counter(key = (ip, window))]`",
                    ));
                }
                key = Some(cols.into_iter().map(|i| i.to_string()).collect());
                Ok(())
            } else {
                Err(meta.error(
                    "#[counter(...)] on a #[derive(Counter)] type takes `of = <Model>` \
                     with `name = \"<column>\"` (a counter attached to a model's row), \
                     or `key = (col_a, col_b)` (a counter keyed by an arbitrary tuple, \
                     attached to no model) — e.g. `#[counter(of = Room, name = \
                     \"post_count\")]` or `#[counter(key = (ip, window))]`",
                ))
            }
        })?;
    }

    match (of, key) {
        (Some(_), Some(_)) => Err(syn::Error::new_spanned(
            &struct_ident,
            "#[counter(...)] cannot combine `of = <Model>` with `key = (...)` — `of` is \
             sugar for keying on that model's row id; use `#[counter(of = Room, name = \
             \"post_count\")]` for a counter attached to a model's row, or `#[counter(key \
             = (col_a, col_b))]` for a counter keyed by an arbitrary tuple attached to no \
             model, not both",
        )),
        (Some(of), None) => {
            let Some(name) = name else {
                return Err(syn::Error::new_spanned(
                    &struct_ident,
                    "#[counter(...)] is missing `name = \"<column>\"` — the name of \
                     the counter column on the model this type addresses, e.g. \
                     `#[counter(of = Room, name = \"post_count\")]`",
                ));
            };
            Ok(quote! {
                impl ::boogy_sdk::model::Counter for #struct_ident {
                    const NAME: &'static str = {
                        const __TABLE: &str = <#of as ::boogy_sdk::model::Model>::TABLE;
                        const __COLUMN: &str = #name;
                        const __BYTES: [u8; __TABLE.len() + 1 + __COLUMN.len()] =
                            ::boogy_sdk::model::concat_counter_name(__TABLE, __COLUMN);
                        match ::core::str::from_utf8(&__BYTES) {
                            ::core::result::Result::Ok(s) => s,
                            // Unreachable: concatenating two valid `&str`s with a
                            // single-byte ASCII separator is always valid UTF-8. A
                            // bare string-literal panic (no interpolation) is the
                            // form `const` contexts accept.
                            ::core::result::Result::Err(_) => {
                                panic!("counter name concatenation produced invalid utf8")
                            }
                        }
                    };
                    type Key = ::boogy_sdk::model::Id<#of>;
                }
            })
        }
        (None, Some(key_cols)) => {
            let counter_name = name
                .map(|l| l.value())
                .unwrap_or_else(|| to_snake_case(&struct_ident.to_string()));
            let n = key_cols.len();
            let key_col_strs = key_cols.iter().map(|s| s.as_str());
            Ok(quote! {
                impl #struct_ident {
                    /// The columns composing this counter's key, in order —
                    /// the same order `Counter::Key`'s `[Val; #n]` values
                    /// must be supplied in.
                    pub const KEY_COLS: &'static [&'static str] = &[#(#key_col_strs),*];
                }
                impl ::boogy_sdk::model::Counter for #struct_ident {
                    const NAME: &'static str = #counter_name;
                    type Key = [::boogy_sdk::store::Val; #n];
                }
            })
        }
        (None, None) => {
            if name.is_some() {
                Err(syn::Error::new_spanned(
                    &struct_ident,
                    "#[counter(...)] is missing `of = <Model>` — the model that \
                     declares the counter column this type addresses, e.g. \
                     `#[counter(of = Room, name = \"post_count\")]`",
                ))
            } else {
                Err(syn::Error::new_spanned(
                    &struct_ident,
                    "#[derive(Counter)] requires either `#[counter(of = <Model>, name = \
                     \"<column>\")]` (a counter attached to a model's counter column) \
                     or `#[counter(key = (col_a, col_b))]` (a counter keyed by an \
                     arbitrary tuple, attached to no model) — e.g. `#[counter(of = Room, \
                     name = \"post_count\")]` or `#[counter(key = (ip, window))]`",
                ))
            }
        }
    }
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
