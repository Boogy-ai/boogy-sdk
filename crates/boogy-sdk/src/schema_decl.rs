//! Declaration of a service's schema as a value.
//!
//! `create_model::<M>()` fired DDL as a side effect, so "what this service
//! declares" was never something the SDK could hold and inspect. That is why the
//! index reconcile has to collect declarations through a side channel before it
//! can decide anything: a per-table pass would read every *other* table's
//! indexes as undeclared and drop them.
//!
//! Building the declaration first makes the whole set available before anything
//! touches the store, and makes it testable without one.

use crate::model::Model;
use crate::store::{Index, Table};

/// Every table a service declares, in declaration order.
#[derive(Default)]
pub struct Schema {
    tables: Vec<Table>,
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a model's table under its own name and declared index set.
    pub fn model<M: Model>(&mut self) -> &mut Self {
        self.tables.push(M::schema());
        self
    }

    /// Declare a model's COLUMNS under a different table name, with a
    /// caller-supplied index set.
    ///
    /// For families of identically-shaped tables whose names are only known at
    /// runtime — one table per time window, say. Their index names have to embed
    /// the per-table suffix, which a single model cannot express.
    pub fn model_as<M: Model>(&mut self, table: &str, indexes: Vec<Index>) -> &mut Self {
        let mut t = M::schema();
        t.name = table.to_string();
        t.indices = indexes;
        self.tables.push(t);
        self
    }

    /// Everything declared so far.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Row, Val};

    // Stands in for a `#[derive(Model)]` type. The derive emits exactly this
    // contract, so a hand-written one exercises the same surface.
    struct Post;
    impl Model for Post {
        const TABLE: &'static str = "posts";
        fn schema() -> Table {
            Table::new("posts").text("author").text("body")
        }
        fn from_row(_row: &Row) -> Self {
            Post
        }
        fn to_columns(&self) -> Vec<(String, Val)> {
            Vec::new()
        }
        fn id(&self) -> Option<u64> {
            None
        }
    }

    #[test]
    fn model_records_the_types_own_table() {
        let mut s = Schema::new();
        s.model::<Post>();
        assert_eq!(s.tables().len(), 1);
        assert_eq!(s.tables()[0].name, "posts");
    }

    #[test]
    fn model_as_overrides_the_name_and_index_set() {
        // The runtime-named family case. Without it a service cannot declare
        // per-window tables at all, and the reconcile would read them as orphans.
        let mut s = Schema::new();
        s.model_as::<Post>(
            "post_score_1h",
            vec![Index {
                name: "ix_post_score_1h_total".into(),
                columns: vec!["score_total".into()],
                unique: false,
                covering: true,
            }],
        );
        assert_eq!(s.tables()[0].name, "post_score_1h");
        assert_eq!(s.tables()[0].indices.len(), 1);
        assert_eq!(
            s.tables()[0].columns.len(),
            2,
            "columns still come from the model; only name and indexes are overridden"
        );
    }

    #[test]
    fn model_as_does_not_disturb_the_models_own_declaration() {
        // Both must be independently declarable: `M::schema()` is called fresh
        // each time, so overriding one must not mutate the other.
        let mut s = Schema::new();
        s.model::<Post>();
        s.model_as::<Post>("archive", vec![]);
        assert_eq!(s.tables()[0].name, "posts");
        assert_eq!(s.tables()[1].name, "archive");
    }

    #[test]
    fn declarations_accumulate_in_order() {
        let mut s = Schema::new();
        s.model::<Post>();
        s.model_as::<Post>("archive", vec![]);
        assert_eq!(
            s.tables().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["posts", "archive"]
        );
    }

    #[test]
    fn declaring_nothing_yields_no_tables() {
        // The default for a service with no tables. Not an error, and nothing
        // for the reconcile to act on.
        assert!(Schema::new().tables().is_empty());
    }
}
