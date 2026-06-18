//! Store helpers for ergonomic data access.
//!
//! These types are SDK-side mirrors of the WIT store types. Each API's template
//! converts between these and the generated WIT bindings.
//!
//! Usage (in API code, with the template-provided `into_*` converters):
//! ```ignore
//! Table::new("todos").text("title").boolean("done").create(&store);
//! let row = Row::from(store::get("todos", &id)?);
//! let title = row.text("title");
//! ```

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::error::ApiError;

/// Structured error from a store operation.
///
/// The host carries a typed error across the WIT `store-error` variant;
/// the SDK mirrors those arms here so handlers discriminate on the
/// variant rather than string-matching message text.
///
/// The `From<StoreError> for ApiError` impl produces the canonical
/// status mapping (QuotaExceeded → 507, NotFound → 404, Conflict /
/// ConstraintViolation / VersionMismatch / CommitUnknown → 409, InvalidArgument → 400,
/// Unsupported → 501, Timeout → 504, ResourceExhausted → 503,
/// Internal → 500), so handler code
/// can `.map_err` store calls into `ApiError` without thinking about it.
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
            | StoreError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

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
            StoreError::ResourceExhausted(_)   => ApiError::service_unavailable(msg),
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
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Text, nullable: false, unique: false, references: None });
        self
    }

    pub fn integer(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Integer, nullable: false, unique: false, references: None });
        self
    }

    pub fn real(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Real, nullable: false, unique: false, references: None });
        self
    }

    pub fn boolean(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Boolean, nullable: false, unique: false, references: None });
        self
    }

    pub fn blob(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Blob, nullable: false, unique: false, references: None });
        self
    }

    pub fn nullable_text(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Text, nullable: true, unique: false, references: None });
        self
    }

    pub fn nullable_integer(mut self, col: &str) -> Self {
        self.columns.push(ColDef { name: col.to_string(), col_type: ColType::Integer, nullable: true, unique: false, references: None });
        self
    }

    /// Mark the most recently added column as `UNIQUE`.
    /// Chains naturally after a column declaration:
    /// `Table::new("users").text("email").unique()`.
    ///
    /// Panics if no column has been added yet — calling `.unique()` on an
    /// empty table is a programming error.
    pub fn unique(mut self) -> Self {
        let last = self.columns.last_mut()
            .expect("Table::unique() called before any column was added");
        last.unique = true;
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
/// col("score", ColType::Integer).not_null().default(Val::Integer(0))
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

    /// Mark the column as `NOT NULL`.
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Mark the column as `UNIQUE`.
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

    #[test]
    fn resource_exhausted_maps_to_503_with_retry_hint() {
        let api: ApiError = StoreError::ResourceExhausted("too many concurrent transactions".into()).into();
        assert_eq!(api.status, 503);
        // The detail carries the message so callers know it's a transient backpressure signal.
        assert!(api.detail.as_deref().unwrap_or_default().contains("too many concurrent transactions"));
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
}
