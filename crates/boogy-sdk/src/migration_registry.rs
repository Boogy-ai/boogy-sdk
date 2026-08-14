//! Collect one-module-per-migration into an ordered list.
//!
//! The alternative — an inline array of closures inside `init_tables` — grows
//! without bound in a single function, and every schema change edits the same
//! lines. One module per migration means each is readable on its own, greppable
//! by name, and diffable without touching its neighbours; the only thing that
//! grows is one registry line per change.
//!
//! This mirrors what the control plane already does for its own schema: one
//! `.sql` file per migration in a directory, rather than one growing function.

/// Build a migration list from modules that each declare their own `VERSION`,
/// `NAME`, and `up`.
///
/// Each listed module must expose:
///
/// ```ignore_snippet: MigrationCtx is emitted into the service crate by wit_glue!
/// pub const VERSION: i64 = 2;
/// pub const NAME: &str = "add_backer_count_columns_to_posts";
/// pub fn up(m: &MigrationCtx) -> Result<(), String> { Ok(()) }
/// ```
///
/// The macro reads `VERSION` and `NAME` **from the module** rather than taking
/// them as arguments. A registry line carrying the version separately can
/// disagree with the migration it names, and the disagreement is invisible —
/// both halves look plausible. Reading them from the module makes it
/// unrepresentable.
#[macro_export]
macro_rules! migrations {
    ($($m:ident),* $(,)?) => {
        ::std::vec![$( migration($m::VERSION, $m::NAME, $m::up) ),*]
    };
}
