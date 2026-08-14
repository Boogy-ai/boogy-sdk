//! Store helpers for ergonomic data access.
//!
//! These types are SDK-side mirrors of the WIT store types. Each API's template
//! converts between these and the generated WIT bindings.
//!
//! Usage (in API code, with the names `wit_glue!` emits):
//! ```ignore
//! fn title_of(id: u64) -> Result<String, ApiError> {
//!     // `Table` is a builder — it is registered by `create_table_from`,
//!     // it has no `.create()` of its own.
//!     create_table_from(&Table::new("todos").text("title").boolean("done"));
//!     // `get_row` wraps the WIT `store::get(table, id)` and hands back a
//!     // typed `Row` — `store::get` takes the id by value, not by reference.
//!     let row = get_row("todos", id)?.ok_or_else(ApiError::not_found)?;
//!     Ok(row.text("title"))
//! }
//! ```

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::error::ApiError;

/// Total attempts `tx` makes when the store aborts a commit with a
/// serialization conflict — the original attempt plus two retries. Used when
/// the platform did not supply `BOOGY_TX_MAX_ATTEMPTS` in the environment, or
/// supplied something unparseable.
pub const DEFAULT_TX_MAX_ATTEMPTS: u32 = 3;

/// Upper clamp `tx` applies to the platform-supplied attempt budget. There is no
/// sleep between attempts, so the budget is the only bound on how long a
/// contended row is re-attempted; a very large value would spin a request for
/// its whole time budget and then time out, which is worse than surfacing the
/// conflict. The platform clamps to the same ceiling, so the two cannot
/// disagree.
pub const MAX_TX_MAX_ATTEMPTS: u32 = 10;

/// Structured error from a store operation.
///
/// The host carries a typed error across the WIT `store-error` variant;
/// the SDK mirrors those arms here so handlers discriminate on the
/// variant rather than string-matching message text.
///
/// The `From<StoreError> for ApiError` impl produces the canonical
/// status mapping (QuotaExceeded → 507, NotFound → 404, Conflict /
/// ConstraintViolation / VersionMismatch / CommitUnknown / Poisoned → 409,
/// InvalidArgument → 400,
/// Unsupported → 501, Timeout → 504, ResourceExhausted / TooContended → 503,
/// Internal → 500), so handler code
/// can `.map_err` store calls into `ApiError` without thinking about it.
///
/// Five arms share HTTP 409 and **only [`StoreError::Conflict`] may be
/// retried**: it is a serialization abort, so nothing landed and re-running is
/// safe. The others are 409 for wire compatibility but must not be retried —
/// [`StoreError::ConstraintViolation`] is deterministic (a unique index, FK,
/// check, not-null, or "already exists"), [`StoreError::CommitUnknown`] is
/// ambiguous, and [`StoreError::Poisoned`] means a transaction participant
/// failed. Match the specific arm; a `_ =>` catch-all silently folds these
/// together and is the bug the separate variants exist to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    QuotaExceeded(String),
    NotFound(String),
    Conflict(String),
    ConstraintViolation(String),
    InvalidArgument(String),
    Unsupported(String),
    Timeout(String),
    VersionMismatch(String),
    /// Ambiguous commit (`commit_unknown_result`): the write MAY or MAY NOT
    /// have been applied. Maps to HTTP 409, but unlike a clean conflict it is
    /// NOT safe to blindly retry — reconcile state (query it) first, since a
    /// retry could double-apply. The message body carries the distinction.
    CommitUnknown(String),
    /// Transient: a host concurrency cap was hit (e.g. too many open
    /// cross-service transactions). Maps to HTTP 503 — retry shortly.
    ResourceExhausted(String),
    /// The transaction was rolled back because a PARTICIPANT failed — a store
    /// op somewhere in the `peer::fetch` call tree errored, so the owner
    /// refused to commit.
    ///
    /// Maps to HTTP 409 like [`StoreError::Conflict`], but the two are NOT
    /// interchangeable and that is the whole point of the variant. A
    /// `Conflict` is a serialization abort: nothing landed, so re-running the
    /// transaction is safe. `Poisoned` is **not** safe to re-run — the
    /// participant that failed would execute again, turning one failure into
    /// one per attempt, and on a cross-service call tree, one callee execution
    /// per attempt. Fix the participant, don't retry.
    Poisoned(String),
    /// Transient: the transaction ran out of retry attempts against a
    /// contended row. Maps to HTTP 503 — retry shortly.
    ///
    /// Sibling of [`StoreError::ResourceExhausted`], not a replacement: the
    /// wire behaviour is identical (503, retry later) so no caller needs a new
    /// code path, but the cause differs and the cause is what you act on.
    /// `ResourceExhausted` means the platform is saturated with concurrent
    /// transactions; `TooContended` means *this transaction's own footprint* is
    /// contended — either a hot row it writes, or a whole table a search inside
    /// it took as its read set because the planner could not serve that search
    /// from an index. The answer is the data model (finer keys, counter columns)
    /// or narrowing the read, not more capacity.
    TooContended(String),
    Internal(String),
}

/// Bridge implemented by the guest-generated `store-error` binding (via
/// the `wit_glue!` macro) so `from_wit` stays binding-agnostic in this
/// crate — boogy-sdk generates no WIT bindings of its own.
pub trait IntoStoreError {
    fn into_store_error(self) -> StoreError;
}

impl StoreError {
    pub fn from_wit<E: IntoStoreError>(e: E) -> Self {
        e.into_store_error()
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::QuotaExceeded(m) | StoreError::NotFound(m)
            | StoreError::Conflict(m) | StoreError::ConstraintViolation(m)
            | StoreError::InvalidArgument(m) | StoreError::Unsupported(m)
            | StoreError::Timeout(m) | StoreError::VersionMismatch(m)
            | StoreError::CommitUnknown(m)
            | StoreError::ResourceExhausted(m)
            | StoreError::Poisoned(m)
            | StoreError::TooContended(m)
            | StoreError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Wire micro-format embedding a machine-readable [`crate::error::cause`]
/// inside a `StoreError::ResourceExhausted` message.
///
/// The WIT `resource-exhausted` variant carries only a `string` — it has no
/// room for a typed sub-cause, and extending it would ripple the WIT
/// binding across every host/guest crossing for what is, underneath, three
/// call sites in one host file. The host is the only side that knows WHICH
/// of its three concurrency caps tripped (the tx-admission cap, the
/// per-request op ceiling, the per-origin op-rate throttle all reject with
/// this same variant), so it prefixes the message with a stable
/// `"<cause>: "` token from [`crate::error::cause`]; [`untag`] strips it
/// back out in `From<StoreError> for ApiError`.
///
/// This is NOT prose-matching a human-authored message that could be
/// reworded for style — it's a small, tested, same-repo wire contract
/// between two ends of the same crossing (see
/// `resource_exhausted_tag_round_trips` and the
/// `resource_exhausted_untags_all_three_host_causes` /
/// `resource_exhausted_falls_back_gracefully_when_untagged` tests below).
pub mod resource_exhausted_tag {
    const SEP: &str = ": ";

    /// Every cause `untag` recognizes. Kept as one list so a new
    /// `ResourceExhausted` emission site can't be added without either
    /// reusing an existing cause or updating this (and its own test).
    const KNOWN: &[&str] = &[
        crate::error::cause::TX_ADMISSION_EXHAUSTED,
        crate::error::cause::STORE_OP_CEILING_EXCEEDED,
        crate::error::cause::STORE_OP_RATE_LIMITED,
    ];

    /// Host-side: embed `cause` into a `ResourceExhausted` message.
    pub fn tag(cause: &str, msg: impl std::fmt::Display) -> String {
        format!("{cause}{SEP}{msg}")
    }

    /// SDK-side: split a possibly-tagged message into `(cause, rest)`.
    /// Falls back to `(None, msg)` unchanged when the message doesn't start
    /// with a recognized token — defensive against the host and SDK
    /// drifting out of sync, rather than panicking or guessing a cause.
    pub fn untag(msg: &str) -> (Option<&'static str>, &str) {
        for &c in KNOWN {
            if let Some(rest) = msg.strip_prefix(c).and_then(|r| r.strip_prefix(SEP)) {
                return (Some(c), rest);
            }
        }
        (None, msg)
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        let msg = e.to_string();
        match e {
            StoreError::QuotaExceeded(_)       => ApiError::insufficient_storage(msg),
            StoreError::NotFound(_)            => ApiError::not_found(),
            StoreError::Conflict(_)            => ApiError::conflict(msg),
            StoreError::ConstraintViolation(_) => ApiError::constraint_violation(msg),
            StoreError::InvalidArgument(_)     => ApiError::invalid_argument(msg),
            StoreError::Unsupported(_)         => ApiError::unsupported(msg),
            StoreError::Timeout(_)             => ApiError::timeout(msg),
            StoreError::VersionMismatch(_)     => ApiError::conflict(msg),
            // 409, but NOT blindly retryable — the ambiguity is conveyed by the
            // message body, not a distinct status (see `CommitUnknown` doc).
            StoreError::CommitUnknown(_)       => ApiError::conflict(msg),
            // F-07: three host emission sites, three remedies, one WIT
            // variant. `resource_exhausted_tag::untag` recovers WHICH one so
            // the wire `cause` field can distinguish them (was previously
            // impossible from any client's point of view).
            StoreError::ResourceExhausted(m)   => {
                let (cause, text) = resource_exhausted_tag::untag(&m);
                match cause {
                    // Retrying an IDENTICAL request trips the same
                    // per-request ceiling again — a retry hint here would be
                    // actively misleading.
                    Some(c) if c == crate::error::cause::STORE_OP_CEILING_EXCEEDED => {
                        ApiError::service_unavailable_with_cause(text, c, None)
                    }
                    Some(c) => ApiError::service_unavailable_with_cause(text, c, Some(1)),
                    // Untagged/unrecognized: degrade to the pre-F-07 shape
                    // (still a real 503, just no cause) instead of guessing.
                    None => ApiError::service_unavailable(text),
                }
            }
            // 409 like a plain conflict — the caller's transaction genuinely
            // failed — but NOT retryable; see the variant doc.
            StoreError::Poisoned(_)            => ApiError::conflict(msg),
            // F-07: same `kind`/`status` as `ResourceExhausted` by design
            // (both are "transient store congestion, retry"), but a
            // DIFFERENT cause — this one IS a data-model/query-shape signal,
            // unlike any of the three `ResourceExhausted` causes.
            StoreError::TooContended(_)        => {
                ApiError::service_unavailable_with_cause(msg, crate::error::cause::TX_CONTENDED, Some(1))
            }
            StoreError::Internal(_)            => ApiError::internal(msg),
        }
    }
}

/// MCP / JSON-RPC handlers work in `RpcError` rather than `ApiError`.
/// Routing the `StoreError → ApiError → RpcError` chain through this
/// `From` impl keeps the conversion lossless: every status code
/// (404 / 409 / 500) survives the trip into JSON-RPC's
/// application-error code band.
impl From<StoreError> for crate::rpc::RpcError {
    fn from(e: StoreError) -> Self {
        let api: ApiError = e.into();
        api.into()
    }
}

/// Per-table encryption setting for `Table` (mirrors the WIT `encryption-mode`).
///
/// **Dormant feature — encrypted tables are on hold.** Only `None` is
/// functional; `Enabled` (via [`Table::encrypted`]) is currently rejected by the
/// host on every engine, so creating an encrypted table fails. The plumbing is
/// kept so the feature can be revived, but no backend implements encryption yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncryptionMode {
    /// Plaintext at rest (default). The only functional value.
    #[default]
    None,
    /// Platform-managed encryption at rest. **Dormant** — currently rejected by
    /// the host; do not rely on it.
    Enabled,
}

/// Column type for table definitions.
#[derive(Debug, Clone, Copy)]
pub enum ColType {
    Text,
    Integer,
    Real,
    Blob,
    Boolean,
}

/// Foreign-key cascade action for `ON DELETE` / `ON UPDATE`.
#[derive(Debug, Clone, Copy)]
pub enum CascadeAction {
    /// `NO ACTION` — default. The DB rejects modifications that would
    /// orphan a child row.
    NoAction,
    /// `RESTRICT` — same as NoAction in SQLite (rejected immediately).
    Restrict,
    /// `CASCADE` — propagate the parent's delete/update to the child.
    Cascade,
    /// `SET NULL` — set the child's FK column to NULL when the parent is
    /// deleted/updated. Requires the child column to be nullable.
    SetNull,
}

/// A column-level foreign-key constraint.
#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub references_table: String,
    pub references_column: String,
    pub on_delete: CascadeAction,
    pub on_update: CascadeAction,
}

/// Column definition for table creation.
#[derive(Debug, Clone)]
pub struct ColDef {
    pub name: String,
    pub col_type: ColType,
    pub nullable: bool,
    pub unique: bool,
    pub references: Option<ForeignKey>,
    /// Conflict-free counter column. Stored in its own cell rather than in the
    /// packed row, and mutated by an atomic add that registers no read-conflict
    /// range — which is what lets concurrent increments compose instead of
    /// conflicting.
    ///
    /// A counter column **cannot back an index**: index maintenance needs the
    /// previous value to remove the old entry, and an atomic add never reads
    /// one. The `Model` derive rejects that combination at compile time.
    ///
    /// The column must be `ColType::Integer` (64-bit signed) and the delta must
    /// be an integer; the add **wraps** on overflow rather than erroring or
    /// saturating.
    ///
    /// On a `Model`, the corresponding field is **read-only**: reads merge the
    /// real value in, writes go only through the increment path. The derive
    /// leaves counter fields out of `to_columns`, so an update never mentions
    /// the column and cannot overwrite it with a stale value.
    pub counter: bool,
    /// Value a read resolves this column to when the row has no value for it —
    /// the equivalent of `DEFAULT 'pending'` in a `CREATE TABLE`.
    ///
    /// Set with [`Table::default`] on the create path, or with
    /// [`ColumnSpec::default`] when adding a column to a live table.
    ///
    /// **Setting or changing a default never rewrites stored rows.** A write
    /// that omits the column records the default in force at that moment, so
    /// that row keeps the old value if the default later changes. Rows that
    /// predate the column entirely have no value recorded and are resolved
    /// against the current default on every read, so those do observe a change.
    /// Neither case is a backfill.
    ///
    /// A column carrying a default satisfies the not-null requirement even when
    /// it is not nullable: it can never end up value-less. Writing an *explicit*
    /// `Val::Null` to a non-nullable column is still rejected — omitting the
    /// column resolves to the default, an explicit null is a caller error.
    ///
    /// Literal values only; there are no expression defaults (no "now()").
    pub default: Option<Val>,
}

/// Index definition for a table.
#[derive(Debug, Clone)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    /// Covering: the index entry carries a copy of the row, so reads ordered by
    /// this index don't fetch the row separately. Faster reads, more write cost
    /// and storage — use it on hot read paths (e.g. a feed's `created_at` index).
    pub covering: bool,
}

/// A sort direction over a column, expressed in use-case English by the
/// `newest`/`oldest`/`highest`/`lowest` helpers — never "ASC/DESC" at the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub column: String,
    pub desc: bool,
}

/// Newest-first over a timestamp column (descending).
pub fn newest(column: &str) -> Order { Order { column: column.into(), desc: true } }
/// Oldest-first over a timestamp column (ascending).
pub fn oldest(column: &str) -> Order { Order { column: column.into(), desc: false } }
/// Highest-first over a score/quantity column (descending).
pub fn highest(column: &str) -> Order { Order { column: column.into(), desc: true } }
/// Lowest-first over a score/quantity column (ascending).
pub fn lowest(column: &str) -> Order { Order { column: column.into(), desc: false } }

/// A declared way the table is queried. The resolver turns these into the
/// physical index shapes the planner needs — authors never name index shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessPattern {
    /// Rows where `filter == v`, ordered by `order`, paginated.
    ListBy { filter: String, order: Order },
    /// Top rows by `order`, paginated, no filter.
    RankedBy { order: Order },
    /// The unique row where `column == v` (point lookup).
    LookupBy { column: String },
    /// Membership on a junction/side table: rows where `tag == v`, exposing
    /// `refs` (the parent id) to join back.
    TaggedBy { tag: String, refs: String },
}

/// The ordering a model DECLARED for listing rows filtered by `filter_col` —
/// its `list_by(filter = filter_col, newest|oldest = ...)`.
///
/// `None` means no declaration, and that is the point: without one, no single
/// index covers the filter AND an order, so a multi-page read of that filter
/// has no stable sequence to page along. Callers turn `None` into a refusal
/// instead of paging by nothing — which is what they did before, returning a
/// row twice or not at all while reporting success.
pub fn declared_list_order(schema: &Table, filter_col: &str) -> Option<Order> {
    schema.access_patterns.iter().find_map(|p| match p {
        AccessPattern::ListBy { filter, order } if filter == filter_col => Some(order.clone()),
        _ => None,
    })
}

/// Refuse a result set that does not fit in ONE page.
///
/// The "return all rows" helpers used to loop with OFFSET paging and no sort.
/// Offset paging needs a stable total order and had none — no sort was
/// requested, and offsets shift under concurrent writes regardless — so a row
/// could come back twice or not at all while the call still returned `Ok` with
/// a plausible count. This makes that impossible: one page, or an error.
///
/// `Internal` (500), deliberately neither 4xx nor 501:
///
/// * **Not 4xx.** The end user's request was valid. A 4xx would tell them they
///   erred and imply retrying or changing it helps. Neither is true.
/// * **Not 501.** "Not Implemented" blames the platform for a missing
///   capability. Keyset pagination exists and is documented; this service did
///   not use it, and pointing at the platform sends the reader to the wrong
///   place.
///
/// It is a service defect surfaced at runtime because the data grew, so the
/// detail carries what the AUTHOR needs — they are the only one who can act.
pub fn refuse_beyond_one_page(
    what: &str,
    got: usize,
    total: u64,
    remedy: &str,
) -> Result<(), StoreError> {
    if total > got as u64 {
        return Err(StoreError::Internal(format!(
            "{what} matched {total} rows but reads at most one page ({got}). It is for small \
             bounded sets and cannot page safely: there is no stable order across pages, so \
             continuing would silently duplicate or skip rows. {remedy}"
        )));
    }
    Ok(())
}

/// How a filtered multi-row read may safely be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadStrategy {
    /// The filter column is UNIQUE (`#[lookup_by]`): at most one row matches,
    /// so there is nothing to page and no order to declare.
    PointLookup,
    /// The model declared `list_by` for this filter: one index covers the
    /// filter AND this order, so pages are bounded ordered ranges.
    Keyset(Order),
    /// Neither a unique column nor a covering composite. One page is still
    /// safe — a set that fits needs no order to be returned correctly — but
    /// continuing PAST a page is exactly where the missing order would have
    /// been needed, so the read stops there and says so.
    SinglePageOnly,
}

/// Decide how a read filtered on `filter_col` may be executed.
///
/// The distinction that matters: a UNIQUE column needs no order at all, while
/// a non-unique one is unsafe to page without a declared composite. Conflating
/// them breaks every point lookup — and breaks it at RUNTIME, since the call
/// still compiles.
pub fn read_strategy(schema: &Table, filter_col: &str) -> ReadStrategy {
    let unique = schema.access_patterns.iter().any(|p| {
        matches!(p, AccessPattern::LookupBy { column } if column == filter_col)
    });
    if unique {
        return ReadStrategy::PointLookup;
    }
    if let Some(order) = declared_list_order(schema, filter_col) {
        return ReadStrategy::Keyset(order);
    }
    // An explicit composite leading with the filter carries the same guarantee
    // as `list_by`, declared the other way: `index(cols = [filter, order])`.
    // Ignoring this form would stop services that ARE correctly indexed.
    // Ascending, since a raw index has no declared direction and ascending is
    // the index's own order.
    for ix in &schema.indices {
        if ix.columns.first().map(|c| c.as_str()) == Some(filter_col) {
            if let Some(second) = ix.columns.get(1) {
                return ReadStrategy::Keyset(Order { column: second.clone(), desc: false });
            }
        }
    }
    ReadStrategy::SinglePageOnly
}

/// Table definition builder.
pub struct Table {
    pub name: String,
    pub columns: Vec<ColDef>,
    pub indices: Vec<Index>,
    pub access_patterns: Vec<AccessPattern>,
    pub encryption: EncryptionMode,
}

impl Table {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), columns: vec![], indices: vec![], access_patterns: vec![], encryption: EncryptionMode::None }
    }

    /// Declare a non-unique index over one or more columns. Index names
    /// must be globally unique across the API's database.
    /// `Table::new("posts").text("author").integer("created_at").index("idx_posts_author_created", &["author", "created_at"])`.
    pub fn index(mut self, name: &str, columns: &[&str]) -> Self {
        self.indices.push(Index {
            name: name.to_string(),
            columns: columns.iter().map(|s| s.to_string()).collect(),
            unique: false,
            covering: false,
        });
        self
    }

    /// Declare a non-unique **covering** index: its entry stores a copy of the
    /// row, so reads ordered by this index skip the per-row fetch. Faster reads
    /// at the cost of write throughput + storage — use on hot read paths.
    pub fn covering_index(mut self, name: &str, columns: &[&str]) -> Self {
        self.indices.push(Index {
            name: name.to_string(),
            columns: columns.iter().map(|s| s.to_string()).collect(),
            unique: false,
            covering: true,
        });
        self
    }

    /// Declare: "rows where `filter == v`, ordered by `order`, paginated."
    /// Derives a covering composite index — the fast keyset list shape.
    pub fn list_by(mut self, filter: &str, order: Order) -> Self {
        self.access_patterns.push(AccessPattern::ListBy { filter: filter.into(), order });
        self
    }
    /// Declare: "top rows by `order`, paginated" (a global feed/leaderboard).
    pub fn ranked_by(mut self, order: Order) -> Self {
        self.access_patterns.push(AccessPattern::RankedBy { order });
        self
    }
    /// Declare: "the unique row where `column == v`" (point lookup; enforces uniqueness).
    pub fn lookup_by(mut self, column: &str) -> Self {
        self.access_patterns.push(AccessPattern::LookupBy { column: column.into() });
        self
    }
    /// Declare (on a junction/side table): "rows tagged `tag`, exposing `refs`
    /// to join back." Derives the covering side-table index.
    pub fn tagged_by(mut self, tag: &str, refs: &str) -> Self {
        self.access_patterns.push(AccessPattern::TaggedBy { tag: tag.into(), refs: refs.into() });
        self
    }
    /// The physical index set this table needs: explicit `.index()`/… declarations
    /// merged with the resolved access patterns. Returns build-time diagnostics
    /// (warnings/errors) for the caller to surface.
    pub fn resolved_indices(&self) -> (Vec<Index>, Vec<crate::schema_resolve::Diagnostic>) {
        crate::schema_resolve::resolve(&self.name, &self.access_patterns, &self.indices)
    }

    /// Declare a unique index over one or more columns. Useful for
    /// compound uniqueness (e.g. `(user_id, email)`) that a column-level
    /// `.unique()` can't express.
    pub fn unique_index(mut self, name: &str, columns: &[&str]) -> Self {
        self.indices.push(Index {
            name: name.to_string(),
            columns: columns.iter().map(|s| s.to_string()).collect(),
            unique: true,
            covering: false,
        });
        self
    }

    /// Declare the conventional owner column ([`crate::DEFAULT_OWNER_COL`])
    /// **and** a non-unique index on it, in one call.
    ///
    /// Use this for any table whose rows are owned by a principal and served
    /// through the `auth::owns_resource` / `auth::find_owned` / `auth::load_owned`
    /// helpers. Those helpers filter by the owner column on every "list my X" /
    /// ownership check; without an index the store must full-scan the table to
    /// satisfy the filter. Declaring the owner column via this helper (instead of
    /// a bare `.text(DEFAULT_OWNER_COL)`) emits the owner index by default so the
    /// ownership-filtered path is index-backed.
    ///
    /// The index is named `idx_<table>_owner`. Equivalent to:
    /// `t.text(DEFAULT_OWNER_COL).index("idx_<table>_owner", &[DEFAULT_OWNER_COL])`.
    /// Idempotent at create time (guarded by `list_indexes`), so adding it to an
    /// existing API is backward-compatible — the index is created on next
    /// `init_tables` run and never duplicated.
    pub fn owned(self) -> Self {
        let col = crate::DEFAULT_OWNER_COL;
        let idx = Self::owner_index_name(&self.name);
        self.text(col).index(&idx, &[col])
    }

    /// Declare a custom owner column AND its index (for tables that don't use the
    /// conventional [`crate::DEFAULT_OWNER_COL`] name). Index name is
    /// `idx_<table>_<owner_col>`.
    pub fn owned_by(self, owner_col: &str) -> Self {
        let idx = format!("idx_{}_{}", self.name, owner_col);
        self.text(owner_col).index(&idx, &[owner_col])
    }

    /// The conventional owner-index name for a table (`idx_<table>_owner`).
    /// Exposed so a migration can `create_index` the same index on a
    /// pre-existing table that predates [`Table::owned`].
    pub fn owner_index_name(table: &str) -> String {
        format!("idx_{table}_owner")
    }

    /// Mark this table for platform-managed encryption at rest (create-time only).
    ///
    /// **DORMANT — do not use yet.** Encrypted tables are on hold: no engine
    /// implements encryption, so creating a table marked `.encrypted()` currently
    /// **fails** with "encrypted tables are not currently available (feature on
    /// hold)". This builder + the WIT option are kept so the feature can be
    /// revived without an API change; until then, omit it.
    pub fn encrypted(mut self) -> Self {
        self.encryption = EncryptionMode::Enabled;
        self
    }

    pub fn text(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Text, nullable: false, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn integer(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Integer, nullable: false, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn real(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Real, nullable: false, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn boolean(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Boolean, nullable: false, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn blob(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Blob, nullable: false, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn nullable_text(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Text, nullable: true, unique: false, references: None, counter: false, default: None });
        self
    }

    pub fn nullable_integer(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Integer, nullable: true, unique: false, references: None, counter: false, default: None });
        self
    }

    /// Declare the most recently added column `UNIQUE`, enforced by a unique
    /// index over it. Chains naturally after a column declaration:
    /// `Table::new("users").text("email").unique()`.
    ///
    /// Two writes of the same value fail the second with
    /// [`StoreError::ConstraintViolation`] (HTTP 409). Uniqueness is enforced
    /// by the index, so this emits one — a bare column flag enforces nothing.
    ///
    /// The index is resolved and named by the same path as every other index
    /// on the table, which makes this idempotent and mergeable: `.unique()`
    /// twice, or `.unique()` alongside `.lookup_by(col)` / an explicit
    /// `.index(...)` over the same column, converge on one index that is
    /// unique. For uniqueness across *several* columns use
    /// [`unique_index`](Self::unique_index).
    ///
    /// Panics if no column has been added yet — calling `.unique()` on an
    /// empty table is a programming error.
    pub fn unique(mut self) -> Self {
        let last = self.columns.last_mut()
            .expect("Table::unique() called before any column was added");
        last.unique = true;
        let column = last.name.clone();
        // Empty name: the resolver canonicalizes index names itself and warns
        // about any hand-typed one, so declaring a name here would emit a
        // diagnostic on a call the author never named. Keying on the column
        // tuple is also what makes repeat calls and pattern overlap merge
        // instead of colliding.
        self.indices.push(Index {
            name: String::new(),
            columns: vec![column],
            unique: true,
            covering: false,
        });
        self
    }

    /// Give the most recently added column a default — the create-table
    /// equivalent of SQL's `status TEXT DEFAULT 'pending'`.
    ///
    /// ```ignore
    /// Table::new("orders")
    ///     .text("status").default(Val::Text("pending".into()))
    ///     .integer("retries").default(Val::Integer(0))
    /// ```
    ///
    /// A row written without the column reads the default back. Setting or
    /// changing a default **never rewrites stored rows**: a write that omits the
    /// column records the default in force at that moment, while rows that
    /// predate the column resolve against the current default on every read.
    ///
    /// A defaulted column satisfies the not-null requirement even when it is not
    /// nullable — it cannot end up value-less — which is the sane way to declare
    /// a required column. Writing an explicit `Val::Null` is still rejected.
    ///
    /// Literal values only. There are no expression defaults.
    ///
    /// Mirrors [`ColumnSpec::default`], which does the same for a column added
    /// to an already-created table.
    ///
    /// Panics if no column has been added yet — calling `.default(...)` on an
    /// empty table is a programming error.
    pub fn default(mut self, v: Val) -> Self {
        let last = self.columns.last_mut()
            .expect("Table::default() called before any column was added");
        last.default = Some(v);
        self
    }

    /// Declare the most recently added column as a foreign-key reference
    /// to another table's column. Defaults `ON DELETE` and `ON UPDATE` to
    /// `NO ACTION`; chain `.on_delete(...)` / `.on_update(...)` to change.
    ///
    /// `Table::new("comments").text("post_id").references("posts", "_id")`
    pub fn references(mut self, table: &str, column: &str) -> Self {
        let last = self.columns.last_mut()
            .expect("Table::references() called before any column was added");
        last.references = Some(ForeignKey {
            references_table: table.to_string(),
            references_column: column.to_string(),
            on_delete: CascadeAction::NoAction,
            on_update: CascadeAction::NoAction,
        });
        self
    }

    /// Set the most recently added column's foreign-key `ON DELETE` action.
    /// Requires `.references(...)` to have been called first.
    pub fn on_delete(mut self, action: CascadeAction) -> Self {
        let fk = self.columns.last_mut()
            .and_then(|c| c.references.as_mut())
            .expect("Table::on_delete() called before .references()");
        fk.on_delete = action;
        self
    }

    /// Set the most recently added column's foreign-key `ON UPDATE` action.
    pub fn on_update(mut self, action: CascadeAction) -> Self {
        let fk = self.columns.last_mut()
            .and_then(|c| c.references.as_mut())
            .expect("Table::on_update() called before .references()");
        fk.on_update = action;
        self
    }
}

/// Typed value for store columns.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Text(String),
    Integer(i64),
    Real(f64),
    Blob(Vec<u8>),
    Boolean(bool),
}

impl Val {
    pub fn as_text(&self) -> String {
        match self {
            Val::Text(s) => s.clone(),
            Val::Integer(i) => i.to_string(),
            Val::Real(f) => f.to_string(),
            Val::Boolean(b) => b.to_string(),
            Val::Null => String::new(),
            Val::Blob(_) => String::new(),
        }
    }

    pub fn as_int(&self) -> i64 {
        match self {
            Val::Integer(i) => *i,
            Val::Boolean(true) => 1,
            Val::Boolean(false) => 0,
            _ => 0,
        }
    }

    pub fn as_real(&self) -> f64 {
        match self {
            Val::Real(f) => *f,
            Val::Integer(i) => *i as f64,
            _ => 0.0,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Val::Boolean(b) => *b,
            Val::Integer(i) => *i != 0,
            Val::Text(s) => s == "true" || s == "1",
            _ => false,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        match self {
            Val::Null => JsonValue::Null,
            Val::Text(s) => JsonValue::String(s.clone()),
            Val::Integer(i) => serde_json::json!(*i),
            Val::Real(f) => serde_json::json!(*f),
            Val::Boolean(b) => JsonValue::Bool(*b),
            Val::Blob(b) => JsonValue::String(base64_encode(b)),
        }
    }
}

/// Column specification for `add_column` migrations.
///
/// Constructed with [`col`] and customized via the builder methods.
/// This is the SDK mirror of the WIT `column-def` record used by
/// `add-column`, with ergonomics matching the [`Table`] builder.
#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub name: String,
    pub col_type: ColType,
    pub nullable: bool,
    pub unique: bool,
    pub default: Option<Val>,
}

/// Build a [`ColumnSpec`] for use in `add_column` migrations.
///
/// Default flags: `nullable = true`, `unique = false`, `default = None`.
/// Chain builder methods to customize:
/// ```ignore
/// use boogy_sdk::store::{col, ColType, Val};
///
/// col("score", ColType::Integer).not_null().default(Val::Integer(0));
/// ```
pub fn col(name: impl Into<String>, col_type: ColType) -> ColumnSpec {
    ColumnSpec { name: name.into(), col_type, nullable: true, unique: false, default: None }
}

impl ColumnSpec {
    /// Set a default value for the column.
    pub fn default(mut self, v: Val) -> Self {
        self.default = Some(v);
        self
    }

    /// Reject an **explicitly supplied** null for this column.
    ///
    /// Narrower than SQL's `NOT NULL`, and the difference matters:
    ///
    /// - Writing `Val::Null` to the column — on insert or update — is rejected
    ///   with a 409 [`StoreError::ConstraintViolation`]. Deterministic: the
    ///   retry loop will not retry it, because it would fail identically.
    /// - **Omitting** the column is rejected on a write that CREATES a row
    ///   (insert, and the row-creating arm of either upsert), with the same
    ///   code. On update it is still fine and means "leave it alone".
    ///
    /// So every row does have a value for the column. Omission is refused
    /// because it is not equivalent to a zero: a row stored without a value
    /// reads that column back as the type's zero value, but no index entry was
    /// written for it, so the same row is invisible to any indexed query over
    /// that column — present to a lookup, absent to a seek.
    ///
    /// The cost lands on `upsert_increment` and `upsert`: their `key ∪ always
    /// ∪ on_insert` (plus the counter, for `upsert_increment`) must cover
    /// every non-nullable column without a default, because the first call
    /// for a key is an insert. Three ways to satisfy that, not equivalent:
    /// give the column a [`default`](Self::default) when it has a sensible
    /// **static** starting value — that satisfies the rule (the engine
    /// materializes it into the row) and, unlike either column list, does not
    /// overwrite it on every later call; put a value that is **computed** at
    /// call time (a timestamp, a derived id) in `on_insert` instead — written
    /// only by the row-creating call, never rewritten after; reach for
    /// `always` only when the value must genuinely change on every call, since
    /// an `always` column is rewritten on the update arm too. A defaulted
    /// column is exempt from the explicit-null check too.
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// **Enforces nothing — do not use.** Sets a column flag that no write
    /// path reads: uniqueness is enforced by a unique *index*, and
    /// `add_column` creates no index.
    ///
    /// It cannot simply start creating one either. A column added to an
    /// existing table takes the same value (the default, or null) in every row
    /// already there, so a unique index over it would be violated by the table
    /// as it stands. The correct migration is three steps, in order:
    ///
    /// 1. `add_column(table, col(name, ty))` — no uniqueness claim;
    /// 2. backfill every existing row with a distinct value;
    /// 3. `create_index(table, IndexDef { unique: true, .. })` over it.
    ///
    /// On a table you are *creating*, declare it at the source instead:
    /// `Table::new(t).text(c).unique()`, or
    /// [`Table::unique_index`](Table::unique_index) for several columns.
    #[deprecated(
        note = "enforces nothing on add_column; backfill, then create a unique index — \
                see the doc comment"
    )]
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }
}

/// Column metadata returned by `list_columns`.
///
/// SDK mirror of the WIT `column-info` record.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: ColType,
    pub nullable: bool,
}

/// Index metadata returned by `list_indexes`.
///
/// SDK mirror of the WIT `index-def` record. Same fields as
/// `Table::index()` produces — indexes have no create-vs-read asymmetry,
/// so this struct also matches the SDK's `Index` (used by the Table
/// builder).
#[derive(Debug, Clone)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    /// Whether the index entry carries a copy of the row.
    ///
    /// Reported because it is part of what distinguishes one index from
    /// another: an index reconcile that could not see this flag would treat a
    /// covering index and a plain one over the same columns as identical and
    /// leave the wrong one in place.
    pub covering: bool,
}

/// SDK mirror of the WIT `table-info` record. What `list_tables`
/// returns: lightweight per-table introspection (name + live
/// column count + user-index count). Callers who want full schema
/// detail use `list_columns(name)` / `list_indexes(name)`.
#[derive(Debug, Clone)]
pub struct TableInfo {
    pub name: String,
    pub column_count: u32,
    pub index_count: u32,
}

/// A row from the store with typed column accessors.
pub struct Row {
    pub columns: Vec<(String, Val)>,
}

impl Row {
    pub fn get(&self, name: &str) -> &Val {
        for (n, v) in &self.columns {
            if n == name {
                return v;
            }
        }
        &Val::Null
    }

    pub fn text(&self, name: &str) -> String {
        self.get(name).as_text()
    }

    pub fn int(&self, name: &str) -> i64 {
        self.get(name).as_int()
    }

    pub fn real(&self, name: &str) -> f64 {
        self.get(name).as_real()
    }

    pub fn bool(&self, name: &str) -> bool {
        self.get(name).as_bool()
    }

    pub fn id(&self) -> u64 {
        self.int("_id") as u64
    }

    /// Serialize selected fields to a JSON object.
    pub fn to_json(&self, fields: &[&str]) -> JsonValue {
        let mut map = serde_json::Map::new();
        // Always include _id as "id"
        map.insert("id".to_string(), serde_json::json!(self.id()));
        for field in fields {
            map.insert(field.to_string(), self.get(field).to_json());
        }
        JsonValue::Object(map)
    }

    /// Serialize all fields to a JSON object.
    pub fn to_json_all(&self) -> JsonValue {
        let mut map = serde_json::Map::new();
        for (name, val) in &self.columns {
            let key = if name == "_id" { "id".to_string() } else { name.clone() };
            map.insert(key, val.to_json());
        }
        JsonValue::Object(map)
    }
}

/// Pagination result with rows and total count.
#[derive(Serialize)]
pub struct Page<T: Serialize> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

/// Comparison operator for a [`Filter`] predicate.
///
/// SDK-owned mirror of the WIT store `filter-op` enum. Used by
/// [`crate::pagination::keyset_resume_filter`] to build keyset resume
/// conditions that callers convert to their WIT-generated equivalents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
    NotLike,
    IsNull,
    IsNotNull,
    In,
}

/// Sort direction for a [`SortBy`] clause.
///
/// SDK-owned mirror of the WIT store `sort-dir` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// A single column filter predicate: `column op val`.
///
/// SDK-owned mirror of the WIT store `filter` record. Returned by
/// [`crate::pagination::keyset_resume_filter`] for callers to convert
/// and splice into their WIT-typed `FindOptions`.
///
/// `in_values` is populated only for `FilterOp::In` (the host reads
/// `in_values`, not `val`, for IN-list predicates); `val` is unused
/// in that case. For all other ops, `in_values` is `None` and `val`
/// carries the scalar.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub val: Val,
    pub in_values: Option<Vec<Val>>,
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_arm_maps_to_expected_api_status() {
        let cases = [
            (StoreError::QuotaExceeded("x".into()), 507),
            (StoreError::NotFound("x".into()), 404),
            (StoreError::Conflict("x".into()), 409),
            (StoreError::ConstraintViolation("x".into()), 409),
            (StoreError::InvalidArgument("x".into()), 400),
            (StoreError::Unsupported("x".into()), 501),
            (StoreError::Timeout("x".into()), 504),
            (StoreError::VersionMismatch("x".into()), 409),
            (StoreError::CommitUnknown("x".into()), 409),
            (StoreError::ResourceExhausted("x".into()), 503),
            (StoreError::Poisoned("x".into()), 409),
            (StoreError::TooContended("x".into()), 503),
            (StoreError::Internal("x".into()), 500),
        ];
        for (e, want) in cases {
            let api: ApiError = e.into();
            assert_eq!(api.status, want);
        }
    }

    #[test]
    fn owned_declares_owner_column_and_index() {
        let t = Table::new("notes").text("title").owned();
        // The conventional owner column is present...
        assert!(
            t.columns.iter().any(|c| c.name == crate::DEFAULT_OWNER_COL),
            "owned() must add the DEFAULT_OWNER_COL column",
        );
        // ...and an index over exactly that column, with the conventional name.
        let idx = t
            .indices
            .iter()
            .find(|i| i.name == "idx_notes_owner")
            .expect("owned() must declare idx_<table>_owner");
        assert_eq!(idx.columns, vec![crate::DEFAULT_OWNER_COL.to_string()]);
        assert!(!idx.unique, "owner index is non-unique (many rows per owner)");
        assert_eq!(Table::owner_index_name("notes"), "idx_notes_owner");
    }

    #[test]
    fn owned_by_uses_custom_owner_column_and_index_name() {
        let t = Table::new("posts").text("body").owned_by("author_id");
        assert!(t.columns.iter().any(|c| c.name == "author_id"));
        let idx = t
            .indices
            .iter()
            .find(|i| i.name == "idx_posts_author_id")
            .expect("owned_by() must declare idx_<table>_<col>");
        assert_eq!(idx.columns, vec!["author_id".to_string()]);
    }

    /// `Poisoned` and `Conflict` are the same status on the wire (409) — the
    /// distinction lives in the VARIANT, which is what a retry loop keys off.
    /// If these two ever collapse into one variant, retrying a poisoned tx
    /// would re-execute the participant that already failed.
    #[test]
    fn poisoned_is_a_409_but_a_distinct_variant_from_conflict() {
        let poisoned: ApiError = StoreError::Poisoned("participant failed".into()).into();
        let conflict: ApiError = StoreError::Conflict("serialization abort".into()).into();
        assert_eq!(poisoned.status, 409);
        assert_eq!(conflict.status, 409, "same wire status is deliberate");
        assert_ne!(
            StoreError::Poisoned("m".into()),
            StoreError::Conflict("m".into()),
            "poison must not be representable as a serialization conflict",
        );
        assert!(poisoned
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("participant failed"));
    }

    /// F-07 (2026-08 platform audit): `TooContended` and `ResourceExhausted`
    /// used to be byte-identical on the wire — same `kind`/`title`/`status`,
    /// same `detail` shape, nothing distinguishing them. That was pinned as
    /// *intentional* by this test's previous version. It wasn't: the two
    /// have different remedies (data-model fix vs. back off), so a client
    /// needs to tell them apart. `status`/`kind`/`title` staying identical
    /// is still correct (splitting `kind` would be a breaking wire change,
    /// per the audit's own remedy ranking) — `cause` is what must now
    /// differ.
    #[test]
    fn too_contended_and_resource_exhausted_share_kind_but_not_cause() {
        let contended: ApiError = StoreError::TooContended("row is hot".into()).into();
        let exhausted: ApiError =
            StoreError::ResourceExhausted(resource_exhausted_tag::tag(
                crate::error::cause::TX_ADMISSION_EXHAUSTED,
                "too many concurrent transactions; retry shortly",
            ))
            .into();
        assert_eq!(contended.status, 503);
        assert_eq!(contended.status, exhausted.status);
        assert_eq!(contended.kind, exhausted.kind, "both stay one problem class on the wire");
        assert_eq!(contended.title, exhausted.title);
        assert_ne!(
            contended.cause, exhausted.cause,
            "F-07: these two causes must be distinguishable on the wire"
        );
        assert_eq!(contended.cause.as_deref(), Some(crate::error::cause::TX_CONTENDED));
        assert_eq!(exhausted.cause.as_deref(), Some(crate::error::cause::TX_ADMISSION_EXHAUSTED));
    }

    /// `TooContended` genuinely means "retry shortly" (the SDK's own
    /// auto-retry just gave up) — carries a real `Retry-After`, not just
    /// prose.
    #[test]
    fn too_contended_carries_a_real_retry_after_hint() {
        let api: ApiError = StoreError::TooContended("row is hot".into()).into();
        assert_eq!(api.retry_after_secs, Some(1));
    }

    /// The three `ResourceExhausted` emission sites are the SAME WIT variant
    /// (the type system can't tell them apart), so the host tags the cause
    /// into the message (`resource_exhausted_tag`) and this conversion
    /// untags it. This is the F-07 fix for "three distinct emission sites,
    /// three different remedies, collapsed onto one wire shape."
    #[test]
    fn resource_exhausted_untags_all_three_host_causes() {
        use crate::error::cause::{
            STORE_OP_CEILING_EXCEEDED, STORE_OP_RATE_LIMITED, TX_ADMISSION_EXHAUSTED,
        };
        let cases = [
            (TX_ADMISSION_EXHAUSTED, "too many concurrent transactions; retry shortly", Some(1)),
            (STORE_OP_CEILING_EXCEEDED, "request exceeded the store-op ceiling of 1", None),
            (STORE_OP_RATE_LIMITED, "store op-rate limit exceeded for this origin; retry shortly", Some(1)),
        ];
        for (cause, human, want_retry) in cases {
            let tagged = resource_exhausted_tag::tag(cause, human);
            let api: ApiError = StoreError::ResourceExhausted(tagged).into();
            assert_eq!(api.status, 503);
            assert_eq!(api.cause.as_deref(), Some(cause), "cause for {human:?}");
            assert_eq!(
                api.detail.as_deref(),
                Some(human),
                "the human-readable text, with the cause tag stripped back out"
            );
            assert_eq!(api.retry_after_secs, want_retry, "retry hint for {cause}");
        }
    }

    /// Defensive fallback: an untagged (or unrecognized) `ResourceExhausted`
    /// message must not panic or silently misclassify — it degrades to the
    /// pre-F-07 generic behaviour (no cause, but still a real 503 with the
    /// message preserved).
    #[test]
    fn resource_exhausted_falls_back_gracefully_when_untagged() {
        let api: ApiError = StoreError::ResourceExhausted("some future message".into()).into();
        assert_eq!(api.status, 503);
        assert_eq!(api.cause, None);
        assert_eq!(api.detail.as_deref(), Some("some future message"));
    }

    #[test]
    fn resource_exhausted_tag_round_trips() {
        use crate::error::cause::TX_ADMISSION_EXHAUSTED;
        let tagged = resource_exhausted_tag::tag(TX_ADMISSION_EXHAUSTED, "retry shortly");
        let (cause, rest) = resource_exhausted_tag::untag(&tagged);
        assert_eq!(cause, Some(TX_ADMISSION_EXHAUSTED));
        assert_eq!(rest, "retry shortly");
    }
}

#[cfg(test)]
mod access_pattern_types_tests {
    use super::*;
    #[test]
    fn order_helpers_build_expected_dir() {
        assert_eq!(newest("created_at"), Order { column: "created_at".into(), desc: true });
        assert_eq!(oldest("created_at"), Order { column: "created_at".into(), desc: false });
        assert_eq!(highest("score"),    Order { column: "score".into(), desc: true });
        assert_eq!(lowest("score"),     Order { column: "score".into(), desc: false });
    }
}

#[cfg(test)]
mod table_verbs_tests {
    use super::*;
    #[test]
    fn verbs_resolve_to_indexes() {
        let t = Table::new("posts")
            .text("author").integer("created_at").text("slug")
            .list_by("author", newest("created_at"))
            .lookup_by("slug");
        let (idx, diags) = t.resolved_indices();
        assert!(diags.is_empty());
        let names: Vec<&str> = idx.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"ix_posts_author_created_at"));
        assert!(names.contains(&"ix_posts_slug"));
        assert!(idx.iter().find(|i| i.name == "ix_posts_slug").unwrap().unique);
    }
    #[test]
    fn explicit_and_pattern_indexes_coexist() {
        let t = Table::new("posts").integer("created_at")
            .ranked_by(newest("created_at"))
            .index("hand_idx", &["created_at"]); // explicit on same tuple → merged covering
        let (idx, _) = t.resolved_indices();
        assert_eq!(idx.len(), 1);
        assert!(idx[0].covering);
    }

    /// `.unique()` used to be the last live way to write an unenforced
    /// uniqueness declaration: it set `ColDef.unique`, which reached the store
    /// and was read by no write path — the only uniqueness probe is over the
    /// table's INDEX list. A duplicate insert into a column declared `.unique()`
    /// simply succeeded. Nothing observable distinguished writing it from not.
    ///
    /// It now resolves to a real UNIQUE index, so the declaration is true.
    #[test]
    fn unique_resolves_to_a_real_unique_index() {
        let t = Table::new("users").text("email").unique();
        let (idx, diags) = t.resolved_indices();
        assert_eq!(idx.len(), 1, "`.unique()` must emit an index, not just a flag: {idx:?}");
        assert_eq!(idx[0].columns, vec!["email".to_string()]);
        assert!(idx[0].unique, "the emitted index must be UNIQUE — that is the whole declaration");
        assert!(
            diags.is_empty(),
            "the emitted index must carry no hand-typed name to warn about: {diags:?}"
        );
    }

    /// `.unique()` twice on the same column, and `.unique()` alongside a
    /// declared access pattern over it, must converge on ONE index. The
    /// resolver keys on the column tuple and ORs the unique flag, so both
    /// shapes collapse — but only if `.unique()` goes through the resolver
    /// rather than pushing a pre-named index of its own.
    #[test]
    fn unique_is_idempotent_and_merges_with_a_pattern_on_the_same_column() {
        let (idx, _) = Table::new("users").text("email").unique().unique().resolved_indices();
        assert_eq!(idx.len(), 1, "repeating `.unique()` must not create a second index: {idx:?}");

        let (idx, _) = Table::new("users").text("email").unique().lookup_by("email").resolved_indices();
        assert_eq!(idx.len(), 1, "`.unique()` and `.lookup_by` on one column are one index: {idx:?}");
        assert!(idx[0].unique);
    }

    /// The uniqueness is per-column, not per-table: declaring it on one column
    /// must not silently mark a sibling.
    #[test]
    fn unique_applies_only_to_the_column_it_follows() {
        let t = Table::new("users").text("email").unique().text("nickname");
        let (idx, _) = t.resolved_indices();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].columns, vec!["email".to_string()]);
    }

    /// `Table::default` attaches the value to the column it FOLLOWS, and to no
    /// other — the same per-column contract as `unique`.
    ///
    /// The negative half is the load-bearing one: a builder that stored the
    /// default on the table (or on every column) would satisfy a test that only
    /// looked at `status`.
    #[test]
    fn default_applies_only_to_the_column_it_follows() {
        let t = Table::new("orders")
            .text("status").default(Val::Text("pending".into()))
            .integer("retries").default(Val::Integer(0))
            .text("note");

        let get = |name: &str| {
            t.columns.iter().find(|c| c.name == name)
                .unwrap_or_else(|| panic!("column {name} missing"))
                .default
                .clone()
        };
        assert_eq!(get("status"), Some(Val::Text("pending".into())));
        assert_eq!(get("retries"), Some(Val::Integer(0)));
        assert_eq!(get("note"), None, "a column declared after the defaults must carry none");
    }

    /// Negative control for the builder: a table built with no `.default(..)`
    /// call carries no defaults at all.
    #[test]
    fn a_table_built_without_defaults_carries_none() {
        let t = Table::new("orders").text("status").integer("retries");
        assert!(
            t.columns.iter().all(|c| c.default.is_none()),
            "no column may acquire a default that was never declared",
        );
    }

    /// A model that declared `list_by(filter, order)` has ONE index covering
    /// the filter and the order together — the only thing that makes a
    /// multi-page read of that filter pageable in a stable sequence.
    #[test]
    fn declared_list_order_finds_the_pattern_for_that_filter() {
        let schema = Table::new("things")
            .text("owner_principal")
            .text("created_at")
            .list_by("owner_principal", newest("created_at"));

        let order = declared_list_order(&schema, "owner_principal")
            .expect("the declared pattern must be found");
        assert_eq!(order.column, "created_at");
        assert!(order.desc, "newest() is descending");
    }

    /// No declaration → None, so the caller can refuse. Defaulting to some
    /// order here would recreate the bug this exists to stop: an order no
    /// index covers is not a stable sequence to page along.
    #[test]
    fn declared_list_order_is_none_when_the_model_never_declared_one() {
        let schema = Table::new("things").text("owner_principal");
        assert!(declared_list_order(&schema, "owner_principal").is_none());
    }

    /// Another column's pattern must not be borrowed: its composite leads
    /// with the wrong column, so it cannot serve this filter's ordered range.
    #[test]
    fn declared_list_order_does_not_match_another_columns_pattern() {
        let schema = Table::new("things")
            .text("team_id")
            .text("created_at")
            .list_by("team_id", newest("created_at"));
        assert!(declared_list_order(&schema, "owner_principal").is_none());
    }

    /// A set that fits in one page is returned, not refused — the guard must
    /// not break the small bounded reads these helpers exist for.
    #[test]
    fn one_page_of_results_is_allowed_through() {
        assert!(refuse_beyond_one_page("find_owned", 40, 40, "use keyset").is_ok());
        assert!(refuse_beyond_one_page("find_owned", 40, 12, "use keyset").is_ok());
    }

    /// More rows than one page → refuse. The old behaviour paged on with
    /// OFFSET and no sort, which could return a row twice or skip it while
    /// still reporting success. A loud error replaces a quiet wrong answer.
    #[test]
    fn more_than_one_page_is_refused() {
        let err = refuse_beyond_one_page("find_owned", 1000, 4321, "use keyset")
            .expect_err("a set larger than one page must be refused");
        assert!(matches!(err, StoreError::Internal(_)), "got {err:?}");
    }

    /// The message must carry what the AUTHOR needs to act: which helper, how
    /// big the set actually is, and the remedy. They are the only one who can
    /// fix it — the end user's request was valid.
    #[test]
    fn the_refusal_names_the_helper_the_size_and_the_remedy() {
        let err = refuse_beyond_one_page("find_owned", 1000, 4321, "switch to keyset paging")
            .expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("find_owned"), "must name the helper: {msg}");
        assert!(msg.contains("4321"), "must name the real size: {msg}");
        assert!(msg.contains("switch to keyset paging"), "must name the remedy: {msg}");
    }

    /// No SDK helper may page by OFFSET any more.
    ///
    /// A source assertion because the helpers live inside `wit_glue!` and are
    /// emitted into the user crate, so there is nothing to call here. The
    /// invariant is still exact: `offset += n` is the offset-walk idiom, and
    /// every one of them had the same defect — paging with no sort, so a
    /// concurrent write could return a row twice or skip it while the call
    /// reported success.
    ///
    /// Five helpers had it: find_all_rows, db_find_by, load_has_many,
    /// find_rows_by, find_owned. Two now page by a model-DECLARED sort column
    /// (keyset); three refuse past one page. Either way the offset walk is
    /// gone, and if one comes back this fails.
    #[test]
    fn no_sdk_helper_paginates_by_offset() {
        let glue = include_str!("glue.rs");
        let offset_walks = glue.matches("offset += n").count();
        assert_eq!(
            offset_walks, 0,
            "an offset-paginated loop is back in the SDK glue ({offset_walks} found). Offset \
             paging has no stable order across pages here, so it silently duplicates or skips \
             rows; use a declared list_by sort column (keyset) or refuse past one page.",
        );
    }

    /// A `#[lookup_by]` column is UNIQUE: the read returns at most one row, so
    /// there is nothing to page and no order to declare. Requiring `list_by`
    /// here would break every point lookup in the example services
    /// (`Room::SLUG`, `Poll::PUBLIC_ID`, …) at runtime while still compiling.
    #[test]
    fn a_unique_lookup_column_needs_no_declared_order() {
        let schema = Table::new("rooms").text("slug").lookup_by("slug");
        assert_eq!(read_strategy(&schema, "slug"), ReadStrategy::PointLookup);
    }

    /// A non-unique filter WITH a declared order pages along that order.
    #[test]
    fn a_declared_list_by_column_pages_by_its_order() {
        let schema = Table::new("things")
            .text("owner_principal")
            .text("created_at")
            .list_by("owner_principal", newest("created_at"));
        match read_strategy(&schema, "owner_principal") {
            ReadStrategy::Keyset(order) => {
                assert_eq!(order.column, "created_at");
                assert!(order.desc);
            }
            other => panic!("expected Keyset, got {other:?}"),
        }
    }

    /// No declaration → ONE page only, not an outright refusal. A small set
    /// fits in a page and is perfectly safe to return; only continuing PAST a
    /// page is unsafe, because that is where the missing order would have been
    /// needed. Failing a three-row lookup would break correct callers to
    /// punish a bug they do not have.
    #[test]
    fn an_undeclared_non_unique_filter_is_limited_to_one_page() {
        let schema = Table::new("things").text("owner_principal");
        assert_eq!(read_strategy(&schema, "owner_principal"), ReadStrategy::SinglePageOnly);
    }

    /// An explicit composite covering (filter, order) is the same guarantee as
    /// `list_by`, declared a different way — `index(cols = [filter, order])`.
    /// Missing it would refuse services that ARE correctly indexed, like
    /// tokenfeed's `idx_invest_post` over (post_id, invested_at).
    #[test]
    fn an_explicit_composite_leading_with_the_filter_is_keyset_too() {
        let schema = Table::new("post_investments")
            .index("idx_invest_post", &["post_id", "invested_at"]);
        match read_strategy(&schema, "post_id") {
            ReadStrategy::Keyset(order) => assert_eq!(order.column, "invested_at"),
            other => panic!("an explicit composite must page by its second column, got {other:?}"),
        }
    }
}
