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
/// - `#[unique]` (field) — column-level UNIQUE.
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
#[proc_macro_derive(Model, attributes(model, pk, unique, index, covering_index, lookup_by))]
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

    struct FieldInfo {
        ident: syn::Ident,
        ty: syn::Type,
        column: String,
        is_pk: bool,
        unique: bool,
        index: bool,
        covering: bool,
    }

    let mut field_infos: Vec<FieldInfo> = Vec::new();
    // Field-level `#[lookup_by]` columns (resolved to LookupBy access patterns
    // after the column name override is known).
    let mut lookup_by_cols: Vec<String> = Vec::new();
    for f in fields {
        let ident = f.ident.clone().unwrap();
        let mut column = ident.to_string();
        let mut is_pk = false;
        let mut unique = false;
        let mut index = false;
        let mut covering = false;
        let mut lookup_by = false;
        for attr in &f.attrs {
            if attr.path().is_ident("pk") {
                is_pk = true;
            } else if attr.path().is_ident("unique") {
                unique = true;
            } else if attr.path().is_ident("index") {
                index = true;
            } else if attr.path().is_ident("covering_index") {
                index = true;
                covering = true;
            } else if attr.path().is_ident("lookup_by") {
                lookup_by = true;
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
        field_infos.push(FieldInfo { ident, ty: f.ty.clone(), column, is_pk, unique, index, covering });
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
        let unique = f.unique;
        quote! {
            __t.columns.push(::boogy_sdk::model::col_def_for::<#ty>(#col, #unique));
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

    // --- to_columns (non-pk only) ---
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
