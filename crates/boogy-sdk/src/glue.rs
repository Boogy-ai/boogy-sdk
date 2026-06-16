//! `wit_glue!` macro — emits the WIT↔SDK conversion layer in the user's crate.
//!
//! `wit_bindgen::generate!` produces a `bindings` module with WIT-defined types
//! that are local to the user's crate. This macro takes that module and emits:
//!
//! - The `Guest` impl for the user's API struct, wired to the
//!   [`Api`](crate::Api) trait (`init_tables` + `build_router`).
//! - Conversion helpers between WIT types and SDK types
//!   (`to_sdk_request`, `to_wit_response`, `to_sdk_row`, `create_table_from`).
//! - User-facing row helpers (`get_row`, `find_all_rows`).
//! - The `bindings::export!` macro invocation.
//! - All the common `use` statements so handlers don't need to repeat them.
//!
//! Why a macro: the helpers reference WIT-generated types under
//! `bindings::boogy::platform::*`, which only exist in the user's crate
//! after `wit_bindgen::generate!` runs. A free function in this SDK crate
//! can't reach those types. The macro lets us write the helpers ONCE here
//! while having them expand into each downstream crate's namespace.

/// Emit the WIT↔SDK glue for a Boogy API.
///
/// Usage:
/// ```ignore
/// mod bindings {
///     wit_bindgen::generate!({ world: "service", path: "../../boogy-wit/wit" });
/// }
/// boogy_sdk::wit_glue!(bindings, TodoApi);
///
/// struct TodoApi;
/// impl boogy_sdk::Api for TodoApi { /* ... */ }
/// ```
///
/// Two arguments:
/// - The bindings module name (typically `bindings`).
/// - The user's API struct name. The macro emits `impl Guest for $struct`
///   and `bindings::export!($struct with_types_in $bindings)`.
///
/// The expansion provides these names in the calling module:
/// - `create_table_from(&Table)` — register a table from the SDK builder.
/// - `to_sdk_row(&store::Row) -> Row` — convert a WIT row to a typed SDK row.
/// - `get_row(table, id) -> Result<Option<Row>, RpcError>` — read+convert.
/// - `find_all_rows(table) -> Result<(Vec<Row>, u64), RpcError>` — list+convert.
/// - `find_row_by(table, column, store::Value) -> Result<Option<Row>, RpcError>` —
///   first-row-matching lookup. Takes the WIT `store::Value` directly so write
///   and lookup paths use the same value type.
/// - `auth::*` — resource-level auth helpers (`current_principal`, `required`,
///   `owns_resource`, `find_owned`, `load_owned`).
/// - Typed-model CRUD over a `#[derive(Model)]` type `M` (see
///   [`boogy_sdk::model`]): `create_model::<M>()` (register in
///   `init_tables`), `db_insert(&M) -> u64`, `db_get::<M>(id) ->
///   Option<M>`, `db_find_by::<M>(col, Val) -> Vec<M>`,
///   `db_update::<M>(id, &M)`, `db_delete::<M>(id)`. These serialize via
///   the model's `Field` impls, so model code never hand-builds columns.
///
/// And these `use` statements are emitted so handlers don't need to repeat them:
/// `Deserialize`, `Serialize`, `json`, `response`, `Params`, `Req`, `Router`,
/// `Ctx`, `Row`, `Table`, `Val`, `DEFAULT_OWNER_COL`, `store` (the WIT
/// bindings module).
///
/// Two write paths exist. (1) **Raw**: `store::insert(table, &[store::Column {
/// name, val: store::Value::* }])` and `store::update` / `store::delete` — used
/// when you don't have a model. Hand-written `(name, Val)` columns are not a
/// raw-write API: in raw writes, build `store::Column` with `store::Value::*`,
/// not `Val::*`. (2) **Typed-model**: `db_insert`/`db_update`/`db_delete` over a
/// `#[derive(Model)]` type, which serialize through the model's `Field` impls
/// (the macro constructs `Val` for you — you never write `Val::*` literals).
/// `Val` remains the SDK's portable value type underneath both the `Row`
/// read accessors and the model write path.
#[macro_export]
macro_rules! wit_glue {
    ($bindings:ident, $api_struct:ident) => {
        // -- Re-exports / common imports --
        // These shadow per-call qualifiers so handler code reads cleanly.
        #[allow(unused_imports)]
        use $bindings::boogy::platform::store;
        // Bridge the guest-generated `store-error` enum onto the SDK's
        // binding-agnostic `StoreError`. Orphan-rule-legal: foreign trait,
        // local guest `Self`. Lets `StoreError::from_wit` stay generic.
        impl $crate::store::IntoStoreError for store::StoreError {
            fn into_store_error(self) -> $crate::store::StoreError {
                use $crate::store::StoreError as S;
                match self {
                    store::StoreError::QuotaExceeded(m)       => S::QuotaExceeded(m),
                    store::StoreError::NotFound(m)            => S::NotFound(m),
                    store::StoreError::Conflict(m)            => S::Conflict(m),
                    store::StoreError::ConstraintViolation(m) => S::ConstraintViolation(m),
                    store::StoreError::InvalidArgument(m)     => S::InvalidArgument(m),
                    store::StoreError::Unsupported(m)         => S::Unsupported(m),
                    store::StoreError::Timeout(m)             => S::Timeout(m),
                    store::StoreError::VersionMismatch(m)     => S::VersionMismatch(m),
                    store::StoreError::ResourceExhausted(m)   => S::ResourceExhausted(m),
                    store::StoreError::Internal(m)            => S::Internal(m),
                }
            }
        }
        // Typed conversion into `ApiError` so `?` on a raw store call
        // inside an `ApiError`-returning handler preserves the variant's
        // status (quota → 507, conflict → 409, …). Orphan-legal: the
        // type parameter `store::StoreError` is the local guest type.
        impl ::core::convert::From<store::StoreError> for $crate::error::ApiError {
            fn from(e: store::StoreError) -> Self {
                $crate::store::StoreError::from_wit(e).into()
            }
        }
        // Lossy String conversion so the SDK's `Result<_, String>` macro
        // helpers (migrations, `tx`, the `__boogy_*` row
        // helpers) keep bridging raw WIT store errors with bare `?`, and
        // `.map_err(ApiError::internal)` at example call sites still
        // compiles. The message survives; the variant is dropped.
        impl ::core::convert::From<store::StoreError> for ::std::string::String {
            fn from(e: store::StoreError) -> Self {
                ::std::string::ToString::to_string(
                    &$crate::store::StoreError::from_wit(e),
                )
            }
        }
        #[allow(unused_imports)]
        use $bindings::boogy::platform::peer as peer_bindings;
        #[allow(unused_imports)]
        use $bindings::boogy::platform::secrets as secrets_bindings;
        #[allow(unused_imports)]
        use $bindings::boogy::platform::signing as signing_bindings;
        #[allow(unused_imports)]
        use $bindings::boogy::platform::background_jobs as jobs_bindings;
        #[allow(unused_imports)]
        use $bindings::boogy::platform::vector as vector_bindings;
        #[allow(unused_imports)]
        use $bindings::boogy::platform::websockets as ws_bindings;
        #[allow(unused_imports)]
        use $crate::json::{self, Deserialize, Serialize};
        #[allow(unused_imports)]
        use $crate::response::{self, Created, IntoResponse, Json, NoContent, Redirect};
        #[allow(unused_imports)]
        use $crate::router::{Params, Req, Router};
        #[allow(unused_imports)]
        use $crate::ctx::Ctx;
        #[allow(unused_imports)]
        use $crate::DEFAULT_OWNER_COL;
        #[allow(unused_imports)]
        use $crate::error::{parse_body, validate_body, ApiError};
        // Note: `Val` is intentionally NOT re-exported. `Val` is the
        // SDK's portable read-side value type returned by `Row`
        // accessors; user write paths always go through the WIT
        // `store::Value::*` enum (e.g. `store::Value::Text(...)`).
        // Re-exporting both confused authors into reaching for `Val::*`
        // in writes, which doesn't compose with `store::insert` /
        // `store::update`. The unqualified surface now teaches one
        // shape per concern.
        #[allow(unused_imports)]
        use $crate::store::{Row, StoreError, Table};
        // NOTE: `Query` from `boogy_sdk::extract` is the handler-parameter
        // extractor (added by the 2026-05-22 handler-extractors slice).
        // The typed-query DSL also emits a `pub struct Query` into
        // consumer scope at the end of this macro — to avoid a name
        // collision we import the extractor as `QueryExtractor` here.
        // Consumers who need the handler extractor should `use
        // boogy_sdk::extract::Query as QueryExtractor` themselves.
        // Long-term fix tracked as a follow-up: rename `extract::Query`
        // to `extract::QueryParams`.
        #[allow(unused_imports)]
        use $crate::{FromRequest, Path, Query as QueryExtractor, Principal};

        // -- WIT ↔ SDK request/response converters (private — used only
        //    by the generated Guest impl) --
        fn __boogy_to_sdk_request(
            req: &$bindings::exports::boogy::platform::http_handler::HttpRequest,
        ) -> $crate::Request {
            $crate::Request {
                method: req.method.clone(),
                path: req.path.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
                path_params: req.path_params.clone(),
                query_params: req.query_params.clone(),
            }
        }

        fn __boogy_to_wit_response(
            resp: $crate::response::HttpResponse,
        ) -> $bindings::exports::boogy::platform::http_handler::HttpResponse {
            $bindings::exports::boogy::platform::http_handler::HttpResponse {
                status: resp.status,
                headers: resp.headers,
                body: resp.body,
            }
        }

        // -- WIT row → SDK row converter (user-facing — handlers may call
        //    this directly when iterating raw store::find results) --
        fn to_sdk_row(row: &$bindings::boogy::platform::store::Row) -> $crate::store::Row {
            $crate::store::Row {
                columns: row.columns.iter().map(|c| {
                    let val = match &c.val {
                        $bindings::boogy::platform::store::Value::Null      => $crate::store::Val::Null,
                        $bindings::boogy::platform::store::Value::Text(s)   => $crate::store::Val::Text(s.clone()),
                        $bindings::boogy::platform::store::Value::Integer(i)=> $crate::store::Val::Integer(*i),
                        $bindings::boogy::platform::store::Value::Real(f)   => $crate::store::Val::Real(*f),
                        $bindings::boogy::platform::store::Value::Blob(b)   => $crate::store::Val::Blob(b.clone()),
                        $bindings::boogy::platform::store::Value::Boolean(b)=> $crate::store::Val::Boolean(*b),
                    };
                    (c.name.clone(), val)
                }).collect(),
            }
        }

        // -- Cascade action SDK→WIT mapping (used by FK threading below) --
        fn __boogy_cascade(
            a: $crate::store::CascadeAction,
        ) -> $bindings::boogy::platform::store::CascadeAction {
            match a {
                $crate::store::CascadeAction::NoAction => $bindings::boogy::platform::store::CascadeAction::NoAction,
                $crate::store::CascadeAction::Restrict => $bindings::boogy::platform::store::CascadeAction::Restrict,
                $crate::store::CascadeAction::Cascade  => $bindings::boogy::platform::store::CascadeAction::Cascade,
                $crate::store::CascadeAction::SetNull  => $bindings::boogy::platform::store::CascadeAction::SetNull,
            }
        }

        // -- Table builder → WIT create_table + create_index calls --
        fn create_table_from(table: &$crate::store::Table) {
            let cols: Vec<$bindings::boogy::platform::store::ColumnDef> =
                table.columns.iter().map(|c| {
                    $bindings::boogy::platform::store::ColumnDef {
                        name: c.name.clone(),
                        col_type: match c.col_type {
                            $crate::store::ColType::Text     => $bindings::boogy::platform::store::ColumnType::Text,
                            $crate::store::ColType::Integer  => $bindings::boogy::platform::store::ColumnType::Integer,
                            $crate::store::ColType::Real     => $bindings::boogy::platform::store::ColumnType::Real,
                            $crate::store::ColType::Blob     => $bindings::boogy::platform::store::ColumnType::Blob,
                            $crate::store::ColType::Boolean  => $bindings::boogy::platform::store::ColumnType::Boolean,
                        },
                        nullable: c.nullable,
                        unique: c.unique,
                        references: c.references.as_ref().map(|fk| {
                            $bindings::boogy::platform::store::ForeignKey {
                                references_table: fk.references_table.clone(),
                                references_column: fk.references_column.clone(),
                                on_delete: __boogy_cascade(fk.on_delete),
                                on_update: __boogy_cascade(fk.on_update),
                            }
                        }),
                        default: None,
                    }
                }).collect();
            // create_table: guarded by list_tables. Skip if table already exists;
            // propagate genuine engine errors via unwrap_or_else with context. The
            // earlier list_tables idempotency drift has been fixed at the engine +
            // host layer — strict propagation, no workaround needed.
            //
            // Two errors are silently skipped, never panicked: "store capability
            // not granted" (access denied — the API fails properly on the first
            // data op) and "already exists" (a concurrent deploy created the
            // table/index between our stale list_* guard and now — idempotent).
            // Panicking here would trap inside wasm instead of returning 500.
            let table_exists = $bindings::boogy::platform::store::list_tables()
                .map(|v| v.iter().any(|t| t.name == table.name))
                .unwrap_or(false);
            if !table_exists {
                let options = $bindings::boogy::platform::store::CreateTableOptions {
                    encryption: match table.encryption {
                        $crate::store::EncryptionMode::None =>
                            $bindings::boogy::platform::store::EncryptionMode::None,
                        $crate::store::EncryptionMode::Enabled =>
                            $bindings::boogy::platform::store::EncryptionMode::Enabled,
                    },
                };
                $bindings::boogy::platform::store::create_table(&table.name, &cols, options)
                    .unwrap_or_else(|e| {
                        let msg = ::std::string::String::from(e);
                        if !msg.contains("not granted") && !msg.contains("already exists") {
                            panic!(
                                "create_table({}) in create_table_from failed: {}",
                                &table.name, msg,
                            );
                        }
                    });
            }

            // Resolve declared access patterns + explicit indexes into the
            // physical index set; surface build-time diagnostics via logging.
            let (__resolved, __diags) = table.resolved_indices();
            for d in &__diags {
                match d {
                    $crate::schema_resolve::Diagnostic::Warning(m) =>
                        $crate::log::warn!("schema {}: {}", table.name, m),
                    $crate::schema_resolve::Diagnostic::Error(m) =>
                        panic!("schema {}: {}", table.name, m),
                }
            }
            // create_index per resolved index: guarded by list_indexes. Skip if
            // already present; propagate genuine engine errors via unwrap_or_else
            // with context. Same "not granted" soft-skip as create_table above.
            for idx in &__resolved {
                let already = list_indexes(&table.name)
                    .map(|v| v.iter().any(|i| i.name == idx.name))
                    .unwrap_or(false);
                if !already {
                    $bindings::boogy::platform::store::create_index(
                        &table.name,
                        &$bindings::boogy::platform::store::IndexDef {
                            name: idx.name.clone(),
                            columns: idx.columns.clone(),
                            unique: idx.unique,
                            covering: idx.covering,
                        },
                    )
                    .unwrap_or_else(|e| {
                        let msg = ::std::string::String::from(e);
                        // "not granted" → store capability denied (soft-skip).
                        // "already exists" → a concurrent deploy created this index
                        // between our stale list_indexes() guard and now; idempotent
                        // success, not a failure. Anything else is a real error.
                        if !msg.contains("not granted") && !msg.contains("already exists") {
                            panic!(
                                "create_index({}.{}) in create_table_from failed: {}",
                                &table.name, &idx.name, msg,
                            );
                        }
                    });
                }
            }

            // Orphaned-index warning: an index left behind by a removed access
            // pattern (present on the table, no longer in the resolved desired
            // set). We never auto-drop (destructive) — surface it so the author
            // adds an explicit drop_index migration. Hand-managed names (no
            // ix_/idx_ prefix) and the implicit _id PK are ignored by `orphaned`.
            if let Ok(existing) = list_indexes(&table.name) {
                let actual: ::std::vec::Vec<::std::string::String> =
                    existing.into_iter().map(|i| i.name).collect();
                for orphan in $crate::schema_resolve::orphaned(&__resolved, &actual) {
                    $crate::log::warn!(
                        "schema {}: index '{}' is no longer declared by any access pattern \
                         (add an explicit drop_index migration to remove it)",
                        table.name, orphan
                    );
                }
            }
        }

        // -- Column migration free fns (map ColumnSpec ↔ ColumnDef / ColumnInfo) --

        /// Add a column to an existing table. Maps the SDK [`ColumnSpec`]
        /// to the WIT `column-def` (same `ColType→ColumnType` match as
        /// `create_table_from`; `default` via `__boogy_val_to_wit`).
        ///
        /// The host enforces the operation strictly — call from a migration
        /// body, not from `init_tables` (which may re-run on a table that
        /// already has the column). For idempotent use, prefer `MigrationCtx`
        /// (Task 5) which guards with `list_columns` first.
        fn add_column(
            table: &str,
            spec: &$crate::store::ColumnSpec,
        ) -> ::core::result::Result<(), ::std::string::String> {
            let cd = $bindings::boogy::platform::store::ColumnDef {
                name: spec.name.clone(),
                col_type: match spec.col_type {
                    $crate::store::ColType::Text     => $bindings::boogy::platform::store::ColumnType::Text,
                    $crate::store::ColType::Integer  => $bindings::boogy::platform::store::ColumnType::Integer,
                    $crate::store::ColType::Real     => $bindings::boogy::platform::store::ColumnType::Real,
                    $crate::store::ColType::Blob     => $bindings::boogy::platform::store::ColumnType::Blob,
                    $crate::store::ColType::Boolean  => $bindings::boogy::platform::store::ColumnType::Boolean,
                },
                nullable: spec.nullable,
                unique: spec.unique,
                references: None,
                default: spec.default.as_ref().map(|v| __boogy_val_to_wit(v)),
            };
            $bindings::boogy::platform::store::add_column(table, &cd)
                .map_err(::std::string::String::from)
        }

        /// Rename a column in an existing table.
        fn rename_column(
            table: &str,
            old: &str,
            new: &str,
        ) -> ::core::result::Result<(), ::std::string::String> {
            $bindings::boogy::platform::store::rename_column(table, old, new)
                .map_err(::std::string::String::from)
        }

        /// Drop a column from an existing table.
        fn drop_column(
            table: &str,
            name: &str,
        ) -> ::core::result::Result<(), ::std::string::String> {
            $bindings::boogy::platform::store::drop_column(table, name)
                .map_err(::std::string::String::from)
        }

        /// List the current columns of a table, returning [`ColumnInfo`]
        /// for each. Useful for idempotency guards in migrations — check
        /// whether a column already exists before calling `add_column`.
        fn list_columns(
            table: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::ColumnInfo>, ::std::string::String> {
            let wit_cols = $bindings::boogy::platform::store::list_columns(table)?;
            Ok(wit_cols.into_iter().map(|ci| {
                $crate::store::ColumnInfo {
                    name: ci.name,
                    col_type: match ci.col_type {
                        $bindings::boogy::platform::store::ColumnType::Text    => $crate::store::ColType::Text,
                        $bindings::boogy::platform::store::ColumnType::Integer => $crate::store::ColType::Integer,
                        $bindings::boogy::platform::store::ColumnType::Real    => $crate::store::ColType::Real,
                        $bindings::boogy::platform::store::ColumnType::Blob    => $crate::store::ColType::Blob,
                        $bindings::boogy::platform::store::ColumnType::Boolean => $crate::store::ColType::Boolean,
                    },
                    nullable: ci.nullable,
                }
            }).collect())
        }

        /// List the current indexes on a table, returning [`IndexInfo`] for
        /// each. Useful for idempotency guards in migrations — check whether
        /// an index already exists before calling `create_index`.
        fn list_indexes(
            table: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::IndexInfo>, ::std::string::String> {
            let wit_idxs = $bindings::boogy::platform::store::list_indexes(table)?;
            Ok(wit_idxs.into_iter().map(|i| {
                $crate::store::IndexInfo {
                    name: i.name,
                    columns: i.columns,
                    unique: i.unique,
                }
            }).collect())
        }

        /// List the tables in this store with lightweight per-table metadata
        /// (name + live column count + user-defined index count).
        ///
        /// Sorted ascending by name. Callers who want full schema use
        /// `list_columns(name)` / `list_indexes(name)`.
        fn list_tables() -> ::core::result::Result<
            ::std::vec::Vec<$crate::store::TableInfo>,
            ::std::string::String,
        > {
            let wit_tables = $bindings::boogy::platform::store::list_tables()?;
            Ok(wit_tables.into_iter().map(|t| $crate::store::TableInfo {
                name: t.name,
                column_count: t.column_count,
                index_count: t.index_count,
            }).collect())
        }

        // -- SDK Val → WIT Value (write direction) --
        fn __boogy_val_to_wit(
            v: &$crate::store::Val,
        ) -> $bindings::boogy::platform::store::Value {
            match v {
                $crate::store::Val::Null       => $bindings::boogy::platform::store::Value::Null,
                $crate::store::Val::Text(s)    => $bindings::boogy::platform::store::Value::Text(s.clone()),
                $crate::store::Val::Integer(i) => $bindings::boogy::platform::store::Value::Integer(*i),
                $crate::store::Val::Real(f)    => $bindings::boogy::platform::store::Value::Real(*f),
                $crate::store::Val::Blob(b)    => $bindings::boogy::platform::store::Value::Blob(b.clone()),
                $crate::store::Val::Boolean(b) => $bindings::boogy::platform::store::Value::Boolean(*b),
            }
        }

        /// Internal: convert SDK `(name, Val)` pairs to WIT `Column`
        /// records. Used by the macro-private write helpers below
        /// (which the api_keys glue calls). User code should NOT use
        /// this — write paths in user code use
        /// `store::insert(table, &[store::Column { name, val:
        /// store::Value::* }])` directly with the WIT types.
        fn __boogy_to_wit_columns(
            cols: &[(::std::string::String, $crate::store::Val)],
        ) -> ::std::vec::Vec<$bindings::boogy::platform::store::Column> {
            cols.iter()
                .map(|(name, val)| $bindings::boogy::platform::store::Column {
                    name: name.clone(),
                    val: __boogy_val_to_wit(val),
                })
                .collect()
        }

        // -- Convenience helpers for typed row reads --
        //
        // Errors flow through `StoreError`. The host carries a typed
        // `store-error` variant across WIT; `StoreError::from_wit` bridges
        // the guest-generated enum (via the `IntoStoreError` impl above)
        // into the SDK's `StoreError` — no string-matching. The
        // `From<StoreError>` impls for `ApiError` and `RpcError` mean `?`
        // works in both REST and JSON-RPC handlers.

        fn get_row(
            table: &str,
            id: u64,
        ) -> ::core::result::Result<::core::option::Option<$crate::store::Row>, $crate::store::StoreError> {
            match $bindings::boogy::platform::store::get(table, id) {
                Ok(Some(r)) => Ok(Some(to_sdk_row(&r))),
                Ok(None) => Ok(None),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Batch get by primary key. One entry per id, positional; a missing row
        /// is `None`. The host pipelines the gets into ~1 round-trip — prefer this
        /// over a `get_row` loop when hydrating a known set of ids.
        #[allow(dead_code)]
        fn get_many(
            table: &str,
            ids: &[u64],
        ) -> ::core::result::Result<::std::vec::Vec<::core::option::Option<$crate::store::Row>>, $crate::store::StoreError> {
            match $bindings::boogy::platform::store::get_many(table, &ids.to_vec()) {
                Ok(rows) => Ok(rows.into_iter().map(|r| r.map(|r| to_sdk_row(&r))).collect()),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Batch of independent `find` queries (possibly different tables), run
        /// as one pipelined host round-trip in autocommit (sequential inside an
        /// ambient `tx`). Build each query with the same `Query` chain as
        /// `fetch_all`; the result is positional — `out[i]` is the rows for
        /// `queries[i]`. Totals are discarded (rows only), matching `fetch_all`.
        /// Prefer this over a loop of `.fetch_all()` when reading independent
        /// sets across tables — the host pipelines them instead of paying one
        /// round-trip chain each.
        #[allow(dead_code)]
        fn find_many(
            queries: ::std::vec::Vec<Query>,
        ) -> ::core::result::Result<
            ::std::vec::Vec<::std::vec::Vec<$crate::store::Row>>,
            $crate::store::StoreError,
        > {
            let wit_queries: ::std::vec::Vec<$bindings::boogy::platform::store::FindQuery> = queries
                .iter()
                .map(|q| {
                    let (filters, or_groups, sort, page) = q.to_wit_args();
                    $bindings::boogy::platform::store::FindQuery {
                        table: q.0.table.clone(),
                        options: $bindings::boogy::platform::store::FindOptions {
                            filters,
                            sort,
                            page,
                            or_groups,
                            allow_full_scan: q.0.allow_full_scan,
                            // Rows-only helper → discard totals (count-elision
                            // fast path on the index walk).
                            skip_total: true,
                        },
                    }
                })
                .collect();
            match $bindings::boogy::platform::store::find_many(&wit_queries) {
                Ok(results) => Ok(results
                    .into_iter()
                    .map(|r| r.rows.iter().map(|row| to_sdk_row(row)).collect())
                    .collect()),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Per-call page size the SDK uses when paginating internally.
        /// The host enforces a hard per-call row ceiling
        /// (`BOOGY_STORE_MAX_PAGE_ROWS`, default 1000) on the raw WIT
        /// `find`, so SDK "return all rows" helpers loop across pages
        /// rather than issuing a single unbounded `page: None` call.
        /// Robust even when the host clamps this batch below 1000.
        const SDK_FIND_BATCH: u32 = 1000;

        fn find_all_rows(
            table: &str,
        ) -> ::core::result::Result<(::std::vec::Vec<$crate::store::Row>, u64), $crate::store::StoreError> {
            let mut rows: ::std::vec::Vec<$crate::store::Row> = ::std::vec::Vec::new();
            let mut offset: u32 = 0;
            let total = loop {
                let res = $bindings::boogy::platform::store::find(
                    table,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters: vec![],
                        sort: vec![],
                        page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset }),
                        or_groups: vec![],
                        allow_full_scan: false,
                        skip_total: false,
                    },
                )
                .map_err($crate::store::StoreError::from_wit)?;
                let n = res.rows.len() as u32;
                rows.extend(res.rows.iter().map(|r| to_sdk_row(r)));
                // Terminate ONLY on an empty page. We deliberately do NOT stop
                // on `n < SDK_FIND_BATCH` (the host may clamp the page below
                // SDK_FIND_BATCH, so a "short" page is normal, not the end) NOR
                // on `rows.len() >= total_count`: a concurrent delete can shrink
                // total_count below the already-collected count and truncate the
                // result early. An empty page is the only reliable terminator.
                if n == 0 {
                    break res.total_count;
                }
                offset += n;
            };
            Ok((rows, total))
        }

        // -- Typed model CRUD (bridge to the WIT store; see boogy_sdk::model) --

        /// Insert a `Model`, returning the new row's `_id`.
        fn db_insert<M: $crate::model::Model>(
            m: &M,
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            let cols = __boogy_to_wit_columns(&m.to_columns());
            match $bindings::boogy::platform::store::insert(M::TABLE, &cols) {
                Ok(id) => Ok(id),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Fetch a `Model` by `_id`.
        fn db_get<M: $crate::model::Model>(
            id: u64,
        ) -> ::core::result::Result<::core::option::Option<M>, $crate::store::StoreError> {
            match get_row(M::TABLE, id)? {
                Some(row) => Ok(Some(M::from_row(&row))),
                None => Ok(None),
            }
        }

        /// Fetch all `Model` rows where `col == val`.
        fn db_find_by<M: $crate::model::Model>(
            col: &str,
            val: $crate::store::Val,
        ) -> ::core::result::Result<::std::vec::Vec<M>, $crate::store::StoreError> {
            let wit_val = __boogy_val_to_wit(&val);
            let mut out: ::std::vec::Vec<M> = ::std::vec::Vec::new();
            let mut offset: u32 = 0;
            loop {
                let res = $bindings::boogy::platform::store::find(
                    M::TABLE,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters: vec![$bindings::boogy::platform::store::Filter {
                            column: col.to_string(),
                            op: $bindings::boogy::platform::store::FilterOp::Eq,
                            val: wit_val.clone(),
                            in_values: None,
                        }],
                        sort: vec![],
                        page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset }),
                        or_groups: vec![],
                        allow_full_scan: false,
                        skip_total: false,
                    },
                )
                .map_err($crate::store::StoreError::from_wit)?;
                let n = res.rows.len() as u32;
                out.extend(res.rows.iter().map(|r| M::from_row(&to_sdk_row(r))));
                if n == 0 || out.len() as u64 >= res.total_count {
                    break;
                }
                offset += n;
            }
            Ok(out)
        }

        /// Overwrite the row at `id` with the model's columns.
        fn db_update<M: $crate::model::Model>(
            id: u64,
            m: &M,
        ) -> ::core::result::Result<(), $crate::store::StoreError> {
            let cols = __boogy_to_wit_columns(&m.to_columns());
            $bindings::boogy::platform::store::update(M::TABLE, id, &cols)
                .map(|_| ())
                .map_err($crate::store::StoreError::from_wit)
        }

        /// Delete the row at `id`.
        fn db_delete<M: $crate::model::Model>(
            id: u64,
        ) -> ::core::result::Result<(), $crate::store::StoreError> {
            $bindings::boogy::platform::store::delete(M::TABLE, id)
                .map(|_| ())
                .map_err($crate::store::StoreError::from_wit)
        }

        /// Register a `Model`'s table + indexes (use in `init_tables`).
        fn create_model<M: $crate::model::Model>() {
            create_table_from(&M::schema());
        }

        /// Register a `Model`'s schema under an OVERRIDDEN table name plus a
        /// caller-supplied index set — for families of identically-shaped tables
        /// whose names are only known at runtime (e.g. one table per time
        /// window). The model supplies the column set + types via its
        /// `schema()`; `table` replaces the model's compile-time `TABLE`, and
        /// `indices` replaces the model's declared indexes (their names usually
        /// need to embed the per-table suffix, which a single model can't
        /// express). Idempotent (CREATE TABLE / index IF NOT EXISTS), same as
        /// `create_model`.
        #[allow(dead_code)]
        fn create_model_as<M: $crate::model::Model>(
            table: &str,
            indices: ::std::vec::Vec<$crate::store::Index>,
        ) {
            let mut schema = M::schema();
            schema.name = table.to_string();
            schema.indices = indices;
            create_table_from(&schema);
        }

        /// Atomic keyed counter: `counter += delta`, upserting the row
        /// identified by the composite `key`. First call inserts
        /// (`counter = delta` + the `set` columns); later calls
        /// increment the counter and overwrite the `set` columns. The
        /// host runs this in the engine's ACID mode so concurrent
        /// increments compose. `delta` must be an integer or real value
        /// (the host rejects others). Requires a `unique` index on the
        /// `key` columns. Returns the row id.
        fn upsert_increment(
            table: &str,
            key: &[$bindings::boogy::platform::store::Column],
            counter: &str,
            delta: $bindings::boogy::platform::store::Value,
            set: &[$bindings::boogy::platform::store::Column],
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            match $bindings::boogy::platform::store::upsert_increment(
                table, key, counter, &delta, set,
            ) {
                Ok(id) => Ok(id),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Atomic insert-or-update keyed on a unique index. If a row
        /// with matching `key` columns exists, update `set` columns
        /// on it (key columns untouched). Otherwise insert a new row
        /// with `key + set`. Returns the row id (existing or new).
        ///
        /// PRECONDITION: the `key` columns must correspond to an
        /// existing unique index on the table.
        ///
        /// ```ignore
        /// upsert(
        ///     "user_affinity_edges",
        ///     &[col("user_a", a.clone()), col("user_b", b.clone())],
        ///     &[col("weight", weight), col("updated_at", now)],
        /// )?;
        /// ```
        fn upsert(
            table: &str,
            key: &[$bindings::boogy::platform::store::Column],
            set: &[$bindings::boogy::platform::store::Column],
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            match $bindings::boogy::platform::store::upsert(table, key, set) {
                Ok(id) => Ok(id),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Stream a table in ordered batches with bounded memory.
        ///
        /// Opens a stateless `row-cursor` over `table` (applying
        /// `filters` + `or_groups` per row, walking in `order_col` /
        /// `dir` order — `order_col = None` is primary-key order) and
        /// calls `f` once per batch of up to `batch_size` rows until the
        /// table is exhausted. Every matching row is visited exactly
        /// once, in order, with no gaps or duplicates. This is the
        /// bounded-memory alternative to `find_all_rows` / offset
        /// paging for large-table batch jobs: only `batch_size` rows are
        /// ever materialized at a time, and the cursor resumes strictly
        /// after the last row of the prior batch (no offset re-scan).
        ///
        /// If `f` returns `Err`, iteration stops and the error
        /// propagates. The cursor resource is dropped when the loop
        /// ends (the host reaps it).
        fn for_each_batch(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
            order_col: ::core::option::Option<&str>,
            dir: $bindings::boogy::platform::store::SortDir,
            batch_size: u32,
            mut f: impl ::core::ops::FnMut(&[$crate::store::Row]) -> ::core::result::Result<(), $crate::store::StoreError>,
        ) -> ::core::result::Result<(), $crate::store::StoreError> {
            let cursor = $bindings::boogy::platform::store::open_cursor(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters,
                    sort: vec![],
                    page: None,
                    or_groups,
                    allow_full_scan: false,
                    skip_total: false,
                },
                &$bindings::boogy::platform::store::ScanOrder {
                    column: order_col.map(|s| s.to_string()),
                    dir,
                },
            )
            .map_err($crate::store::StoreError::from_wit)?;
            loop {
                let batch = cursor
                    .next_batch(batch_size)
                    .map_err($crate::store::StoreError::from_wit)?;
                if batch.is_empty() {
                    break;
                }
                let rows: ::std::vec::Vec<$crate::store::Row> =
                    batch.iter().map(|r| to_sdk_row(r)).collect();
                f(&rows)?;
            }
            Ok(())
        }

        /// Eager-load related child rows for a set of parent ids,
        /// grouped by FK so handlers can splice children onto
        /// parents in O(1) per parent. The whole batch is one
        /// `SELECT * FROM <child> WHERE <fk> IN (?, ?, ...)` call,
        /// regardless of how many parents are in scope — closes the
        /// N+1 trap for `User::with(Posts)`-style listing endpoints.
        ///
        /// Empty `parent_ids` short-circuits without a query.
        ///
        /// See [`boogy_sdk::relations`] for the design rationale
        /// and the [`group_by_column`] / [`build_in_query`] primitives
        /// this wrapper composes.
        fn load_has_many(
            child_table: &str,
            fk_column: &str,
            parent_ids: &[u64],
        ) -> ::core::result::Result<
            ::std::collections::HashMap<u64, ::std::vec::Vec<$crate::store::Row>>,
            $crate::store::StoreError,
        > {
            if parent_ids.is_empty() {
                return Ok(::std::collections::HashMap::new());
            }
            let in_vals: ::std::vec::Vec<$bindings::boogy::platform::store::Value> =
                parent_ids
                    .iter()
                    .map(|id| $bindings::boogy::platform::store::Value::Integer(*id as i64))
                    .collect();
            let mut sdk_rows: ::std::vec::Vec<$crate::store::Row> = ::std::vec::Vec::new();
            let mut offset: u32 = 0;
            loop {
                let res = $bindings::boogy::platform::store::find(
                    child_table,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters: vec![$bindings::boogy::platform::store::Filter {
                            column: fk_column.to_string(),
                            op: $bindings::boogy::platform::store::FilterOp::In,
                            val: $bindings::boogy::platform::store::Value::Null,
                            in_values: Some(in_vals.clone()),
                        }],
                        sort: vec![],
                        page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset }),
                        or_groups: vec![],
                        allow_full_scan: false,
                        skip_total: false,
                    },
                )
                .map_err($crate::store::StoreError::from_wit)?;
                let n = res.rows.len() as u32;
                sdk_rows.extend(res.rows.iter().map(|r| to_sdk_row(r)));
                if n == 0 || sdk_rows.len() as u64 >= res.total_count {
                    break;
                }
                offset += n;
            }
            Ok($crate::relations::group_by_column_u64(sdk_rows, fk_column))
        }

        /// Find the first row whose `column` equals `val`. Returns
        /// `Ok(None)` when no row matches. Convenience for indexed
        /// uniqueness lookups (e.g. find an api-key row by its prefix).
        ///
        /// Takes the WIT `store::Value` directly so user code can
        /// write `find_row_by(t, c, store::Value::Text(x))` consistent
        /// with `store::insert` and `store::update`.
        fn find_row_by(
            table: &str,
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> ::core::result::Result<::core::option::Option<$crate::store::Row>, $crate::store::StoreError> {
            match $bindings::boogy::platform::store::find(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters: vec![$bindings::boogy::platform::store::Filter {
                        column: column.to_string(),
                        op: $bindings::boogy::platform::store::FilterOp::Eq,
                        val,
                        in_values: None,
                    }],
                    sort: vec![],
                    page: Some($bindings::boogy::platform::store::Page { limit: 1, offset: 0 }),
                    or_groups: vec![],
                    allow_full_scan: false,
                    skip_total: false,
                },
            ) {
                Ok(result) => Ok(result.rows.first().map(to_sdk_row)),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Build a single-equality `Filter` for `column = val`. Tiny
        /// helper so the WHERE clause boilerplate is one line at every
        /// call site instead of four (`column`, `op`, `val`,
        /// `in_values`).
        ///
        /// `filter_eq` is one of a family of builders covering the full
        /// `store::FilterOp` set so callers never hand-write the
        /// `Filter { column, op, val, in_values }` literal (and never
        /// fumble the `in_values: None` boilerplate that only `In`
        /// uses): [`filter_neq`], [`filter_gt`], [`filter_gte`],
        /// [`filter_lt`], [`filter_lte`], [`filter_like`],
        /// [`filter_not_like`], [`filter_is_null`],
        /// [`filter_is_not_null`], [`filter_in`].
        fn filter_eq(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Eq,
                val,
                in_values: None,
            }
        }

        /// `column != val`. See [`filter_eq`] for the builder family.
        fn filter_neq(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Neq,
                val,
                in_values: None,
            }
        }

        /// `column > val`. See [`filter_eq`] for the builder family.
        fn filter_gt(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Gt,
                val,
                in_values: None,
            }
        }

        /// `column >= val`. See [`filter_eq`] for the builder family.
        fn filter_gte(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Gte,
                val,
                in_values: None,
            }
        }

        /// `column < val`. See [`filter_eq`] for the builder family.
        fn filter_lt(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Lt,
                val,
                in_values: None,
            }
        }

        /// `column <= val`. See [`filter_eq`] for the builder family.
        fn filter_lte(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Lte,
                val,
                in_values: None,
            }
        }

        /// `column LIKE val` (SQL `LIKE` pattern; `%` and `_` wildcards).
        /// See [`filter_eq`] for the builder family.
        fn filter_like(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Like,
                val,
                in_values: None,
            }
        }

        /// `column NOT LIKE val`. See [`filter_eq`] for the builder family.
        fn filter_not_like(
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::NotLike,
                val,
                in_values: None,
            }
        }

        /// `column IS NULL`. Takes no value (passes `Value::Null` to
        /// satisfy the record's required `val` field). See [`filter_eq`].
        fn filter_is_null(
            column: &str,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::IsNull,
                val: $bindings::boogy::platform::store::Value::Null,
                in_values: None,
            }
        }

        /// `column IS NOT NULL`. Takes no value. See [`filter_eq`].
        fn filter_is_not_null(
            column: &str,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::IsNotNull,
                val: $bindings::boogy::platform::store::Value::Null,
                in_values: None,
            }
        }

        /// `column IN (vals)`. The only op that uses `in_values`; the
        /// scalar `val` field is set to `Value::Null` and ignored by the
        /// host. See [`filter_eq`] for the builder family.
        fn filter_in(
            column: &str,
            vals: ::std::vec::Vec<$bindings::boogy::platform::store::Value>,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: column.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::In,
                val: $bindings::boogy::platform::store::Value::Null,
                in_values: Some(vals),
            }
        }

        /// Wrapper around `runtime::now_millis()` so user code in any
        /// module can call `crate::now_millis()` without spelling out
        /// `bindings::boogy::platform::runtime::now_millis()`. Returns
        /// unix milliseconds (u64).
        fn now_millis() -> u64 {
            $bindings::boogy::platform::runtime::now_millis()
        }

        /// This service's own host-pinned identity — the
        /// `(owner, service_id)` of the deployment currently executing.
        /// Wrapper around `runtime::self_identity()` so user code can call
        /// `crate::self_identity()` without spelling out the bindings
        /// path.
        ///
        /// The value is set by the host from the matched route (HTTP edge)
        /// / the CALLEE on a `peer::fetch` hop (so a callee reads ITS OWN
        /// identity, never the caller's) / the job target in a background
        /// job. It can never be derived from guest input or an inbound
        /// header, so it's safe to authorize on. Always available — no
        /// `[capabilities]` grant required.
        fn self_identity() -> $bindings::boogy::platform::runtime::ServiceIdentity {
            $bindings::boogy::platform::runtime::self_identity()
        }

        /// True iff the CALLER is this service's owner — the provisioner's own
        /// agent (their human/dashboard token, resolved host-side) or one of
        /// their own workloads. False for anonymous, a different owner, or an
        /// unresolvable caller (fail-closed). Host-attested — safe to authorize
        /// on. Lets a provisionable module gate an owner-only surface (e.g.
        /// `/admin`) WITHOUT hardcoding an identity in its manifest:
        /// ```ignore
        /// if !crate::caller_is_service_owner() { return Err(ApiError::forbidden("operator only")); }
        /// ```
        fn caller_is_service_owner() -> bool {
            $bindings::boogy::platform::runtime::caller_is_service_owner()
        }

        /// Build an ascending `SortBy` for `column`. Pairs with
        /// [`sort_desc`]; pass a `Vec` of these to `find_rows` for
        /// composite sort (e.g. `vec![sort_desc("score"), sort_asc("_id")]`).
        fn sort_asc(column: &str) -> $bindings::boogy::platform::store::SortBy {
            $bindings::boogy::platform::store::SortBy {
                column: column.to_string(),
                dir: $bindings::boogy::platform::store::SortDir::Asc,
            }
        }

        /// Build a descending `SortBy` for `column`. See [`sort_asc`].
        fn sort_desc(column: &str) -> $bindings::boogy::platform::store::SortBy {
            $bindings::boogy::platform::store::SortBy {
                column: column.to_string(),
                dir: $bindings::boogy::platform::store::SortDir::Desc,
            }
        }

        /// Build a `Page` (limit + offset). For the first page use
        /// `page(limit, 0)`. Wrap in `Some(...)` for `find_rows`'s
        /// `page` argument.
        fn page(limit: u32, offset: u32) -> $bindings::boogy::platform::store::Page {
            $bindings::boogy::platform::store::Page { limit, offset }
        }

        /// Multi-row read with an OR-of-AND clause. A row matches when
        /// `ALL(filters) AND (or_groups empty OR ANY(group: ALL(group)))`:
        /// `filters` is a mandatory AND-prefix, each inner `Vec` is one
        /// group (its own AND), and the groups are ORed together. Empty
        /// `or_groups` is exactly [`find_rows`].
        ///
        /// The canonical use is composite keyset pagination — AND-only
        /// filters can't express `(score < c) OR (score = c AND id < cursor)`:
        ///
        /// ```ignore
        /// let (page_rows, _total) = find_rows_grouped(
        ///     "posts",
        ///     vec![filter_eq("deleted_at", store::Value::Text(String::new()))], // AND-prefix
        ///     vec![
        ///         vec![filter_lt("score", store::Value::Integer(c))],
        ///         vec![filter_eq("score", store::Value::Integer(c)),
        ///              filter_lt("_id", store::Value::Integer(cursor))],
        ///     ],
        ///     vec![sort_desc("score"), sort_desc("_id")],
        ///     Some(page(20, 0)),
        /// )?;
        /// ```
        fn find_rows_grouped(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
            sort: ::std::vec::Vec<$bindings::boogy::platform::store::SortBy>,
            page: ::core::option::Option<$bindings::boogy::platform::store::Page>,
            allow_full_scan: bool,
            skip_total: bool,
        ) -> ::core::result::Result<(::std::vec::Vec<$crate::store::Row>, u64), $crate::store::StoreError> {
            match $bindings::boogy::platform::store::find(
                table,
                &$bindings::boogy::platform::store::FindOptions { filters, sort, page, or_groups, allow_full_scan, skip_total },
            ) {
                Ok(result) => {
                    let rows: ::std::vec::Vec<$crate::store::Row> =
                        result.rows.iter().map(|r| to_sdk_row(r)).collect();
                    Ok((rows, result.total_count))
                }
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// General-purpose multi-row read with composite filters,
        /// composite sort, and optional paging. Returns `(rows,
        /// total_count)` — the count is the total matching rows
        /// ignoring page limits (useful for showing "X total" in a
        /// UI). For simpler call sites prefer [`find_rows_by`] or
        /// [`find_all_rows`]; for an OR clause use [`find_rows_grouped`].
        ///
        /// ```ignore
        /// // Top-N posts by score in a window, paginated by created_at:
        /// let (posts, _total) = find_rows(
        ///     "posts",
        ///     vec![filter_eq("parent_post_id", store::Value::Integer(0))],
        ///     vec![sort_desc("score_1h"), sort_asc("_id")],
        ///     Some(page(20, 0)),
        /// )?;
        /// ```
        fn find_rows(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            sort: ::std::vec::Vec<$bindings::boogy::platform::store::SortBy>,
            page: ::core::option::Option<$bindings::boogy::platform::store::Page>,
        ) -> ::core::result::Result<(::std::vec::Vec<$crate::store::Row>, u64), $crate::store::StoreError> {
            find_rows_grouped(table, filters, ::std::vec::Vec::new(), sort, page, false, false)
        }

        /// Count rows matching `filters` in `table`. Delegates to the
        /// store `count` WIT fn. Free-fn sibling of `find_rows`.
        fn count_rows(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            $bindings::boogy::platform::store::count(table, &filters)
                .map_err(|e| $crate::store::StoreError::from_wit(e))
        }

        /// Find every row whose `column` equals `val`. Returns the full
        /// matching set (no limit). Parallel to [`find_row_by`] but
        /// returns `Vec<Row>` instead of `Option<Row>`.
        ///
        /// Internally paginates across host pages (the host enforces a
        /// hard per-call ceiling via `BOOGY_STORE_MAX_PAGE_ROWS`, default
        /// 1000) so all matching rows are always returned, not just the
        /// first 1000. Mirrors the loop semantics of [`find_all_rows`].
        ///
        /// For "all rows matching a filter, indexed query" — typical
        /// for many-row joins, backer-list lookups, etc.
        ///
        /// ```ignore
        /// let backers = find_rows_by(
        ///     "investments", "post_id", store::Value::Integer(post_id as i64),
        /// )?;
        /// ```
        fn find_rows_by(
            table: &str,
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::Row>, $crate::store::StoreError> {
            let mut rows: ::std::vec::Vec<$crate::store::Row> = ::std::vec::Vec::new();
            let mut offset: u32 = 0;
            loop {
                let res = $bindings::boogy::platform::store::find(
                    table,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters: vec![$bindings::boogy::platform::store::Filter {
                            column: column.to_string(),
                            op: $bindings::boogy::platform::store::FilterOp::Eq,
                            val: val.clone(),
                            in_values: None,
                        }],
                        sort: vec![],
                        page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset }),
                        or_groups: vec![],
                        allow_full_scan: false,
                        skip_total: false,
                    },
                )
                .map_err($crate::store::StoreError::from_wit)?;
                let n = res.rows.len() as u32;
                rows.extend(res.rows.iter().map(|r| to_sdk_row(r)));
                // Terminate ONLY on an empty page — see find_all_rows. Stopping
                // on `rows.len() >= total_count` truncates the result when a
                // concurrent delete shrinks total_count below the collected
                // count; a short page is normal under host page clamping.
                if n == 0 {
                    break;
                }
                offset += n;
            }
            Ok(rows)
        }

        // -- SDK Filter → WIT Filter conversion (used by keyset_paginate) --

        fn __boogy_filter_op_to_wit(
            op: &$crate::store::FilterOp,
        ) -> $bindings::boogy::platform::store::FilterOp {
            match op {
                $crate::store::FilterOp::Eq       => $bindings::boogy::platform::store::FilterOp::Eq,
                $crate::store::FilterOp::Neq      => $bindings::boogy::platform::store::FilterOp::Neq,
                $crate::store::FilterOp::Gt       => $bindings::boogy::platform::store::FilterOp::Gt,
                $crate::store::FilterOp::Gte      => $bindings::boogy::platform::store::FilterOp::Gte,
                $crate::store::FilterOp::Lt       => $bindings::boogy::platform::store::FilterOp::Lt,
                $crate::store::FilterOp::Lte      => $bindings::boogy::platform::store::FilterOp::Lte,
                $crate::store::FilterOp::Like     => $bindings::boogy::platform::store::FilterOp::Like,
                $crate::store::FilterOp::NotLike  => $bindings::boogy::platform::store::FilterOp::NotLike,
                $crate::store::FilterOp::IsNull   => $bindings::boogy::platform::store::FilterOp::IsNull,
                $crate::store::FilterOp::IsNotNull=> $bindings::boogy::platform::store::FilterOp::IsNotNull,
                $crate::store::FilterOp::In       => $bindings::boogy::platform::store::FilterOp::In,
            }
        }

        fn __boogy_sdk_filter_to_wit(
            f: &$crate::store::Filter,
        ) -> $bindings::boogy::platform::store::Filter {
            $bindings::boogy::platform::store::Filter {
                column: f.column.clone(),
                op: __boogy_filter_op_to_wit(&f.op),
                val: __boogy_val_to_wit(&f.val),
                in_values: f.in_values.as_ref().map(|vs| {
                    vs.iter().map(__boogy_val_to_wit).collect()
                }),
            }
        }

        fn __boogy_sdk_dir_to_wit(
            dir: $crate::store::SortDir,
        ) -> $bindings::boogy::platform::store::SortDir {
            match dir {
                $crate::store::SortDir::Asc  => $bindings::boogy::platform::store::SortDir::Asc,
                $crate::store::SortDir::Desc => $bindings::boogy::platform::store::SortDir::Desc,
            }
        }

        /// Keyset-paginate `table` sorted by `(sort_col, _id) <dir>`.
        ///
        /// Overfetches `limit + 1` rows to detect whether a next page
        /// exists without a separate count query. Returns
        /// `CursorPage<T>` with `next_cursor` set when more rows
        /// remain, and `None` on the last page.
        ///
        /// **`base_filters` / `base_or_groups`** are the caller's domain
        /// filters (soft-delete guards, FK filters, visibility rules, etc.).
        /// The keyset resume condition is merged in automatically:
        /// extra AND-filters are appended to `base_filters`; the keyset
        /// OR-group is appended to `base_or_groups`.
        ///
        /// **`row_to_item_and_cursor`** maps each kept `Row` to a pair
        /// `(T, Cursor)`. The `next_cursor` in the returned page is
        /// taken from the *last kept* row's cursor.
        ///
        /// Uses the correct OR-keyset expansion (via
        /// [`boogy_sdk::pagination::keyset_resume_filter`]) so all tied
        /// rows (rows with the same `sort_col` value) are included on
        /// subsequent pages — the single-column `sort_col < last_value`
        /// compromise that silently skips tied rows at page boundaries
        /// is NOT used here.
        ///
        /// # Example
        ///
        /// ```ignore
        /// let cursor = q.cursor.as_deref().and_then(decode);
        /// let page = keyset_paginate::<PostView, _>(
        ///     "posts",
        ///     vec![filter_eq("deleted_at", store::Value::Text(String::new()))],
        ///     vec![],
        ///     "created_at",
        ///     SortDir::Desc,
        ///     cursor,
        ///     q.limit as usize,
        ///     |row| {
        ///         let view = PostView { /* map fields */ };
        ///         let last_id    = row.id().to_string();
        ///         let last_value = serde_json::json!(row.int("created_at"));
        ///         (view, Cursor::keyset(last_id, last_value))
        ///     },
        /// )?;
        /// ```
        fn keyset_paginate<T, F>(
            table: &str,
            base_filters: ::std::vec::Vec<$crate::store::Filter>,
            base_or_groups: ::std::vec::Vec<::std::vec::Vec<$crate::store::Filter>>,
            sort_col: &str,
            dir: $crate::store::SortDir,
            cursor: ::core::option::Option<$crate::pagination::Cursor>,
            limit: usize,
            allow_full_scan: bool,
            row_to_item_and_cursor: F,
        ) -> ::core::result::Result<$crate::pagination::CursorPage<T>, $crate::error::ApiError>
        where
            T: ::serde::Serialize,
            F: ::core::ops::Fn(&$crate::store::Row) -> (T, $crate::pagination::Cursor),
        {
            use $crate::pagination::keyset_resume_filter;

            // Build the resume filter from the cursor (empty on first page).
            let (extra_filters, kset_or) = keyset_resume_filter(cursor.as_ref(), sort_col, dir);

            // Merge the keyset extras into the caller's base sets.
            let mut all_filters = base_filters;
            all_filters.extend(extra_filters);

            let mut all_or_groups = base_or_groups;
            all_or_groups.extend(kset_or);

            // Convert SDK Filter/SortDir to WIT types.
            let wit_filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter> =
                all_filters.iter().map(|f| __boogy_sdk_filter_to_wit(f)).collect();
            let wit_or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>> =
                all_or_groups.iter().map(|group| {
                    group.iter().map(|f| __boogy_sdk_filter_to_wit(f)).collect()
                }).collect();
            let wit_dir = __boogy_sdk_dir_to_wit(dir);

            // Sort: primary sort_col, then _id as deterministic tiebreak.
            // Skip the _id tiebreak when sort_col is already "_id" — a row
            // can't have two distinct _id values to tiebreak on, so the
            // second entry would be a no-op (same query plan, wasted bytes).
            let mut sort = ::std::vec![
                $bindings::boogy::platform::store::SortBy {
                    column: sort_col.to_string(),
                    dir: wit_dir,
                },
            ];
            if sort_col != "_id" {
                sort.push($bindings::boogy::platform::store::SortBy {
                    column: "_id".to_string(),
                    dir: wit_dir,
                });
            }

            // Overfetch by 1 to detect next-page existence.
            let wit_page = Some($bindings::boogy::platform::store::Page {
                limit: (limit + 1) as u32,
                offset: 0,
            });

            // Execute the query (converts rows to SDK Row via to_sdk_row inside).
            // Keyset pagination derives "has next page" from the limit+1 overfetch,
            // never from the total — so skip the count entirely.
            let (rows, _total) = find_rows_grouped(table, wit_filters, wit_or_groups, sort, wit_page, allow_full_scan, true)
                .map_err($crate::error::ApiError::from)?;

            // Map each row to (T, Cursor) before slicing.
            let mapped: ::std::vec::Vec<(T, $crate::pagination::Cursor)> =
                rows.iter().map(&row_to_item_and_cursor).collect();

            // Slice and emit next_cursor.
            let page = if mapped.len() > limit {
                let kept: ::std::vec::Vec<(T, $crate::pagination::Cursor)> =
                    mapped.into_iter().take(limit).collect();
                let last_cursor = kept.last()
                    .expect("limit > 0 and kept non-empty")
                    .1
                    .clone();
                let items: ::std::vec::Vec<T> = kept.into_iter().map(|(t, _)| t).collect();
                $crate::pagination::CursorPage {
                    items,
                    next_cursor: Some($crate::pagination::encode(&last_cursor)),
                }
            } else {
                let items: ::std::vec::Vec<T> = mapped.into_iter().map(|(t, _)| t).collect();
                $crate::pagination::CursorPage { items, next_cursor: None }
            };

            Ok(page)
        }

        // ---------------------------------------------------------------
        // Typed Query DSL (slice a). The QueryArgs data + builder methods
        // live in `boogy_sdk::query`; this Query newtype wraps them and
        // adds the four terminal methods that call the macro-emitted WIT
        // primitives (find_rows_grouped, count_rows, keyset_paginate).
        // ---------------------------------------------------------------

        /// Typed query-builder. Wraps [`boogy_sdk::query::QueryArgs`] and
        /// adds the terminal methods (`fetch_one`, `fetch_all`,
        /// `fetch_all_with_total`, `count`, `fetch_page`) that execute
        /// the query against the WIT store.
        ///
        /// ```ignore
        /// let page = Query::on("posts")
        ///     .where_eq("parent_post_id", 0)
        ///     .where_eq("deleted_at", "")
        ///     .keyset_by("created_at", SortDir::Desc).limit(20).cursor(c)
        ///     .fetch_page(|row| PostView::from_row(row))?;
        /// ```
        pub struct Query(pub $crate::query::QueryArgs);

        impl Query {
            // -- Construction --

            pub fn on(table: &str) -> Self {
                Self($crate::query::QueryArgs::on(table))
            }

            // -- Filter chaining (thin wrappers) --

            pub fn where_eq<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_eq(col, val))
            }
            pub fn where_neq<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_neq(col, val))
            }
            pub fn where_gt<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_gt(col, val))
            }
            pub fn where_gte<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_gte(col, val))
            }
            pub fn where_lt<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_lt(col, val))
            }
            pub fn where_lte<V: $crate::query::IntoVal>(self, col: &str, val: V) -> Self {
                Self(self.0.where_lte(col, val))
            }
            pub fn where_like<V: $crate::query::IntoVal>(self, col: &str, pattern: V) -> Self {
                Self(self.0.where_like(col, pattern))
            }
            pub fn where_not_like<V: $crate::query::IntoVal>(self, col: &str, pattern: V) -> Self {
                Self(self.0.where_not_like(col, pattern))
            }
            pub fn where_null(self, col: &str) -> Self { Self(self.0.where_null(col)) }
            pub fn where_not_null(self, col: &str) -> Self { Self(self.0.where_not_null(col)) }
            pub fn where_in<I, V>(self, col: &str, vals: I) -> Self
            where
                I: ::core::iter::IntoIterator<Item = V>,
                V: $crate::query::IntoVal,
            {
                Self(self.0.where_in(col, vals))
            }

            // -- OR-of-AND --

            pub fn or<F>(self, build: F) -> Self
            where
                F: ::core::ops::FnOnce(Self) -> Self,
            {
                // Wrap the user's closure to operate on QueryArgs internally,
                // then re-wrap.
                Self(self.0.or(|args| build(Self(args)).0))
            }

            // -- Sort --

            pub fn order_by(self, col: &str, dir: $crate::store::SortDir) -> Self {
                Self(self.0.order_by(col, dir))
            }
            pub fn order_by_asc(self, col: &str) -> Self { Self(self.0.order_by_asc(col)) }
            pub fn order_by_desc(self, col: &str) -> Self { Self(self.0.order_by_desc(col)) }

            // -- Pagination --

            pub fn limit(self, n: usize) -> Self { Self(self.0.limit(n)) }
            pub fn offset(self, n: u32) -> Self { Self(self.0.offset(n)) }
            pub fn cursor(self, c: ::core::option::Option<$crate::pagination::Cursor>) -> Self {
                Self(self.0.cursor(c))
            }
            pub fn keyset_by(self, col: &str, dir: $crate::store::SortDir) -> Self {
                Self(self.0.keyset_by(col, dir))
            }

            // -- Full-scan opt-in --

            pub fn allow_full_scan(self, reason: &str) -> Self {
                Self(self.0.allow_full_scan(reason))
            }

            /// Explicitly permit a table scan for this query, with an audited
            /// reason — the escape hatch when no access pattern is declared.
            /// Alias for `allow_full_scan` with a logged justification.
            pub fn allow_scan(self, reason: &str) -> Self {
                $crate::log::info!("allow_scan: {} (table {})", reason, self.0.table);
                Self(self.0.allow_full_scan(reason))
            }

            // -- Internal: convert QueryArgs to the WIT-typed args find_rows_grouped expects --

            fn to_wit_args(&self) -> (
                ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
                ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
                ::std::vec::Vec<$bindings::boogy::platform::store::SortBy>,
                ::core::option::Option<$bindings::boogy::platform::store::Page>,
            ) {
                let wit_filters: ::std::vec::Vec<_> = self.0.base_filters.iter()
                    .map(__boogy_sdk_filter_to_wit).collect();
                let wit_or_groups: ::std::vec::Vec<_> = self.0.or_groups.iter()
                    .map(|grp| grp.iter().map(__boogy_sdk_filter_to_wit).collect())
                    .collect();
                let wit_sort: ::std::vec::Vec<_> = self.0.sort.iter()
                    .map(|(col, dir)| $bindings::boogy::platform::store::SortBy {
                        column: col.clone(),
                        dir: __boogy_sdk_dir_to_wit(*dir),
                    })
                    .collect();
                let wit_page = self.0.limit.map(|lim| $bindings::boogy::platform::store::Page {
                    limit: lim as u32,
                    offset: self.0.offset,
                });
                (wit_filters, wit_or_groups, wit_sort, wit_page)
            }

            // -- Terminal methods --

            /// Fetch the first matching row. Returns `Ok(None)` if no rows match.
            /// Overrides any prior `.limit(n)` with `limit = 1` and resets
            /// `.offset(n)` to `0` — the method name promises "the first
            /// matching row", not the first row past N skipped. Same
            /// silent-first-of-many semantics as `find_row_by`.
            pub fn fetch_one(mut self) -> ::core::result::Result<
                ::core::option::Option<$crate::store::Row>,
                $crate::error::ApiError,
            > {
                self.0 = self.0.for_fetch_one();
                let afs = self.0.allow_full_scan;
                let (f, og, s, p) = self.to_wit_args();
                // Discards the total → skip the count.
                let (rows, _total) = find_rows_grouped(&self.0.table, f, og, s, p, afs, true)
                    .map_err($crate::error::ApiError::from)?;
                Ok(rows.into_iter().next())
            }

            /// Fetch all matching rows (subject to `limit` if set).
            pub fn fetch_all(self) -> ::core::result::Result<
                ::std::vec::Vec<$crate::store::Row>,
                $crate::error::ApiError,
            > {
                // Discards the total → skip the count (don't route through
                // `fetch_all_with_total`, which must compute it).
                let afs = self.0.allow_full_scan;
                let (f, og, s, p) = self.to_wit_args();
                let (rows, _total) = find_rows_grouped(&self.0.table, f, og, s, p, afs, true)
                    .map_err($crate::error::ApiError::from)?;
                Ok(rows)
            }

            /// Fetch all matching rows + the total count (ignoring page).
            pub fn fetch_all_with_total(self) -> ::core::result::Result<
                (::std::vec::Vec<$crate::store::Row>, u64),
                $crate::error::ApiError,
            > {
                let afs = self.0.allow_full_scan;
                let (f, og, s, p) = self.to_wit_args();
                find_rows_grouped(&self.0.table, f, og, s, p, afs, false)
                    .map_err($crate::error::ApiError::from)
            }

            /// Count matching rows. Does NOT materialize rows.
            ///
            /// **NOTE:** ignores `.or(...)`, `.order_by(...)`, `.limit/.offset/.cursor` —
            /// only the base AND-filters are sent to the host. The WIT `count`
            /// op is filters-only. If your count needs OR semantics, fetch
            /// the rows and count them, or restructure with an explicit
            /// `where_in`/composite predicate.
            pub fn count(self) -> ::core::result::Result<u64, $crate::error::ApiError> {
                // count_filters() encodes the "WIT count is filters-only"
                // contract — unit-tested in boogy_sdk::query::tests.
                let wit_filters: ::std::vec::Vec<_> = self.0.count_filters().iter()
                    .map(__boogy_sdk_filter_to_wit).collect();
                count_rows(&self.0.table, wit_filters)
                    .map_err($crate::error::ApiError::from)
            }

            /// Keyset-paginated fetch. Auto-extracts the cursor value from
            /// the row using the keyset column set by `.keyset_by(col, dir)`.
            /// User closure just maps row → item.
            ///
            /// Defaults to `limit = 20` if `.limit()` was not chained.
            ///
            /// Returns `ApiError::internal("fetch_page requires .keyset_by()")`
            /// if called without an earlier `.keyset_by(...)` call.
            pub fn fetch_page<T, F>(self, row_to_item: F) -> ::core::result::Result<
                $crate::pagination::CursorPage<T>,
                $crate::error::ApiError,
            >
            where
                T: ::serde::Serialize,
                F: ::core::ops::Fn(&$crate::store::Row) -> T,
            {
                let (keyset_col, dir) = match &self.0.keyset_mode {
                    Some((c, d)) => (c.clone(), *d),
                    None => return Err($crate::error::ApiError::internal(
                        "fetch_page requires .keyset_by()".to_string(),
                    )),
                };
                let limit = self.0.limit.unwrap_or(20);
                let allow_full_scan = self.0.allow_full_scan;

                keyset_paginate::<T, _>(
                    &self.0.table,
                    self.0.base_filters,
                    self.0.or_groups,
                    &keyset_col,
                    dir,
                    self.0.cursor,
                    limit,
                    allow_full_scan,
                    |row| {
                        let item = row_to_item(row);
                        let cursor = $crate::query::build_keyset_cursor(row, &keyset_col);
                        (item, cursor)
                    },
                )
            }
        }

        /// Internal: insert a row from SDK-typed `(name, Val)` pairs.
        /// Used by the api_keys glue, whose `prepare_create` returns
        /// values in the SDK's portable `Val` form. User code should
        /// call `store::insert(table, &[store::Column { name, val:
        /// store::Value::* }])` directly.
        fn __boogy_insert_row(
            table: &str,
            cols: &[(::std::string::String, $crate::store::Val)],
        ) -> ::core::result::Result<u64, $crate::rpc::RpcError> {
            let wit = __boogy_to_wit_columns(cols);
            $bindings::boogy::platform::store::insert(table, &wit)
                .map_err($crate::rpc::RpcError::internal)
        }

        // -- MigrationCtx: idempotent schema + data ops for migration `up` fns --
        //
        // Schema ops check the current live state (via `list_columns` /
        // `list_tables`) and skip when the target state is already
        // satisfied, so a migration that applied k-of-n ops before
        // crashing can be re-run to completion without error.
        //
        // The underlying WIT/engine ops (add_column, rename_column, …)
        // stay STRICT — idempotency lives HERE, in the ctx layer, scoped
        // to the run-once / re-run-after-failure semantics migrations need.
        //
        // Data ops for backfills delegate to the store free fns (find,
        // insert, count, update_where, delete_where). Backfill ops are not
        // made idempotent by the framework — authors should write naturally
        // idempotent backfills (e.g. update_where that sets a value to a
        // known constant). The runner already wraps each migration in one
        // store tx, so backfills are atomic with the schema changes; no
        // additional tx wrapping is needed or meaningful.

        /// Context passed to each migration's `up` closure. Provides
        /// **idempotent** schema ops (guarded by `list_columns` /
        /// `list_tables` introspection) and store data ops for backfills.
        ///
        /// # Re-run safety
        ///
        /// Schema operations (`add_column`, `rename_column`, `drop_column`,
        /// `create_table`, `create_index`, `drop_index`) are idempotent:
        /// calling them when the target state already holds is a no-op.
        /// This means a migration that crashed partway through can be
        /// re-run and will pick up where it left off.
        ///
        /// Data backfills run inside the migration's transaction (the runner wraps
        /// each migration in one store tx), so they are already atomic with the schema
        /// changes and the version-row write. Authors should still prefer naturally
        /// idempotent backfills (e.g. `update_where` to a fixed default) so a
        /// migration is safe to re-run after a transient commit conflict.
        pub struct MigrationCtx;

        impl MigrationCtx {
            /// Add a column to `table` with the given spec. **Idempotent:**
            /// if `list_columns` already shows a column with `spec.name`,
            /// this is a no-op.
            pub fn add_column(
                &self,
                table: &str,
                spec: &$crate::store::ColumnSpec,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if list_columns(table)?.iter().any(|c| c.name == spec.name) {
                    return Ok(()); // already applied
                }
                add_column(table, spec)
            }

            /// Rename a column in `table` from `old` to `new`. **Idempotent:**
            /// - If `new` is present and `old` is absent → already renamed, no-op.
            /// - If `old` is absent (and `new` is also absent) → error: nothing to rename.
            /// - Otherwise calls the underlying rename op.
            pub fn rename_column(
                &self,
                table: &str,
                old: &str,
                new: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                let cols = list_columns(table)?;
                let has_new = cols.iter().any(|c| c.name == new);
                let has_old = cols.iter().any(|c| c.name == old);
                if has_new && !has_old {
                    return Ok(()); // already renamed
                }
                if !has_old {
                    return Err(::std::format!(
                        "rename_column: no column `{}` in `{}`",
                        old, table
                    ));
                }
                rename_column(table, old, new)
            }

            /// Drop a column from `table`. **Idempotent:** if `list_columns`
            /// does not contain `name`, this is a no-op (already dropped).
            pub fn drop_column(
                &self,
                table: &str,
                name: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if !list_columns(table)?.iter().any(|c| c.name == name) {
                    return Ok(()); // already dropped
                }
                drop_column(table, name)
            }

            /// Create a table from a [`Table`] spec. **Idempotent:** if
            /// `list_tables` already contains the table name, this is a no-op.
            /// Indexes declared on the table are created via `create_index`,
            /// each guarded by `list_indexes` introspection — no duplicate-index
            /// errors, genuine engine errors propagate.
            pub fn create_table(
                &self,
                table: &$crate::store::Table,
            ) -> ::core::result::Result<(), ::std::string::String> {
                let existing = $bindings::boogy::platform::store::list_tables()?;
                if existing.iter().any(|t| t.name == table.name) {
                    return Ok(()); // already exists
                }
                // create_table_from uses list_tables/list_indexes introspection
                // guards internally; genuine engine errors propagate via expect.
                create_table_from(table);
                Ok(())
            }

            /// Create an index on `table`. **Idempotent:** if `list_indexes(table)`
            /// already shows an index named `index.name`, this is a no-op. Errors
            /// from the underlying engine propagate (no silent swallow).
            pub fn create_index(
                &self,
                table: &str,
                index: &$bindings::boogy::platform::store::IndexDef,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if list_indexes(table)?.iter().any(|i| i.name == index.name) {
                    return Ok(());
                }
                $bindings::boogy::platform::store::create_index(table, index)
                    .map_err(::std::string::String::from)
            }

            /// Drop an index from `table`. **Idempotent:** if `list_indexes(table)`
            /// shows no index named `name`, this is a no-op. Errors propagate.
            pub fn drop_index(
                &self,
                table: &str,
                name: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if !list_indexes(table)?.iter().any(|i| i.name == name) {
                    return Ok(());
                }
                $bindings::boogy::platform::store::drop_index(table, name)
                    .map_err(::std::string::String::from)
            }

            // -- Data ops for backfills --

            /// Find rows matching `filters` in `table`. Returns
            /// `(rows, total_count)`. Delegates to the store `find` WIT fn.
            pub fn find_rows(
                &self,
                table: &str,
                filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
                sort: ::std::vec::Vec<$bindings::boogy::platform::store::SortBy>,
                page: ::core::option::Option<$bindings::boogy::platform::store::Page>,
            ) -> ::core::result::Result<
                (::std::vec::Vec<$crate::store::Row>, u64),
                ::std::string::String,
            > {
                let result = $bindings::boogy::platform::store::find(
                    table,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters,
                        sort,
                        page,
                        or_groups: vec![],
                        allow_full_scan: false,
                        skip_total: false,
                    },
                )?;
                let rows = result.rows.iter().map(|r| to_sdk_row(r)).collect();
                Ok((rows, result.total_count))
            }

            /// Count rows matching `filters` in `table`. Delegates to the
            /// store `count` WIT fn.
            pub fn count(
                &self,
                table: &str,
                filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            ) -> ::core::result::Result<u64, ::std::string::String> {
                $bindings::boogy::platform::store::count(table, &filters)
                    .map_err(::std::string::String::from)
            }

            /// Insert a row into `table` from WIT-typed columns. Returns
            /// the new row's `_id`. Delegates to the store `insert` WIT fn.
            pub fn insert(
                &self,
                table: &str,
                cols: &[$bindings::boogy::platform::store::Column],
            ) -> ::core::result::Result<u64, ::std::string::String> {
                $bindings::boogy::platform::store::insert(table, cols)
                    .map_err(::std::string::String::from)
            }

            /// Update all rows in `table` matching `filters`, setting the
            /// given `fields`. Returns the number of updated rows. Delegates
            /// to the store `update-where` WIT fn.
            pub fn update_where(
                &self,
                table: &str,
                filters: &[$bindings::boogy::platform::store::Filter],
                fields: &[$bindings::boogy::platform::store::Column],
            ) -> ::core::result::Result<u64, ::std::string::String> {
                $bindings::boogy::platform::store::update_where(table, filters, fields)
                    .map_err(::std::string::String::from)
            }

            /// Delete all rows in `table` matching `filters`. Returns the
            /// number of deleted rows. Delegates to the store `delete-where`
            /// WIT fn.
            pub fn delete_where(
                &self,
                table: &str,
                filters: &[$bindings::boogy::platform::store::Filter],
            ) -> ::core::result::Result<u64, ::std::string::String> {
                $bindings::boogy::platform::store::delete_where(table, filters)
                    .map_err(::std::string::String::from)
            }

            /// Run a closure as a grouped step within the migration's transaction.
            ///
            /// The entire migration already runs as ONE atomic store transaction (the
            /// `migrations()` runner opens it), so this is purely a structural grouping
            /// helper: the closure's writes join the migration tx and commit/roll back
            /// with it.
            ///
            /// Do NOT call `begin`/`commit`/`rollback` inside a migration: the host has
            /// no nested transactions, so an inner `commit_transaction` commits the
            /// partial migration state as a finished store tx, after which further writes
            /// start a NEW tx the runner never commits — breaking migration atomicity.
            ///
            /// If the closure returns `Err`, the error propagates and the runner rolls
            /// the migration back.
            pub fn tx<F, R>(
                &self,
                f: F,
            ) -> ::core::result::Result<R, ::std::string::String>
            where
                F: ::core::ops::FnOnce() -> ::core::result::Result<R, ::std::string::String>,
            {
                f()
            }
        }

        /// One versioned schema migration. Run once per (api, version);
        /// the SDK records applied versions in
        /// `__boogy_schema_version` so re-running on subsequent
        /// requests is a no-op. The `up` function receives a
        /// [`MigrationCtx`] whose schema ops are idempotent — a
        /// migration that crashes partway through can be re-run to
        /// completion without error.
        ///
        /// Versions must be strictly increasing; gaps are allowed but
        /// migrations are applied in numeric order. Names are
        /// informational (recorded for audit / debugging).
        pub struct Migration {
            pub version: i64,
            pub name: &'static str,
            pub up: fn(&MigrationCtx) -> ::core::result::Result<(), ::std::string::String>,
        }

        /// Build a Migration with conventional argument order.
        pub fn migration(
            version: i64,
            name: &'static str,
            up: fn(&MigrationCtx) -> ::core::result::Result<(), ::std::string::String>,
        ) -> Migration {
            Migration { version, name, up }
        }

        /// Apply pending schema migrations.
        ///
        /// Maintains a per-service `__boogy_schema_version` table that
        /// records which migrations have run. For each pending migration
        /// (version > max applied), the entire migration runs as one store
        /// transaction: schema DDL + backfill + version-row insert commit or
        /// roll back together. If the store signals the operation is
        /// unavailable, `begin_transaction` returns `unsupported` (→ `Err`);
        /// bounded by the store's ~5 s / 10 MB transaction envelope.
        ///
        /// # Re-run safety
        ///
        /// Schema ops inside `MigrationCtx` are idempotent via `list_columns`
        /// / `list_tables` / `list_indexes` introspection — a migration whose
        /// `up` fn crashed partway can be re-run safely; ops already applied
        /// are no-ops and the remainder proceeds. The version row is committed
        /// atomically with the rest of the migration, so a partial run never
        /// leaves a committed version row without the accompanying schema
        /// changes (defense-in-depth on top of the idempotency guards).
        ///
        /// Data backfills authored in the `up` fn should be idempotent
        /// (e.g. `ctx.update_where(...)` setting a column to a fixed default
        /// is naturally idempotent) — they already run inside the migration's
        /// store transaction and are atomic with the schema changes.
        ///
        /// # Concurrency note
        ///
        /// If two instances run `migrations()` at the same time, one
        /// migration's `commit_transaction` may fail with a conflict. That tx
        /// rolls back (nothing applied), so the error surfaces to the caller
        /// but the migration is NOT half-applied; the next request re-reads
        /// `max_applied` (now advanced by the instance that won) and skips it.
        /// Re-running the request after a startup conflict is safe.
        ///
        /// Call from `init_tables`, AFTER the `create_table_from` calls for
        /// any tables the migrations target. The function is idempotent across
        /// requests: on the no-op path it costs one read on a small table.
        ///
        /// ```ignore
        /// fn init_tables() {
        ///     create_table_from(&Table::new("notes").text("title").text(DEFAULT_OWNER_COL));
        ///     migrations(&[
        ///         migration(1, "add_priority", |m| {
        ///             m.add_column("notes", &col("priority", ColType::Integer).default(Val::Integer(0)))?;
        ///             Ok(())
        ///         }),
        ///         migration(2, "index_priority", |m| {
        ///             m.create_index("notes", &store::IndexDef {
        ///                 name: "idx_notes_priority".into(),
        ///                 columns: vec!["priority".into()],
        ///                 unique: false,
        ///             })?;
        ///             Ok(())
        ///         }),
        ///     ]).expect("migrations failed");
        /// }
        /// ```
        pub fn migrations(list: &[Migration]) -> ::core::result::Result<(), ::std::string::String> {
            // Ensure the version table exists. Idempotent.
            create_table_from(
                &$crate::store::Table::new("__boogy_schema_version")
                    .integer("version")
                    .text("name"),
            );

            // Find the highest applied version via structured find:
            // sort by version DESC, limit 1.
            let find_result = $bindings::boogy::platform::store::find(
                "__boogy_schema_version",
                &$bindings::boogy::platform::store::FindOptions {
                    filters: vec![],
                    sort: vec![$bindings::boogy::platform::store::SortBy {
                        column: "version".to_string(),
                        dir: $bindings::boogy::platform::store::SortDir::Desc,
                    }],
                    page: Some($bindings::boogy::platform::store::Page { limit: 1, offset: 0 }),
                    or_groups: vec![],
                    allow_full_scan: false,
                    skip_total: false,
                },
            )?;
            let max_applied: i64 = find_result
                .rows
                .first()
                .and_then(|r| r.columns.iter().find(|c| c.name == "version"))
                .map(|c| match &c.val {
                    $bindings::boogy::platform::store::Value::Integer(i) => *i,
                    _ => 0,
                })
                .unwrap_or(0);

            // Sort by version ascending — author may declare in any order.
            let mut sorted: ::std::vec::Vec<&Migration> = list.iter().collect();
            sorted.sort_by_key(|m| m.version);

            for m in sorted {
                if m.version <= max_applied {
                    continue;
                }
                // Each migration is ONE atomic store transaction: schema DDL +
                // backfill + the version row commit or roll back together.
                // If the store can't open a transaction, begin_transaction
                // surfaces `unsupported`. Bounded by the store's ~5 s /
                // 10 MB transaction envelope.
                $bindings::boogy::platform::store::begin_transaction()
                    .map_err(::std::string::String::from)?;

                let run = || -> ::core::result::Result<(), ::std::string::String> {
                    let ctx = MigrationCtx;
                    (m.up)(&ctx)?;
                    $bindings::boogy::platform::store::insert(
                        "__boogy_schema_version",
                        &[
                            $bindings::boogy::platform::store::Column {
                                name: "version".to_string(),
                                val: $bindings::boogy::platform::store::Value::Integer(m.version),
                            },
                            $bindings::boogy::platform::store::Column {
                                name: "name".to_string(),
                                val: $bindings::boogy::platform::store::Value::Text(m.name.to_string()),
                            },
                        ],
                    ).map_err(::std::string::String::from)?;
                    Ok(())
                };

                match run() {
                    Ok(()) => {
                        $bindings::boogy::platform::store::commit_transaction()
                            .map_err(::std::string::String::from)?;
                    }
                    Err(e) => {
                        let _ = $bindings::boogy::platform::store::rollback_transaction();
                        return Err(e);
                    }
                }
            }
            Ok(())
        }

        /// Run a closure inside a database transaction. All `store::*` calls made while
        /// the closure runs — locally AND across any `peer::fetch` — join one atomic
        /// store transaction. On `Ok` the transaction commits; on `Err` it rolls back.
        /// If the closure panics, the unwinding request is torn down by the host, which
        /// discards the open transaction (it is never committed). `outbound_http` is
        /// denied inside the closure (peer/outbound calls return a capability-denied
        /// error). For `background_jobs`: `enqueue` is allowed inside a transaction —
        /// the job is submitted only if the transaction commits — but `cancel` and
        /// `status` are unavailable inside a transaction (they return
        /// backend-unavailable). If the store can't open a transaction it returns the
        /// typed `unsupported` store error (→ HTTP 501 once it lifts into `ApiError`).
        /// Must run as the transaction owner: calling `tx` from a
        /// handler already enrolled as a peer participant of a caller's transaction
        /// fails at commit (only the originating request commits).
        ///
        /// The closure may return any error type `E` that implements
        /// `From<store::StoreError>`, so handlers raise **structured** errors (e.g.
        /// `ApiError::conflict(...)`, `ApiError::unprocessable(...)`) from inside the
        /// transaction without flattening every failure to `internal` at the boundary.
        /// `ApiError` implements `From<store::StoreError>` (and
        /// `From<$crate::store::StoreError>`), so bare `?` on `store::insert(...)` /
        /// `find_row_by(...)` inside the closure lifts the typed store error into
        /// `ApiError`, preserving the variant. `String` also implements
        /// `From<store::StoreError>`, so `tx::<_, _, String>` compiles too (message
        /// survives; variant flattens).
        ///
        /// `begin`/`commit` errors are mapped through `E::from(store_error)` as well, so
        /// a commit `Conflict` (store serialization abort) or an `Unsupported`
        /// reaches the client as 409 / 501 when `E = ApiError`. A handler returning
        /// `Result<_, ApiError>` writes `tx(|| ...)?` and the existing
        /// `From<store::StoreError> for ApiError` maps the variant to the correct status
        /// (Conflict → 409, Unsupported → 501, …). When `E` can't be inferred, name it
        /// with a turbofish: `tx::<_, _, ApiError>(|| ...)`.
        ///
        /// ```ignore
        /// // Store-only closure — error type inferred from the surrounding `?`.
        /// tx(|| {
        ///     let user_id = store::insert("users", &user_cols)?;
        ///     store::insert("profiles", &profile_cols)?;
        ///     Ok(user_id)
        /// })?;
        ///
        /// // Structured errors + mixed store/find_row_by — name the error type.
        /// let (created, balance): (Created<View>, f64) =
        ///     tx::<_, _, ApiError>(|| {
        ///         let bal_row = find_row_by("balances",
        ///             "principal", store::Value::Text(me.clone()))?;
        ///         let bal = bal_row
        ///             .map(|r| r.text("balance").parse::<f64>().unwrap_or(0.0))
        ///             .unwrap_or(default);
        ///         if bal < amount {
        ///             // Raises a structured 409 from inside the tx.
        ///             return Err(ApiError::conflict("insufficient balance"));
        ///         }
        ///         // ... writes ...
        ///         Ok((Created(view), bal - amount))
        ///     })?;
        /// ```
        fn tx<F, R, E>(f: F) -> ::core::result::Result<R, E>
        where
            F: ::core::ops::FnOnce() -> ::core::result::Result<R, E>,
            E: ::core::convert::From<store::StoreError>,
        {
            if let Err(e) = $bindings::boogy::platform::store::begin_transaction() {
                return Err(E::from(e));
            }
            match f() {
                Ok(r) => match $bindings::boogy::platform::store::commit_transaction() {
                    Ok(()) => Ok(r),
                    Err(e) => Err(E::from(e)),
                },
                Err(e) => {
                    let _ = $bindings::boogy::platform::store::rollback_transaction();
                    Err(e)
                }
            }
        }

        /// Internal: update a single row by id from SDK-typed
        /// `(name, Val)` pairs. See `__boogy_insert_row` for
        /// rationale; user code uses `store::update` directly.
        fn __boogy_update_row(
            table: &str,
            id: u64,
            cols: &[(::std::string::String, $crate::store::Val)],
        ) -> ::core::result::Result<bool, $crate::rpc::RpcError> {
            let wit = __boogy_to_wit_columns(cols);
            $bindings::boogy::platform::store::update(table, id, &wit)
                .map_err($crate::rpc::RpcError::internal)
        }

        // -- Resource-level auth helpers --
        //
        // Everything auth-related lives under `auth::*`. This is the
        // canonical surface for handler / guard authoring; it shadows
        // direct WIT auth access (`bindings::boogy::platform::auth`)
        // so authoring code reads the same way regardless of whether
        // the API was hand-written or codegen-emitted.
        //
        // Convention: an "owned" resource is a row whose ownership is
        // recorded in a single column (`DEFAULT_OWNER_COL` =
        // `"owner_principal"` by convention, configurable per call).
        //
        // `auth::owns_resource(...)` returns a guard for single-resource
        // routes (`GET/PATCH/DELETE /things/{id}`); on success it
        // stashes the loaded row in `req.ctx` so the handler doesn't
        // re-fetch. `auth::find_owned(...)` returns the
        // principal-scoped row list for index endpoints.
        //
        // Both deny-by-existence-mask: missing row and other-owner
        // map to the same 404, preventing enumeration via guess + 403.

        pub mod auth {
            /// Configuration for the [`owns_resource`] guard. Built via
            /// the free function and registered with `Router::guard(...)`
            /// directly — the SDK's `IntoGuard` impl is below.
            pub struct OwnsResource {
                pub table: &'static str,
                pub owner_col: &'static str,
                pub id_param: &'static str,
                pub slot: &'static str,
            }

            impl OwnsResource {
                /// Stash the loaded row at a named slot in `req.ctx`. Use
                /// when more than one row of the same shape is loaded by
                /// distinct guards on the same route.
                pub fn slot(mut self, slot: &'static str) -> Self {
                    self.slot = slot;
                    self
                }
            }

            /// Build an "owns this resource" guard configuration.
            ///
            /// - `table` — the table the resource lives in.
            /// - `owner_col` — the column carrying the owner's principal
            ///   string (typically [`super::DEFAULT_OWNER_COL`]).
            /// - `id_param` — the path-param name carrying the resource id
            ///   (typically `"id"`).
            ///
            /// Default slot is the empty string. Use `.slot("name")` to
            /// disambiguate when more than one resource is loaded.
            pub fn owns_resource(
                table: &'static str,
                owner_col: &'static str,
                id_param: &'static str,
            ) -> OwnsResource {
                OwnsResource { table, owner_col, id_param, slot: "" }
            }

            impl $crate::router::IntoGuard for OwnsResource {
                fn into_guard(self) -> $crate::router::Guard {
                    ::std::rc::Rc::new(move |req: &mut $crate::router::Req<'_>| {
                        // Fetch the id from path params. A missing id is
                        // a routing bug — the route pattern should
                        // guarantee the param exists. Treat as 404.
                        let id: u64 = match req.params.get(self.id_param) {
                            Some(s) if !s.is_empty() => s.parse().map_err(|_| $crate::response::not_found())?,
                            _ => return Err($crate::response::not_found()),
                        };
                        // A blank principal (empty/whitespace) is treated as
                        // anonymous: it must never be admitted, and in
                        // particular must never match a row whose owner column
                        // is also empty/unset. `_request_principal_nonblank`
                        // returns None for a blank identity → 401.
                        let principal = match $crate::request_state::_request_principal_nonblank() {
                            Some(p) => p,
                            None => return Err(unauthenticated_response()),
                        };
                        // Load the row. Missing or other-owner → 404
                        // (the existence-mask: don't let a guesser
                        // distinguish "doesn't exist" from "exists but
                        // isn't yours").
                        let row = match super::get_row(self.table, id) {
                            Ok(Some(r)) => r,
                            Ok(None) => return Err($crate::response::not_found()),
                            Err(e) => return Err(
                                $crate::error::ApiError::from(e).into(),
                            ),
                        };
                        // Defensive equality: blank principal never owns
                        // anything (already filtered above, but keep the
                        // ownership test fail-closed via the shared helper).
                        if !$crate::request_state::_principal_owns(&principal, &row.text(self.owner_col)) {
                            return Err($crate::response::not_found());
                        }
                        if self.slot.is_empty() {
                            req.ctx.insert(row);
                        } else {
                            req.ctx.insert_at(self.slot, row);
                        }
                        Ok(())
                    })
                }
            }

            /// 401-responding guard. Use on routes that require
            /// authentication but don't load a specific resource (so
            /// `owns_resource` doesn't apply) — e.g. a "list my X"
            /// endpoint that uses `find_owned` already, or a per-user
            /// dashboard summary.
            pub fn required() -> $crate::router::Guard {
                ::std::rc::Rc::new(|_req: &mut $crate::router::Req<'_>| {
                    if current_principal().is_some() {
                        Ok(())
                    } else {
                        Err(unauthenticated_response())
                    }
                })
            }

            /// Resolve the caller's principal string. `None` for
            /// anonymous requests.
            ///
            /// PASETO is the primary path: the WIT
            /// `auth::current_identity()` carries the principal the
            /// host attached to the request. When WIT auth is `None`,
            /// the SDK falls back to a per-request slot set by
            /// `api_key_routes::guard` — that slot carries the
            /// principal an `sk_*` bearer resolved to. The result is
            /// uniform: handlers and resource-level guards
            /// (`auth::owns_resource`, `auth::find_owned`) work the
            /// same regardless of credential type. The slot is
            /// cleared at request exit by the `wit_glue!` RAII guard.
            pub fn current_principal() -> ::core::option::Option<::std::string::String> {
                // Both the WIT principal (PASETO/session) and the API-key
                // fallback are now stashed in `request_state` at request
                // entry so that `Principal::from_request` (in the SDK
                // proper) can read them without access to `$bindings`.
                // `_request_principal()` unifies both with WIT precedence.
                $crate::request_state::_request_principal()
            }

            /// Scopes attached to the caller's session. Empty vec
            /// for anonymous requests AND for authenticated requests
            /// with no scopes — handlers should treat both as "no
            /// special grants" rather than try to distinguish them.
            pub fn current_scopes() -> ::std::vec::Vec<::std::string::String> {
                if let Some(i) = super::$bindings::boogy::platform::auth::current_identity() {
                    return i.scopes;
                }
                // sk_* fallback: scopes stashed by api_key_routes::guard, so
                // scope checks unify across PASETO and API-key callers.
                $crate::request_state::_fallback_scopes().unwrap_or_default()
            }

            /// True iff the caller has the named scope. Returns
            /// `false` for anonymous callers and authenticated
            /// callers whose scopes don't include `scope`. Match is
            /// exact (case-sensitive) — scope strings are
            /// platform-defined, not user input.
            ///
            /// Prefer this over `current_scopes().iter().any(...)`:
            /// the bindings call is cached behind the same
            /// `current_identity` host call regardless.
            pub fn has_scope(scope: &str) -> bool {
                // PASETO/session identity wins (host-side check). When WIT auth
                // is anonymous, fall back to the sk_* scopes stashed by
                // api_key_routes::guard so require_scope() admits API keys that
                // hold the scope (previously every sk_* caller was denied).
                if super::$bindings::boogy::platform::auth::current_identity().is_some() {
                    return super::$bindings::boogy::platform::auth::has_scope(&scope.to_string());
                }
                $crate::request_state::_fallback_scopes()
                    .map(|s| s.iter().any(|x| x == scope))
                    .unwrap_or(false)
            }

            /// Guard that admits requests with `scope`, returns 401
            /// when no identity is in scope, and 403 when an
            /// identity is in scope but lacks the named scope.
            ///
            /// The 401 vs 403 split matters: 401 tells a client
            /// "log in," 403 tells the same client "you're logged in
            /// but you can't do this." A flat 403 for both confuses
            /// retry logic in HTTP clients.
            pub fn require_scope(scope: &'static str) -> $crate::router::Guard {
                ::std::rc::Rc::new(move |_req: &mut $crate::router::Req<'_>| {
                    if current_principal().is_none() {
                        return Err(unauthenticated_response());
                    }
                    if has_scope(scope) {
                        Ok(())
                    } else {
                        Err(forbidden_response(scope))
                    }
                })
            }

            /// Load a row by id and confirm the caller owns it. Returns
            /// `Ok(Some(row))` when the row exists AND the `owner_col`
            /// matches `current_principal()`. Returns `Ok(None)` for
            /// both "row missing" AND "row exists but not yours" — the
            /// existence-mask that prevents enumeration via 403.
            ///
            /// `Err(RpcError)` on infrastructure failures (store error,
            /// anonymous request → 401-coded RpcError).
            ///
            /// Use this in MCP tool handlers and JSON-RPC methods where
            /// the resource id arrives in a JSON body rather than a
            /// path param (so [`owns_resource`] doesn't apply).
            pub fn load_owned(
                table: &str,
                owner_col: &str,
                id: u64,
            ) -> ::core::result::Result<
                ::core::option::Option<$crate::store::Row>,
                $crate::error::ApiError,
            > {
                // Blank principal → unauthenticated (never matches a row with a
                // blank owner). See `_principal_owns` / `_request_principal_nonblank`.
                let principal = $crate::request_state::_request_principal_nonblank()
                    .ok_or_else($crate::error::ApiError::unauthenticated)?;
                match super::get_row(table, id)? {
                    Some(row) => {
                        if $crate::request_state::_principal_owns(&principal, &row.text(owner_col)) {
                            Ok(Some(row))
                        } else {
                            Ok(None)
                        }
                    }
                    None => Ok(None),
                }
            }

            /// List rows owned by the current principal.
            ///
            /// Composes `current_principal()` with a `WHERE owner_col =
            /// principal` filter. Returns `ApiError::unauthenticated()`
            /// when the request is anonymous; store failures route through
            /// `StoreError → ApiError` so unique-violation / FK
            /// preservation works for any caller using `?` into a
            /// Result-typed handler.
            pub fn find_owned(
                table: &str,
                owner_col: &str,
            ) -> ::core::result::Result<::std::vec::Vec<$crate::store::Row>, $crate::error::ApiError>
            {
                // Blank principal → unauthenticated. Otherwise a blank
                // `current_principal()` would issue `WHERE owner_col = ''` and
                // return every un-owned row as if the anonymous caller owned it.
                let principal = $crate::request_state::_request_principal_nonblank()
                    .ok_or_else($crate::error::ApiError::unauthenticated)?;
                let mut rows: ::std::vec::Vec<$crate::store::Row> = ::std::vec::Vec::new();
                let mut offset: u32 = 0;
                loop {
                    let res = super::$bindings::boogy::platform::store::find(
                        table,
                        &super::$bindings::boogy::platform::store::FindOptions {
                            filters: vec![super::$bindings::boogy::platform::store::Filter {
                                column: owner_col.to_string(),
                                op: super::$bindings::boogy::platform::store::FilterOp::Eq,
                                val: super::$bindings::boogy::platform::store::Value::Text(principal.clone()),
                                in_values: None,
                            }],
                            sort: vec![],
                            page: Some(super::$bindings::boogy::platform::store::Page {
                                limit: super::SDK_FIND_BATCH,
                                offset,
                            }),
                            or_groups: vec![],
                            allow_full_scan: false,
                            skip_total: false,
                        },
                    )
                    .map_err($crate::store::StoreError::from_wit)?;
                    let n = res.rows.len() as u32;
                    rows.extend(res.rows.iter().map(super::to_sdk_row));
                    if n == 0 || rows.len() as u64 >= res.total_count {
                        break;
                    }
                    offset += n;
                }
                Ok(rows)
            }

            // Both helpers route through ApiError so every auth-rejection
            // response uses the same RFC 7807 wire shape as the rest of
            // the SDK. The forbidden case adds the `required_scope` as a
            // detail — clients that key off the `detail` string get the
            // same information they had pre-A2 in the legacy
            // `{"error":"forbidden","required_scope":"..."}` form, just
            // wrapped in the standard problem+json envelope.

            fn unauthenticated_response() -> $crate::response::HttpResponse {
                $crate::error::ApiError::unauthenticated().into()
            }

            fn forbidden_response(scope: &str) -> $crate::response::HttpResponse {
                $crate::error::ApiError::forbidden(
                    ::std::format!("required scope: {scope}"),
                )
                .into()
            }
        }

        // -- Idempotency-key middleware --
        //
        // `idempotent(handler)` wraps any handler so retries with
        // the same `Idempotency-Key` header replay the cached
        // response instead of re-running the handler. The cache
        // lives in `__boogy_idempotency` (table created via
        // `idempotency_init_table` from the user's `init_tables`).
        //
        // Failure modes:
        //   * No header on the request → pass-through (no caching).
        //   * Cache hit, body fingerprint matches → 200 (or whatever
        //     was cached) with the original response replayed.
        //   * Cache hit, body fingerprint MISMATCH → 409 Conflict
        //     ("Idempotency key reused with a different request").
        //     Catches the common bug where a client retries with a
        //     different payload under the same key.
        //   * Cache miss → run handler, cache successful (2xx)
        //     responses, return. 4xx/5xx skip caching so the client
        //     can retry against a transient failure.
        //
        // Concurrency: best-effort. Two parallel retries with the
        // same key race to the cache; one wins, the other re-runs
        // the handler. Closes the duplicate-write window for
        // sequential retries (the common case).

        /// Create the idempotency cache table. Idempotent — the
        /// underlying `create_table` is. Call from `init_tables()`
        /// before registering routes that use [`idempotent`].
        pub fn idempotency_init_table() {
            create_table_from(
                &$crate::store::Table::new($crate::idempotency::TABLE)
                    .text("scope_key")
                    .text("body_fingerprint")
                    .integer("status")
                    .text("headers_json")
                    .text("body_b64")
                    .integer("created_at")
                    .unique_index(&::std::format!("idx_{}_scope", $crate::idempotency::TABLE), &["scope_key"]),
            );
        }

        /// Wrap a handler with idempotency-key replay. See module
        /// docs in [`boogy_sdk::idempotency`] for the contract
        /// and caveats.
        #[allow(dead_code)]
        pub fn idempotent<H, Args>(handler: H) -> impl ::core::ops::Fn(&mut $crate::router::Req<'_>) -> $crate::response::HttpResponse + 'static
        where
            H: $crate::router::IntoHandler<Args>,
        {
            let inner = handler.into_handler();
            move |req: &mut $crate::router::Req<'_>| {
                let Some(key) = req.header($crate::idempotency::HEADER) else {
                    // No idempotency key → pass-through unchanged.
                    return inner(req);
                };
                let key = key.to_string();
                let principal = $crate::request_state::_request_principal().unwrap_or_default();
                let scope = $crate::idempotency::scope_key(
                    &key,
                    &req.request.method,
                    &req.request.path,
                    &principal,
                );
                let fp = $crate::idempotency::body_fingerprint(req.body());

                // Look up cached row.
                let cached = match find_row_by(
                    $crate::idempotency::TABLE,
                    "scope_key",
                    $bindings::boogy::platform::store::Value::Text(scope.clone()),
                ) {
                    Ok(opt) => opt,
                    Err(_) => {
                        // Store error on lookup → fall through to
                        // running the handler. Don't reject the
                        // request just because the cache is broken.
                        return inner(req);
                    }
                };

                if let Some(row) = cached {
                    let cached_fp = row.text("body_fingerprint").to_string();
                    if cached_fp != fp {
                        // Key reuse with a different body — caller bug.
                        // Routes through ApiError::conflict so the wire
                        // shape matches every other error response from
                        // the SDK (RFC 7807 application/problem+json).
                        return $crate::error::ApiError::conflict(
                            "Idempotency-Key reused with a different request payload",
                        )
                        .into();
                    }
                    // Cache hit, body matches → replay.
                    let status = match row.get("status") {
                        $crate::store::Val::Integer(i) => *i as u16,
                        _ => 500,
                    };
                    let headers: Vec<(String, String)> = row
                        .text("headers_json")
                        .parse::<::serde_json::Value>()
                        .ok()
                        .and_then(|v| {
                            v.as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|pair| {
                                        let p = pair.as_array()?;
                                        let k = p.first()?.as_str()?.to_string();
                                        let v = p.get(1)?.as_str()?.to_string();
                                        Some((k, v))
                                    })
                                    .collect()
                            })
                        })
                        .unwrap_or_default();
                    let body = match row.get("body_b64") {
                        $crate::store::Val::Text(s) if !s.is_empty() => {
                            __sdk_base64_decode(s)
                        }
                        _ => None,
                    };
                    return $crate::response::HttpResponse { status, headers, body };
                }

                // Cache miss → run handler.
                let resp = inner(req);

                // Cache only successful (2xx) responses. Errors are
                // transient by convention; the caller should retry
                // and ideally get a different outcome.
                if (200..300).contains(&resp.status) {
                    let headers_json = ::serde_json::to_string(&resp.headers)
                        .unwrap_or_else(|_| "[]".to_string());
                    let body_b64 = resp
                        .body
                        .as_deref()
                        .map(__sdk_base64_encode)
                        .unwrap_or_default();
                    let now = $bindings::boogy::platform::runtime::now_millis() as i64 / 1000;
                    let _ = $bindings::boogy::platform::store::upsert(
                        $crate::idempotency::TABLE,
                        &[$bindings::boogy::platform::store::Column {
                            name: "scope_key".into(),
                            val: $bindings::boogy::platform::store::Value::Text(scope),
                        }],
                        &[
                            $bindings::boogy::platform::store::Column {
                                name: "body_fingerprint".into(),
                                val: $bindings::boogy::platform::store::Value::Text(fp),
                            },
                            $bindings::boogy::platform::store::Column {
                                name: "status".into(),
                                val: $bindings::boogy::platform::store::Value::Integer(
                                    resp.status as i64,
                                ),
                            },
                            $bindings::boogy::platform::store::Column {
                                name: "headers_json".into(),
                                val: $bindings::boogy::platform::store::Value::Text(headers_json),
                            },
                            $bindings::boogy::platform::store::Column {
                                name: "body_b64".into(),
                                val: $bindings::boogy::platform::store::Value::Text(body_b64),
                            },
                            $bindings::boogy::platform::store::Column {
                                name: "created_at".into(),
                                val: $bindings::boogy::platform::store::Value::Integer(now),
                            },
                        ],
                    );
                }
                resp
            }
        }

        // Minimal base64 (standard alphabet, with padding) for
        // shuttling response bodies through the idempotency cache's
        // TEXT column. Bodies that aren't UTF-8 (e.g. binary
        // downloads) survive the round-trip cleanly.
        fn __sdk_base64_encode(data: &[u8]) -> ::std::string::String {
            const CHARS: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = ::std::string::String::with_capacity((data.len() + 2) / 3 * 4);
            for chunk in data.chunks(3) {
                let b0 = chunk[0] as u32;
                let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
                let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
                let triple = (b0 << 16) | (b1 << 8) | b2;
                out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
                out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
                if chunk.len() > 1 {
                    out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
                } else {
                    out.push('=');
                }
                if chunk.len() > 2 {
                    out.push(CHARS[(triple & 0x3F) as usize] as char);
                } else {
                    out.push('=');
                }
            }
            out
        }

        fn __sdk_base64_decode(s: &str) -> ::core::option::Option<::std::vec::Vec<u8>> {
            let mut table = [0xFFu8; 256];
            const CHARS: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            for (i, &c) in CHARS.iter().enumerate() {
                table[c as usize] = i as u8;
            }
            let bytes: ::std::vec::Vec<u8> = s.bytes().filter(|b| *b != b'=').collect();
            let mut out = ::std::vec::Vec::with_capacity(bytes.len() * 3 / 4);
            let mut i = 0;
            while i + 4 <= bytes.len() {
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
            // Trailing 2- or 3-byte tail (padding stripped above).
            match bytes.len() - i {
                0 => {}
                2 => {
                    let v0 = table[bytes[i] as usize];
                    let v1 = table[bytes[i + 1] as usize];
                    if v0 == 0xFF || v1 == 0xFF { return None; }
                    out.push(((v0 as u32) << 2 | (v1 as u32) >> 4) as u8);
                }
                3 => {
                    let v0 = table[bytes[i] as usize];
                    let v1 = table[bytes[i + 1] as usize];
                    let v2 = table[bytes[i + 2] as usize];
                    if v0 == 0xFF || v1 == 0xFF || v2 == 0xFF { return None; }
                    out.push(((v0 as u32) << 2 | (v1 as u32) >> 4) as u8);
                    out.push((((v1 as u32) & 0xF) << 4 | (v2 as u32) >> 2) as u8);
                }
                _ => return None,
            }
            Some(out)
        }

        // -- Cross-service peer fetch bridge --
        // Translates SDK PeerRequest / PeerResponse / PeerError to
        // and from the WIT-generated equivalents in the user's
        // crate. Capability gating happens host-side; if the
        // manifest doesn't grant `peer`, the bindings call returns
        // FetchError::CapabilityDenied.
        fn peer_fetch(
            target: &str,
            request: &$crate::peer::PeerRequest,
        ) -> ::core::result::Result<$crate::peer::PeerResponse, $crate::peer::PeerError> {
            let wit_req = peer_bindings::PeerRequest {
                method: request.method.clone(),
                path: request.path.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            };
            match peer_bindings::fetch(target, &wit_req) {
                Ok(resp) => Ok($crate::peer::PeerResponse {
                    status: resp.status,
                    headers: resp.headers,
                    body: resp.body,
                }),
                Err(e) => Err(__peer_error_to_sdk(e)),
            }
        }

        fn __peer_error_to_sdk(e: peer_bindings::FetchError) -> $crate::peer::PeerError {
            match e {
                peer_bindings::FetchError::InvalidTarget(s) => $crate::peer::PeerError::InvalidTarget(s),
                peer_bindings::FetchError::TargetNotFound(s) => $crate::peer::PeerError::TargetNotFound(s),
                peer_bindings::FetchError::Denied(s) => $crate::peer::PeerError::Denied(s),
                peer_bindings::FetchError::Timeout(s) => $crate::peer::PeerError::Timeout(s),
                peer_bindings::FetchError::DepthExceeded => $crate::peer::PeerError::DepthExceeded,
                peer_bindings::FetchError::CapabilityDenied => $crate::peer::PeerError::CapabilityDenied,
                peer_bindings::FetchError::Internal(s) => $crate::peer::PeerError::Internal(s),
            }
        }

        // -- Host-mediated secret verification bridge --
        //
        // Translates the SDK `VerifyError` to/from the WIT-generated
        // equivalent. The host resolves + KMS-unwraps the named secret
        // and verifies the HMAC entirely host-side — the secret value,
        // the message, and the computed tag never cross back into wasm.
        // There is NO `[capabilities]` flag for `secrets`: the gate is
        // the per-secret `[secrets]` `hmac-verify` usage declaration. An
        // undeclared / wrong-usage / unbound ref returns
        // `VerifyError::UnknownSecret`.
        #[allow(dead_code)]
        fn secrets_verify_hmac(
            secret_ref: &str,
            algorithm: $crate::secrets::HmacAlgorithm,
            message: &[u8],
            expected_hex: &str,
        ) -> ::core::result::Result<bool, $crate::secrets::VerifyError> {
            let wit_alg = match algorithm {
                $crate::secrets::HmacAlgorithm::Sha256 => {
                    secrets_bindings::HmacAlgorithm::Sha256
                }
            };
            match secrets_bindings::verify_hmac(
                &secret_ref.to_string(),
                wit_alg,
                &message.to_vec(),
                &expected_hex.to_string(),
            ) {
                Ok(b) => Ok(b),
                Err(e) => Err(__secrets_verify_error_to_sdk(e)),
            }
        }

        /// SHA-256 convenience over [`secrets_verify_hmac`] — the common
        /// case for webhook signature verification. Equivalent to passing
        /// `HmacAlgorithm::Sha256`. Catalog handlers call this:
        /// `secrets_verify_hmac_sha256("stripe_webhook_secret",
        /// &signed_message, &expected_hex)?`.
        #[allow(dead_code)]
        fn secrets_verify_hmac_sha256(
            secret_ref: &str,
            message: &[u8],
            expected_hex: &str,
        ) -> ::core::result::Result<bool, $crate::secrets::VerifyError> {
            secrets_verify_hmac(
                secret_ref,
                $crate::secrets::HmacAlgorithm::Sha256,
                message,
                expected_hex,
            )
        }

        fn __secrets_verify_error_to_sdk(
            e: secrets_bindings::VerifyError,
        ) -> $crate::secrets::VerifyError {
            match e {
                secrets_bindings::VerifyError::UnknownSecret(s) => {
                    $crate::secrets::VerifyError::UnknownSecret(s)
                }
                secrets_bindings::VerifyError::Internal(s) => {
                    $crate::secrets::VerifyError::Internal(s)
                }
            }
        }

        // -- Host-mediated signing bridge --
        //
        // Translates the SDK `signing` types to/from their WIT-generated
        // equivalents. The host generates + holds the private key and signs
        // entirely host-side — the component only ever receives the public
        // key, a produced signature, or a typed error; the private key never
        // crosses back into wasm and there is no read/export op. The gate is
        // the `[capabilities] signing = true` manifest grant; without it the
        // bindings call returns `SignError::Internal`.

        fn __signing_alg_to_wit(
            alg: $crate::signing::SigAlg,
        ) -> signing_bindings::SigAlg {
            match alg {
                $crate::signing::SigAlg::Ed25519 => signing_bindings::SigAlg::Ed25519,
                $crate::signing::SigAlg::EcdsaSecp256k1 => {
                    signing_bindings::SigAlg::EcdsaSecp256k1
                }
                $crate::signing::SigAlg::EcdsaP256 => signing_bindings::SigAlg::EcdsaP256,
            }
        }

        fn __signing_alg_to_sdk(
            alg: signing_bindings::SigAlg,
        ) -> $crate::signing::SigAlg {
            match alg {
                signing_bindings::SigAlg::Ed25519 => $crate::signing::SigAlg::Ed25519,
                signing_bindings::SigAlg::EcdsaSecp256k1 => {
                    $crate::signing::SigAlg::EcdsaSecp256k1
                }
                signing_bindings::SigAlg::EcdsaP256 => $crate::signing::SigAlg::EcdsaP256,
            }
        }

        fn __signing_signature_to_sdk(
            sig: signing_bindings::Signature,
        ) -> $crate::signing::Signature {
            $crate::signing::Signature {
                bytes: sig.bytes,
                recovery_id: sig.recovery_id,
            }
        }

        fn __signing_error_to_sdk(
            e: signing_bindings::SignError,
        ) -> $crate::signing::SignError {
            match e {
                signing_bindings::SignError::UnknownKey(s) => {
                    $crate::signing::SignError::UnknownKey(s)
                }
                signing_bindings::SignError::BadInput(s) => {
                    $crate::signing::SignError::BadInput(s)
                }
                signing_bindings::SignError::Internal(s) => {
                    $crate::signing::SignError::Internal(s)
                }
            }
        }

        /// Generate a new signing key under `label`. Returns the public key.
        /// The private key stays host-side and is never returned.
        #[allow(dead_code)]
        fn signing_create_key(
            label: &str,
            alg: $crate::signing::SigAlg,
        ) -> ::core::result::Result<::std::vec::Vec<u8>, $crate::signing::SignError> {
            match signing_bindings::create_key(&label.to_string(), __signing_alg_to_wit(alg)) {
                Ok(pk) => Ok(pk),
                Err(e) => Err(__signing_error_to_sdk(e)),
            }
        }

        /// Sign a prehashed 32-byte digest (the ECDSA path). Non-32-byte
        /// input is rejected `BadInput`; an Ed25519 key rejects here.
        #[allow(dead_code)]
        fn signing_sign_digest(
            label: &str,
            digest: &[u8],
            alg: $crate::signing::SigAlg,
        ) -> ::core::result::Result<$crate::signing::Signature, $crate::signing::SignError> {
            match signing_bindings::sign_digest(
                &label.to_string(),
                &digest.to_vec(),
                __signing_alg_to_wit(alg),
            ) {
                Ok(sig) => Ok(__signing_signature_to_sdk(sig)),
                Err(e) => Err(__signing_error_to_sdk(e)),
            }
        }

        /// Sign a full message (the Ed25519 path). An ECDSA key rejects here.
        #[allow(dead_code)]
        fn signing_sign_message(
            label: &str,
            message: &[u8],
            alg: $crate::signing::SigAlg,
        ) -> ::core::result::Result<$crate::signing::Signature, $crate::signing::SignError> {
            match signing_bindings::sign_message(
                &label.to_string(),
                &message.to_vec(),
                __signing_alg_to_wit(alg),
            ) {
                Ok(sig) => Ok(__signing_signature_to_sdk(sig)),
                Err(e) => Err(__signing_error_to_sdk(e)),
            }
        }

        /// List this service's signing keys (label + alg + public key only).
        #[allow(dead_code)]
        fn signing_list_keys() -> ::std::vec::Vec<$crate::signing::KeyInfo> {
            signing_bindings::list_keys()
                .into_iter()
                .map(|k| $crate::signing::KeyInfo {
                    label: k.label,
                    alg: __signing_alg_to_sdk(k.alg),
                    public_key: k.public_key,
                })
                .collect()
        }

        /// Remove a signing key. Idempotent.
        #[allow(dead_code)]
        fn signing_remove_key(
            label: &str,
        ) -> ::core::result::Result<(), $crate::signing::SignError> {
            match signing_bindings::remove_key(&label.to_string()) {
                Ok(()) => Ok(()),
                Err(e) => Err(__signing_error_to_sdk(e)),
            }
        }

        // -- Background-jobs bridging --
        //
        // Same shape as peer_fetch: clean SDK types in, WIT types out
        // via `jobs_bindings::enqueue` etc. Capability gate is host-
        // side; if `[capabilities] background_jobs = false`, bindings
        // call returns BackendUnavailable.

        fn jobs_enqueue(
            spec: $crate::jobs::JobSpec,
        ) -> ::core::result::Result<String, $crate::jobs::EnqueueError> {
            let wit_spec = jobs_bindings::JobSpec {
                handler: spec.handler,
                payload: spec.payload,
                not_before_unix_s: spec.not_before_unix_s,
                max_attempts: spec.max_attempts,
                idempotency_key: spec.idempotency_key,
            };
            match jobs_bindings::enqueue(&wit_spec) {
                Ok(job_id) => Ok(job_id),
                Err(e) => Err(__jobs_enqueue_error_to_sdk(e)),
            }
        }

        fn __jobs_enqueue_error_to_sdk(
            e: jobs_bindings::EnqueueError,
        ) -> $crate::jobs::EnqueueError {
            match e {
                jobs_bindings::EnqueueError::QueueFull(d) => {
                    $crate::jobs::EnqueueError::QueueFull($crate::jobs::TenantDepth {
                        depth: d.depth,
                        cap: d.cap,
                    })
                }
                jobs_bindings::EnqueueError::InvalidHandler(s) => {
                    $crate::jobs::EnqueueError::InvalidHandler(s)
                }
                jobs_bindings::EnqueueError::InvalidSpec(s) => {
                    $crate::jobs::EnqueueError::InvalidSpec(s)
                }
                jobs_bindings::EnqueueError::BackendUnavailable => {
                    $crate::jobs::EnqueueError::BackendUnavailable
                }
            }
        }

        fn jobs_cancel(
            job_id: &str,
        ) -> ::core::result::Result<$crate::jobs::CancelOutcome, $crate::jobs::CancelError> {
            match jobs_bindings::cancel(&job_id.to_string()) {
                Ok(o) => Ok(__jobs_cancel_outcome_to_sdk(o)),
                Err(e) => Err(__jobs_cancel_error_to_sdk(e)),
            }
        }

        fn jobs_status(
            job_id: &str,
        ) -> ::core::result::Result<$crate::jobs::JobStatusInfo, $crate::jobs::CancelError> {
            match jobs_bindings::status(&job_id.to_string()) {
                Ok(s) => Ok(__jobs_status_to_sdk(s)),
                Err(e) => Err(__jobs_cancel_error_to_sdk(e)),
            }
        }

        fn __jobs_cancel_outcome_to_sdk(
            o: jobs_bindings::CancelOutcome,
        ) -> $crate::jobs::CancelOutcome {
            match o {
                jobs_bindings::CancelOutcome::Cancelled => {
                    $crate::jobs::CancelOutcome::Cancelled
                }
                jobs_bindings::CancelOutcome::CancellationRequested => {
                    $crate::jobs::CancelOutcome::CancellationRequested
                }
                jobs_bindings::CancelOutcome::AlreadyTerminal => {
                    $crate::jobs::CancelOutcome::AlreadyTerminal
                }
            }
        }

        fn __jobs_cancel_error_to_sdk(
            e: jobs_bindings::CancelError,
        ) -> $crate::jobs::CancelError {
            match e {
                jobs_bindings::CancelError::NotFound => $crate::jobs::CancelError::NotFound,
                jobs_bindings::CancelError::BackendUnavailable => {
                    $crate::jobs::CancelError::BackendUnavailable
                }
            }
        }

        fn __jobs_status_to_sdk(
            s: jobs_bindings::JobStatusInfo,
        ) -> $crate::jobs::JobStatusInfo {
            match s {
                jobs_bindings::JobStatusInfo::Pending => $crate::jobs::JobStatusInfo::Pending,
                jobs_bindings::JobStatusInfo::Running => $crate::jobs::JobStatusInfo::Running,
                jobs_bindings::JobStatusInfo::Succeeded => $crate::jobs::JobStatusInfo::Succeeded,
                jobs_bindings::JobStatusInfo::Failed(s) => {
                    $crate::jobs::JobStatusInfo::Failed(s)
                }
                jobs_bindings::JobStatusInfo::DeadLetter(s) => {
                    $crate::jobs::JobStatusInfo::DeadLetter(s)
                }
                jobs_bindings::JobStatusInfo::Cancelled => {
                    $crate::jobs::JobStatusInfo::Cancelled
                }
            }
        }

        // -- Websockets bridging --
        //
        // Same shape as jobs: clean SDK types in, WIT types out via
        // `ws_bindings::publish` / `mint_subscribe_grant`. Capability
        // gate is host-side; if `[capabilities] websockets = false`,
        // calls return CapabilityDenied.

        fn ws_publish(
            channel: &str,
            payload: &str,
        ) -> ::core::result::Result<(), $crate::websockets::PublishError> {
            match ws_bindings::publish(&channel.to_string(), &payload.to_string()) {
                Ok(()) => Ok(()),
                Err(e) => Err(__ws_publish_error_to_sdk(e)),
            }
        }

        fn ws_mint_subscribe_grant(
            channel: &str,
            ttl_seconds: u32,
        ) -> ::core::result::Result<String, $crate::websockets::GrantError> {
            match ws_bindings::mint_subscribe_grant(&channel.to_string(), ttl_seconds) {
                Ok(grant) => Ok(grant),
                Err(e) => Err(__ws_grant_error_to_sdk(e)),
            }
        }

        fn ws_publish_to_principal(
            channel: &str,
            principal: &str,
            payload: &str,
        ) -> ::core::result::Result<(), $crate::websockets::PublishError> {
            match ws_bindings::publish_to_principal(
                &channel.to_string(),
                &principal.to_string(),
                &payload.to_string(),
            ) {
                Ok(()) => Ok(()),
                Err(e) => Err(__ws_publish_error_to_sdk(e)),
            }
        }

        fn ws_mint_principal_subscribe_grant(
            channel: &str,
            principal: &str,
            ttl_seconds: u32,
        ) -> ::core::result::Result<String, $crate::websockets::GrantError> {
            match ws_bindings::mint_principal_subscribe_grant(
                &channel.to_string(),
                &principal.to_string(),
                ttl_seconds,
            ) {
                Ok(g) => Ok(g),
                Err(e) => Err(__ws_grant_error_to_sdk(e)),
            }
        }

        /// Build a typed envelope and publish it to a principal's room. The
        /// preferred publish entrypoint for per-principal channels — always
        /// send an envelope, never a bare payload. `ts` is filled from the
        /// host clock (milliseconds since Unix epoch).
        fn ws_publish_event(
            channel: &str,
            principal: &str,
            type_: &str,
            v: u32,
            data: ::serde_json::Value,
        ) -> ::core::result::Result<(), $crate::websockets::PublishError> {
            let env = $crate::websockets::Envelope::new(type_, v, now_millis(), data);
            ws_publish_to_principal(channel, principal, &env.to_json())
        }

        fn __ws_publish_error_to_sdk(
            e: ws_bindings::PublishError,
        ) -> $crate::websockets::PublishError {
            match e {
                ws_bindings::PublishError::CapabilityDenied => {
                    $crate::websockets::PublishError::CapabilityDenied
                }
                ws_bindings::PublishError::UnknownChannel => {
                    $crate::websockets::PublishError::UnknownChannel
                }
                ws_bindings::PublishError::PayloadTooLarge => {
                    $crate::websockets::PublishError::PayloadTooLarge
                }
                ws_bindings::PublishError::RateLimited => {
                    $crate::websockets::PublishError::RateLimited
                }
                ws_bindings::PublishError::BackendUnavailable => {
                    $crate::websockets::PublishError::BackendUnavailable
                }
                ws_bindings::PublishError::WrongClass => {
                    $crate::websockets::PublishError::WrongClass
                }
            }
        }

        fn __ws_grant_error_to_sdk(
            e: ws_bindings::GrantError,
        ) -> $crate::websockets::GrantError {
            match e {
                ws_bindings::GrantError::CapabilityDenied => {
                    $crate::websockets::GrantError::CapabilityDenied
                }
                ws_bindings::GrantError::UnknownChannel => {
                    $crate::websockets::GrantError::UnknownChannel
                }
                ws_bindings::GrantError::NotPrivate => {
                    $crate::websockets::GrantError::NotPrivate
                }
                ws_bindings::GrantError::InvalidTtl => {
                    $crate::websockets::GrantError::InvalidTtl
                }
                ws_bindings::GrantError::RateLimited => {
                    $crate::websockets::GrantError::RateLimited
                }
                ws_bindings::GrantError::WrongClass => {
                    $crate::websockets::GrantError::WrongClass
                }
            }
        }

        // -- Vector search bridging --
        //
        // Same pattern as peer_fetch / jobs: SDK types in, WIT types
        // out via `vector_bindings::*`. Capability gate is host-side;
        // if `[capabilities] vector = false`, the bindings calls will
        // return an error string.

        #[allow(dead_code)]
        fn create_vector_collection(
            table: &str,
            name: &str,
            dims: u32,
            metric: $crate::vector::DistanceMetric,
        ) -> Result<(), String> {
            let wit_metric = match metric {
                $crate::vector::DistanceMetric::Cosine => vector_bindings::DistanceMetric::Cosine,
                $crate::vector::DistanceMetric::Euclidean => vector_bindings::DistanceMetric::Euclidean,
                $crate::vector::DistanceMetric::DotProduct => vector_bindings::DistanceMetric::DotProduct,
            };
            vector_bindings::create_collection(table, name, vector_bindings::VectorCollectionOptions {
                dimensions: dims,
                metric: wit_metric,
                m: None,
                ef_construction: None,
            })
        }

        #[allow(dead_code)]
        fn create_vector_collection_with_options(
            table: &str,
            name: &str,
            opts: &$crate::vector::VectorCollectionOptions,
        ) -> Result<(), String> {
            let wit_metric = match opts.metric {
                $crate::vector::DistanceMetric::Cosine => vector_bindings::DistanceMetric::Cosine,
                $crate::vector::DistanceMetric::Euclidean => vector_bindings::DistanceMetric::Euclidean,
                $crate::vector::DistanceMetric::DotProduct => vector_bindings::DistanceMetric::DotProduct,
            };
            vector_bindings::create_collection(table, name, vector_bindings::VectorCollectionOptions {
                dimensions: opts.dimensions,
                metric: wit_metric,
                m: opts.m,
                ef_construction: opts.ef_construction,
            })
        }

        #[allow(dead_code)]
        fn drop_vector_collection(table: &str, name: &str) -> Result<(), String> {
            vector_bindings::drop_collection(table, name)
        }

        #[allow(dead_code)]
        fn unlock_vector_collection(table: &str, name: &str, key: &[u8]) -> Result<(), String> {
            vector_bindings::unlock_collection(table, name, key)
        }

        #[allow(dead_code)]
        fn vector_insert(table: &str, collection: &str, rowid: u64, vector: &[f32]) -> Result<(), String> {
            vector_bindings::insert(table, collection, rowid, vector)
        }

        #[allow(dead_code)]
        fn vector_insert_batch(table: &str, collection: &str, entries: &[(u64, Vec<f32>)]) -> Result<(), String> {
            let wit_entries: Vec<(u64, Vec<f32>)> = entries.to_vec();
            vector_bindings::insert_batch(table, collection, &wit_entries)
        }

        #[allow(dead_code)]
        fn vector_update(table: &str, collection: &str, rowid: u64, vector: &[f32]) -> Result<(), String> {
            vector_bindings::update(table, collection, rowid, vector)
        }

        #[allow(dead_code)]
        fn vector_delete(table: &str, collection: &str, rowid: u64) -> Result<(), String> {
            vector_bindings::delete(table, collection, rowid)
        }

        #[allow(dead_code)]
        fn vector_search(
            table: &str,
            collection: &str,
            query: &[f32],
            k: u32,
        ) -> Result<Vec<$crate::vector::VectorResult>, String> {
            let results = vector_bindings::search(table, collection, query, &vector_bindings::VectorSearchOptions {
                k,
                ef_search: None,
                filter: None,
            })?;
            Ok(results.into_iter().map(|r| $crate::vector::VectorResult {
                rowid: r.rowid,
                distance: r.distance,
            }).collect())
        }

        #[allow(dead_code)]
        fn vector_search_with_options(
            table: &str,
            collection: &str,
            query: &[f32],
            k: u32,
            ef_search: Option<u32>,
            filter: Option<store::Filter>,
        ) -> Result<Vec<$crate::vector::VectorResult>, String> {
            let results = vector_bindings::search(table, collection, query, &vector_bindings::VectorSearchOptions {
                k,
                ef_search,
                filter,
            })?;
            Ok(results.into_iter().map(|r| $crate::vector::VectorResult {
                rowid: r.rowid,
                distance: r.distance,
            }).collect())
        }

        #[allow(dead_code)]
        fn vector_search_filtered(
            table: &str,
            collection: &str,
            query: &[f32],
            k: u32,
            filter: store::Filter,
        ) -> Result<Vec<$crate::vector::VectorResult>, String> {
            let results = vector_bindings::search(table, collection, query, &vector_bindings::VectorSearchOptions {
                k,
                ef_search: None,
                filter: Some(filter),
            })?;
            Ok(results.into_iter().map(|r| $crate::vector::VectorResult {
                rowid: r.rowid,
                distance: r.distance,
            }).collect())
        }

        // -- The Guest impl that wires everything together --
        //
        // Wraps every dispatch in:
        //   1. log thunk registration — the SDK's `log::info!`/etc.
        //      macros need a function pointer to this user crate's
        //      `bindings::...::runtime::log` (the bindings module is
        //      private to the user crate, so we hand the pointer over
        //      via SDK-side thread-local).
        //   2. request id setup — pulled from the `x-boogy-
        //      request-id` header the host plumbs through. Cleared on
        //      return so handlers in cold paths don't see a stale id.
        impl $bindings::exports::boogy::platform::http_handler::Guest for $api_struct {
            fn handle(
                req: $bindings::exports::boogy::platform::http_handler::HttpRequest,
            ) -> $bindings::exports::boogy::platform::http_handler::HttpResponse {
                fn __sdk_runtime_log(level: &str, msg: &str) {
                    $bindings::boogy::platform::runtime::log(
                        &level.to_string(),
                        &msg.to_string(),
                    );
                }
                $crate::log::_register_runtime_log(__sdk_runtime_log);

                let request_id = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("x-boogy-request-id"))
                    .map(|(_, v)| v.clone());
                $crate::log::_set_request_id(request_id);

                // Stash the WIT principal + install the shared cleanup guard.
                // `enter` sets the WIT principal slot so `Principal::from_request`
                // and `auth::current_principal()` resolve it without a WIT call;
                // the returned guard clears request id + both principal slots on
                // drop (even on panic).
                let _state_guard = $crate::request_state::enter(
                    $bindings::boogy::platform::auth::current_identity()
                        .map(|i| i.principal),
                );

                <$api_struct as $crate::Api>::init_tables();
                let sdk_req = __boogy_to_sdk_request(&req);
                let resp = <$api_struct as $crate::Api>::build_router().handle(&sdk_req);
                __boogy_to_wit_response(resp)
            }
        }

        // -- WIT export macro --
        $bindings::export!($api_struct with_types_in $bindings);
    };

    // -----------------------------------------------------------------------
    // Three-argument form: `wit_glue!(bindings, MyApi, with_jobs)`
    //
    // Emits everything the two-argument form emits PLUS an
    // `impl job_handler::Guest` that dispatches through
    // `<MyApi as Api>::build_job_router()`.
    //
    // Only use this form when the consumer's `wit_bindgen::generate!` declares
    // `world: "service-with-jobs"`.  HTTP-only consumers (world: "service") must use
    // the two-argument form — the `job_handler` export does not exist in their
    // generated bindings and the impl block would fail to compile.
    // -----------------------------------------------------------------------
    ($bindings:ident, $api_struct:ident, with_jobs) => {
        // Expand the full two-argument form first (HTTP Guest + helpers + export!).
        $crate::wit_glue!($bindings, $api_struct);

        // Add the parallel job_handler::Guest impl on top.
        impl $bindings::exports::boogy::platform::job_handler::Guest for $api_struct {
            fn handle_job(
                ctx: $bindings::exports::boogy::platform::job_handler::JobContext,
                payload: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                $bindings::exports::boogy::platform::job_handler::HandlerError,
            > {
                // Same per-request state setup as the HTTP path so identity-scoped
                // helpers (`auth::current_principal`, `auth::load_owned`/`find_owned`,
                // the `Principal` extractor) work inside job handlers. The job's
                // replayed identity is exposed via the WIT `auth` cap exactly as on
                // the HTTP path. Guard clears the slots on return.
                let _state_guard = $crate::request_state::enter(
                    $bindings::boogy::platform::auth::current_identity()
                        .map(|i| i.principal),
                );

                // Build the SDK-side JobContext mirror from the WIT context so
                // handlers can read `ctx.attempts` (the terminal-attempt signal).
                let sdk_ctx = $crate::JobContext {
                    job_id: ctx.job_id.clone(),
                    handler: ctx.handler.clone(),
                    attempts: ctx.attempts,
                    not_before_unix_s: ctx.not_before_unix_s,
                };
                match <$api_struct as $crate::Api>::build_job_router().dispatch(&sdk_ctx, &payload) {
                    ::core::result::Result::Ok(bytes) => ::core::result::Result::Ok(bytes),
                    ::core::result::Result::Err($crate::JobError::Retry(msg)) => ::core::result::Result::Err(
                        $bindings::exports::boogy::platform::job_handler::HandlerError::Retry(msg),
                    ),
                    ::core::result::Result::Err($crate::JobError::Terminal(msg)) => ::core::result::Result::Err(
                        $bindings::exports::boogy::platform::job_handler::HandlerError::Terminal(msg),
                    ),
                }
            }
        }
    };
}
