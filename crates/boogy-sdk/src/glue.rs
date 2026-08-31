//! `wit_glue!` macro — emits the WIT↔SDK conversion layer in the user's crate.
//!
//! `wit_bindgen::generate!` produces a `bindings` module with WIT-defined types
//! that are local to the user's crate. This macro takes that module and emits:
//!
//! - The `Guest` impl for the user's API struct, wired to the
//!   [`Api`](crate::Api) trait (`schema` / `migrate` / `bootstrap` + `build_router`).
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
/// ```ignore, ignore_snippet: the crate-level glue macro itself. Its trait impls are crate-scoped, so a harness holding two invocations is a duplicate-impl error by construction.
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
/// - `random_*` — random values over the platform entropy source
///   (`entropy` capability): `random_int`, `random_int_exclusive`,
///   `random_float`, `random_unit_float`, `random_bool`,
///   `random_bool_with_probability`, `random_string`, `random_id`,
///   `random_hex`, `random_bytes`, `random_choose`, `random_shuffle`,
///   `random_sample`, `random_vec_of`, `random_uuid_v4`,
///   `random_uuid_v7`, plus `try_random_int` / `try_random_float` /
///   `try_random_string` and `rng()` for the full
///   [`boogy_sdk::random::Rng`](crate::random::Rng) surface.
/// - Typed-model CRUD over a `#[derive(Model)]` type `M` (see
///   [`boogy_sdk::model`]): `create_model::<M>()` (register in
///   `init_tables`), `db_insert(&M) -> u64`, `db_get::<M>(id) ->
///   Option<M>`, `db_find_by::<M>(col, Val) -> Vec<M>`,
///   `db_update::<M>(id, &M)`, `db_delete::<M>(id)`. These serialize via
///   the model's `Field` impls, so model code never hand-builds columns.
///
/// And these `use` statements are emitted so handlers don't need to repeat
/// them. The list is exhaustive — anything not named here you import yourself:
///
/// - Routing / request: `Router`, `Req`, `Params`, `Ctx`, `FromRequest`,
///   `Path`, `Principal`, `QueryExtractor` (that is
///   [`boogy_sdk::extract::Query`](crate::extract::Query), renamed to leave the
///   name `Query` free for the typed-query DSL struct this macro also emits).
/// - Responses / errors: `response` (the module), `Json`, `Created`,
///   `NoContent`, `Redirect`, `IntoResponse`, `ApiError`, `StoreError`.
/// - Bodies: `json` (the module), `Deserialize`, `Serialize`, `parse_body`,
///   `validate_body`.
/// - Store: `store` (the WIT bindings module), `Row`, `Table`,
///   `DEFAULT_OWNER_COL`.
/// - Other bindings modules: `peer_bindings`, `secrets_bindings`,
///   `signing_bindings`, `jobs_bindings`, `ws_bindings`.
/// - Random: `Alphabet`.
///
/// `Val` is **not** among them — see the note below on why it is deliberately
/// excluded. Reach it as [`boogy_sdk::store::Val`](crate::store::Val) when a
/// signature names it.
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
                    store::StoreError::CommitUnknown(m)       => S::CommitUnknown(m),
                    store::StoreError::ResourceExhausted(m)   => S::ResourceExhausted(m),
                    store::StoreError::Poisoned(m)            => S::Poisoned(m),
                    store::StoreError::TooContended(m)        => S::TooContended(m),
                    store::StoreError::Gone(m)                => S::Gone(m),
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
        use $bindings::boogy::platform::websockets as ws_bindings;
        use $bindings::boogy::platform::files as files_bindings;
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
        // Named symbol sets for `random_string(len, &Alphabet::HEX)`.
        #[allow(unused_imports)]
        use $crate::random::Alphabet;
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

        /// One WIT value → the SDK's `Val`. Same mapping `to_sdk_row` applies
        /// per column, named so the aggregate path can reuse it rather than
        /// writing a second copy that could drift.
        fn __boogy_wit_to_val(v: &$bindings::boogy::platform::store::Value) -> $crate::store::Val {
            match v {
                $bindings::boogy::platform::store::Value::Null      => $crate::store::Val::Null,
                $bindings::boogy::platform::store::Value::Text(s)   => $crate::store::Val::Text(s.clone()),
                $bindings::boogy::platform::store::Value::Integer(i)=> $crate::store::Val::Integer(*i),
                $bindings::boogy::platform::store::Value::Real(f)   => $crate::store::Val::Real(*f),
                $bindings::boogy::platform::store::Value::Blob(b)   => $crate::store::Val::Blob(b.clone()),
                $bindings::boogy::platform::store::Value::Boolean(b)=> $crate::store::Val::Boolean(*b),
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

        /// One declared column as the WIT `column-def`.
        ///
        /// The single ColDef→WIT mapping: `create_table` sends the whole set
        /// through it and the column reconcile sends one column at a time. It
        /// was inline in `create_table_from` until the reconcile needed the
        /// same mapping — and a second copy would be free to disagree about,
        /// say, whether a default rides along, which is precisely the class of
        /// bug the reconcile exists to fix.
        fn __boogy_col_def_to_wit(
            c: &$crate::store::ColDef,
        ) -> $bindings::boogy::platform::store::ColumnDef {
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
                default: c.default.as_ref().map(|v| __boogy_val_to_wit(v)),
                counter: c.counter,
                counter_max: c.counter_max,
            }
        }

        /// Conflicts the schema pass could not apply, in developer-facing prose.
        ///
        /// A thread-local for the same reason `__BOOGY_DECLARED` is one:
        /// `create_table_from` is a free fn called per table, and the value has
        /// to outlive it and reach the response the resolution pass returns.
        ///
        /// **Reset at the start of every schema pass**, so once the pass
        /// returns the buffer holds exactly that pass's conflicts. That is what
        /// makes it a snapshot rather than a running total: the declaration
        /// re-runs per request whenever a deployment's schema was never
        /// resolved, and a buffer that only ever grew would report the same
        /// conflict N times and leak.
        ///
        /// Conflicts also reach the developer through `log::warn!` as they are
        /// found; `__boogy_take_schema_conflicts` is for the pass that will
        /// turn them into a response header.
        ///
        /// A conflict is NOT an error here. The pass applies everything it can
        /// and reports what it could not, because the alternative — trapping on
        /// the first one — hides every conflict after it and reaches the
        /// developer as an opaque 500.
        thread_local! {
            static __BOOGY_SCHEMA_CONFLICTS: ::std::cell::RefCell<
                ::std::vec::Vec<::std::string::String>
            > = ::std::cell::RefCell::new(::std::vec::Vec::new());
        }

        /// Record one unappliable schema difference.
        fn __boogy_note_schema_conflict(msg: ::std::string::String) {
            $crate::log::warn!("schema: {msg}");
            __BOOGY_SCHEMA_CONFLICTS.with(|c| c.borrow_mut().push(msg));
        }

        /// Take everything recorded by the schema pass, emptying the buffer.
        fn __boogy_take_schema_conflicts() -> ::std::vec::Vec<::std::string::String> {
            __BOOGY_SCHEMA_CONFLICTS.with(|c| ::std::mem::take(&mut *c.borrow_mut()))
        }

        /// Has anything already been recorded as a conflict in THIS pass?
        ///
        /// Peeks; it must not drain. The buffer is what the `ApplyOnly` response
        /// builder turns into the `x-boogy-schema-conflict` header, so emptying
        /// it here would silently disable the whole refusal.
        ///
        /// Exists because the abandon-the-plan rule is a property of the
        /// DEPLOYMENT, not of one table: `create_table_from` runs once per model,
        /// so a service whose first table plans a rename and whose second plans a
        /// conflict would otherwise apply the rename and then be refused —
        /// the same irrecoverable state, one level up.
        fn __boogy_schema_conflicts_pending() -> bool {
            __BOOGY_SCHEMA_CONFLICTS.with(|c| !c.borrow().is_empty())
        }

        // -- Table builder → WIT create_table + create_index calls --
        fn create_table_from(table: &$crate::store::Table) {
            let cols: Vec<$bindings::boogy::platform::store::ColumnDef> =
                table.columns.iter().map(__boogy_col_def_to_wit).collect();

            // Resolve declared access patterns + explicit indexes into the
            // physical index set; surface build-time diagnostics via logging.
            // Resolved BEFORE anything is applied: an `Error` diagnostic means
            // the declaration is impossible to satisfy, and creating the table
            // first would leave a half-built schema behind the panic. The
            // reconcile below also needs the resolved set, to say which added
            // columns are indexed.
            #[allow(unused)]
            let (__resolved, __diags) = table.resolved_indices();
            for d in &__diags {
                match d {
                    $crate::schema_resolve::Diagnostic::Warning(m) =>
                        $crate::log::warn!("schema {}: {}", table.name, m),
                    $crate::schema_resolve::Diagnostic::Error(m) =>
                        panic!("schema {}: {}", table.name, m),
                }
            }
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
            } else {
                __boogy_reconcile_columns(table, &__resolved);
            }

            // Record the resolved set; the reconcile pass applies every
            // table's changes once `Api::init_tables()` has declared them all.
            // A per-table pass cannot work here: it would read every OTHER
            // table's indexes as undeclared and drop them.
            __BOOGY_DECLARED.with(|d| {
                d.borrow_mut().push((table.name.clone(), __resolved.clone()))
            });
            let __rollups: ::std::vec::Vec<(
                ::std::string::String,
                ::std::vec::Vec<::std::string::String>,
                ::std::vec::Vec<::std::string::String>,
                bool,
            )> = table
                .access_patterns
                .iter()
                .filter_map(|p| match p {
                    $crate::store::AccessPattern::Rollup { group, sum, count } => Some((
                        // The name carries every grouping column, so declaring
                        // `[room_id, post_id]` and `[room_id]` on one table
                        // gives two rollups rather than one silently winning.
                        ::std::format!("rollup_{}", group.join("_")),
                        group.clone(),
                        sum.clone(),
                        *count,
                    )),
                    _ => None,
                })
                .collect();
            if !__rollups.is_empty() {
                __BOOGY_DECLARED_ROLLUPS
                    .with(|d| d.borrow_mut().push((table.name.clone(), __rollups)));
            }
        }

        /// Converge one EXISTING table's columns on what its model declares.
        ///
        /// Only reached when `list_tables` already shows the table: a table
        /// that does not exist is created whole, columns included.
        ///
        /// This arm used not to exist. A table's columns were fixed at
        /// creation, so adding a field to a `#[derive(Model)]` struct deployed
        /// cleanly and then every write to that table was refused — forever,
        /// with no diagnostic — because the row carried a column the stored
        /// schema had never heard of.
        ///
        /// Every action here is O(1) metadata and **nothing is backfilled**,
        /// which is what makes the pass affordable at provision time and is
        /// also its one visible limitation — see the index warning below.
        ///
        /// `Revive` is the exception, and it is an INDIRECT one: dropping a
        /// column discarded every index spec that mentioned it, so the index
        /// reconcile that runs after this pass sees those indexes as missing
        /// and recreates them — and `create_index` backfills. Reviving a column
        /// on a large table therefore costs O(rows), just not here. Left as is:
        /// the alternative is an index that exists in the metadata and has no
        /// entries, which reads as an empty result rather than as an error.
        ///
        /// Safe to re-run: the plan is computed from a fresh `list_columns`, so
        /// a converged schema plans nothing at all, and the residual race (a
        /// second host applying the same plan concurrently) lands on
        /// "already exists", which is treated as success exactly as the create
        /// path treats it.
        ///
        /// **All-or-nothing.** The whole plan is scanned for conflicts before
        /// any of it is applied, and a single conflict applies NOTHING. That is
        /// not tidiness: a conflict refuses the deploy, and the refusal restores
        /// the previous WASM, not the previous SCHEMA. So a partially-applied
        /// plan leaves the restored deployment declaring one thing and the store
        /// holding another — for a `Rename`, irrecoverably, because the next
        /// resolution re-adds the old name EMPTY beside the renamed column that
        /// still holds the data. Applying nothing is the only outcome the
        /// refusal can actually undo. It also removes the cross-action ordering
        /// coupling entirely: actions are sorted by column name, so before this
        /// scan a rename of `a_col` was applied before a conflict on `z_col` was
        /// even looked at, and any future action order could reintroduce that.
        ///
        /// The rule is a property of the DEPLOYMENT, so it does not stop at this
        /// table: this function also refuses to apply anything once ANY earlier
        /// table in the same pass has recorded a conflict
        /// (`__boogy_schema_conflicts_pending`). Without that, a service whose
        /// first model plans a rename and whose second plans a conflict reaches
        /// the identical unrecoverable state one level up.
        ///
        /// **What that does NOT give you is full atomicity across tables.**
        /// Models reconcile in declaration order, and nothing here can un-apply
        /// an earlier table's actions — so a conflict in a LATER table still
        /// leaves an EARLIER table converged while the deploy is refused. That
        /// residual is bounded to actions this pass considers safe on their own
        /// (an add, a default change, a rename or a soft drop the model asked
        /// for), and the restored deployment declares the schema it was
        /// deployed with, so its own next resolution converges rather than
        /// stranding data. Closing it properly needs the whole declaration
        /// planned before any of it is applied, which is a different shape than
        /// `create_table_from`'s per-model call.
        fn __boogy_reconcile_columns(
            table: &$crate::store::Table,
            resolved: &[$crate::store::Index],
        ) {
            use $crate::schema_resolve::ColumnAction as CA;
            let actual = match list_columns(&table.name) {
                Ok(v) => v,
                // Store capability not granted, or the table went away between
                // the `list_tables` guard and here. Nothing to reconcile
                // against; the service fails properly on its first data op
                // rather than trapping during init. Same posture as
                // `__boogy_reconcile_indexes`.
                Err(_) => return,
            };
            let plan = $crate::schema_resolve::plan_column_reconcile(
                &table.columns,
                &actual,
                table.allow_dropped,
            );

            // Read BEFORE this table records anything of its own, or its own
            // pushes would be indistinguishable from an earlier table's.
            let __earlier_table_conflicted = __boogy_schema_conflicts_pending();

            // Pass one: report, apply nothing. Every conflict is recorded (not
            // just the first) so the deploy's 409 names the whole disagreement
            // rather than one column of it, and the warnings are logged here so
            // an aborted pass still says everything it found. This runs even
            // when an earlier table already conflicted: the deploy is refused
            // either way, and a developer fixing it should see every column
            // involved, not just the ones before the first failure.
            let mut __conflicted = false;
            for action in &plan {
                match action {
                    CA::Conflict { column, reason } => {
                        __conflicted = true;
                        __boogy_note_schema_conflict(::std::format!(
                            "{}.{}: {}", &table.name, column, reason));
                    }
                    // Logged, never recorded. A warning describes a schema the
                    // service RUNS on — most often a column a hand-written
                    // migration owns and the model never declared — so pushing
                    // it into the conflict buffer would fail a deploy that has
                    // nothing wrong with it.
                    CA::Warn { column, reason } => {
                        $crate::log::warn!("schema {}.{}: {}", &table.name, column, reason);
                    }
                    // Enumerated rather than `_`: a new action variant must be
                    // classified here as "applies something" or "reports
                    // something", not silently inherit either answer.
                    CA::Add(_)
                    | CA::SetDefault { .. }
                    | CA::Rename { .. }
                    | CA::SoftDrop(_)
                    | CA::Revive { .. } => {}
                }
            }
            if __conflicted || __earlier_table_conflicted {
                return;
            }

            // Pass two: apply. Reached only when the plan is entirely mutating.
            for action in &plan {
                let res: ::core::result::Result<(), ::std::string::String> = match &action {
                    CA::Add(c) => {
                        let r = $bindings::boogy::platform::store::add_column(
                            &table.name,
                            &__boogy_col_def_to_wit(c),
                        )
                        .map_err(::std::string::String::from);
                        if r.is_ok() {
                            __boogy_warn_if_indexed(&table.name, &c.name, resolved);
                        }
                        r
                    }
                    CA::SetDefault { column, value } => {
                        // `add-column` on a column whose name, type and
                        // nullability already match replaces the default in
                        // place — the documented path, and idempotent. The rest
                        // of the definition must therefore come from the STORED
                        // shape, not the declaration: any other difference is
                        // refused as a conflict, which is the behaviour we want
                        // for a real shape change and not for this one.
                        match actual.iter().find(|a| &a.name == column) {
                            Some(a) => {
                                let mut c = a.to_col_def();
                                c.default = ::core::option::Option::Some(value.clone());
                                $bindings::boogy::platform::store::add_column(
                                    &table.name,
                                    &__boogy_col_def_to_wit(&c),
                                )
                                .map_err(::std::string::String::from)
                            }
                            // `plan_column_reconcile` only emits `SetDefault`
                            // for a column it matched in `actual`, so this is
                            // unreachable. Reported rather than unwrapped: a
                            // schema pass must not trap the request.
                            ::core::option::Option::None => ::core::result::Result::Err(
                                ::std::string::String::from(
                                    "planned a default change for a column that is not in the store",
                                ),
                            ),
                        }
                    }
                    CA::Rename { from, to } => rename_column(&table.name, from, to),
                    CA::SoftDrop(n) => drop_column(&table.name, n),
                    CA::Revive { column, default } =>
                        revive_column(&table.name, column, default.as_ref()),
                    // Both were handled by the reporting pass above, and a
                    // conflict returned before reaching here. Left as explicit
                    // no-op arms rather than `unreachable!()`: a schema pass
                    // must not trap the request under any input.
                    CA::Conflict { .. } | CA::Warn { .. } => ::core::result::Result::Ok(()),
                };
                if let ::core::result::Result::Err(msg) = res {
                    if !__boogy_schema_action_is_benign(action, &msg) {
                        __boogy_note_schema_conflict(::std::format!(
                            "{}.{}: {}", &table.name, action.column_name(), msg));
                    }
                }
            }
        }

        /// Is this failure the target state already holding, rather than a
        /// conflict?
        ///
        /// The plan is computed from a `list_columns` snapshot, so on ONE host
        /// a converged schema plans nothing and nothing here can fire. The case
        /// this covers is two hosts resolving the same deployment at once: the
        /// other host applied the action between our read and our call, and the
        /// column is now in exactly the state we wanted.
        ///
        /// Per-action, not one blanket substring list, because the benign
        /// message differs by action and a blanket list gets it wrong in both
        /// directions — "column not found" is convergence for a drop and a real
        /// failure for a rename, and matching "already exists" on a soft drop
        /// would swallow nothing while leaving the drop's own race unhandled.
        ///
        /// "not granted" is benign for every action: the store capability is
        /// absent, which the first data op reports properly.
        fn __boogy_schema_action_is_benign(
            action: &$crate::schema_resolve::ColumnAction,
            msg: &str,
        ) -> bool {
            use $crate::schema_resolve::ColumnAction as CA;
            if msg.contains("not granted") {
                return true;
            }
            match action {
                // NOTHING. Read `add_column_core`: an identical concurrent
                // re-add returns Ok and replaces the default in place, so
                // convergence never reaches this function at all. The store
                // says "already exists" on exactly one input — a DIFFERENT
                // column wearing the same name (type, nullability or
                // accumulator differs) — the table refusing every write, all
                // over again.
                // Swallowing it here would hide the one error that matters
                // most, in the one arm where it matters.
                CA::Add(_) | CA::SetDefault { .. } => false,
                // Either the new name is taken (renamed already) or the old one
                // is gone (same thing, seen from the other side).
                CA::Rename { .. } => msg.contains("already exists") || msg.contains("not found"),
                // No LIVE column under that name: already dropped.
                CA::SoftDrop(_) => msg.contains("not found"),
                // "is not dropped" — already revived. Deliberately does NOT
                // match "is not a dropped column", which means the name is
                // absent entirely: that is a real conflict, not convergence.
                CA::Revive { .. } => msg.contains("not dropped"),
                // Never reached: both reporting arms return Ok.
                CA::Conflict { .. } | CA::Warn { .. } => false,
            }
        }

        /// Say so when a newly added column is one an index covers.
        ///
        /// The pass adds columns; it never backfills VALUES. A row written
        /// before the column existed has no value for it, so what a seek on
        /// that column finds is not what the declaration implies:
        ///
        /// - an index created in the same pass backfills, and indexes every
        ///   pre-existing row under the column's DEFAULT;
        /// - an index that already existed has no entry for those rows at all.
        ///
        /// Either way the rows that predate this deployment are not findable by
        /// any real value on this column, and nothing in the declaration says
        /// so. Backfill is out of scope; saying so is the minimum this pass
        /// owes the developer.
        fn __boogy_warn_if_indexed(
            table: &str,
            column: &str,
            resolved: &[$crate::store::Index],
        ) {
            if resolved.iter().any(|i| i.columns.iter().any(|n| n == column)) {
                $crate::log::warn!(
                    "schema {table}: column `{column}` was added to a table that may \
                     already hold rows, and it is covered by an index. NOTHING IS \
                     BACKFILLED: rows written before this deployment have no value \
                     for `{column}`, so they are indexed under its default (if the \
                     index is built now) or carry no entry for it at all (if the \
                     index already existed). A seek on `{column}` will not find them \
                     by any real value until they are rewritten."
                );
            }
        }

        /// Rollups recorded by each `create_table_from`, applied after the
        /// index reconcile for the same reason: the complete declared set has
        /// to be known before anything is dropped.
        #[allow(clippy::type_complexity)]
        thread_local! {
            static __BOOGY_DECLARED_ROLLUPS: ::std::cell::RefCell<
                ::std::vec::Vec<(
                    ::std::string::String,
                    ::std::vec::Vec<(
                        ::std::string::String,
                        ::std::vec::Vec<::std::string::String>,
                        ::std::vec::Vec<::std::string::String>,
                        bool,
                    )>,
                )>
            > = ::std::cell::RefCell::new(::std::vec::Vec::new());
        }

        /// Converge each table's declared rollups on what its models declare.
        ///
        /// **Idempotence is the property that matters here, far more than for
        /// indexes.** This runs on EVERY request, and creating a rollup walks
        /// the whole table to count the rows already there. A comparison that
        /// wrongly reported a declared rollup as missing would re-backfill the
        /// table on every request — so the match is on the full shape (group,
        /// summed columns, count), and `rollups_are_declared_once_not_per_request`
        /// pins it.
        fn __boogy_reconcile_rollups() {
            // Drained first, unconditionally: the declarations are refilled by
            // `Api::schema()` on every request, so leaving them would grow the
            // thread-local without bound.
            let declared =
                __BOOGY_DECLARED_ROLLUPS.with(|d| ::std::mem::take(&mut *d.borrow_mut()));
            for (table, desired) in &declared {
                let actual = match list_rollups(table) {
                    Ok(v) => v,
                    // Store capability not granted, or the table is gone.
                    // Nothing to reconcile against; the service fails properly
                    // on its first data op rather than trapping during init.
                    Err(_) => continue,
                };
                for (name, group, sum, count) in desired {
                    let matches = actual.iter().any(|r| {
                        r.name == *name
                            && r.group == *group
                            && r.sum == *sum
                            && r.count == *count
                    });
                    if matches {
                        continue;
                    }
                    // A rollup under this name whose SHAPE changed is dropped
                    // first, or the create would be refused as already
                    // existing and the declaration would silently never take
                    // effect.
                    if actual.iter().any(|r| r.name == *name) {
                        let _ = drop_rollup(table, name);
                    }
                    if let Err(e) = create_rollup(
                        table,
                        &$crate::store::RollupInfo {
                            name: name.clone(),
                            group: group.clone(),
                            sum: sum.clone(),
                            count: *count,
                        },
                    ) {
                        // Declaring is a schema step, and the schema phase must
                        // not trap the request: a refusal here (a nullable
                        // summed column, an unbounded group key) is an
                        // authoring error the developer needs to SEE, and a
                        // trap inside wasm reaches them as an opaque 500.
                        $crate::log::error!(
                            "schema {table}: rollup {name} was refused: {}. \
                             Aggregates over this grouping will be computed \
                             from the rows instead.",
                            e
                        );
                    }
                }
            }
        }

        /// Table + resolved index set recorded by each `create_table_from` call
        /// during `Api::init_tables()`.
        ///
        /// Declaration and application are separated by the init pass because
        /// the reconcile needs the COMPLETE declared set before it can decide
        /// anything. This is also what keeps runtime-named table families safe:
        /// `create_model_as` records like any other declaration, so a service
        /// that builds one table per time window has those tables in the
        /// declared set rather than looking like a pile of orphans.
        thread_local! {
            static __BOOGY_DECLARED: ::std::cell::RefCell<
                ::std::vec::Vec<(::std::string::String, ::std::vec::Vec<$crate::store::Index>)>
            > = ::std::cell::RefCell::new(::std::vec::Vec::new());
        }


        /// Converge every declared table's index set on what its models declare.
        ///
        /// Runs after `Api::init_tables()` returns, on every request — which is
        /// affordable because the reads it needs are FREE: `list_indexes` is
        /// schema introspection and carries only a capability check, not the
        /// `charge_read!` that meters `find` against the tenant's op-rate
        /// budget. A converged pass therefore costs zero metered ops, and only
        /// an actual change spends anything.
        ///
        /// An earlier version cached a fingerprint of the declared set in a
        /// table to avoid re-reading. That was solving a cost that does not
        /// exist: the cache's own `find` was metered, so it made every request
        /// strictly more expensive than the introspection it was avoiding, and
        /// exhausted a small burst budget before the handler could run.
        fn __boogy_reconcile_indexes() {
            let declared = __BOOGY_DECLARED.with(|d| ::std::mem::take(&mut *d.borrow_mut()));
            for (table, desired) in &declared {
                let actual: ::std::vec::Vec<$crate::store::Index> = match list_indexes(table) {
                    Ok(v) => v.into_iter().map(|i| {
                        // Destructured exhaustively on purpose. Adding a field to the WIT
// record then fails to compile HERE instead of being silently
// dropped — which is how `covering` went missing from IndexInfo.
// Never add `..` to this pattern.
                        let $crate::store::IndexInfo { name, columns, unique, covering } = i;
                        $crate::store::Index { name, columns, unique, covering }
                    }).collect(),
                    // Store capability not granted, or the table is gone. Nothing
                    // to reconcile against; the service fails properly on its
                    // first data op rather than trapping during init.
                    Err(_) => continue,
                };
                for action in $crate::schema_resolve::plan_reconcile(desired, &actual) {
                    __boogy_apply_index_action(table, &action);
                }
            }
        }

        /// Apply one planned change. Returns whether it landed.
        ///
        /// Drop precedes create for a rebuild, so the table never carries two
        /// copies of one index at once.
        fn __boogy_apply_index_action(
            table: &str,
            action: &$crate::schema_resolve::IndexAction,
        ) -> bool {
            use $crate::schema_resolve::ActionKind;
            let name = action.index.name.clone();
            if matches!(action.kind, ActionKind::Drop | ActionKind::Rebuild) {
                if let Err(e) = $bindings::boogy::platform::store::drop_index(table, &name) {
                    let msg = ::std::format!("{e:?}");
                    if !msg.contains("not granted") && !msg.contains("not found") {
                        $crate::log::warn!("schema {table}: drop index '{name}' failed: {msg}");
                        // A drop we could not perform must NOT be followed by a
                        // create under the same name: that fails as "already
                        // exists", which the create arm treats as success, and the
                        // stale definition would survive while the pass reported
                        // convergence.
                        return false;
                    }
                } else {
                    $crate::log::info!("schema {table}: dropped index '{name}'");
                }
            }
            if matches!(action.kind, ActionKind::Create | ActionKind::Rebuild) {
                match $bindings::boogy::platform::store::create_index(
                    table,
                    &$bindings::boogy::platform::store::IndexDef {
                        name: name.clone(),
                        columns: action.index.columns.clone(),
                        unique: action.index.unique,
                        covering: action.index.covering,
                    },
                ) {
                    Ok(()) => $crate::log::info!("schema {table}: created index '{name}'"),
                    Err(e) => {
                        let msg = ::std::format!("{e:?}");
                        // "not granted" → capability denied (soft-skip, as above).
                        // "already exists" → a concurrent deploy created it between
                        // our list_indexes and now; idempotent success.
                        if !msg.contains("not granted") && !msg.contains("already exists") {
                            $crate::log::warn!("schema {table}: create index '{name}' failed: {msg}");
                            return false;
                        }
                    }
                }
            }
            true
        }

        // -- Column migration free fns (map ColumnSpec ↔ ColumnDef / ColumnInfo) --

        /// Add a column to an existing table. Maps the SDK [`ColumnSpec`]
        /// to the WIT `column-def` (same `ColType→ColumnType` match as
        /// `create_table_from`; `default` via `__boogy_val_to_wit`).
        ///
        /// The host enforces the operation strictly — call from a migration
        /// body, not from `init_tables` (which may re-run on a table that
        /// already has the column). For idempotent use, prefer
        /// `MigrationCtx::add_column`, which guards with `list_columns` first.
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
                // A migration-added column is never a counter: converting an
                // existing column to one is the backfill path, which has to move
                // the value out of already-written rows.
                counter: false,
                // …and therefore has no accumulator op to choose.
                counter_max: false,
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

        /// Restore a soft-dropped column: the undo for `drop_column`.
        ///
        /// Needed as its own primitive because `add_column` refuses a name that
        /// is already present — including a tombstoned one — so a re-declared
        /// column could not otherwise come back. Fails when the name is live
        /// or absent; only a dropped column can be revived.
        ///
        /// `default`, when given, is installed in the same write that clears
        /// the tombstone. A soft drop does not stop the table taking writes, so
        /// rows written while the column was gone hold no value for it — a
        /// required column revived without a default would leave those rows
        /// with nothing to resolve against, which is precisely the state
        /// `add_column`'s synthesised default exists to prevent.
        fn revive_column(
            table: &str,
            name: &str,
            default: ::core::option::Option<&$crate::store::Val>,
        ) -> ::core::result::Result<(), ::std::string::String> {
            $bindings::boogy::platform::store::revive_column(
                table,
                name,
                default.map(|v| __boogy_val_to_wit(v)).as_ref(),
            )
            .map_err(::std::string::String::from)
        }

        /// One WIT `column-info` as the SDK [`ColumnInfo`].
        ///
        /// Destructured exhaustively on purpose, for the reason spelled out on
        /// `list_indexes`: a new WIT field then fails to compile HERE rather
        /// than being silently dropped from the comparison the column
        /// reconcile makes.
        fn __boogy_to_sdk_column_info(
            ci: $bindings::boogy::platform::store::ColumnInfo,
        ) -> $crate::store::ColumnInfo {
            let $bindings::boogy::platform::store::ColumnInfo {
                name, col_type, nullable, unique, counter, counter_max, dropped,
                dropped_at, has_references, default,
            } = ci;
            $crate::store::ColumnInfo {
                name,
                col_type: match col_type {
                    $bindings::boogy::platform::store::ColumnType::Text    => $crate::store::ColType::Text,
                    $bindings::boogy::platform::store::ColumnType::Integer => $crate::store::ColType::Integer,
                    $bindings::boogy::platform::store::ColumnType::Real    => $crate::store::ColType::Real,
                    $bindings::boogy::platform::store::ColumnType::Blob    => $crate::store::ColType::Blob,
                    $bindings::boogy::platform::store::ColumnType::Boolean => $crate::store::ColType::Boolean,
                },
                nullable,
                unique,
                counter,
                counter_max,
                dropped,
                dropped_at,
                has_references,
                default: default.as_ref().map(__boogy_wit_to_val),
            }
        }

        /// List the current columns of a table, returning [`ColumnInfo`]
        /// for each. Useful for idempotency guards in migrations — check
        /// whether a column already exists before calling `add_column`.
        ///
        /// Reports DROPPED columns too, flagged. A reconcile has to see a
        /// tombstone to tell "never declared" from "deliberately removed";
        /// callers doing a presence check want `!c.dropped` (see
        /// `MigrationCtx::add_column`).
        fn list_columns(
            table: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::ColumnInfo>, ::std::string::String> {
            let wit_cols = $bindings::boogy::platform::store::list_columns(table)?;
            Ok(wit_cols.into_iter().map(__boogy_to_sdk_column_info).collect())
        }

        /// List the current indexes on a table, returning [`IndexInfo`] for
        /// each. Useful for idempotency guards in migrations — check whether
        /// an index already exists before calling `create_index`.
        fn list_indexes(
            table: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::IndexInfo>, ::std::string::String> {
            let wit_idxs = $bindings::boogy::platform::store::list_indexes(table)?;
            Ok(wit_idxs.into_iter().map(|i| {
                // Destructured exhaustively on purpose. Adding a field to the WIT
// record then fails to compile HERE instead of being silently
// dropped — which is how `covering` went missing from IndexInfo.
// Never add `..` to this pattern.
                let $bindings::boogy::platform::store::IndexDef { name, columns, unique, covering } = i;
                $crate::store::IndexInfo { name, columns, unique, covering }
            }).collect())
        }

        /// The maintained rollups declared on a table.
        ///
        /// Useful as an idempotency guard in a migration: check before calling
        /// `create_rollup`, which refuses a name that already exists.
        fn list_rollups(
            table: &str,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::RollupInfo>, ::std::string::String> {
            let wit = $bindings::boogy::platform::store::list_rollups(table)
                .map_err(::std::string::String::from)?;
            Ok(wit.into_iter().map(|r| {
                // Destructured exhaustively on purpose, for the reason spelled
                // out on `list_indexes`: a new WIT field then fails to compile
                // HERE rather than being silently dropped from the comparison
                // the reconcile makes.
                let $bindings::boogy::platform::store::RollupDef { name, group, sum, count } = r;
                $crate::store::RollupInfo { name, group, sum, count }
            }).collect())
        }

        /// Declare a maintained rollup. Prefer `#[model(rollup(...))]`, which
        /// declares it with the table and keeps it in the reconcile.
        fn create_rollup(
            table: &str,
            rollup: &$crate::store::RollupInfo,
        ) -> ::core::result::Result<(), ::std::string::String> {
            $bindings::boogy::platform::store::create_rollup(
                table,
                &$bindings::boogy::platform::store::RollupDef {
                    name: rollup.name.clone(),
                    group: rollup.group.clone(),
                    sum: rollup.sum.clone(),
                    count: rollup.count,
                },
            )
            .map_err(::std::string::String::from)
        }

        /// Remove a maintained rollup and the totals it holds.
        fn drop_rollup(table: &str, name: &str) -> ::core::result::Result<(), ::std::string::String> {
            $bindings::boogy::platform::store::drop_rollup(table, name)
                .map_err(::std::string::String::from)
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
            Ok(wit_tables.into_iter().map(|t| {
                // Destructured exhaustively on purpose. Adding a field to the WIT
// record then fails to compile HERE instead of being silently
// dropped — which is how `covering` went missing from IndexInfo.
// Never add `..` to this pattern.
                let $bindings::boogy::platform::store::TableInfo { name, column_count, index_count } = t;
                $crate::store::TableInfo { name, column_count, index_count }
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

        /// The store handle the [`Counter`](boogy_sdk::model::Counter) and
        /// [`MaxAccum`](boogy_sdk::model::MaxAccum) verbs take.
        ///
        /// **Why this exists.** Those traits are generic over a `CounterStore`
        /// so `boogy-sdk` can stay free of any dependency on generated
        /// bindings. Nothing supplied one, so `Counter::get(store, key)` — the
        /// form the derive's docs, the trait docs and the guides all teach —
        /// was unreachable from a deployed service, and the only way to touch a
        /// counter was the raw binding call. Closes guarantee-audit §1az.
        ///
        /// Zero-sized: `BOOGY_COUNTERS` is a const, so `Room::HITS`-style calls
        /// cost nothing at runtime.
        #[derive(Clone, Copy, Debug, Default)]
        pub struct GuestCounterStore;

        /// The handle to pass to a counter verb: `RoomHits::add(&BOOGY_COUNTERS, id, 1)?`.
        pub const BOOGY_COUNTERS: GuestCounterStore = GuestCounterStore;

        impl $crate::model::CounterStore for GuestCounterStore {
            fn counter_add(
                &self,
                name: &str,
                key: &[$crate::store::Val],
                delta: i64,
            ) -> ::core::result::Result<(), $crate::store::StoreError> {
                let k: ::std::vec::Vec<_> = key.iter().map(__boogy_val_to_wit).collect();
                $bindings::boogy::platform::store::counter_add(name, &k, delta)
                    .map_err($crate::store::StoreError::from_wit)
            }

            fn counter_get(
                &self,
                name: &str,
                key: &[$crate::store::Val],
                snapshot: bool,
            ) -> ::core::result::Result<i64, $crate::store::StoreError> {
                let k: ::std::vec::Vec<_> = key.iter().map(__boogy_val_to_wit).collect();
                $bindings::boogy::platform::store::counter_get(name, &k, snapshot)
                    .map_err($crate::store::StoreError::from_wit)
            }

            fn max_observe(
                &self,
                name: &str,
                key: &[$crate::store::Val],
                value: i64,
            ) -> ::core::result::Result<(), $crate::store::StoreError> {
                let k: ::std::vec::Vec<_> = key.iter().map(__boogy_val_to_wit).collect();
                $bindings::boogy::platform::store::max_observe(name, &k, value)
                    .map_err($crate::store::StoreError::from_wit)
            }

            fn max_get(
                &self,
                name: &str,
                key: &[$crate::store::Val],
                snapshot: bool,
            ) -> ::core::result::Result<::core::option::Option<i64>, $crate::store::StoreError>
            {
                let k: ::std::vec::Vec<_> = key.iter().map(__boogy_val_to_wit).collect();
                $bindings::boogy::platform::store::max_get(name, &k, snapshot)
                    .map_err($crate::store::StoreError::from_wit)
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
        /// `queries[i]`.
        ///
        /// Takes `Query<Bounded>` — every query in the batch must have stated a
        /// `.limit(n)`. Batching is not a bound: an unbounded query in a batch
        /// sends `page: None` exactly as it would alone, and the host then
        /// substitutes a ceiling of its own and returns no cursor with which to
        /// tell a truncated answer from a complete one. N of those in one round
        /// trip is N times the same defect, not a cheaper version of it. Totals are discarded (rows only), matching `fetch_all`.
        /// Prefer this over a loop of `.fetch_all()` when reading independent
        /// sets across tables — the host pipelines them instead of paying one
        /// round-trip chain each.
        #[allow(dead_code)]
        fn find_many(
            queries: ::std::vec::Vec<Query<$crate::query::Bounded>>,
        ) -> ::core::result::Result<
            ::std::vec::Vec<::std::vec::Vec<$crate::store::Row>>,
            $crate::store::StoreError,
        > {
            let mut wit_queries: ::std::vec::Vec<$bindings::boogy::platform::store::FindQuery> =
                ::std::vec::Vec::with_capacity(queries.len());
            for q in &queries {
                // Same enforcement `to_wit_args` gives every other terminal —
                // a `.with_counter(..)` on one of the batched queries that
                // names an untouched key column is refused here, before any
                // of the batch dispatches.
                let (filters, or_groups, sort, page, counters) = q.to_wit_args()
                    .map_err(|e| $crate::store::StoreError::InvalidArgument(e.to_string()))?;
                wit_queries.push($bindings::boogy::platform::store::FindQuery {
                    table: q.0.table.clone(),
                    options: $bindings::boogy::platform::store::FindOptions {
                        filters,
                        order_by: sort,
                        page,
                        or_groups,
                        // Rows-only helper → discard totals (count-elision
                        // fast path on the index walk).
                        skip_total: SDK_SKIP_TOTAL,
                        group_cursor: ::core::option::Option::None,
                        counters,
                    },
                });
            }
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

        /// What the SDK puts in the WIT `find`'s `skip-total` for a read that
        /// DISCARDS the count — which is nearly every read it issues.
        ///
        /// `true`: the cheap path is the default one. An exact total cannot be
        /// produced without visiting every matching row, so a request that
        /// carries a page AND asks for a total is bounded in ROWS and unbounded
        /// in WORK. Measured on a 3,000-row table, the exact-total page
        /// examined **61x** the index keys the skip-total page did for the
        /// identical 50 rows. A caller that never looks at the count paid all
        /// of that.
        ///
        /// That 61x is one PATH and one PLAN — an autocommit read on a plan
        /// whose walk can stop at the page's edge — and is not what every read
        /// saves. On a plan that must materialise the whole matching set before
        /// it can order or filter it (a fan-out of `IN` seeks, an equality seek
        /// on a composite's leading column with no sort, a scan) nothing stops
        /// early and the count comes free with the drain, so declining saves
        /// that read nothing at all. Both are the same default, for different
        /// reasons: one name with countable exceptions beats a bare literal per
        /// call site.
        ///
        /// `has-more` is unaffected — it is the page's own edge, the store
        /// states it under either value, and it is what the one-page refusals
        /// test. So declining the total costs a REFUSAL nothing; it costs the
        /// refusal's MESSAGE an exact row count, which
        /// `refuse_beyond_one_page` already has a branch for.
        const SDK_SKIP_TOTAL: bool = true;

        /// The opposite, spelled at every call site whose own contract is to
        /// hand a total back: `find_all_rows`, `find_rows`,
        /// `fetch_all_with_total`, `MigrationOps::find_rows`.
        ///
        /// These four end in `required_total`, which refuses to fold an absent
        /// count into `0` — so a verb that stopped asking would fail as a
        /// platform fault rather than quietly report an empty table. That is
        /// the safe direction, and it is why the flip above is expressible at
        /// all; naming the exception here is what keeps it from being read as
        /// an oversight and "tidied" into the default.
        const SDK_WANT_TOTAL: bool = false;

        fn find_all_rows(
            table: &str,
        ) -> ::core::result::Result<(::std::vec::Vec<$crate::store::Row>, u64), $crate::store::StoreError> {
            let res = $bindings::boogy::platform::store::find(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters: vec![],
                    order_by: vec![],
                    page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset: 0 }),
                    or_groups: vec![],
                    skip_total: SDK_WANT_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
                },
            )
            .map_err($crate::store::StoreError::from_wit)?;
            let rows: ::std::vec::Vec<$crate::store::Row> =
                res.rows.iter().map(|r| to_sdk_row(r)).collect();
            // ONE page, then refuse. This helper has no filter, so there is no
            // `list_by` composite that could give it a stable paged order —
            // a whole-table read past one page cannot be made safe here.
            $crate::store::refuse_beyond_one_page(
                "find_all_rows",
                rows.len(),
                res.total_count,
                res.has_more,
                "Stream it instead: for_each_batch(table, .., None, ..) walks in primary-key \
                 order with a cursor and bounded memory.",
            )?;
            let total = $crate::store::required_total("find_all_rows", res.total_count)?;
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

        /// The `Model` rows where `col == val` — **at most one page of them.**
        ///
        /// For the two shapes where one page IS the answer: a `#[lookup_by]`
        /// column (unique, so at most one row) and a small set the handler wants
        /// whole. If more rows match than one page holds, this returns a named
        /// error carrying the remedy — it never returns a prefix that looks
        /// complete.
        ///
        /// For a set that grows with the tenant, use
        /// [`db_find_by_page`] instead: same filter, plus the cursor that
        /// continues the listing.
        fn db_find_by<M: $crate::model::Model>(
            col: &str,
            val: $crate::store::Val,
        ) -> ::core::result::Result<::std::vec::Vec<M>, $crate::store::StoreError> {
            let wit_val = __boogy_val_to_wit(&val);
            // Page along the model's DECLARED order for this filter. That
            // declaration is what guarantees one index covers the filter AND
            // the order, so each page is a bounded ordered range rather than a
            // drain-and-sort. Without it there is no stable sequence and the
            // old offset walk could return a row twice or skip it — so refuse
            // instead of guessing an order no index serves.
            let schema = M::schema();
            // Every arm RETURNS: this verb reads at most one page in all three
            // strategies, so the match is the whole body rather than a
            // preamble that picks an order for a loop below it.
            match $crate::store::read_strategy(&schema, col) {
                // `#[lookup_by]`: unique, so at most one row — nothing to page.
                // One bounded read, and the guard is a belt-and-braces check
                // that the uniqueness the model claims actually held.
                $crate::store::ReadStrategy::PointLookup => {
                    let res = $bindings::boogy::platform::store::find(
                        M::TABLE,
                        &$bindings::boogy::platform::store::FindOptions {
                            filters: ::std::vec![$bindings::boogy::platform::store::Filter {
                                column: col.to_string(),
                                op: $bindings::boogy::platform::store::FilterOp::Eq,
                                val: wit_val.clone(),
                                in_values: None,
                            }],
                            order_by: ::std::vec![],
                            page: Some($bindings::boogy::platform::store::Page {
                                limit: SDK_FIND_BATCH,
                                offset: 0,
                            }),
                            or_groups: ::std::vec![],
                            skip_total: SDK_SKIP_TOTAL,
                            group_cursor: ::core::option::Option::None,
                            counters: ::std::vec::Vec::new(),
                        },
                    )
                    .map_err($crate::store::StoreError::from_wit)?;
                    let out: ::std::vec::Vec<M> =
                        res.rows.iter().map(|r| M::from_row(&to_sdk_row(r))).collect();
                    $crate::store::refuse_beyond_one_page(
                        "db_find_by",
                        out.len(),
                        res.total_count,
                        res.has_more,
                        "This column is declared unique (#[lookup_by]) but matched more rows \
                         than one page — the uniqueness does not hold.",
                    )?;
                    return Ok(out);
                }
                // No covering order: one page is still correct, so serve it and
                // stop there. Only continuing past a page needs the order this
                // model never declared.
                $crate::store::ReadStrategy::SinglePageOnly => {
                    let res = $bindings::boogy::platform::store::find(
                        M::TABLE,
                        &$bindings::boogy::platform::store::FindOptions {
                            filters: ::std::vec![$bindings::boogy::platform::store::Filter {
                                column: col.to_string(),
                                op: $bindings::boogy::platform::store::FilterOp::Eq,
                                val: wit_val.clone(),
                                in_values: None,
                            }],
                            order_by: ::std::vec![],
                            page: Some($bindings::boogy::platform::store::Page {
                                limit: SDK_FIND_BATCH,
                                offset: 0,
                            }),
                            or_groups: ::std::vec![],
                            skip_total: SDK_SKIP_TOTAL,
                            group_cursor: ::core::option::Option::None,
                            counters: ::std::vec::Vec::new(),
                        },
                    )
                    .map_err($crate::store::StoreError::from_wit)?;
                    let out: ::std::vec::Vec<M> =
                        res.rows.iter().map(|r| M::from_row(&to_sdk_row(r))).collect();
                    $crate::store::refuse_beyond_one_page(
                        "db_find_by",
                        out.len(),
                        res.total_count,
                        res.has_more,
                        &::std::format!(
                            "Declare how {}.{} is listed so it can page safely: add \
                             list_by(filter = \"{}\", newest = \"<a timestamp or sequence \
                             column>\") to the model, or an index over [\"{}\", \"<sort \
                             col>\"]. One index must cover the filter AND the order.",
                            M::TABLE, col, col, col,
                        ),
                    )?;
                    return Ok(out);
                }
                // A non-unique column WITH a declared order. The index covers
                // filter AND order, so this listing CAN be paged safely — which
                // is exactly why it must not be drained here.
                //
                // It used to loop keyset pages until an empty one and
                // concatenate every page into this `Vec`. That is the same
                // shape `find_owned` had when it trapped a guest on
                // `handle_alloc_error` at ~2k rows: safe paging is not a bound,
                // it is what makes a bound possible. The loop is gone; the
                // pageable form is `db_find_by_page::<M>(col, val, &page)`,
                // which hands back the cursor instead of the whole table.
                $crate::store::ReadStrategy::Keyset(order) => {
                    let res = $bindings::boogy::platform::store::find(
                        M::TABLE,
                        &$bindings::boogy::platform::store::FindOptions {
                            filters: ::std::vec![$bindings::boogy::platform::store::Filter {
                                column: col.to_string(),
                                op: $bindings::boogy::platform::store::FilterOp::Eq,
                                val: wit_val.clone(),
                                in_values: None,
                            }],
                            order_by: ::std::vec![
                                $bindings::boogy::platform::store::OrderTerm::Column(
                                    $bindings::boogy::platform::store::SortBy {
                                        column: order.column.clone(),
                                        dir: __boogy_sdk_dir_to_wit(if order.desc {
                                            $crate::store::SortDir::Desc
                                        } else {
                                            $crate::store::SortDir::Asc
                                        }),
                                    },
                                ),
                            ],
                            page: Some($bindings::boogy::platform::store::Page {
                                limit: SDK_FIND_BATCH,
                                offset: 0,
                            }),
                            or_groups: ::std::vec![],
                            skip_total: SDK_SKIP_TOTAL,
                            group_cursor: ::core::option::Option::None,
                            counters: ::std::vec::Vec::new(),
                        },
                    )
                    .map_err($crate::store::StoreError::from_wit)?;
                    let out: ::std::vec::Vec<M> =
                        res.rows.iter().map(|r| M::from_row(&to_sdk_row(r))).collect();
                    $crate::store::refuse_beyond_one_page(
                        "db_find_by",
                        out.len(),
                        res.total_count,
                        res.has_more,
                        &::std::format!(
                            "This listing CAN be paged — {}.{} declares an order over {} — so \
                             page it instead of draining it: db_find_by_page::<{}>(\"{}\", val, \
                             &PageRequest::new(limit, token)) returns one page plus the cursor \
                             that continues it.",
                            M::TABLE, col, order.column, M::TABLE, col,
                        ),
                    )?;
                    return Ok(out);
                }
            }
        }

        /// One BOUNDED page of the `M` rows where `col == val`, plus the cursor
        /// the next page resumes from.
        ///
        /// The pageable sibling of [`db_find_by`], and the reason that verb no
        /// longer loops. `db_find_by` answers "this set is small and I want all
        /// of it, and if I am wrong say so"; this one answers "this set grows
        /// with the tenant and I will walk it".
        ///
        /// Needs the same DECLARED order `db_find_by` needs to page: one index
        /// covering the filter AND the sort, from `list_by(filter = col, …)` or
        /// an `index(cols = [col, sort_col])`. Without it there is no stable
        /// sequence across pages, so this errors and names the declaration to
        /// add rather than paging by an order no index serves — which would
        /// silently duplicate or skip rows under concurrent writes.
        ///
        /// A `#[lookup_by]` column is unique: there is at most one row and
        /// nothing to page, so use [`db_find_by`] for that shape.
        #[allow(dead_code)]
        fn db_find_by_page<M: $crate::model::Model>(
            col: &str,
            val: $crate::store::Val,
            page: &$crate::pagination::PageRequest,
        ) -> ::core::result::Result<$crate::pagination::ModelPage<M>, $crate::store::StoreError> {
            let wit_val = __boogy_val_to_wit(&val);
            // A token we cannot read is REFUSED, never silently dropped: dropping
            // it restarts the listing while the caller believes it is continuing
            // one, so the walk re-serves page one forever. Same rule
            // `find_owned` enforces, for the same reason.
            if page.has_unreadable_token() {
                return Err($crate::store::StoreError::InvalidArgument(
                    "cursor is not a listing position this service issued; omit it to start the \
                     listing from the beginning"
                        .to_string(),
                ));
            }
            let schema = M::schema();
            let order = match $crate::store::read_strategy(&schema, col) {
                $crate::store::ReadStrategy::Keyset(o) => o,
                $crate::store::ReadStrategy::PointLookup => {
                    return Err($crate::store::StoreError::InvalidArgument(::std::format!(
                        "{}.{} is declared unique (#[lookup_by]), so it matches at most one row \
                         and there is nothing to page — use db_find_by::<{}>(..) instead.",
                        M::TABLE, col, M::TABLE,
                    )));
                }
                $crate::store::ReadStrategy::SinglePageOnly => {
                    return Err($crate::store::StoreError::InvalidArgument(::std::format!(
                        "{}.{} has no declared order, so this listing has no stable sequence to \
                         resume from and cannot be paged. Declare how it is listed: add \
                         list_by(filter = \"{}\", newest = \"<a timestamp or sequence column>\") \
                         to the model, or an index over [\"{}\", \"<sort col>\"]. One index must \
                         cover the filter AND the order.",
                        M::TABLE, col, col, col,
                    )));
                }
            };
            let dir = if order.desc {
                $crate::store::SortDir::Desc
            } else {
                $crate::store::SortDir::Asc
            };
            // Resume strictly after the last row on (sort value, _id) — the
            // composite boundary, so ties on the sort column cannot repeat or
            // drop a row.
            let (extra, kset_or) =
                $crate::pagination::keyset_resume_filter(page.cursor(), &order.column, dir.clone());
            let mut filters = ::std::vec![$bindings::boogy::platform::store::Filter {
                column: col.to_string(),
                op: $bindings::boogy::platform::store::FilterOp::Eq,
                val: wit_val.clone(),
                in_values: None,
            }];
            filters.extend(extra.iter().map(__boogy_sdk_filter_to_wit));
            let or_groups: ::std::vec::Vec<::std::vec::Vec<_>> = kset_or
                .iter()
                .map(|g| g.iter().map(__boogy_sdk_filter_to_wit).collect())
                .collect();
            let wit_dir = __boogy_sdk_dir_to_wit(dir);
            let res = $bindings::boogy::platform::store::find(
                M::TABLE,
                &$bindings::boogy::platform::store::FindOptions {
                    filters,
                    order_by: ::std::vec![
                        $bindings::boogy::platform::store::OrderTerm::Column(
                            $bindings::boogy::platform::store::SortBy {
                                column: order.column.clone(),
                                dir: wit_dir.clone(),
                            },
                        ),
                        $bindings::boogy::platform::store::OrderTerm::Column(
                            $bindings::boogy::platform::store::SortBy {
                                column: "_id".to_string(),
                                dir: wit_dir,
                            },
                        ),
                    ],
                    page: Some($bindings::boogy::platform::store::Page {
                        limit: page.limit() as u32,
                        offset: 0,
                    }),
                    or_groups,
                    // Keyset never needs the count: the page itself says whether
                    // another follows, and asking for the total would walk the
                    // whole matching set to answer a question the cursor already
                    // answers. This is the difference between a verb bounded in
                    // ROWS and one bounded in WORK.
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
                },
            )
            .map_err($crate::store::StoreError::from_wit)?;

            let rows: ::std::vec::Vec<$crate::store::Row> =
                res.rows.iter().map(|r| to_sdk_row(r)).collect();
            // A SHORT page is not the end, and neither is a full one: the host
            // clamps a page to BOOGY_STORE_MAX_PAGE_ROWS, which can sit below
            // the requested limit, so "fewer than asked for" and "exactly as
            // many as asked for" are both produced by the ceiling as well as by
            // the data. Nothing about the page's SIZE answers the question.
            //
            // `has_more` is the host stating it. The host applies the ceiling,
            // so it is the only party that can — and a cursor is emitted only
            // when it says more follows, which is what makes the last page
            // carrying rows also the last request.
            let next_cursor = match rows.last() {
                ::core::option::Option::Some(last) if res.has_more => {
                    let next = $crate::pagination::Cursor {
                        last_id: last.id().to_string(),
                        last_value: last.get(&order.column).to_json(),
                    };
                    // A page that ends where the previous one did would re-serve
                    // itself forever; refuse loudly rather than spin or truncate.
                    $crate::pagination::keyset_advanced(
                        M::TABLE, &order.column, page.cursor(), &next,
                    )
                    .map_err($crate::store::StoreError::Internal)?;
                    ::core::option::Option::Some($crate::pagination::encode(&next))
                }
                _ => ::core::option::Option::None,
            };
            ::core::result::Result::Ok($crate::pagination::ModelPage {
                items: rows.iter().map(|r| M::from_row(r)).collect(),
                next_cursor,
            })
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

        /// The two column sets an upsert writes.
        ///
        /// `always` is written on both arms; `on_insert` only by the call that
        /// creates the row. Prefer `on_insert` for a creation stamp: an
        /// `always` column is rewritten on every later call, which rewrites the
        /// whole row and makes concurrent upserts of one row contend.
        pub struct UpsertColumns<'a> {
            pub always: &'a [$bindings::boogy::platform::store::Column],
            pub on_insert: &'a [$bindings::boogy::platform::store::Column],
        }

        impl<'a> UpsertColumns<'a> {
            /// Neither arm writes anything but the key and the counter.
            pub fn none() -> Self {
                Self { always: &[], on_insert: &[] }
            }
            /// Written on both arms.
            pub fn always(v: &'a [$bindings::boogy::platform::store::Column]) -> Self {
                Self { always: v, on_insert: &[] }
            }
            /// Written only by the arm that creates the row.
            pub fn on_insert_only(v: &'a [$bindings::boogy::platform::store::Column]) -> Self {
                Self { always: &[], on_insert: v }
            }
        }

        /// Keyed counter: `counter += delta`, upserting the row identified by
        /// the composite `key`. First call inserts (`counter = delta` + the
        /// `always` + `on_insert` columns); later calls increment the counter
        /// and overwrite only `always`. `delta` must be an integer or real
        /// value (the host rejects others). Requires a `unique` index on the
        /// `key` columns. Returns the row id.
        ///
        /// **Whether concurrent increments compose depends on the column.** When
        /// `counter` names a counter column the store performs a native
        /// atomic add on that column's own cell, taking no read-conflict range,
        /// and any number of concurrent increments commit. Any other column is
        /// read-modify-written: the row is read, the sum computed, and the whole
        /// row written back, so concurrent increments conflict — absorbed by the
        /// retry loop on the autocommit path, surfaced as contention inside a
        /// transaction.
        ///
        /// A non-empty `always` is written through that same row update either
        /// way, so passing one reintroduces the conflict even for a counter
        /// column. `on_insert` avoids that — it is written only by the arm that
        /// creates the row — but it does not buy conflict-freedom on the
        /// read-modify-write arm: there, the counter itself is written through
        /// the ordinary row update regardless of `on_insert`.
        fn upsert_increment(
            table: &str,
            key: &[$bindings::boogy::platform::store::Column],
            counter: &str,
            delta: $bindings::boogy::platform::store::Value,
            cols: UpsertColumns<'_>,
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            let columns = $bindings::boogy::platform::store::UpsertColumns {
                always: cols.always.to_vec(),
                on_insert: cols.on_insert.to_vec(),
            };
            match $bindings::boogy::platform::store::upsert_increment(
                table, key, counter, &delta, &columns,
            ) {
                Ok(id) => Ok(id),
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// Atomic insert-or-update keyed on a unique index. If a row
        /// with matching `key` columns exists, update `cols.always`
        /// on it (key columns untouched). Otherwise insert a new row
        /// with `key + cols.always + cols.on_insert`. Returns the row
        /// id (existing or new).
        ///
        /// PRECONDITION: the `key` columns must correspond to an
        /// existing unique index on the table.
        ///
        /// ```ignore
        /// // `key` and the `UpsertColumns` fields are WIT `store::Column`s — name + value.
        /// fn text(name: &str, v: &str) -> store::Column {
        ///     store::Column { name: name.into(), val: store::Value::Text(v.into()) }
        /// }
        /// fn int(name: &str, v: i64) -> store::Column {
        ///     store::Column { name: name.into(), val: store::Value::Integer(v) }
        /// }
        ///
        /// fn touch_edge(a: &str, b: &str, weight: i64) -> Result<u64, ApiError> {
        ///     let id = upsert(
        ///         "user_affinity_edges",
        ///         &[text("user_a", a), text("user_b", b)],
        ///         UpsertColumns::always(&[int("weight", weight), int("updated_at", now_millis() as i64)]),
        ///     )?;
        ///     Ok(id)
        /// }
        /// ```
        #[allow(dead_code)]
        fn upsert(
            table: &str,
            key: &[$bindings::boogy::platform::store::Column],
            cols: UpsertColumns<'_>,
        ) -> ::core::result::Result<u64, $crate::store::StoreError> {
            let columns = $bindings::boogy::platform::store::UpsertColumns {
                always: cols.always.to_vec(),
                on_insert: cols.on_insert.to_vec(),
            };
            match $bindings::boogy::platform::store::upsert(table, key, &columns) {
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
        /// table is exhausted.
        ///
        /// **Exactly-once holds in PRIMARY-KEY order** (`order_col =
        /// None`): the cursor resumes from the row key, which never
        /// moves for a live row.
        ///
        /// **In INDEX order it does not.** The resume bound is the index
        /// entry, which embeds the indexed VALUE, so a row whose indexed
        /// column is updated mid-walk relocates: backward past the
        /// cursor it is visited TWICE, forward it is SKIPPED. Cursors are
        /// read-committed, not snapshot-isolated, and no guard can
        /// recover an entry that moved across the resume bound. If `f`
        /// does anything non-idempotent — charging, sending,
        /// incrementing — either walk in primary-key order or make `f`
        /// safe to repeat. This is the
        /// bounded-memory alternative to `find_all_rows` / offset
        /// paging for large-table batch jobs: only `batch_size` rows are
        /// ever materialized at a time, and the cursor resumes strictly
        /// after the last row of the prior batch (no offset re-scan).
        ///
        /// If `f` returns `Err`, iteration stops and the error
        /// propagates. The cursor resource is dropped when the loop
        /// ends (the host reaps it).
        ///
        /// `counters` names counter columns to merge into every row `f`
        /// sees — same opt-in `Query::with_counter` gives row listings, on
        /// the SAME `find-options.counters` wire field, matched to this
        /// free function's existing "raw types, no builder" shape rather
        /// than adding one just for this parameter. Empty (every existing
        /// caller before this parameter existed) merges nothing: a counter
        /// never arrives in a streamed row unasked. Per-table granularity,
        /// same as every other opt-in path — naming one counter merges
        /// every live counter column the table declares.
        ///
        /// ```ignore
        /// for_each_batch(
        ///     "widgets", vec![], vec![], None, store::SortDir::Asc, 100,
        ///     &["widgets.touches"],
        ///     |batch| {
        ///         for row in batch {
        ///             let touches = row.int("touches"); // merged, not Nil
        ///         }
        ///         Ok(())
        ///     },
        /// )?;
        /// ```
        fn for_each_batch(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
            order_col: ::core::option::Option<&str>,
            dir: $bindings::boogy::platform::store::SortDir,
            batch_size: u32,
            counters: &[&str],
            mut f: impl ::core::ops::FnMut(&[$crate::store::Row]) -> ::core::result::Result<(), $crate::store::StoreError>,
        ) -> ::core::result::Result<(), $crate::store::StoreError> {
            let cursor = $bindings::boogy::platform::store::open_cursor(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters,
                    order_by: vec![],
                    page: None,
                    or_groups,
                    // Inert on this path — `open_cursor` drives its own
                    // ordering and batching and never reads the field. Named
                    // anyway so the one construction that would be a real
                    // exception has to say so.
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: counters.iter().map(|s| s.to_string()).collect(),
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
        /// and the [`group_by_column`] primitive this wrapper composes.
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
            let res = $bindings::boogy::platform::store::find(
                child_table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters: vec![$bindings::boogy::platform::store::Filter {
                        column: fk_column.to_string(),
                        op: $bindings::boogy::platform::store::FilterOp::In,
                        val: $bindings::boogy::platform::store::Value::Null,
                        in_values: Some(in_vals),
                    }],
                    order_by: vec![],
                    page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset: 0 }),
                    or_groups: vec![],
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
                },
            )
            .map_err($crate::store::StoreError::from_wit)?;
            let sdk_rows: ::std::vec::Vec<$crate::store::Row> =
                res.rows.iter().map(|r| to_sdk_row(r)).collect();
            // ONE page, then refuse. An `IN` list is planned as one seek PER
            // parent, unioned and sorted in host memory — so unlike the single
            // equality helpers, no declared composite would make this page in
            // bounded work. Batching the parents is the caller's lever.
            $crate::store::refuse_beyond_one_page(
                "load_has_many",
                sdk_rows.len(),
                res.total_count,
                res.has_more,
                "Split the parent ids into smaller batches and call it once per batch, or load \
                 the children per parent with a keyset page each.",
            )?;
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
                    order_by: vec![],
                    page: Some($bindings::boogy::platform::store::Page { limit: 1, offset: 0 }),
                    or_groups: vec![],
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
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

        /// Is an ambient transaction open for THIS request?
        ///
        /// Only useful in a service that is called by another service. A
        /// read-modify-write — read a value, decide from it, write it back —
        /// is serializable only inside a transaction; outside one, each store
        /// call is its own auto-commit transaction and two racing callers can
        /// both pass the same check.
        ///
        /// A callee cannot open one defensively: a callee that calls `tx`
        /// fails at commit. So a service whose correctness depends on the
        /// caller having opened one should refuse when it has not:
        ///
        /// ```ignore_snippet: illustrative guard; ApiError and the handler around it are not in scope in this doc block
        /// if !in_transaction() {
        ///     return Err(ApiError::conflict(
        ///         "reserve must be called inside a transaction",
        ///     ));
        /// }
        /// ```
        ///
        /// Reads no data and takes no conflict range. Returns `false` when the
        /// store capability is not granted.
        fn in_transaction() -> bool {
            $bindings::boogy::platform::store::in_transaction()
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

        // -- Random values --
        //
        // `runtime::random_bytes` is the platform's only entropy
        // primitive, and it is an import, so the call site has to live
        // here. Everything above raw bytes — ranges, alphabets,
        // shuffles, UUIDs — is host-testable arithmetic in
        // `$crate::random`, and these wrappers are one line each.
        //
        // All of them need `entropy = true` in `[capabilities]`. Without
        // it the host returns zero bytes, so values stay in range but
        // stop being random.
        //
        // See `boogy_sdk::random` for the full method list, the
        // rejection-sampling guarantee, and the `try_*` forms.

        /// A `Rng` over this service's platform entropy source. Use it
        /// when you want several values from one handle, or a method the
        /// flat `random_*` wrappers below don't expose.
        #[allow(dead_code)]
        type HostRng = $crate::random::Rng<fn(usize) -> ::std::vec::Vec<u8>>;

        /// Exactly `n` random bytes from the platform entropy source.
        /// Short reads are NOT padded here — see [`rng()`] /
        /// `boogy_sdk::random::Rng::bytes` for the padded form.
        #[allow(dead_code)]
        fn random_bytes(n: usize) -> ::std::vec::Vec<u8> {
            $bindings::boogy::platform::runtime::random_bytes(n as u32)
        }

        /// A random-value generator over the platform entropy source.
        /// Every `random_*` function below is `rng().<method>()`.
        #[allow(dead_code)]
        fn rng() -> HostRng {
            $crate::random::Rng::new(random_bytes as fn(usize) -> ::std::vec::Vec<u8>)
        }

        /// A uniformly random integer in `[min, max]` — both ends
        /// inclusive, free of modulo bias. Panics if `min > max`; use
        /// `try_random_int` for bounds derived from request input.
        #[allow(dead_code)]
        fn random_int(min: i64, max: i64) -> i64 {
            rng().int(min, max)
        }

        /// Total form of [`random_int`]: `Err(RandomError::EmptyRange)`
        /// instead of a panic when `min > max`.
        #[allow(dead_code)]
        fn try_random_int(min: i64, max: i64) -> Result<i64, $crate::random::RandomError> {
            rng().try_int(min, max)
        }

        /// A uniformly random integer in `[start, end)` — end exclusive.
        /// Panics if `end <= start`.
        #[allow(dead_code)]
        fn random_int_exclusive(start: i64, end: i64) -> i64 {
            rng().int_exclusive(start, end)
        }

        /// A uniformly random float in `[min, max)`. Panics if `min >
        /// max` or a bound is not finite.
        #[allow(dead_code)]
        fn random_float(min: f64, max: f64) -> f64 {
            rng().float(min, max)
        }

        /// Total form of [`random_float`].
        #[allow(dead_code)]
        fn try_random_float(min: f64, max: f64) -> Result<f64, $crate::random::RandomError> {
            rng().try_float(min, max)
        }

        /// A uniformly random float in `[0.0, 1.0)`.
        #[allow(dead_code)]
        fn random_unit_float() -> f64 {
            rng().unit_float()
        }

        /// `true` or `false`, each with probability 1/2.
        #[allow(dead_code)]
        fn random_bool() -> bool {
            rng().bool()
        }

        /// `true` with probability `p` (clamped to `[0, 1]`).
        #[allow(dead_code)]
        fn random_bool_with_probability(p: f64) -> bool {
            rng().bool_with_probability(p)
        }

        /// A random string of exactly `len` characters from `alphabet` —
        /// e.g. `random_string(6, &Alphabet::HEX)`. Unbiased over the
        /// alphabet whatever its size. Panics only on a malformed custom
        /// alphabet; the `Alphabet` constants never panic.
        #[allow(dead_code)]
        fn random_string(len: usize, alphabet: &$crate::random::Alphabet<'_>) -> ::std::string::String {
            rng().string(len, alphabet)
        }

        /// Total form of [`random_string`], for an alphabet built from
        /// input.
        #[allow(dead_code)]
        fn try_random_string(
            len: usize,
            alphabet: &$crate::random::Alphabet<'_>,
        ) -> Result<::std::string::String, $crate::random::RandomError> {
            rng().try_string(len, alphabet)
        }

        /// An opaque public id: 22 URL-safe characters, ~131 bits. The
        /// default answer for a user-facing id that must not leak row
        /// counts. Store it in a TEXT column with a unique index.
        #[allow(dead_code)]
        fn random_id() -> ::std::string::String {
            rng().id()
        }

        /// A lowercase hex string of exactly `len` characters.
        #[allow(dead_code)]
        fn random_hex(len: usize) -> ::std::string::String {
            rng().hex(len)
        }

        /// `n` values, each produced by calling `f` with the generator.
        #[allow(dead_code)]
        fn random_vec_of<T, F>(n: usize, f: F) -> ::std::vec::Vec<T>
        where
            F: FnMut(&mut HostRng) -> T,
        {
            rng().vec_of(n, f)
        }

        /// One element of `items`, chosen uniformly. `None` for an empty
        /// slice.
        #[allow(dead_code)]
        fn random_choose<T>(items: &[T]) -> Option<&T> {
            rng().choose(items)
        }

        /// Shuffle `items` in place into a uniformly random permutation.
        #[allow(dead_code)]
        fn random_shuffle<T>(items: &mut [T]) {
            rng().shuffle(items)
        }

        /// `k` distinct elements of `items`, in random order. Returns
        /// everything (shuffled) when `k` exceeds the slice length.
        #[allow(dead_code)]
        fn random_sample<T>(items: &[T], k: usize) -> ::std::vec::Vec<&T> {
            rng().sample(items, k)
        }

        /// A random (version 4) UUID in canonical hyphenated form.
        #[allow(dead_code)]
        fn random_uuid_v4() -> ::std::string::String {
            rng().uuid_v4()
        }

        /// A time-ordered (version 7) UUID in canonical hyphenated form.
        /// The timestamp is caller-supplied — pass `now_millis()`, which
        /// needs the `clock` capability.
        #[allow(dead_code)]
        fn random_uuid_v7(unix_millis: u64) -> ::std::string::String {
            rng().uuid_v7(unix_millis)
        }

        #[allow(dead_code)]
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
        fn sort_asc(column: &str) -> $bindings::boogy::platform::store::OrderTerm {
            $bindings::boogy::platform::store::OrderTerm::Column(
                $bindings::boogy::platform::store::SortBy {
                    column: column.to_string(),
                    dir: $bindings::boogy::platform::store::SortDir::Asc,
                },
            )
        }

        /// Build a descending `SortBy` for `column`. See [`sort_asc`].
        fn sort_desc(column: &str) -> $bindings::boogy::platform::store::OrderTerm {
            $bindings::boogy::platform::store::OrderTerm::Column(
                $bindings::boogy::platform::store::SortBy {
                    column: column.to_string(),
                    dir: $bindings::boogy::platform::store::SortDir::Desc,
                },
            )
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
        /// `skip_total` is the one trailing bool — it returns `None` for the
        /// count instead of computing it — pass `true` whenever you discard
        /// the total.
        ///
        /// Returns `(rows, total, has_more)`. `total` is `None` exactly when
        /// `skip_total` was set; `has_more` is the store's own statement about
        /// this page and is meaningful under either value, so a caller that
        /// declined the count can still tell a full page from the end of the
        /// listing.
        ///
        /// The canonical use is composite keyset pagination — AND-only
        /// filters can't express `(score < c) OR (score = c AND id < cursor)`:
        ///
        /// ```ignore
        /// /// `c` / `cursor` are the (score, _id) of the previous page's last row.
        /// fn page_after(c: i64, cursor: i64) -> Result<Vec<Row>, ApiError> {
        ///     let (page_rows, _total, _has_more) = __boogy_find_rows_grouped(
        ///         "posts",
        ///         vec![filter_eq("deleted_at", store::Value::Text(String::new()))], // AND-prefix
        ///         vec![
        ///             vec![filter_lt("score", store::Value::Integer(c))],
        ///             vec![filter_eq("score", store::Value::Integer(c)),
        ///                  filter_lt("_id", store::Value::Integer(cursor))],
        ///         ],
        ///         vec![sort_desc("score"), sort_desc("_id")],
        ///         Some(page(20, 0)),
        ///         true,  // skip_total — the total is discarded here
        ///         vec![],  // counters — no counter merge in this example
        ///     )?;
        ///     Ok(page_rows)
        /// }
        /// ```
        fn __boogy_find_rows_grouped(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
            sort: ::std::vec::Vec<$bindings::boogy::platform::store::OrderTerm>,
            page: ::core::option::Option<$bindings::boogy::platform::store::Page>,
            skip_total: bool,
            counters: ::std::vec::Vec<::std::string::String>,
        ) -> ::core::result::Result<
            (::std::vec::Vec<$crate::store::Row>, ::core::option::Option<u64>, bool),
            $crate::store::StoreError,
        > {
            __boogy_find_rows_ranked(table, filters, or_groups, sort, page, skip_total, counters)
        }

        /// The row read, with an optional ranking by a related total.
        ///
        /// `None` is every existing caller and costs one `Option` test — the
        /// ordinary listing path is untouched.
        ///
        /// `counters` is `.with_counter(..)`'s ONLY path onto the wire —
        /// empty (every free-function caller below) merges nothing; the
        /// `Query` terminals (`fetch_one`/`fetch_all`/`fetch_all_with_total`/
        /// `fetch_page`) are the only callers that ever pass a non-empty list,
        /// sourced from `to_wit_args`.
        fn __boogy_find_rows_ranked(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            or_groups: ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
            order_by: ::std::vec::Vec<$bindings::boogy::platform::store::OrderTerm>,
            page: ::core::option::Option<$bindings::boogy::platform::store::Page>,
            skip_total: bool,
            counters: ::std::vec::Vec<::std::string::String>,
        ) -> ::core::result::Result<
            (::std::vec::Vec<$crate::store::Row>, ::core::option::Option<u64>, bool),
            $crate::store::StoreError,
        > {
            match $bindings::boogy::platform::store::find(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters, order_by, page, or_groups, skip_total,
                    group_cursor: ::core::option::Option::None,
                    counters,
                },
            ) {
                Ok(result) => {
                    let rows: ::std::vec::Vec<$crate::store::Row> =
                        result.rows.iter().map(|r| to_sdk_row(r)).collect();
                    Ok((rows, result.total_count, result.has_more))
                }
                Err(e) => Err($crate::store::StoreError::from_wit(e)),
            }
        }

        /// General-purpose multi-row read with composite filters, composite
        /// sort, and a REQUIRED page. Returns `(rows, total_count)` — the count
        /// is the total matching rows ignoring the page limit (useful for
        /// showing "X total" in a UI).
        ///
        /// `page` is a [`page`](page) value, not an `Option`: there is no
        /// "unpaged" spelling. The `None` this used to accept sent `page: None`
        /// on the wire, whereupon the store substituted its own ceiling
        /// (`BOOGY_STORE_MAX_PAGE_ROWS`) and answered with that many rows — the
        /// same silent truncation the `Query` typestate exists to prevent,
        /// reached through the free-function surface instead.
        ///
        /// For simpler call sites prefer [`find_rows_by`] or [`find_all_rows`].
        ///
        /// ```ignore
        /// // Top-N posts by score in a window, paginated by created_at:
        /// let (posts, _total) = find_rows(
        ///     "posts",
        ///     vec![filter_eq("parent_post_id", store::Value::Integer(0))],
        ///     vec![sort_desc("score_1h"), sort_asc("_id")],
        ///     page(20, 0),
        /// )?;
        /// ```
        fn find_rows(
            table: &str,
            filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
            sort: ::std::vec::Vec<$bindings::boogy::platform::store::OrderTerm>,
            page: $bindings::boogy::platform::store::Page,
        ) -> ::core::result::Result<(::std::vec::Vec<$crate::store::Row>, u64), $crate::store::StoreError> {
            let (rows, total, _has_more) = __boogy_find_rows_grouped(
                table,
                filters,
                ::std::vec::Vec::new(),
                sort,
                Some(page),
                SDK_WANT_TOTAL,
                ::std::vec::Vec::new(),
            )?;
            // `skip_total` is `SDK_WANT_TOTAL` (false) two lines up, so the
            // store owes a total.
            ::core::result::Result::Ok((rows, $crate::store::required_total("find_rows", total)?))
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

        /// The rows whose `column` equals `val` — **at most one page of them.**
        /// Parallel to [`find_row_by`] but returns `Vec<Row>` instead of
        /// `Option<Row>`.
        ///
        /// It does NOT return the full matching set, and it used to say it did.
        /// This is the UNTYPED lookup: with only a table name there is no model
        /// schema to read a declared `list_by` from, so there is no sort column
        /// it could page along safely. If more rows match than one page holds,
        /// this returns a named error carrying the remedy rather than a prefix
        /// that looks complete. The typed equivalents that CAN page are
        /// `db_find_by_page::<M>(..)` and `auth::find_owned::<M>(..)`.
        ///
        /// ```ignore
        /// fn backers_of(post_id: u64) -> Result<Vec<Row>, ApiError> {
        ///     let backers = find_rows_by(
        ///         "investments", "post_id", store::Value::Integer(post_id as i64),
        ///     )?;
        ///     Ok(backers)
        /// }
        /// ```
        fn find_rows_by(
            table: &str,
            column: &str,
            val: $bindings::boogy::platform::store::Value,
        ) -> ::core::result::Result<::std::vec::Vec<$crate::store::Row>, $crate::store::StoreError> {
            let res = $bindings::boogy::platform::store::find(
                table,
                &$bindings::boogy::platform::store::FindOptions {
                    filters: vec![$bindings::boogy::platform::store::Filter {
                        column: column.to_string(),
                        op: $bindings::boogy::platform::store::FilterOp::Eq,
                        val: val.clone(),
                        in_values: None,
                    }],
                    order_by: vec![],
                    page: Some($bindings::boogy::platform::store::Page { limit: SDK_FIND_BATCH, offset: 0 }),
                    or_groups: vec![],
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
                },
            )
            .map_err($crate::store::StoreError::from_wit)?;
            let rows: ::std::vec::Vec<$crate::store::Row> =
                res.rows.iter().map(|r| to_sdk_row(r)).collect();
            // ONE page, then refuse. This is the UNTYPED lookup: with only a
            // table name there is no model schema to read a declared `list_by`
            // from, so there is no sort column it could page along safely.
            // `db_find_by::<M>` is the typed equivalent that can.
            $crate::store::refuse_beyond_one_page(
                "find_rows_by",
                rows.len(),
                res.total_count,
                res.has_more,
                "Use the typed db_find_by::<M>(..) with a model that declares \
                 list_by(filter = \"<this column>\", newest = \"<sort col>\"), or page it \
                 yourself with the Query DSL's keyset terminal.",
            )?;
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

        /// The one `AggSpec` -> wire converter.
        ///
        /// This match used to be written out inline at the ranked-listing call
        /// site, where it was the only copy and therefore invisible as a
        /// duplicate. It is a function now because `to_wit_args` needs it too,
        /// and two hand-written copies of a five-arm enum map is how an arm
        /// goes missing.
        fn __boogy_sdk_agg_to_wit(
            a: &$crate::store::AggSpec,
        ) -> $bindings::boogy::platform::store::AggSpec {
            $bindings::boogy::platform::store::AggSpec {
                kind: match a.kind {
                    $crate::store::AggFunc::CountAll =>
                        $bindings::boogy::platform::store::AggFunc::CountAll,
                    $crate::store::AggFunc::Sum =>
                        $bindings::boogy::platform::store::AggFunc::Sum,
                    $crate::store::AggFunc::Avg =>
                        $bindings::boogy::platform::store::AggFunc::Avg,
                    $crate::store::AggFunc::Min =>
                        $bindings::boogy::platform::store::AggFunc::Min,
                    $crate::store::AggFunc::Max =>
                        $bindings::boogy::platform::store::AggFunc::Max,
                },
                column: a.column.clone(),
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
        /// use boogy_sdk::pagination::{Cursor, CursorPage};
        /// use boogy_sdk::store::{Filter, FilterOp, SortDir, Val};
        ///
        /// #[derive(Serialize)]
        /// struct PostView { id: u64, title: String }
        ///
        /// fn list_posts(cursor: Option<Cursor>, limit: usize)
        ///     -> Result<CursorPage<PostView>, ApiError>
        /// {
        ///     // Note the filter type: `keyset_paginate` takes SDK-side
        ///     // `store::Filter`s (over `Val`), NOT the WIT `store::Filter`s the
        ///     // `filter_eq` / `filter_lt` builders return.
        ///     let page = keyset_paginate::<PostView, _>(
        ///         "posts",
        ///         vec![Filter {
        ///             column: "deleted_at".into(),
        ///             op: FilterOp::Eq,
        ///             val: Val::Text(String::new()),
        ///             in_values: None,
        ///         }],
        ///         vec![],
        ///         "created_at",
        ///         SortDir::Desc,
        ///         cursor,
        ///         limit,
        ///         vec![],  // counters: no counter merge in this example
        ///         |row| {
        ///             let view = PostView { id: row.id(), title: row.text("title") };
        ///             let last_id    = row.id().to_string();
        ///             let last_value = json::json!(row.int("created_at"));
        ///             (view, Cursor::keyset(last_id, last_value))
        ///         },
        ///     )?;
        ///     Ok(page)
        /// }
        /// ```
        fn keyset_paginate<T, F>(
            table: &str,
            base_filters: ::std::vec::Vec<$crate::store::Filter>,
            base_or_groups: ::std::vec::Vec<::std::vec::Vec<$crate::store::Filter>>,
            sort_col: &str,
            dir: $crate::store::SortDir,
            cursor: ::core::option::Option<$crate::pagination::Cursor>,
            limit: usize,
            counters: ::std::vec::Vec<::std::string::String>,
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
                $bindings::boogy::platform::store::OrderTerm::Column(
                    $bindings::boogy::platform::store::SortBy {
                        column: sort_col.to_string(),
                        dir: wit_dir,
                    },
                ),
            ];
            if sort_col != "_id" {
                sort.push($bindings::boogy::platform::store::OrderTerm::Column(
                    $bindings::boogy::platform::store::SortBy {
                        column: "_id".to_string(),
                        dir: wit_dir,
                    },
                ));
            }

            // Ask for the page, not the page plus one.
            //
            // This DID overfetch by one and call a short result final. That is
            // the guest doing the one thing a guest cannot do: the host clamps
            // a page limit to its own per-call ceiling, so a request for
            // `limit + 1` comes back holding `limit` whenever the ceiling sits
            // there — and the listing then ended, silently, at the ceiling. The
            // host answers the same question from the other side of the clamp,
            // where the extra row is either present or the data ran out.
            let wit_page = Some($bindings::boogy::platform::store::Page {
                limit: limit as u32,
                offset: 0,
            });

            // Execute the query (converts rows to SDK Row via to_sdk_row inside).
            // Keyset pagination derives "has next page" from `has_more`, never
            // from the total — so skip the count entirely.
            let (rows, _total, has_more) = __boogy_find_rows_grouped(table, wit_filters, wit_or_groups, sort, wit_page, true, counters)
                .map_err($crate::error::ApiError::from)?;

            // Map each row to (T, Cursor) before slicing.
            let mapped: ::std::vec::Vec<(T, $crate::pagination::Cursor)> =
                rows.iter().map(&row_to_item_and_cursor).collect();

            // Emit next_cursor when — and only when — the store says more
            // follows. Nothing is sliced off here any more: every row that came
            // back is a row of this page.
            let last_cursor = mapped.last().map(|(_, c)| c.clone());
            let items: ::std::vec::Vec<T> = mapped.into_iter().map(|(t, _)| t).collect();
            let page = match last_cursor {
                Some(c) if has_more => $crate::pagination::CursorPage {
                    items,
                    next_cursor: Some($crate::pagination::encode(&c)),
                },
                _ => $crate::pagination::CursorPage { items, next_cursor: None },
            };

            Ok(page)
        }

        // ---------------------------------------------------------------
        // Typed Query DSL (slice a). The QueryArgs data + builder methods
        // live in `boogy_sdk::query`; this Query newtype wraps them and
        // adds the four terminal methods that call the macro-emitted WIT
        // primitives (__boogy_find_rows_grouped, count_rows, keyset_paginate).
        // ---------------------------------------------------------------

        /// Typed query-builder. Wraps [`boogy_sdk::query::QueryArgs`] and
        /// adds the terminal methods (`fetch_one`, `fetch_all`,
        /// `fetch_all_with_total`, `count`, `fetch_page`) that execute
        /// the query against the WIT store.
        ///
        /// ```ignore
        /// use boogy_sdk::pagination::CursorPage;
        ///
        /// #[derive(Serialize)]
        /// struct PostView { id: u64, title: String }
        ///
        /// fn list_replies(cursor: Option<String>) -> Result<CursorPage<PostView>, ApiError> {
        ///     let page = Query::on(Post::TABLE)
        ///         .filter(Post::room_id.eq(7_i64))
        ///         .filter(Post::deleted_at.is_null())
        ///         .order(Post::created_at.desc())   // the ordering IS the cursor key
        ///         .limit(20)
        ///         .cursor(cursor)                   // the opaque token, no decode
        ///         .fetch_page(|row| PostView { id: row.id(), title: row.text("title") })?;
        ///     Ok(page)
        /// }
        /// ```
        /// The typestate parameter is the row ceiling. A query starts
        /// [`Unbounded`](boogy_sdk::query::Unbounded) and `.limit(n)` moves it to
        /// [`Bounded`](boogy_sdk::query::Bounded); the row-materializing terminals
        /// (`fetch_all`, `fetch_all_with_total`) exist only on the latter, so
        /// "read the whole table into the guest's heap" is a compile error rather
        /// than a page the host silently truncates. `fetch_one`, `count`,
        /// `fetch_page` and the aggregate terminals are bounded by their own
        /// construction and stay available in both states.
        pub struct Query<B = $crate::query::Unbounded>(
            pub $crate::query::QueryArgs,
            ::core::marker::PhantomData<B>,
        );

        /// Build and issue the WIT `aggregate` call for a query's aggregate
        /// clauses. Separate from the terminals so both share one conversion.
        fn __boogy_aggregate(
            args: &$crate::query::QueryArgs,
        ) -> ::core::result::Result<
            $bindings::boogy::platform::store::AggregateResult,
            $crate::error::ApiError,
        > {
            let to_spec = |s: &$crate::store::AggSpec| {
                $bindings::boogy::platform::store::AggSpec {
                    kind: match s.kind {
                        $crate::store::AggFunc::CountAll =>
                            $bindings::boogy::platform::store::AggFunc::CountAll,
                        $crate::store::AggFunc::Sum =>
                            $bindings::boogy::platform::store::AggFunc::Sum,
                        $crate::store::AggFunc::Avg =>
                            $bindings::boogy::platform::store::AggFunc::Avg,
                        $crate::store::AggFunc::Min =>
                            $bindings::boogy::platform::store::AggFunc::Min,
                        $crate::store::AggFunc::Max =>
                            $bindings::boogy::platform::store::AggFunc::Max,
                    },
                    column: s.column.clone(),
                }
            };
            // Through the SAME lowering the row path uses. Reading
            // `base_filters` directly here dropped every expression predicate,
            // which on a principal-scoped aggregate returned other people's
            // totals — caught by `checksums`, not by review.
            let (__lowered_leaves, __lowered_groups) = args.lower_predicate()?;
            let opts = $bindings::boogy::platform::store::AggregateOptions {
                filters: __lowered_leaves.iter().map(__boogy_sdk_filter_to_wit).collect(),
                or_groups: __lowered_groups
                    .iter()
                    .map(|g| g.iter().map(__boogy_sdk_filter_to_wit).collect())
                    .collect(),
                group_by: args.group_by.clone(),
                aggregates: args.aggregates.iter().map(to_spec).collect(),
                having: args
                    .having
                    .iter()
                    .map(|h| $bindings::boogy::platform::store::AggFilter {
                        agg: to_spec(&h.agg),
                        op: __boogy_filter_op_to_wit(&h.op),
                        val: __boogy_val_to_wit(&h.val),
                    })
                    .collect(),
                // From `agg_sort()`, which reads BOTH the expression ordering
                // and the older field. Reading only the latter silently dropped
                // `.order(agg::count_all().desc())` and returned the ranking
                // ASCENDING — an inversion that looks like a plausible list.
                order_by: args
                    .agg_sort()
                    .map(|(a, dir)| $bindings::boogy::platform::store::OrderTerm::Aggregate(
                        $bindings::boogy::platform::store::AggSort {
                            agg: to_spec(&a),
                            dir: __boogy_sdk_dir_to_wit(dir),
                        },
                    ))
                    .into_iter()
                    .collect(),
                page: args.limit.map(|l| $bindings::boogy::platform::store::Page {
                    limit: l as u32,
                    offset: args.offset,
                }),
                group_cursor: args.cursor_token.clone(),
                want_group_cursor: args.want_group_cursor,
            };
            $bindings::boogy::platform::store::aggregate(&args.table, &opts)
                .map_err(|e| {
                    use $crate::store::IntoStoreError;
                    $crate::error::ApiError::from(e.into_store_error())
                })
        }

        impl Query<$crate::query::Unbounded> {
            // -- Construction --
            //
            // A new query has stated no ceiling, so it starts unbounded. The
            // only transition out is `.limit(n)`.

            pub fn on(table: &str) -> Self {
                Self($crate::query::QueryArgs::on(table), ::core::marker::PhantomData)
            }
        }

        impl<B> Query<B> {

            // -- Filter chaining (thin wrappers) --

            // -- Aggregates. `count_all`, not `count`: `count()` is already a
            // terminal returning Result<u64>, and one name cannot be both that
            // and a `-> Self` selector.
            /// Group the aggregates by a column — and, in doing so, state that
            /// this query's result size is now DATA-DEPENDENT.
            ///
            /// The return type is the enforcement: an ungrouped aggregate is one
            /// group whatever the table holds, a grouped one is a group per
            /// distinct value, and only the second needs a ceiling. So this moves
            /// a query that stated no `.limit(n)` into
            /// [`Grouped`](boogy_sdk::query::Grouped), where `fetch_groups` does
            /// not exist. A query that already stated one stays bounded, so
            /// `.limit(20).group_by(c)` and `.group_by(c).limit(20)` are the same
            /// query in the same state.
            pub fn group_by(self, column: &str) -> Query<<B as $crate::query::AfterGroupBy>::Out>
            where
                B: $crate::query::AfterGroupBy,
            {
                Query(self.0.group_by(column), ::core::marker::PhantomData)
            }
            pub fn sum(self, column: &str) -> Self { Self(self.0.sum(column), ::core::marker::PhantomData) }
            pub fn avg(self, column: &str) -> Self { Self(self.0.avg(column), ::core::marker::PhantomData) }
            pub fn min(self, column: &str) -> Self { Self(self.0.min(column), ::core::marker::PhantomData) }
            pub fn max(self, column: &str) -> Self { Self(self.0.max(column), ::core::marker::PhantomData) }
            pub fn count_all(self) -> Self { Self(self.0.count_all(), ::core::marker::PhantomData) }
            pub fn having<V: $crate::query::IntoVal>(
                self,
                aggregate: $crate::store::AggSpec,
                op: $crate::store::FilterOp,
                val: V,
            ) -> Self {
                Self(self.0.having(aggregate, op, val), ::core::marker::PhantomData)
            }

            // -- Sort --

            /// Keep the rows this expression matches. Repeated calls AND
            /// together.
            ///
            /// ```ignore
            /// let room_id = 7_i64;
            /// Query::on(Post::TABLE)
            ///     .filter(Post::room_id.eq(room_id))
            ///     .filter(Post::deleted_at.is_null())
            ///     .limit(50)          // `fetch_all` has no unbounded form
            ///     .fetch_all()?;
            /// ```
            pub fn filter(self, e: $crate::expr::Expr) -> Self {
                Self(self.0.filter(e), ::core::marker::PhantomData)
            }

            /// Order the result — by a column, or by an aggregate over a
            /// related table. The ordering is also what a cursor is built from.
            ///
            /// ```ignore
            /// Query::on(Post::TABLE)
            ///     .order(Post::created_at.desc())               // newest first
            ///     .limit(20)
            ///     .fetch_all()?;
            /// Query::on(Post::TABLE)
            ///     .order(agg::sum(PostVote::DIRECTION).desc())  // best first
            ///     .limit(20)
            ///     .fetch_all()?;
            /// ```
            pub fn order(self, o: $crate::expr::Order) -> Self {
                Self(self.0.order(o), ::core::marker::PhantomData)
            }

            /// Order by a column, or by an aggregate over a related table.
            /// One verb, because `ORDER BY` is one clause:
            ///
            /// ```ignore
            /// Query::on(Post::TABLE)
            ///     .order(Post::created_at.desc())               // newest first
            ///     .limit(20)
            ///     .fetch_all()?;
            /// Query::on(Post::TABLE)
            ///     .order(agg::sum(PostVote::DIRECTION).desc())  // best first
            ///     .limit(20)
            ///     .fetch_all()?;
            /// ```
            // -- Pagination --

            /// State the ceiling on how many rows this listing may materialize.
            ///
            /// This is also the **state transition**: it is what makes
            /// `fetch_all` / `fetch_all_with_total` exist on the query at all.
            /// `n` is the number of rows the handler is prepared to hold in a
            /// 32 MiB guest heap and to serialize into one response — it is not
            /// a hint, and there is no value meaning "all of them".
            ///
            /// If the matching set is open-ended (it grows with the tenant),
            /// this is the wrong verb: pair `.limit(..)` with `.cursor(..)` and
            /// end on [`fetch_page`](Self::fetch_page), which hands the caller
            /// the token that continues the listing instead of truncating it.
            pub fn limit(self, n: usize) -> Query<$crate::query::Bounded> {
                Query(self.0.limit(n), ::core::marker::PhantomData)
            }
            pub fn offset(self, n: u32) -> Self { Self(self.0.offset(n), ::core::marker::PhantomData) }
            /// Where this listing resumes — the opaque token from the previous
            /// page's `next_cursor`, straight from the query string.
            ///
            /// One verb for both kinds of listing, and no `decode` at the call
            /// site: a row page seeks from the position inside the token, a
            /// ranked group page uses it to pin the generation of the ordering
            /// it started in.
            pub fn cursor(self, token: ::core::option::Option<::std::string::String>) -> Self {
                Self(self.0.cursor(token), ::core::marker::PhantomData)
            }

            // -- Counters --

            /// Merge a counter's cells into this query's rows.
            ///
            /// A counter never arrives unasked: without this call the query
            /// reads no counter cells at all, however many counter columns
            /// the table declares. With it, the cells are read as ONE
            /// batch inside the same transaction the page itself uses.
            ///
            /// `name` is the counter's declared name (`Counter::NAME` —
            /// `"<table>.<column>"` for an `of = Model` counter). `key_cols`
            /// names the columns whose PER-ROW values supply the counter's
            /// key: pass `&[]` for a counter keyed by the row's own id, which
            /// every row carries. Repeat for more than one counter.
            ///
            /// ```ignore
            /// let page = Query::on(Room::TABLE)
            ///     .filter(Room::visibility.eq("public".to_string()))
            ///     .with_counter("rooms.post_count", &[])
            ///     .fetch_page(|row| row.int("post_count"))?;
            /// ```
            pub fn with_counter(self, name: &str, key_cols: &[&str]) -> Self {
                Self(self.0.with_counter(name, key_cols), ::core::marker::PhantomData)
            }

            // -- Internal: convert QueryArgs to the WIT-typed args __boogy_find_rows_grouped expects --

            fn to_wit_args(&self) -> ::core::result::Result<(
                ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
                ::std::vec::Vec<::std::vec::Vec<$bindings::boogy::platform::store::Filter>>,
                ::std::vec::Vec<$bindings::boogy::platform::store::OrderTerm>,
                ::core::option::Option<$bindings::boogy::platform::store::Page>,
                ::std::vec::Vec<::std::string::String>,
            ), $crate::error::ApiError> {
                // THE enforcement site for `counter_build_refusal` — every
                // terminal (`fetch_one`/`fetch_all`/`fetch_all_with_total`/
                // `fetch_page`/`find_many`) funnels through here, so a
                // `.with_counter(..)` naming a key column this query never
                // establishes is refused before ANY of them dispatch, not
                // just some. Same enforcement shape `fetch_one_group` uses
                // for `single_group_refusal` — checked inline, at the one
                // place every caller passes through, not duplicated per
                // terminal.
                if let Some(msg) = self.0.counter_build_refusal() {
                    return ::core::result::Result::Err($crate::error::ApiError::internal(msg));
                }
                // Expression predicates lower onto the wire's flat shape here.
                // A shape it cannot carry is refused rather than dropped, so a
                // clause never silently widens the query.
                let (leaves, or_groups) = self.0.lower_predicate()
                    .expect("predicate shape checked by the terminal");
                let wit_filters: ::std::vec::Vec<_> = leaves.iter()
                    .map(__boogy_sdk_filter_to_wit).collect();
                let wit_or_groups: ::std::vec::Vec<_> = or_groups.iter()
                    .map(|grp| grp.iter().map(__boogy_sdk_filter_to_wit).collect())
                    .collect();
                // `ordering` maps 1:1 onto the wire's `order-term`, which is
                // the whole reason the wire carries that type: one clause, one
                // list, and no place for a column ordering and an aggregate
                // ordering to be tracked separately and disagree.
                let wit_sort: ::std::vec::Vec<_> = self.0.ordering.iter()
                    .map(|o| match o {
                        $crate::expr::Order::Column(col, dir) =>
                            $bindings::boogy::platform::store::OrderTerm::Column(
                                $bindings::boogy::platform::store::SortBy {
                                    column: col.clone(),
                                    dir: __boogy_sdk_dir_to_wit(*dir),
                                },
                            ),
                        $crate::expr::Order::Aggregate(a, dir) =>
                            $bindings::boogy::platform::store::OrderTerm::Aggregate(
                                $bindings::boogy::platform::store::AggSort {
                                    agg: __boogy_sdk_agg_to_wit(a),
                                    dir: __boogy_sdk_dir_to_wit(*dir),
                                },
                            ),
                    })
                    .collect();
                let wit_page = self.0.limit.map(|lim| $bindings::boogy::platform::store::Page {
                    limit: lim as u32,
                    offset: self.0.offset,
                });
                // Every `.with_counter(..)` name, in call order. Per-table on
                // the host (naming one merges every live counter column on
                // the table, not just the named one — see the store's own
                // opt-in read-merge doc), so this list only needs to be
                // non-empty to opt in; duplicates across repeated calls cost
                // nothing extra.
                let wit_counters: ::std::vec::Vec<::std::string::String> =
                    self.0.counters.iter().map(|c| c.name.clone()).collect();
                ::core::result::Result::Ok((wit_filters, wit_or_groups, wit_sort, wit_page, wit_counters))
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
                let (f, og, s, p, counters) = self.to_wit_args()?;
                // Discards the total → skip the count.
                let (rows, _total, _has_more) = __boogy_find_rows_grouped(&self.0.table, f, og, s, p, true, counters)
                    .map_err($crate::error::ApiError::from)?;
                Ok(rows.into_iter().next())
            }

            /// Count matching rows. Does NOT materialize rows.
            ///
            /// Ordering and paging are ignored — they cannot change a count.
            ///
            /// An **OR predicate is REFUSED**, not dropped. The WIT `count` op
            /// takes a conjunction only, and a count is the one terminal whose
            /// answer carries no evidence of what it counted, so silently
            /// ignoring a clause would return a number that looks right and is
            /// not. Count the length of a fetch instead, or restructure the
            /// predicate with `is_in`.
            pub fn count(self) -> ::core::result::Result<u64, $crate::error::ApiError> {
                // count_filters() encodes the "WIT count is filters-only"
                // contract — unit-tested in boogy_sdk::query::tests.
                let wit_filters: ::std::vec::Vec<_> = self.0.count_filters()?.iter()
                    .map(__boogy_sdk_filter_to_wit).collect();
                count_rows(&self.0.table, wit_filters)
                    .map_err($crate::error::ApiError::from)
            }

            /// One page of a ranked group listing, with the token that
            /// continues it.
            ///
            /// The page type is [`CursorPage`], the same one row listings
            /// return, so a client pages groups exactly as it pages rows: read
            /// `next_cursor`, hand it back as `?cursor=…`, stop when it is
            /// absent.
            ///
            /// What it adds over `fetch_groups` is that the pages agree with
            /// each other. A ranked listing is ordered by a total, and totals
            /// move while a client is reading; `next_cursor` names the version
            /// of the ordering this page came from, so the following page
            /// continues it rather than re-ranking against a newer one.
            ///
            /// Needs an `.order(agg::….desc())` and a `.limit(..)` — without an ordering
            /// there is no traversal to continue, and the call errors rather
            /// than returning a page whose cursor would mean nothing.
            ///
            /// ```ignore
            /// let room_id = 7_i64;
            /// let token: Option<String> = None;
            /// let page = Query::on(PostVote::TABLE)
            ///     .filter(PostVote::room_id.eq(room_id))
            ///     .group_by(PostVote::POST_ID)
            ///     .sum(PostVote::DIRECTION)
            ///     .order(agg::sum(PostVote::DIRECTION).desc())
            ///     .limit(20)
            ///     .cursor(token)
            ///     .fetch_group_page(|g| (
            ///         g.key().map(Val::as_int).unwrap_or(0),
            ///         g.sum(PostVote::DIRECTION),
            ///     ))?;
            /// ```
            pub fn fetch_group_page<T, F>(self, group_to_item: F) -> ::core::result::Result<
                $crate::pagination::CursorPage<T>,
                $crate::error::ApiError,
            >
            where
                T: ::serde::Serialize,
                F: ::core::ops::Fn(&$crate::store::Group) -> T,
            {
                // Through `agg_sort()`, never the raw field: the aggregate
                // ordering can arrive from `.order(agg::….desc())` (which lands
                // in `ordering`) and the accessor is what knows that. Reading
                // retired-spelling: `order_by_agg` was deleted
                // 2026-08-17; ordering arrives as `order-term` values
                // through `agg_sort()`. This records the bug that
                // deletion caused.
                // `order_by_agg` directly saw only the pre-expression verb, so
                // once that verb was deleted this refused every ranked page —
                // and it compiled clean, because a field that is merely never
                // written is not a type error.
                if self.0.agg_sort().is_none() || self.0.limit.is_none() {
                    return ::core::result::Result::Err($crate::error::ApiError::internal(
                        "fetch_group_page needs an aggregate ordering \
                         (.order(agg::….desc())) and a .limit(..): a listing \
                         with no ordering has no next page to continue to",
                    ));
                }
                let specs = self.0.aggregates.clone();
                if specs.is_empty() {
                    return ::core::result::Result::Err($crate::error::ApiError::internal(
                        "this query selects no aggregates; add .sum(..)/.count_all()/…                          before fetch_group_page",
                    ));
                }
                let mut args = self.0;
                // The terminal declares the intent, not the caller: asking for a
                // page IS asking for something to continue from.
                args.want_group_cursor = true;
                let out = __boogy_aggregate(&args)?;
                let items = out
                    .groups
                    .iter()
                    .map(|g| {
                        group_to_item(&$crate::store::Group::new(
                            g.keys.iter().map(__boogy_wit_to_val).collect(),
                            g.values
                                .iter()
                                .map(|v| v.as_ref().map(__boogy_wit_to_val))
                                .collect(),
                            specs.clone(),
                        ))
                    })
                    .collect();
                ::core::result::Result::Ok($crate::pagination::CursorPage {
                    items,
                    next_cursor: out.next_group_cursor,
                })
            }

            /// The single group an ungrouped aggregate produces.
            ///
            /// Errors if the query has a `group_by`, rather than silently
            /// returning whichever group happened to come first.
            pub fn fetch_one_group(self) -> ::core::result::Result<
                $crate::store::Group,
                $crate::error::ApiError,
            > {
                if !self.0.group_by.is_empty() {
                    return ::core::result::Result::Err($crate::error::ApiError::internal(
                        "fetch_one_group is for a query with no group_by; use                          fetch_groups",
                    ));
                }
                // Through the shared helper, NOT through `fetch_groups`: that
                // terminal is gated on `BoundedGroups`, which `Grouped` does not
                // satisfy, and this one is reachable from every state (it is the
                // ungrouped shape, and its own check above is what refuses a
                // grouped query).
                let mut groups = __boogy_groups(self.0, |g: &$crate::store::Group| g.clone())?;
                // The store returns exactly one row for an ungrouped
                // aggregate. If that ever stops being true this is a platform
                // bug, and reporting it beats handing back a default.
                if groups.len() == 1 {
                    ::core::result::Result::Ok(groups.remove(0))
                } else {
                    ::core::result::Result::Err($crate::error::ApiError::internal(::std::format!(
                        "an ungrouped aggregate must return exactly one group; got {}",
                        groups.len()
                    )))
                }
            }

            /// Cursor-paginated fetch. **The ordering is the cursor key** — the
            /// resume value comes from the column `.order(..)` named, so there is
            /// no second verb to state it with and nothing to keep in step. The
            /// user closure just maps row → item.
            ///
            /// A ranked ordering (`.order(agg::….desc())`) pages through the
            /// ranked path instead, whose cursor pins the GENERATION of the
            /// ordering rather than a row key — same call, different mechanism,
            /// which is the platform's choice to make.
            ///
            /// Defaults to `limit = 20` if `.limit()` was not chained.
            ///
            /// Errors if the query carries no ordering at all: there is then
            /// nothing to page by, and a cursor built from it would mean
            /// nothing.
            pub fn fetch_page<T, F>(self, row_to_item: F) -> ::core::result::Result<
                $crate::pagination::CursorPage<T>,
                $crate::error::ApiError,
            >
            where
                T: ::serde::Serialize,
                F: ::core::ops::Fn(&$crate::store::Row) -> T,
            {
                // Checked ONCE, ahead of both branches below: neither goes
                // through `to_wit_args` (the ranked branch builds its own
                // `FindOptions`; the keyset branch lowers its predicate
                // directly), so `to_wit_args`'s inline enforcement of
                // `counter_build_refusal` does not cover this terminal on its
                // own — this is `fetch_page`'s own copy of the SAME check,
                // the same shape `fetch_one_group` keeps its own copy of
                // `single_group_refusal`'s check rather than relying on a
                // shared call site to reach it.
                if let Some(msg) = self.0.counter_build_refusal() {
                    return ::core::result::Result::Err($crate::error::ApiError::internal(msg));
                }
                let wit_counters: ::std::vec::Vec<::std::string::String> =
                    self.0.counters.iter().map(|c| c.name.clone()).collect();

                // A ranked listing has no column to key on — its order comes
                // from a total the children carry — so it pages through the
                // ranked path, whose cursor pins the generation of the ordering
                // rather than a row key.
                if let Some((a, dir)) = self.0.related_order() {
                    let limit = self.0.limit.unwrap_or(20);
                    let (f, og, s_, p, counters) = self.to_wit_args()?;
                    let res = $bindings::boogy::platform::store::find(
                        &self.0.table,
                        &$bindings::boogy::platform::store::FindOptions {
                            filters: f,
                            // Already carries the aggregate term.
                            order_by: s_,
                            page: p.or(Some($bindings::boogy::platform::store::Page {
                                limit: limit as u32,
                                offset: 0,
                            })),
                            or_groups: og,
                            skip_total: SDK_SKIP_TOTAL,
                            group_cursor: self.0.cursor_token.clone(),
                            counters,
                        },
                    )
                    .map_err(|e| {
                        use $crate::store::IntoStoreError;
                        $crate::error::ApiError::from(e.into_store_error())
                    })?;
                    let items: ::std::vec::Vec<T> = res
                        .rows
                        .iter()
                        .map(|r| row_to_item(&to_sdk_row(r)))
                        .collect();
                    return ::core::result::Result::Ok($crate::pagination::CursorPage {
                        items,
                        next_cursor: res.next_group_cursor,
                    });
                }

                // The ORDERING is the cursor key. A query that says
                // `.order(created_at.desc()).limit(20).cursor(c)` has already
                // named the sort column, its direction and the page size, so
                // asking for them again as `keyset_by(..)` would be asking the
                // developer to state the mechanism and then keep it in step with
                // the intent by hand — which nothing checks.
                let (keyset_col, dir) = match self.0.column_sorts().first() {
                    Some((c, d)) => (c.clone(), *d),
                    None => return Err($crate::error::ApiError::internal(
                        "this listing has no ordering to page by: add \
                         `.order(Model::column.desc())`, which is also what \
                         the cursor is built from",
                    )),
                };
                let limit = self.0.limit.unwrap_or(20);

                let (__page_leaves, __page_groups) = self.0.lower_predicate()?;
                keyset_paginate::<T, _>(
                    &self.0.table,
                    __page_leaves,
                    __page_groups,
                    &keyset_col,
                    dir,
                    self.0.cursor,
                    limit,
                    wit_counters,
                    |row| {
                        let item = row_to_item(row);
                        let cursor = $crate::query::build_keyset_cursor(row, &keyset_col);
                        (item, cursor)
                    },
                )
            }
        }


        /// The group mapping every aggregate terminal shares.
        ///
        /// A free fn rather than a method so `fetch_one_group` (reachable from
        /// every state) and `fetch_groups` (reachable only from a state whose
        /// group count is bounded) can both use it without the second's bound
        /// leaking onto the first.
        fn __boogy_groups<T, F>(
            args: $crate::query::QueryArgs,
            group_to_item: F,
        ) -> ::core::result::Result<::std::vec::Vec<T>, $crate::error::ApiError>
        where
            F: ::core::ops::Fn(&$crate::store::Group) -> T,
        {
            let specs = args.aggregates.clone();
            if specs.is_empty() {
                return ::core::result::Result::Err($crate::error::ApiError::internal(
                    "this query selects no aggregates; add .sum(..)/.count_all()/… before \
                     fetch_groups",
                ));
            }
            let out = __boogy_aggregate(&args)?;
            ::core::result::Result::Ok(
                out.groups
                    .iter()
                    .map(|g| {
                        group_to_item(&$crate::store::Group::new(
                            g.keys.iter().map(__boogy_wit_to_val).collect(),
                            g.values.iter().map(|v| v.as_ref().map(__boogy_wit_to_val)).collect(),
                            specs.clone(),
                        ))
                    })
                    .collect(),
            )
        }

        /// The aggregate terminal that materializes ONE ITEM PER GROUP.
        ///
        /// On a separate impl block, gated on
        /// [`BoundedGroups`](boogy_sdk::query::BoundedGroups), for the reason
        /// [`fetch_all`](Query::fetch_all) is gated on `BoundedRead` — and NOT
        /// the same reason, which is why it is a second predicate rather than
        /// the same one.
        ///
        /// **Group cardinality is a different quantity from row count.** A
        /// grouped query over a million rows may return three groups; an
        /// ungrouped one returns exactly one over any table at all. So the
        /// unbounded quantity here does not exist until `.group_by(col)` is
        /// called, and once it is, it is the number of DISTINCT VALUES of
        /// `col` — data-dependent, invisible in the query text, and materialized
        /// one item at a time into a 32 MiB guest heap.
        ///
        /// `Unbounded` (no `group_by`) and `Bounded` (a ceiling was stated) both
        /// satisfy the predicate; `Grouped` does not.
        impl<B: $crate::query::BoundedGroups> Query<B> {
            /// Run the aggregates this query selected, one item per group.
            ///
            /// Whether the answer comes from stored per-group totals (a
            /// declared `rollup(...)`) or is computed from the rows is the
            /// platform's choice and does not change the result — so this is
            /// the only terminal for either, and there is deliberately no way
            /// to ask for one.
            ///
            /// An ungrouped query yields exactly ONE group, even over no rows:
            /// `SELECT sum(x) FROM t` on an empty table is one row holding
            /// NULL, not zero rows. Use `fetch_one_group` for that shape.
            ///
            /// **This truncates when a `.limit(n)` was stated**, by the caller's
            /// own instruction — same as `fetch_all`. When the number of groups
            /// grows with the tenant, use `fetch_group_page` instead, which
            /// hands back the token that continues the ranked listing.
            pub fn fetch_groups<T, F>(self, group_to_item: F) -> ::core::result::Result<
                ::std::vec::Vec<T>,
                $crate::error::ApiError,
            >
            where
                F: ::core::ops::Fn(&$crate::store::Group) -> T,
            {
                __boogy_groups(self.0, group_to_item)
            }
        }

        /// The terminals that materialize rows into the guest's heap.
        ///
        /// They are on a SEPARATE impl block, gated on
        /// [`BoundedRead`](boogy_sdk::query::BoundedRead), because that is the
        /// whole mechanism: a `Query` that never called `.limit(n)` is
        /// `Query<Unbounded>`, `Unbounded` does not implement `BoundedRead`, and
        /// so these two methods do not exist on it. Writing `fetch_all()` with
        /// no bound is a compile error naming the missing `.limit(..)`, not a
        /// request the host answers with a page of its own choosing.
        ///
        /// The previous shape said "subject to `limit` if set" in a doc comment
        /// and sent `page: None` when it was not — whereupon the host
        /// substituted `BOOGY_STORE_MAX_PAGE_ROWS` (default 1000) rows at offset
        /// 0 and returned them with no cursor and no total. The guest could not
        /// tell a complete answer from a truncated one, which is the failure
        /// mode this replaces. `auth::find_owned` carried the same kind of doc
        /// retired-spelling: the "small bounded sets" label is obsolete —
        /// the helper returns a bounded `RowPage`. Quoted because the
        /// label IS the defect being described.
        /// comment ("small bounded sets ONLY") right up until it exhausted a
        /// 32 MiB guest heap.
        impl<B: $crate::query::BoundedRead> Query<B> {
            /// The first `limit` matching rows, in this query's order.
            ///
            /// **This truncates, by the caller's own instruction.** It answers
            /// "give me at most N" — a top-N ranking, an `is_in` over a list of
            /// N ids, an existence probe at `.limit(1)`. It does NOT answer "give
            /// me all of them": if more than `limit` rows match, the rest are
            /// simply not returned and nothing here says so. When the matching
            /// set grows with the tenant, use
            /// [`fetch_page`](Self::fetch_page) instead — same `.limit(..)`, plus
            /// the cursor that continues the listing.
            pub fn fetch_all(self) -> ::core::result::Result<
                ::std::vec::Vec<$crate::store::Row>,
                $crate::error::ApiError,
            > {
                __boogy_refuse_unservable_limit("fetch_all", self.0.limit)?;
                // Discards the total → skip the count (don't route through
                // `fetch_all_with_total`, which must compute it).
                let (f, og, s, p, counters) = self.to_wit_args()?;
                let (rows, _total, _has_more) =
                    // `s` already carries the aggregate term: it came from the
                    // same `ordering` list, so there is nothing to pass beside it.
                    __boogy_find_rows_ranked(&self.0.table, f, og, s, p, SDK_SKIP_TOTAL, counters)
                        .map_err($crate::error::ApiError::from)?;
                Ok(rows)
            }

            /// [`fetch_all`](Self::fetch_all) plus the total number of matching
            /// rows, which is NOT limited by the page.
            ///
            /// The total is the one signal that tells a truncated page from a
            /// complete one: `rows.len() < total` means this page is a prefix.
            /// It costs a full count, so prefer `fetch_all` when the bound is
            /// the answer and `fetch_page` when the listing is open-ended.
            pub fn fetch_all_with_total(self) -> ::core::result::Result<
                (::std::vec::Vec<$crate::store::Row>, u64),
                $crate::error::ApiError,
            > {
                __boogy_refuse_unservable_limit("fetch_all_with_total", self.0.limit)?;
                let (f, og, s, p, counters) = self.to_wit_args()?;
                let (rows, total, _has_more) =
                    __boogy_find_rows_grouped(&self.0.table, f, og, s, p, SDK_WANT_TOTAL, counters)
                        .map_err($crate::error::ApiError::from)?;
                // `skip_total` is `SDK_WANT_TOTAL` (false) one line up, so the
                // store owes a total.
                let total = $crate::store::required_total("fetch_all_with_total", total)
                    .map_err($crate::error::ApiError::from)?;
                ::core::result::Result::Ok((rows, total))
            }
        }

        /// A stated bound the store cannot serve in one call is refused, not
        /// clamped.
        ///
        /// The typestate closes "forgot a bound"; this closes the one keystroke
        /// that reopens it, `.limit(usize::MAX)`. Above the host's per-call page
        /// ceiling the store returns its own cap and the guest has no way to
        /// know the answer was cut — the exact undetectable truncation the
        /// typestate exists to prevent, reached by a different route. A page
        /// this size is also not a thing to hold in a 32 MiB heap.
        fn __boogy_refuse_unservable_limit(
            terminal: &str,
            limit: ::core::option::Option<usize>,
        ) -> ::core::result::Result<(), $crate::error::ApiError> {
            match limit {
                ::core::option::Option::Some(n) if n > SDK_FIND_BATCH as usize => {
                    ::core::result::Result::Err($crate::error::ApiError::internal(::std::format!(
                        "{terminal} was asked for {n} rows; the store serves at most {} in one \
                         call (BOOGY_STORE_MAX_PAGE_ROWS), so it would return that many and the \
                         answer would be silently short. Page the listing instead: keep the \
                         .limit(..) at or below {}, add .cursor(token), and end on fetch_page, \
                         which returns the token that continues it.",
                        SDK_FIND_BATCH, SDK_FIND_BATCH,
                    )))
                }
                _ => ::core::result::Result::Ok(()),
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
            /// if `list_columns` already shows a LIVE column with `spec.name`,
            /// this is a no-op.
            ///
            /// Checks `!c.dropped` explicitly: `list_columns` reports
            /// soft-dropped columns as well as live ones (a schema reconcile
            /// needs to see them to tell "already dropped" from "never
            /// existed"), so a name match alone would misread "was added, then
            /// dropped" as "already applied" and silently skip re-adding it.
            pub fn add_column(
                &self,
                table: &str,
                spec: &$crate::store::ColumnSpec,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if list_columns(table)?.iter().any(|c| c.name == spec.name && !c.dropped) {
                    return Ok(()); // already applied
                }
                add_column(table, spec)
            }

            /// Rename a column in `table` from `old` to `new`. **Idempotent:**
            /// - If `new` is present and `old` is absent → already renamed, no-op.
            /// - If `old` is absent (and `new` is also absent) → error: nothing to rename.
            /// - Otherwise calls the underlying rename op.
            ///
            /// Presence checks exclude dropped columns for the same reason
            /// `add_column`'s does — see its doc comment.
            pub fn rename_column(
                &self,
                table: &str,
                old: &str,
                new: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                let cols = list_columns(table)?;
                let has_new = cols.iter().any(|c| c.name == new && !c.dropped);
                let has_old = cols.iter().any(|c| c.name == old && !c.dropped);
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
            /// shows no LIVE column named `name`, this is a no-op (already
            /// dropped) — the underlying `drop_column` errors "column not
            /// found" on a name that is already dropped, so this guard must
            /// exclude dropped entries rather than merely check presence (see
            /// `add_column`'s doc comment for why `list_columns` now returns
            /// dropped columns at all).
            pub fn drop_column(
                &self,
                table: &str,
                name: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if !list_columns(table)?.iter().any(|c| c.name == name && !c.dropped) {
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

            /// Drop a table entirely — irreversibly removes ALL of its rows, every
            /// index, the row-id counter, and its catalog entry. Idempotent: a
            /// no-op if the table does not exist (re-run safe). DESTRUCTIVE — the
            /// data cannot be recovered.
            ///
            /// Use to reset a table whose schema changed incompatibly. The table is
            /// only removed here; recreate it explicitly (drop-then-recreate within
            /// the migration) — `create_model`/`create_table` rebuild only a
            /// *missing* table, so a fresh create after the drop yields the new
            /// schema.
            pub fn drop_table(
                &self,
                table: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                if !$bindings::boogy::platform::store::list_tables()?
                    .iter()
                    .any(|t| t.name == table)
                {
                    return Ok(()); // already dropped
                }
                $bindings::boogy::platform::store::drop_table(table)
                    .map_err(::std::string::String::from)
            }

            // -- Data ops for backfills --

            /// Find rows matching `filters` in `table`. Returns
            /// `(rows, total_count)`. Delegates to the store `find` WIT fn.
            ///
            /// `page` is REQUIRED, for the reason the free-function
            /// [`find_rows`] requires one: an absent page is not "no limit", it
            /// is "the store picks", and the migration then reads a ceiling it
            /// did not choose with no way to tell a complete answer from a
            /// truncated one. A backfill that must visit every row walks it —
            /// page by page on a declared order, comparing against
            /// `total_count` — rather than asking for the table in one call.
            pub fn find_rows(
                &self,
                table: &str,
                filters: ::std::vec::Vec<$bindings::boogy::platform::store::Filter>,
                sort: ::std::vec::Vec<$bindings::boogy::platform::store::OrderTerm>,
                page: $bindings::boogy::platform::store::Page,
            ) -> ::core::result::Result<
                (::std::vec::Vec<$crate::store::Row>, u64),
                ::std::string::String,
            > {
                let result = $bindings::boogy::platform::store::find(
                    table,
                    &$bindings::boogy::platform::store::FindOptions {
                        filters,
                        order_by: sort,
                        page: Some(page),
                        or_groups: vec![],
                        skip_total: SDK_WANT_TOTAL,
                        group_cursor: ::core::option::Option::None,
                        counters: ::std::vec::Vec::new(),
                    },
                )?;
                let rows = result.rows.iter().map(|r| to_sdk_row(r)).collect();
                // `skip_total` is `false` above, so the store owes a total. A
                // backfill compares its progress against it, and a `None`
                // folded to `0` would read as "nothing left to do".
                let total = $crate::store::required_total("MigrationOps::find_rows", result.total_count)
                    .map_err(|e| e.to_string())?;
                Ok((rows, total))
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
        /// // `col` / `ColType` / `Val` are not re-exported by `wit_glue!`
        /// // (the unqualified `store` is the WIT bindings module).
        /// use boogy_sdk::store::{col, ColType, Val};
        ///
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
        ///                 covering: false,
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
                    order_by: vec![$bindings::boogy::platform::store::OrderTerm::Column(
                        $bindings::boogy::platform::store::SortBy {
                            column: "version".to_string(),
                            dir: $bindings::boogy::platform::store::SortDir::Desc,
                        },
                    )],
                    page: Some($bindings::boogy::platform::store::Page { limit: 1, offset: 0 }),
                    or_groups: vec![],
                    skip_total: SDK_SKIP_TOTAL,
                    group_cursor: ::core::option::Option::None,
                    counters: ::std::vec::Vec::new(),
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

        /// Total attempts `tx` makes before giving up on a store serialization
        /// conflict, from the `BOOGY_TX_MAX_ATTEMPTS` environment variable the
        /// platform sets for every service (default 3 = the original attempt
        /// plus two retries; `1` disables retry). Read once per instance and
        /// cached; a value that isn't a positive integer falls back to the
        /// default. Clamped to at least 1 so a `0` can never mean "never run
        /// the body", and to `MAX_TX_MAX_ATTEMPTS` at the top so a mistyped
        /// value can't spin a request for its whole time budget. The platform
        /// applies both clamps too, so the two cannot disagree.
        fn __boogy_tx_max_attempts() -> u32 {
            use ::std::sync::OnceLock;
            static MAX: OnceLock<u32> = OnceLock::new();
            *MAX.get_or_init(|| {
                ::std::env::var("BOOGY_TX_MAX_ATTEMPTS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or($crate::store::DEFAULT_TX_MAX_ATTEMPTS)
                    .clamp(1, $crate::store::MAX_TX_MAX_ATTEMPTS)
            })
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
        /// fails at commit (only the originating request commits) — the closure still
        /// runs and its `store::*` calls still join the caller's ambient transaction
        /// (committing or rolling back with it), but THIS `tx()` call reports `Err`
        /// (the same `unsupported` → 501 store error noted above) to its own handler
        /// rather than the `Ok` a transparent no-op would imply. A callee should not
        /// call `tx` at all — this is what happens if it does anyway.
        ///
        /// Inside the closure, `peer_fetch` already fails (`Err`) on a callee's
        /// non-success HTTP status by default, so the ordinary `peer_fetch(...)?`
        /// shape stops the closure — and therefore the commit — on an explicit
        /// rejection from a peer. The host enforces the same rule independently
        /// (a non-2xx participant response poisons the open ambient transaction), so
        /// this holds even for a closure that bypasses the SDK default via
        /// `peer_fetch_raw` and doesn't check the status itself.
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
        /// a commit `Unsupported` reaches the client as 501 when `E = ApiError`. A
        /// handler returning `Result<_, ApiError>` writes `tx(|| ...)?` and the existing
        /// `From<store::StoreError> for ApiError` maps the variant to the correct status
        /// (Unsupported → 501, …). When `E` can't be inferred, name it
        /// with a turbofish: `tx::<_, _, ApiError>(|| ...)`.
        ///
        /// # Automatic retry
        ///
        /// A **serialization conflict** — the store aborting the attempt at commit,
        /// with nothing from it applied — is retried automatically. Two distinct
        /// causes produce it, and both are safe to re-run:
        ///
        /// - **contention**: another transaction committed a write that overlaps
        ///   this one's read/write set, so the store aborts the loser;
        /// - **a stale snapshot**: the transaction's read snapshot aged out of the
        ///   store's version window before it committed — a *duration* failure, not
        ///   a contention one. A transaction body that reads, computes for several
        ///   seconds, then writes hits this with no competing writer at all.
        ///
        /// Nothing from an aborted attempt landed, so re-running is safe either way,
        /// and the retry re-runs **only the closure**: everything before it
        /// (parsing, auth, guards, expensive computation) runs exactly once. That is
        /// why the guidance is to keep the closure small and store-only, and to do
        /// costly work before opening the transaction — that guidance answers *both*
        /// causes, since a small store-only closure neither contends widely nor
        /// outlives the version window.
        ///
        /// The closure is therefore `Fn`, not `FnOnce`: it may run more than once.
        /// A closure that must consume a value should clone it inside, or compute
        /// the value before the transaction and borrow it. It is not `FnMut`
        /// either — which would be the *less* restrictive choice — because a body
        /// that mutates state across attempts is always a bug under retry: attempt
        /// 2 would start from attempt 1's leftovers even though the store discarded
        /// attempt 1's writes, so the closure and the database would disagree.
        ///
        /// Total attempts come from `BOOGY_TX_MAX_ATTEMPTS` (default 3); `1`
        /// disables retry entirely, and then a conflict surfaces unchanged as
        /// `Conflict` → 409, exactly as it did before auto-retry existed. When
        /// retry is enabled and the attempts
        /// are exhausted the error is `TooContended` → **HTTP 503 with a retry
        /// hint**, not 409: retry exhaustion is congestion on a hot row, not a
        /// malformed request. 409 keeps its narrow meaning — *your write genuinely
        /// conflicts* — which is what a `ConstraintViolation` (e.g. a unique-index
        /// violation) or an `ApiError::conflict(...)` raised by your own code means.
        /// Persistent `TooContended` is a signal about the transaction body, and
        /// which signal depends on the cause above: contention on one row is a
        /// data-model problem (split the key, or use a counter column), a search
        /// inside the closure that the planner cannot serve from an index puts
        /// the whole table in the read set so every concurrent writer conflicts
        /// with you (narrow the read, or move it out of the closure), while a
        /// body that keeps outliving the version window is a *duration* problem
        /// (move the slow work out of the closure).
        ///
        /// Retry applies to serialization conflicts **only**. These are never
        /// retried:
        ///
        /// - an error returned by the closure itself (it is deterministic — the
        ///   transaction rolls back and the error propagates unchanged);
        /// - `ConstraintViolation` — deterministic, so a retry could never succeed;
        /// - `Poisoned` — a participant service failed inside the transaction;
        ///   re-running would re-execute that failure once per attempt, and across
        ///   a call tree that multiplies into repeated callee executions;
        /// - `CommitUnknown` — the outcome is genuinely unknown, so a blind retry
        ///   could double-apply.
        ///
        /// There is no delay between attempts: a conflict resolves because some
        /// other writer won and committed, so the retry immediately re-reads a
        /// settled value. The request's overall time budget bounds pathological
        /// cases.
        ///
        /// # The closure is `Fn`, and that constrains what you may write in it
        ///
        /// Retrying means calling the closure again, so it cannot be `FnOnce` —
        /// and `FnMut` would still not let a captured value be moved out. The
        /// bound is therefore `Fn`, and **the closure may not consume anything it
        /// captures**. Two shapes that look natural and do not compile:
        ///
        /// ```ignore, ignore_snippet: shows the two shapes the compiler REJECTS (E0507) — compiling it would assert the opposite of what it teaches.
        /// // E0507 — struct-update moves the captured `row`.
        /// tx::<_, _, ApiError>(|| { db_update(id, &Row { name, ..row })?; Ok(()) })
        ///
        /// // E0507 — matching by value moves the captured `Option`.
        /// tx::<_, _, ApiError>(|| { match existing { Some(r) => …, None => … } })
        /// ```
        ///
        /// Build the owned value **before** the closure and borrow it inside
        /// (`&updated`), or match on a reference (`match &existing`). Constructing
        /// values inside the closure is always fine — the restriction is only on
        /// consuming captures.
        ///
        /// ```ignore
        /// #[derive(Serialize)]
        /// struct View { id: u64 }
        ///
        /// fn signup(user_cols: &[store::Column], profile_cols: &[store::Column])
        ///     -> Result<u64, ApiError>
        /// {
        ///     // Store-only closure. `E` is NOT inferable from the surrounding
        ///     // `?` (several error types convert from a store error), so name it.
        ///     let user_id = tx::<_, _, store::StoreError>(|| {
        ///         let user_id = store::insert("users", user_cols)?;
        ///         store::insert("profiles", profile_cols)?;
        ///         Ok(user_id)
        ///     })?;
        ///     Ok(user_id)
        /// }
        ///
        /// fn debit(me: &str, amount: f64, default: f64) -> Result<(Created<View>, f64), ApiError> {
        ///     // Structured errors + mixed store/find_row_by — name the error type.
        ///     let (created, balance): (Created<View>, f64) =
        ///         tx::<_, _, ApiError>(|| {
        ///             let bal_row = find_row_by("balances",
        ///                 "principal", store::Value::Text(me.to_string()))?;
        ///             let bal = bal_row
        ///                 .map(|r| r.text("balance").parse::<f64>().unwrap_or(0.0))
        ///                 .unwrap_or(default);
        ///             if bal < amount {
        ///                 // Raises a structured 409 from inside the tx.
        ///                 return Err(ApiError::conflict("insufficient balance"));
        ///             }
        ///             let id = store::insert("ledger", &[store::Column {
        ///                 name: "delta".into(),
        ///                 val: store::Value::Text(format!("{:.6}", -amount)),
        ///             }])?;
        ///             Ok((Created(View { id }), bal - amount))
        ///         })?;
        ///     Ok((created, balance))
        /// }
        /// ```
        fn tx<F, R, E>(f: F) -> ::core::result::Result<R, E>
        where
            F: ::core::ops::Fn() -> ::core::result::Result<R, E>,
            E: ::core::convert::From<store::StoreError>,
        {
            let max_attempts = __boogy_tx_max_attempts();
            let mut attempt: u32 = 1;
            loop {
                if let Err(e) = $bindings::boogy::platform::store::begin_transaction() {
                    return Err(E::from(e));
                }
                match f() {
                    Ok(r) => match $bindings::boogy::platform::store::commit_transaction() {
                        Ok(()) => return Ok(r),
                        // A serialization conflict means the store aborted this
                        // attempt and nothing landed — the one store error that is
                        // safe to re-run. It is produced only by the commit path;
                        // deterministic 409s (unique-index violations, "already
                        // exists") are `ConstraintViolation` and fall through to the
                        // catch-all below, as do `Poisoned` and `CommitUnknown`.
                        Err(store::StoreError::Conflict(_)) if attempt < max_attempts => {
                            attempt += 1;
                            // The refused commit already discarded the transaction,
                            // so the next iteration opens a fresh one. No delay: the
                            // conflict resolved because another writer committed, so
                            // re-reading now sees a settled value.
                            continue;
                        }
                        Err(store::StoreError::Conflict(m)) => {
                            if max_attempts <= 1 {
                                // Retry is switched off, so nothing was retried and
                                // there is no exhaustion to report: the conflict
                                // surfaces exactly as it did before auto-retry
                                // existed. This is what makes `1` a true kill
                                // switch rather than a one-attempt variant of the
                                // new behaviour.
                                return Err(E::from(store::StoreError::Conflict(m)));
                            }
                            // Out of attempts. This is congestion on a contended
                            // row, not a client mistake — 503 with a retry hint, so
                            // 409 keeps meaning "your write genuinely conflicts".
                            return Err(E::from(store::StoreError::TooContended(
                                ::std::format!(
                                    "transaction conflicted on all {max_attempts} attempts; \
                                     the rows it touches are contended, or the closure runs \
                                     longer than the store's transaction window"
                                ),
                            )));
                        }
                        Err(e) => return Err(E::from(e)),
                    },
                    Err(e) => {
                        // A closure error is deterministic — re-running it would
                        // fail the same way. Roll back and propagate unchanged.
                        let _ = $bindings::boogy::platform::store::rollback_transaction();
                        return Err(e);
                    }
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
                /// Override the `req.ctx` slot the loaded row is stashed
                /// at. The default slot is already the table name (see
                /// [`owns_resource`]), so this is only needed when two
                /// guards on the same route load the *same* table — they'd
                /// otherwise land on the same auto-derived slot and the
                /// second guard's stash would fail loudly at runtime
                /// instead of silently overwriting the first.
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
            /// The loaded row is stashed in `req.ctx` at a slot keyed by
            /// `table`, so stacking guards over *different* tables on one
            /// route — the natural shape for a route that owns two
            /// resources — can never collide: read each back with
            /// `req.ctx.require_at::<Row>(table)`, using the same table
            /// name the guard was built with. Use `.slot("name")` only to
            /// override that default (e.g. two guards over the *same*
            /// table need distinct names, since they'd otherwise both
            /// target that one auto-derived slot).
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
                        // Auto-key by table unless the caller overrode it
                        // via `.slot(...)`. This is what makes stacking two
                        // `owns_resource` guards over different tables on
                        // one route correct without any annotation: each
                        // guard's insert lands at its own table-named slot,
                        // so neither can overwrite the other's stash. Two
                        // guards over the SAME table (no override on
                        // either) still target the same slot on purpose —
                        // that's a genuine ambiguity, and `Ctx::insert_at`
                        // now fails loudly on it instead of silently
                        // overwriting.
                        let slot = if self.slot.is_empty() { self.table } else { self.slot };
                        req.ctx.insert_at(slot, row);
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
            /// the SDK falls back to a per-request slot resolved from an
            /// inbound `sk_*` bearer at request entry — before any route
            /// guard runs, so this works whether or not the route
            /// declares `api_key_routes::guard`, and regardless of where
            /// in the guard array it sits. The result is uniform:
            /// handlers and resource-level guards (`auth::owns_resource`,
            /// `auth::find_owned`) work the same regardless of credential
            /// type. The slot is cleared at request exit by the
            /// `wit_glue!` RAII guard.
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

            /// The caller's platform handle, when they consented to share
            /// it with this service. `None` for anonymous callers, API-key
            /// callers, and callers who haven't shared a handle.
            pub fn current_handle() -> ::core::option::Option<::std::string::String> {
                super::$bindings::boogy::platform::auth::current_identity()
                    .and_then(|i| i.handle)
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

            /// One BOUNDED page of the rows owned by the current principal,
            /// plus the cursor the next page resumes from.
            ///
            /// This returns a page and not a `Vec` on purpose. It used to
            /// return every row the principal owned, and on a table of a few
            /// thousand rows that is not slow but FATAL: measured against
            /// `memory_mb = 32`, `GET /api/notes` exhausted the guest heap and
            /// trapped on `handle_alloc_error`. The helper carried a doc
            /// retired-spelling: the "small bounded sets" label is
            /// obsolete — the helper returns a bounded `RowPage`. Quoted
            /// as the evidence that a doc comment is not an enforcement
            /// site.
            /// comment saying it was "for small bounded sets" at the time. A
            /// comment is not an enforcement site, so the shape is gone
            /// instead: [`PageRequest`](boogy_sdk::pagination::PageRequest)
            /// clamps its limit and keeps it private, so "give me all of them"
            /// cannot be written.
            ///
            /// Composes `current_principal()` with a `WHERE owner_col =
            /// principal` filter. Returns `ApiError::unauthenticated()` when
            /// the request is anonymous; store failures route through
            /// `StoreError → ApiError` so unique-violation / FK preservation
            /// works for any caller using `?` into a Result-typed handler.
            ///
            /// TYPED so the model's declared access patterns are reachable:
            /// the table comes from `M::TABLE` and the paging order from
            /// `M::schema()`. A model that declares `list_by(filter = <owner
            /// col>, …)` — or an index leading with the owner column — pages
            /// by that order, keyset, `_id` as the tiebreak. A model that
            /// declares NEITHER has no order to resume from, so it serves one
            /// page with no cursor and errors if the set does not fit; the
            /// remedy names the declaration to add.
            pub fn find_owned<M: $crate::model::Model>(
                owner_col: &str,
                page: &$crate::pagination::PageRequest,
            ) -> ::core::result::Result<
                $crate::pagination::RowPage,
                $crate::error::ApiError,
            > {
                let table = <M as $crate::model::Model>::TABLE;
                // Blank principal → unauthenticated. Otherwise a blank
                // `current_principal()` would issue `WHERE owner_col = ''` and
                // return every un-owned row as if the anonymous caller owned it.
                let principal = $crate::request_state::_request_principal_nonblank()
                    .ok_or_else($crate::error::ApiError::unauthenticated)?;
                // A token we cannot read is REFUSED, never silently dropped:
                // dropping it restarts the listing while the caller believes it
                // is continuing one, so the walk re-serves page one forever.
                if page.has_unreadable_token() {
                    return ::core::result::Result::Err($crate::error::ApiError::bad_request(
                        "cursor is not a listing position this service issued; omit it to start \
                         the listing from the beginning",
                    ));
                }
                let schema = <M as $crate::model::Model>::schema();
                let order = match $crate::store::read_strategy(&schema, owner_col) {
                    $crate::store::ReadStrategy::Keyset(o) => ::core::option::Option::Some(o),
                    // Unique owner column (one row per principal) or no declared
                    // order: one page is correct either way, and only going past
                    // it would need the order this model never declared.
                    _ => ::core::option::Option::None,
                };

                let ::core::option::Option::Some(o) = order else {
                    // No safe order to resume from, so this listing hands back
                    // no cursor — and therefore must never have been given one.
                    if page.cursor().is_some() {
                        return ::core::result::Result::Err($crate::error::ApiError::bad_request(
                            "this listing cannot be resumed: its model declares no order over the \
                             owner column, so it never issues a cursor",
                        ));
                    }
                    let filters = ::std::vec![
                        super::$bindings::boogy::platform::store::Filter {
                            column: owner_col.to_string(),
                            op: super::$bindings::boogy::platform::store::FilterOp::Eq,
                            val: super::$bindings::boogy::platform::store::Value::Text(
                                principal.clone(),
                            ),
                            in_values: ::core::option::Option::None,
                        }
                    ];
                    let res = super::$bindings::boogy::platform::store::find(
                        table,
                        &super::$bindings::boogy::platform::store::FindOptions {
                            filters,
                            order_by: ::std::vec![],
                            page: ::core::option::Option::Some(
                                super::$bindings::boogy::platform::store::Page {
                                    limit: page.limit() as u32,
                                    offset: 0,
                                },
                            ),
                            or_groups: ::std::vec![],
                            skip_total: super::SDK_SKIP_TOTAL,
                            group_cursor: ::core::option::Option::None,
                            counters: ::std::vec::Vec::new(),
                        },
                    )
                    .map_err($crate::store::StoreError::from_wit)?;
                    let rows: ::std::vec::Vec<$crate::store::Row> =
                        res.rows.iter().map(super::to_sdk_row).collect();
                    $crate::store::refuse_beyond_one_page(
                        "find_owned",
                        rows.len(),
                        res.total_count,
                        res.has_more,
                        "Declare how this table is listed for its owner column so it can \
                         page safely: add list_by(filter = \"<owner col>\", newest = \
                         \"<a timestamp or sequence column>\") to the model.",
                    )?;
                    return ::core::result::Result::Ok($crate::pagination::RowPage {
                        rows,
                        next_cursor: ::core::option::Option::None,
                    });
                };

                let dir = if o.desc {
                    $crate::store::SortDir::Desc
                } else {
                    $crate::store::SortDir::Asc
                };
                let (extra, kset_or) =
                    $crate::pagination::keyset_resume_filter(page.cursor(), &o.column, dir.clone());
                let mut filters = ::std::vec![
                    super::$bindings::boogy::platform::store::Filter {
                        column: owner_col.to_string(),
                        op: super::$bindings::boogy::platform::store::FilterOp::Eq,
                        val: super::$bindings::boogy::platform::store::Value::Text(
                            principal.clone(),
                        ),
                        in_values: ::core::option::Option::None,
                    }
                ];
                filters.extend(extra.iter().map(super::__boogy_sdk_filter_to_wit));
                let or_groups: ::std::vec::Vec<::std::vec::Vec<_>> = kset_or
                    .iter()
                    .map(|g| g.iter().map(super::__boogy_sdk_filter_to_wit).collect())
                    .collect();
                let wit_dir = super::__boogy_sdk_dir_to_wit(dir);
                let sort = ::std::vec![
                    super::$bindings::boogy::platform::store::OrderTerm::Column(
                        super::$bindings::boogy::platform::store::SortBy {
                            column: o.column.clone(),
                            dir: wit_dir.clone(),
                        },
                    ),
                    super::$bindings::boogy::platform::store::OrderTerm::Column(
                        super::$bindings::boogy::platform::store::SortBy {
                            column: "_id".to_string(),
                            dir: wit_dir,
                        },
                    ),
                ];

                // The HOST says where the listing ends (`has_more`), because
                // the host is the only party that can.
                //
                // Neither thing a guest could measure answers the question. The
                // host's own per-call row ceiling
                // (`BOOGY_STORE_MAX_PAGE_ROWS`) can sit BELOW the page asked
                // for, and it clamps silently — so a short page is not the end
                // (it may be the ceiling), and a full page is not "more
                // follows" (it may be the ceiling too). Overfetching by one
                // does not escape it: the overfetched ask is clamped as well.
                //
                // That left exactly one safe rule for the guest — treat an
                // EMPTY page as the only terminator — at the cost of one extra
                // request per complete listing. `has-more` retires the cost
                // rather than the safety: the answer now comes from the side of
                // the clamp that knows, so the last page carrying rows is also
                // the last request, and a truncation still has no way in.
                let limit = page.limit();
                let res = super::$bindings::boogy::platform::store::find(
                    table,
                    &super::$bindings::boogy::platform::store::FindOptions {
                        filters,
                        order_by: sort,
                        page: ::core::option::Option::Some(
                            super::$bindings::boogy::platform::store::Page {
                                limit: limit as u32,
                                offset: 0,
                            },
                        ),
                        or_groups,
                        skip_total: super::SDK_SKIP_TOTAL,
                        group_cursor: ::core::option::Option::None,
                        counters: ::std::vec::Vec::new(),
                    },
                )
                .map_err($crate::store::StoreError::from_wit)?;

                let rows: ::std::vec::Vec<$crate::store::Row> =
                    res.rows.iter().map(super::to_sdk_row).collect();
                let next_cursor = match rows.last() {
                    ::core::option::Option::Some(last) if res.has_more => {
                        let next = $crate::pagination::Cursor {
                            last_id: last.id().to_string(),
                            last_value: last.get(&o.column).to_json(),
                        };
                        // Same progress guarantee the batching loop carried: a
                        // page that ends on the row the previous page ended on
                        // would hand the caller a cursor that re-serves it
                        // forever. It surfaces as an error rather than as a
                        // silent stop, because a stop here returns a partial
                        // listing that looks complete.
                        $crate::pagination::keyset_advanced(
                            table, &o.column, page.cursor(), &next,
                        )
                        .map_err($crate::error::ApiError::internal)?;
                        ::core::option::Option::Some($crate::pagination::encode(&next))
                    }
                    _ => ::core::option::Option::None,
                };

                ::core::result::Result::Ok($crate::pagination::RowPage { rows, next_cursor })
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
        // The row for a scope key is claimed with a plain `insert`
        // BEFORE the handler runs, not written after it returns. The
        // table's `scope_key` unique index is what makes that a real
        // claim: a second concurrent `insert` for the same scope key
        // is refused by the store (`ConstraintViolation`), so two
        // overlapping requests can never both fall through to running
        // the handler — one always observes the other's row.
        //
        // Failure modes:
        //   * No header on the request → pass-through (no caching).
        //   * Claim succeeds (row didn't exist) → run handler, then
        //     finalize: 2xx updates the row with the real result,
        //     non-2xx deletes it (transient failures aren't cached,
        //     so the client can retry).
        //   * Claim fails, existing row is PENDING and fresh (within
        //     `STALE_CLAIM_SECONDS`) → 409 Conflict ("request already
        //     in progress"). This is the concurrent case: the other
        //     request is still running the handler right now.
        //   * Claim fails, existing row is PENDING and stale (holder
        //     crashed/trapped without finalizing) → attempt to steal
        //     it via a conditional update; on success, run the
        //     handler and finalize as above.
        //   * Claim fails, existing row is COMPLETE and fresh (within
        //     `DEFAULT_TTL_SECONDS`), fingerprint matches → replay the
        //     cached response.
        //   * Claim fails, existing row is COMPLETE and fresh,
        //     fingerprint MISMATCH → 409 Conflict ("Idempotency-Key
        //     reused with a different request"). Catches the common
        //     bug where a client retries with a different payload
        //     under the same key.
        //   * Claim fails, existing row is COMPLETE and expired (past
        //     `DEFAULT_TTL_SECONDS`) → reclaim the row and run the
        //     handler again, as if the key were fresh.

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
                let now = $bindings::boogy::platform::runtime::now_millis() as i64 / 1000;

                fn decode_replay(row: &$crate::store::Row) -> $crate::response::HttpResponse {
                    let status = row.int("status") as u16;
                    let headers: ::std::vec::Vec<(::std::string::String, ::std::string::String)> = row
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
                    $crate::response::HttpResponse { status, headers, body }
                }

                fn eq_filter(
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

                // Claim the scope key BEFORE running the handler. `scope_key`
                // carries a unique index (`idempotency_init_table`), so this
                // `insert` is the actual enforcement point: at most one
                // request can ever create this row. A concurrent second
                // caller gets `ConstraintViolation` here — a real error, not
                // a cache miss — so "two overlapping requests both see no
                // row and both run the handler" is not representable.
                let claim = $bindings::boogy::platform::store::insert(
                    $crate::idempotency::TABLE,
                    &[
                        $bindings::boogy::platform::store::Column {
                            name: "scope_key".into(),
                            val: $bindings::boogy::platform::store::Value::Text(scope.clone()),
                        },
                        $bindings::boogy::platform::store::Column {
                            name: "body_fingerprint".into(),
                            val: $bindings::boogy::platform::store::Value::Text(fp.clone()),
                        },
                        $bindings::boogy::platform::store::Column {
                            name: "status".into(),
                            val: $bindings::boogy::platform::store::Value::Integer(
                                $crate::idempotency::PENDING_STATUS,
                            ),
                        },
                        $bindings::boogy::platform::store::Column {
                            name: "headers_json".into(),
                            val: $bindings::boogy::platform::store::Value::Text(::std::string::String::new()),
                        },
                        $bindings::boogy::platform::store::Column {
                            name: "body_b64".into(),
                            val: $bindings::boogy::platform::store::Value::Text(::std::string::String::new()),
                        },
                        $bindings::boogy::platform::store::Column {
                            name: "created_at".into(),
                            val: $bindings::boogy::platform::store::Value::Integer(now),
                        },
                    ],
                );

                // `owns_claim == true` means: we hold the scope key
                // (outright, or by stealing an abandoned/expired row) and
                // must run the handler and finalize the row ourselves.
                // Every other outcome returns directly from this block.
                let owns_claim: bool = match claim {
                    Ok(_row_id) => true,
                    Err(_) => {
                        let existing = match find_row_by(
                            $crate::idempotency::TABLE,
                            "scope_key",
                            $bindings::boogy::platform::store::Value::Text(scope.clone()),
                        ) {
                            Ok(Some(row)) => row,
                            // The row our insert collided with is already
                            // gone (raced with a non-2xx release) or the
                            // cache is broken — run unguarded rather than
                            // reject the request.
                            Ok(None) => return inner(req),
                            Err(_) => return inner(req),
                        };
                        let row_status = existing.int("status");
                        let row_created_at = existing.int("created_at");

                        if row_status == $crate::idempotency::PENDING_STATUS {
                            let age = now - row_created_at;
                            if age <= $crate::idempotency::STALE_CLAIM_SECONDS {
                                // Genuinely concurrent: another request is
                                // running the handler for this key right
                                // now. Say so instead of silently re-running
                                // the handler ourselves.
                                return $crate::error::ApiError::conflict(
                                    "Idempotency-Key request is already in progress",
                                )
                                .into();
                            }
                            // Past the reclaim window: the original holder
                            // never finalized (trap, crash, kill). Steal the
                            // claim with a conditional update — succeeds
                            // only if the row is still that exact abandoned
                            // PENDING row.
                            let stolen = $bindings::boogy::platform::store::update_where(
                                $crate::idempotency::TABLE,
                                &[
                                    eq_filter("scope_key", $bindings::boogy::platform::store::Value::Text(scope.clone())),
                                    eq_filter("status", $bindings::boogy::platform::store::Value::Integer($crate::idempotency::PENDING_STATUS)),
                                    eq_filter("created_at", $bindings::boogy::platform::store::Value::Integer(row_created_at)),
                                ],
                                &[$bindings::boogy::platform::store::Column {
                                    name: "created_at".into(),
                                    val: $bindings::boogy::platform::store::Value::Integer(now),
                                }],
                            );
                            matches!(stolen, Ok(1))
                        } else {
                            // A completed response is cached. Honor
                            // DEFAULT_TTL_SECONDS: past it, the row is
                            // reclaimed for a fresh execution instead of
                            // replayed.
                            let age = now - row_created_at;
                            if age <= $crate::idempotency::DEFAULT_TTL_SECONDS {
                                let cached_fp = existing.text("body_fingerprint");
                                if cached_fp != fp {
                                    // Key reuse with a different body —
                                    // caller bug. Routes through
                                    // ApiError::conflict so the wire shape
                                    // matches every other error response
                                    // from the SDK (RFC 7807
                                    // application/problem+json).
                                    return $crate::error::ApiError::conflict(
                                        "Idempotency-Key reused with a different request payload",
                                    )
                                    .into();
                                }
                                return decode_replay(&existing);
                            }
                            let stolen = $bindings::boogy::platform::store::update_where(
                                $crate::idempotency::TABLE,
                                &[
                                    eq_filter("scope_key", $bindings::boogy::platform::store::Value::Text(scope.clone())),
                                    eq_filter("created_at", $bindings::boogy::platform::store::Value::Integer(row_created_at)),
                                ],
                                &[
                                    $bindings::boogy::platform::store::Column {
                                        name: "status".into(),
                                        val: $bindings::boogy::platform::store::Value::Integer(
                                            $crate::idempotency::PENDING_STATUS,
                                        ),
                                    },
                                    $bindings::boogy::platform::store::Column {
                                        name: "created_at".into(),
                                        val: $bindings::boogy::platform::store::Value::Integer(now),
                                    },
                                ],
                            );
                            matches!(stolen, Ok(1))
                        }
                    }
                };

                if !owns_claim {
                    // Lost a steal race against another reclaimer for an
                    // abandoned/expired row. Rare (crash-recovery path
                    // only); ask the caller to retry rather than loop.
                    return $crate::error::ApiError::conflict(
                        "Idempotency-Key request is already in progress",
                    )
                    .into();
                }

                // We own the claim: run the handler, then finalize.
                let resp = inner(req);
                let finished_at = $bindings::boogy::platform::runtime::now_millis() as i64 / 1000;

                if (200..300).contains(&resp.status) {
                    let headers_json = ::serde_json::to_string(&resp.headers)
                        .unwrap_or_else(|_| "[]".to_string());
                    let body_b64 = resp
                        .body
                        .as_deref()
                        .map(__sdk_base64_encode)
                        .unwrap_or_default();
                    let _ = $bindings::boogy::platform::store::update_where(
                        $crate::idempotency::TABLE,
                        &[eq_filter("scope_key", $bindings::boogy::platform::store::Value::Text(scope.clone()))],
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
                                val: $bindings::boogy::platform::store::Value::Integer(finished_at),
                            },
                        ],
                    );
                } else {
                    // Transient failure — release the claim instead of
                    // caching it, so a legitimate retry isn't blocked as
                    // "in progress" or (worse) replayed against an error.
                    let _ = $bindings::boogy::platform::store::delete_where(
                        $crate::idempotency::TABLE,
                        &[eq_filter("scope_key", $bindings::boogy::platform::store::Value::Text(scope))],
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
        //
        // `peer_fetch` is the checked default: a non-success response
        // (`status >= 400`) becomes `Err(PeerError::Rejected(resp))` so the
        // idiomatic `?` at the call site stops on a callee's explicit
        // rejection instead of silently continuing past it (audit finding
        // H-02 — see `crates/boogy-sdk/src/peer.rs` module docs). Reach for
        // `peer_fetch_raw` when a route genuinely needs the raw status
        // (a relay/proxy, or a caller mapping the callee's status to its own
        // domain error) — including any route that wants to inspect a
        // peer's status from inside a `tx(|| ...)` closure, which is exactly
        // the opt-in that shape now requires.
        fn peer_fetch(
            target: &str,
            request: &$crate::peer::PeerRequest,
        ) -> ::core::result::Result<$crate::peer::PeerResponse, $crate::peer::PeerError> {
            let resp = peer_fetch_raw(target, request)?;
            if resp.status >= 400 {
                ::core::result::Result::Err($crate::peer::PeerError::Rejected(resp))
            } else {
                ::core::result::Result::Ok(resp)
            }
        }

        /// Same call shape as [`peer_fetch`], but never classifies a
        /// non-success HTTP status as failure: returns `Ok` for ANY status
        /// the peer responds with. Only a genuine dispatch failure (target
        /// not found, denied by the target's ingress policy, timed out,
        /// depth exceeded, capability not granted, ...) is `Err` here.
        #[allow(dead_code)]
        fn peer_fetch_raw(
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
                Ok(resp) => {
                    // Destructured exhaustively on purpose. Adding a field to the WIT
// record then fails to compile HERE instead of being silently
// dropped — which is how `covering` went missing from IndexInfo.
// Never add `..` to this pattern.
                    let peer_bindings::PeerResponse { status, headers, body } = resp;
                    Ok($crate::peer::PeerResponse { status, headers, body })
                },
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
        // bindings call returns `SignError::CapabilityDenied` — the same
        // variant a signing write attempted inside a transaction produces.

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
                signing_bindings::SignError::CapabilityDenied(s) => {
                    $crate::signing::SignError::CapabilityDenied(s)
                }
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

        // -- Files bridging --
        //
        // Same shape as websockets: clean SDK types in, WIT types out. The
        // capability gate is host-side — with `[capabilities] files = false`
        // every call returns CapabilityDenied.
        //
        // NOTE the WIT function is `list-files`, not `list`: `list` is a
        // reserved WIT keyword. Authors still write `files_list`.

        fn __files_error_to_sdk(
            e: files_bindings::FilesError,
        ) -> $crate::files::FilesError {
            use $crate::files::FilesError as E;
            match e {
                files_bindings::FilesError::CapabilityDenied => E::CapabilityDenied,
                files_bindings::FilesError::UnknownCollection(c) => E::UnknownCollection(c),
                files_bindings::FilesError::NotFound => E::NotFound,
                files_bindings::FilesError::TooLarge(n) => E::TooLarge(n),
                files_bindings::FilesError::UnsupportedContentType(c) => {
                    E::UnsupportedContentType(c)
                }
                files_bindings::FilesError::QuotaExceeded => E::QuotaExceeded,
                files_bindings::FilesError::NotReady => E::NotReady,
                files_bindings::FilesError::InvalidKey(m) => E::InvalidKey(m),
                files_bindings::FilesError::RateLimited => E::RateLimited,
                files_bindings::FilesError::DeniedInTransaction => E::DeniedInTransaction,
                files_bindings::FilesError::Internal(m) => E::Internal(m),
            }
        }

        fn __files_info_to_sdk(i: files_bindings::FileInfo) -> $crate::files::FileInfo {
            $crate::files::FileInfo {
                key: i.key,
                collection: i.collection,
                size: i.size,
                content_type: i.content_type,
                owner: i.owner,
                created_at_millis: i.created_at_millis,
                ready: i.ready,
            }
        }

        fn files_create_upload(
            collection: &str,
            options: $crate::files::Upload,
        ) -> ::core::result::Result<$crate::files::UploadTicket, $crate::files::FilesError> {
            let wit = files_bindings::UploadOptions {
                key: options.key,
                content_type: options.content_type,
                owner: options.owner,
                ttl_seconds: options.ttl_seconds,
                size_hint: options.size_hint,
            };
            match files_bindings::create_upload(&collection.to_string(), &wit) {
                Ok(t) => Ok($crate::files::UploadTicket {
                    url: t.url,
                    method: t.method,
                    headers: t.headers,
                    key: t.key,
                    expires_at_millis: t.expires_at_millis,
                }),
                Err(e) => Err(__files_error_to_sdk(e)),
            }
        }

        fn files_stat(
            collection: &str,
            key: &str,
        ) -> ::core::result::Result<$crate::files::FileInfo, $crate::files::FilesError> {
            match files_bindings::stat(&collection.to_string(), &key.to_string()) {
                Ok(i) => Ok(__files_info_to_sdk(i)),
                Err(e) => Err(__files_error_to_sdk(e)),
            }
        }

        fn files_list(
            collection: &str,
            owner: Option<&str>,
            cursor: Option<&str>,
            limit: u32,
        ) -> ::core::result::Result<$crate::files::FilePage, $crate::files::FilesError> {
            match files_bindings::list_files(
                &collection.to_string(),
                owner.map(|o| o.to_string()).as_ref().map(|o| o.as_str()),
                cursor.map(|c| c.to_string()).as_ref().map(|c| c.as_str()),
                limit,
            ) {
                Ok(p) => Ok($crate::files::FilePage {
                    files: p.files.into_iter().map(__files_info_to_sdk).collect(),
                    next_cursor: p.next_cursor,
                }),
                Err(e) => Err(__files_error_to_sdk(e)),
            }
        }

        fn files_delete(
            collection: &str,
            key: &str,
        ) -> ::core::result::Result<(), $crate::files::FilesError> {
            files_bindings::delete(&collection.to_string(), &key.to_string())
                .map_err(__files_error_to_sdk)
        }

        /// Mint a URL for a client. Call this at RENDER time; never store it.
        fn files_url(
            collection: &str,
            key: &str,
            ttl_seconds: Option<u32>,
        ) -> ::core::result::Result<String, $crate::files::FilesError> {
            files_bindings::url(&collection.to_string(), &key.to_string(), ttl_seconds)
                .map_err(__files_error_to_sdk)
        }

        fn files_put_bytes(
            collection: &str,
            key: &str,
            content_type: &str,
            bytes: &[u8],
        ) -> ::core::result::Result<$crate::files::FileInfo, $crate::files::FilesError> {
            match files_bindings::put_bytes(
                &collection.to_string(),
                &key.to_string(),
                &content_type.to_string(),
                bytes,
            ) {
                Ok(i) => Ok(__files_info_to_sdk(i)),
                Err(e) => Err(__files_error_to_sdk(e)),
            }
        }

        fn files_read_bytes(
            collection: &str,
            key: &str,
        ) -> ::core::result::Result<Vec<u8>, $crate::files::FilesError> {
            files_bindings::read_bytes(&collection.to_string(), &key.to_string())
                .map_err(__files_error_to_sdk)
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

        // Resolve an inbound `Authorization: Bearer sk_*` credential against
        // the local `__boogy_api_keys` table, independent of whether the
        // consumer crate invoked `api_keys_glue!` at all — this fn only
        // needs the always-available `$crate::api_keys` logic helpers plus
        // the store glue this same macro already emits (`find_row_by`,
        // `__boogy_update_row`), so it works even for crates with no
        // `api_key_routes` module (the store lookup then just finds no
        // matching table/row and this returns `None`, same as an anonymous
        // request).
        //
        // Called once at request entry (see `Guest::handle` below) — NOT
        // from a per-route guard — so principal resolution for `sk_*`
        // callers no longer depends on a guard called `api_key_routes::guard`
        // having run first. This is what fixes the guard-ordering footgun
        // (an author writing `[owns_resource, api_key_routes::guard]` used
        // to 401 every sk_* request; either order works now because the
        // resolution below has already happened before any guard runs).
        fn __boogy_resolve_api_key_principal(
            req: &$crate::Request,
        ) -> ::core::option::Option<(::std::string::String, ::std::vec::Vec<::std::string::String>)> {
            let bearer = $crate::api_keys::parse_bearer(req)?;
            // Validate format up-front (CRC + structure) — saves a store
            // round-trip on garbage input.
            $crate::api_keys::parse(bearer).ok()?;

            let prefix = $crate::api_keys::compute_lookup_prefix(bearer);
            let row = find_row_by(
                $crate::api_keys::TABLE,
                "prefix",
                $bindings::boogy::platform::store::Value::Text(prefix),
            )
            .ok()??;
            let now = $crate::api_keys::__unix_now_for_glue();
            if !$crate::api_keys::verify_against_row(bearer, &row, now) {
                return ::core::option::Option::None;
            }
            let dto = $crate::api_keys::parse_row(&row).ok()?;
            let issuer = row.text("created_by");
            if issuer.is_empty() {
                return ::core::option::Option::None;
            }

            // Best-effort last_used_at update. Failures here don't affect
            // authorization.
            let _ = __boogy_update_row(
                $crate::api_keys::TABLE,
                row.id(),
                &[("last_used_at".to_string(), $crate::store::Val::Integer(now as i64))],
            );

            ::core::option::Option::Some((issuer, dto.scopes))
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
                let wit_identity = $bindings::boogy::platform::auth::current_identity();
                let _state_guard = $crate::request_state::enter(
                    wit_identity.as_ref().map(|i| i.principal.clone()),
                );

                // What this invocation must do about the DECLARED schema.
                //
                // A declared schema belongs to a deployed VERSION, not to a
                // request — it cannot change between two requests served by the
                // same version. So the platform resolves it once, when the
                // version is deployed, and every later invocation is told to
                // SKIP the pass: no `list-tables`, no `list-indexes`, no
                // `list-rollups`. Skipped, not cached — there is nothing to
                // serve from memory because nothing is read.
                //
                // `apply-only` is the resolution pass itself: run the
                // declaration and return WITHOUT dispatching a route, so the
                // platform never has to guess whether a synthetic request hit
                // a catch-all handler.
                //
                // The mode is host-attested; a caller cannot supply it.
                let __schema_mode =
                    $bindings::boogy::platform::runtime::current_schema_mode();
                if !::core::matches!(
                    __schema_mode,
                    $bindings::boogy::platform::runtime::SchemaMode::Skip
                ) {
                    // Phase order is load-bearing. Declaration completes before
                    // the reconcile, which decides what to drop from the FULL
                    // declared set; the reconcile completes before migrations,
                    // which may rely on a declared index; bootstrap is last,
                    // because seeding needs the final schema.
                    // Start from empty: this buffer is a snapshot of THIS
                    // pass, and the pass re-runs per request for a deployment
                    // whose schema was never resolved.
                    let _ = __boogy_take_schema_conflicts();
                    let mut __schema = $crate::schema_decl::Schema::new();
                    <$api_struct as $crate::Api>::schema(&mut __schema);
                    for __t in __schema.tables() {
                        create_table_from(__t);
                    }
                    __boogy_reconcile_indexes();
                    __boogy_reconcile_rollups();
                    <$api_struct as $crate::Api>::migrate();
                    <$api_struct as $crate::Api>::bootstrap();
                }
                if ::core::matches!(
                    __schema_mode,
                    $bindings::boogy::platform::runtime::SchemaMode::ApplyOnly
                ) {
                    // The response header is the PROOF the pass ran. Without it
                    // a component built before this contract existed — which
                    // ignores the mode and serves the synthetic request as an
                    // ordinary route — would be indistinguishable from one that
                    // resolved its schema, and the platform would mark a
                    // deployment resolved on the strength of a 404.
                    let mut __headers = ::std::vec![(
                        ::std::string::String::from("x-boogy-schema-applied"),
                        ::std::string::String::from("1"),
                    )];
                    // Drained AFTER the pass above (declaration, reconcile,
                    // migrate, bootstrap) has fully run, so this holds exactly
                    // that pass's conflicts — not the empty buffer a drain any
                    // earlier would read. Only a `Conflict` ever reaches this
                    // list; a `Warn` (a harmless stored-but-undeclared column)
                    // is logged and never recorded, so it can never turn into a
                    // fatal header. The applied header above keeps its existing
                    // meaning regardless: a pass can legitimately apply some
                    // tables and conflict on another.
                    let __schema_conflicts = __boogy_take_schema_conflicts();
                    if let ::core::option::Option::Some(__joined) =
                        $crate::schema_resolve::schema_conflict_header_value(&__schema_conflicts)
                    {
                        __headers.push((
                            ::std::string::String::from("x-boogy-schema-conflict"),
                            __joined,
                        ));
                    }
                    return $bindings::exports::boogy::platform::http_handler::HttpResponse {
                        status: 204,
                        headers: __headers,
                        body: ::core::option::Option::None,
                    };
                }
                let sdk_req = __boogy_to_sdk_request(&req);

                // Resolve an `sk_*` bearer up front, before any guard runs
                // (F-10): PASETO already wins unconditionally regardless of
                // guard order because it's read straight from the WIT auth
                // capability above, with no per-route opt-in guard needed to
                // populate it. This puts `sk_*` on the same footing — resolved
                // once here, so `auth::current_principal()`, `owns_resource`,
                // `find_owned`/`load_owned`, and `api_key_routes::guard` all
                // see the same answer no matter which guard runs first, or
                // whether `api_key_routes::guard` is in the route's guard
                // array at all. Skipped when a WIT identity is already
                // present (PASETO wins; matches `_request_principal`'s
                // precedence and avoids a needless store round-trip).
                if wit_identity.is_none() {
                    if let Some((issuer, scopes)) = __boogy_resolve_api_key_principal(&sdk_req) {
                        $crate::request_state::_set_fallback_principal(Some(issuer));
                        $crate::request_state::_set_fallback_scopes(Some(scopes));
                    }
                }

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
