//! Schema definition for the per-API `__boogy_api_keys` table.
//!
//! The table is shipped with every API that opts into API-key support.
//! Reserving the `__boogy_` prefix keeps it out of any user
//! namespace.

use crate::store::Table;

/// Reserved table name. Hard-coded to keep deployments from drifting.
pub const TABLE: &str = "__boogy_api_keys";

/// Build the [`Table`] declaration for `__boogy_api_keys`. Pass
/// directly to `create_table_from` (emitted by `wit_glue!`):
///
/// ```ignore
/// create_table_from(&boogy_sdk::api_keys::schema_table());
/// ```
///
/// Columns:
/// - `id`            — UUID v7 row identifier (PK; distinct from the
///   prefix so revoke/rotate stay stable across rotations).
/// - `prefix`        — first 11 chars of the secret. Indexed and unique
///   for O(1) lookup on inbound bearer presentation.
/// - `hash`          — SHA-256 hex of the full secret. Compared
///   constant-time during verification.
/// - `name`          — operator-supplied label for management UIs.
/// - `scopes`        — comma-separated scopes. Empty string = no scopes.
/// - `created_by`    — agent_id that minted the key, when known.
/// - `created_at`    — Unix seconds.
/// - `last_used_at`  — Unix seconds; nullable (never used).
/// - `expires_at`    — Unix seconds; nullable (never expires).
/// - `revoked`       — 0/1 flag. Revoked rows are kept (audit trail)
///   but [`prepare_create`](super::prepare_create) and the runtime
///   guard reject them.
pub fn schema_table() -> Table {
    Table::new(TABLE)
        .text("id")
        .text("prefix")
        .text("hash")
        .text("name")
        .text("scopes")
        .nullable_text("created_by")
        .integer("created_at")
        .nullable_integer("last_used_at")
        .nullable_integer("expires_at")
        .integer("revoked")
        .unique_index("api_keys_prefix_idx", &["prefix"])
        .index("api_keys_revoked_idx", &["revoked"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_expected_columns() {
        let t = schema_table();
        assert_eq!(t.name, TABLE);
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "id",
                "prefix",
                "hash",
                "name",
                "scopes",
                "created_by",
                "created_at",
                "last_used_at",
                "expires_at",
                "revoked",
            ]
        );
    }

    #[test]
    fn schema_indexes_prefix_uniquely() {
        let t = schema_table();
        let idx = t
            .indices
            .iter()
            .find(|i| i.columns == vec!["prefix".to_string()])
            .expect("prefix index missing");
        assert!(idx.unique, "prefix index must be UNIQUE");
    }
}
