use cairn_core::domain::scope::ScopeTuple;
use std::sync::Arc;

use crate::store::SqliteMemoryStore;

/// Read-only graph-traversal driver. Constructed per-request after
/// the MCP handler has resolved the caller's scope set; every
/// method bakes both the §3.0 active-at-now temporal predicate
/// and the §3.0a stable-lineage scope predicate into its SQL.
///
/// `allowed_scopes` is the resolver's `Vec<ScopeTuple>` and MUST be
/// non-empty — empty means deny-all and is the caller's
/// responsibility to short-circuit upstream (`materialize_graph_request`).
pub struct GraphQueries {
    // Fields consumed by Tasks 3–10; unused in this skeleton task.
    #[allow(dead_code)]
    pub(crate) store: Arc<SqliteMemoryStore>,
    #[allow(dead_code)]
    pub(crate) allowed_scopes: Vec<ScopeTuple>,
    #[allow(dead_code)]
    pub(crate) now: i64,
}

impl GraphQueries {
    /// Construct a new [`GraphQueries`] driver.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds when `allowed_scopes` is empty.
    /// Production callers must short-circuit upstream via
    /// `materialize_graph_request` before reaching this constructor.
    #[must_use]
    pub fn new(
        store: Arc<SqliteMemoryStore>,
        allowed_scopes: Vec<ScopeTuple>,
        now: i64,
    ) -> Self {
        debug_assert!(
            !allowed_scopes.is_empty(),
            "invariant: GraphQueries requires non-empty allowed_scopes; \
             materialize_graph_request must short-circuit upstream"
        );
        Self { store, allowed_scopes, now }
    }

    /// Render the §3.0a six-dimension scope match clause.
    ///
    /// Returns `(sql, bind_count)` where `sql` is intended to drop into
    /// an EXISTS subquery as the `<ScopeTuple-match-clause>` placeholder
    /// in the spec, and `bind_count` is the number of `?` parameters the
    /// caller must bind in order (`tenant`, `workspace`, `session_id`, `entity`,
    /// `user`, `agent` — six per tuple, OR'd between tuples).
    #[must_use]
    pub fn scope_match_clause(tuples: &[ScopeTuple]) -> (String, usize) {
        const DIMS: [&str; 6] = [
            "tenant", "workspace", "session_id", "entity", "user", "agent",
        ];
        let mut arms: Vec<String> = Vec::with_capacity(tuples.len());
        for _ in tuples {
            let mut conds: Vec<String> = Vec::with_capacity(DIMS.len());
            for d in DIMS {
                conds.push(format!(
                    "coalesce(json_extract(r_src.scope, '$.{d}'), '') \
                     = coalesce(?, '')"
                ));
            }
            arms.push(format!("({})", conds.join(" AND ")));
        }
        (arms.join(" OR "), tuples.len() * DIMS.len())
    }

    /// Expand a `len`-wide `IN (?,?,...)` placeholder list. `len = 0`
    /// returns `"(NULL)"` so the caller may unconditionally splice it
    /// into SQL — a `NULL` membership test is always false, matching
    /// the spec's "empty visited / empty input → omit clause" guidance
    /// without runtime branching at the SQL level.
    ///
    /// Used by Tasks 3–10; unused in this skeleton task.
    #[allow(dead_code)]
    pub(crate) fn placeholders(len: usize) -> String {
        if len == 0 {
            return "(NULL)".to_string();
        }
        let mut s = String::with_capacity(len * 2 + 1);
        s.push('(');
        for i in 0..len {
            if i > 0 {
                s.push(',');
            }
            s.push('?');
        }
        s.push(')');
        s
    }
}
