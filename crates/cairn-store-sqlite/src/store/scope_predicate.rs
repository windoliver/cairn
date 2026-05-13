//! Authorization-scope SQL predicate builder (issue #191).
//!
//! Translates a [`ScopeTuple`] into a `JSON1`-based `AND ...` fragment that
//! can be composed into any `records` query. For each `Some(_)` dimension on
//! the tuple, emits a `json_extract({alias}.scope, '$.<dim>') = ?` predicate
//! and binds the value. `None` dimensions are elided (they mean "no
//! narrowing on this axis"), and an empty tuple emits the empty fragment.
//!
//! Used by all four record-reading SQL paths to keep auth enforcement
//! identical:
//!
//! 1. `do_search_keyword` (FTS5 leg)
//! 2. `do_search_semantic` (ANN leg)
//! 3. `do_search_graph_neighbors` (graph 1-hop, neighbor side)
//! 4. `hybrid::hydrate_graph_only` (defense-in-depth on graph-only ids)

use cairn_core::domain::ScopeTuple;
use rusqlite::types::Value as SqlVal;

/// Emit a scope predicate fragment for the given alias (e.g. `"r"`) and
/// scope tuple. The fragment is intended to be appended after an existing
/// `WHERE` clause; it always begins with `" AND "` when non-empty so the
/// caller does not need to track whether prior conditions exist.
///
/// Returns `("", vec![])` when every dimension is `None`.
#[must_use]
pub(crate) fn build_scope_predicate(alias: &str, scope: &ScopeTuple) -> (String, Vec<SqlVal>) {
    let mut parts: Vec<(&'static str, &str)> = Vec::with_capacity(7);
    if let Some(v) = &scope.tenant {
        parts.push(("tenant", v));
    }
    if let Some(v) = &scope.workspace {
        parts.push(("workspace", v));
    }
    if let Some(v) = &scope.project {
        parts.push(("project", v));
    }
    if let Some(v) = &scope.session_id {
        parts.push(("session_id", v));
    }
    if let Some(v) = &scope.entity {
        parts.push(("entity", v));
    }
    if let Some(v) = &scope.user {
        parts.push(("user", v));
    }
    if let Some(v) = &scope.agent {
        parts.push(("agent", v));
    }
    if parts.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut sql = String::with_capacity(parts.len() * 64);
    let mut params: Vec<SqlVal> = Vec::with_capacity(parts.len());
    for (key, value) in parts {
        sql.push_str(" AND json_extract(");
        sql.push_str(alias);
        sql.push_str(".scope, '$.");
        sql.push_str(key);
        sql.push_str("') = ?");
        params.push(SqlVal::Text(value.to_owned()));
    }
    (sql, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scope_emits_no_fragment() {
        let (sql, params) = build_scope_predicate("r", &ScopeTuple::default());
        assert!(sql.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn single_dimension_emits_one_clause() {
        let scope = ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        };
        let (sql, params) = build_scope_predicate("r", &scope);
        assert_eq!(sql, " AND json_extract(r.scope, '$.tenant') = ?");
        assert_eq!(params.len(), 1);
        match &params[0] {
            SqlVal::Text(s) => assert_eq!(s, "acme"),
            other => panic!("unexpected param: {other:?}"),
        }
    }

    #[test]
    fn multiple_dimensions_emit_in_canonical_order() {
        let scope = ScopeTuple {
            user: Some("alice".into()),
            tenant: Some("acme".into()),
            workspace: Some("eng".into()),
            ..Default::default()
        };
        let (sql, params) = build_scope_predicate("r", &scope);
        // Order is the field-declaration order, deterministic regardless of
        // which Some() branches fired.
        let expected = " AND json_extract(r.scope, '$.tenant') = ?\
                        AND json_extract(r.scope, '$.workspace') = ?\
                        AND json_extract(r.scope, '$.user') = ?";
        // Concrete check is on substring presence (avoids whitespace
        // ambiguity from the line-continuation `\`).
        assert!(sql.contains("'$.tenant') = ?"));
        assert!(sql.contains("'$.workspace') = ?"));
        assert!(sql.contains("'$.user') = ?"));
        assert!(!sql.contains("'$.session_id'"));
        assert_eq!(params.len(), 3);
        let _ = expected;
    }

    #[test]
    fn alias_is_threaded_through() {
        let scope = ScopeTuple {
            tenant: Some("t".into()),
            ..Default::default()
        };
        let (sql, _) = build_scope_predicate("neighbor", &scope);
        assert!(sql.contains("json_extract(neighbor.scope"));
    }
}
