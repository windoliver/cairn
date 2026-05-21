//! Read-only lint checks for persisted session-tree metadata.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::StoreError;

/// One session-tree metadata lint finding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionTreeLintFinding {
    /// Stable check identifier.
    pub check_id: &'static str,
    /// Related session id.
    pub session_id: String,
    /// Operator-facing explanation.
    pub message: String,
}

/// Inspect session-tree tables for lineage and metadata drift.
///
/// This function is read-only and intentionally does not hydrate
/// [`cairn_core::domain::SessionTree`]. The linter must be able to report
/// malformed rows that normal hydration would reject early.
///
/// # Errors
///
/// Returns [`StoreError::Sqlite`] when the database cannot be opened or queried.
pub async fn lint_session_tree_metadata(
    db_path: impl AsRef<Path>,
) -> Result<Vec<SessionTreeLintFinding>, StoreError> {
    lint_session_tree_metadata_sync(db_path.as_ref())
}

fn lint_session_tree_metadata_sync(
    db_path: &Path,
) -> Result<Vec<SessionTreeLintFinding>, StoreError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut findings = Vec::new();
    append_node_findings(&conn, &mut findings)?;
    append_lineage_findings(&conn, &mut findings)?;
    append_merge_findings(&conn, &mut findings)?;
    Ok(findings)
}

fn append_node_findings(
    conn: &Connection,
    findings: &mut Vec<SessionTreeLintFinding>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT session_id, parent_session_id, at_turn_id, branch_kind, tool_call_id \
           FROM session_tree_nodes \
          ORDER BY created_at, session_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (session_id, parent_id, at_turn_id, branch_kind, tool_call_id) = row?;
        let session_exists = exists(conn, "sessions", "session_id", &session_id)?;
        if !session_exists {
            findings.push(SessionTreeLintFinding {
                check_id: "orphaned_branch",
                session_id: session_id.clone(),
                message: format!("session_tree_nodes row {session_id} has no sessions row"),
            });
        }
        match parent_id.as_deref() {
            None => {
                if at_turn_id.is_some() || branch_kind.is_some() || tool_call_id.is_some() {
                    findings.push(SessionTreeLintFinding {
                        check_id: "inconsistent_branch_metadata",
                        session_id: session_id.clone(),
                        message: format!(
                            "session tree root {session_id} must not carry branch metadata"
                        ),
                    });
                }
            }
            Some(parent) => {
                if !exists(conn, "session_tree_nodes", "session_id", parent)? {
                    findings.push(SessionTreeLintFinding {
                        check_id: "broken_lineage",
                        session_id: session_id.clone(),
                        message: format!(
                            "session tree node {session_id} references missing parent {parent}"
                        ),
                    });
                }
                if at_turn_id.as_deref().is_none_or(str::is_empty)
                    || branch_kind.as_deref().is_none_or(str::is_empty)
                {
                    findings.push(SessionTreeLintFinding {
                        check_id: "inconsistent_branch_metadata",
                        session_id: session_id.clone(),
                        message: format!(
                            "session tree node {session_id} is missing branch kind or turn boundary"
                        ),
                    });
                }
                match branch_kind.as_deref() {
                    Some("tool_spawned") if tool_call_id.as_deref().is_none_or(str::is_empty) => {
                        findings.push(SessionTreeLintFinding {
                            check_id: "inconsistent_branch_metadata",
                            session_id: session_id.clone(),
                            message: format!(
                                "tool-spawned session tree node {session_id} is missing tool_call_id"
                            ),
                        });
                    }
                    Some("fork" | "clone") if tool_call_id.is_some() => {
                        findings.push(SessionTreeLintFinding {
                            check_id: "inconsistent_branch_metadata",
                            session_id: session_id.clone(),
                            message: format!(
                                "fork/clone session tree node {session_id} must not carry tool_call_id"
                            ),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn append_lineage_findings(
    conn: &Connection,
    findings: &mut Vec<SessionTreeLintFinding>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT n.session_id \
           FROM session_tree_nodes n \
          WHERE NOT EXISTS ( \
            WITH RECURSIVE ancestors(session_id, parent_session_id) AS ( \
              SELECT session_id, parent_session_id \
                FROM session_tree_nodes \
               WHERE session_id = n.session_id \
              UNION \
              SELECT p.session_id, p.parent_session_id \
                FROM session_tree_nodes p \
                JOIN ancestors ON ancestors.parent_session_id = p.session_id \
            ) \
            SELECT 1 FROM ancestors WHERE parent_session_id IS NULL \
          ) \
          ORDER BY n.created_at, n.session_id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for row in rows {
        let session_id = row?;
        findings.push(SessionTreeLintFinding {
            check_id: "broken_lineage",
            session_id: session_id.clone(),
            message: format!("session tree node {session_id} lineage does not reach a root"),
        });
    }
    Ok(())
}

fn append_merge_findings(
    conn: &Connection,
    findings: &mut Vec<SessionTreeLintFinding>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "SELECT merge_id, source_session_id, destination_session_id, strategy_kind, \
                summary_record_id, first_turn_id, last_turn_id, applied_at_turn_id \
           FROM session_tree_merges \
          ORDER BY created_at, merge_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    for row in rows {
        let (merge_id, source, destination, strategy, summary, first, last, applied_at) = row?;
        for session_id in [&source, &destination] {
            if !exists(conn, "session_tree_nodes", "session_id", session_id)? {
                findings.push(SessionTreeLintFinding {
                    check_id: "broken_lineage",
                    session_id: session_id.clone(),
                    message: format!(
                        "session tree merge {merge_id} references session {session_id} outside tree metadata"
                    ),
                });
            }
        }
        if applied_at.is_empty() {
            findings.push(SessionTreeLintFinding {
                check_id: "inconsistent_branch_metadata",
                session_id: destination.clone(),
                message: format!("session tree merge {merge_id} has an empty applied_at_turn_id"),
            });
        }
        match strategy.as_str() {
            "reasoning_summary" => {
                let Some(summary_id) = summary else {
                    findings.push(SessionTreeLintFinding {
                        check_id: "stale_merge_summary",
                        session_id: destination.clone(),
                        message: format!(
                            "session tree merge {merge_id} is missing summary_record_id"
                        ),
                    });
                    continue;
                };
                if !active_record_exists(conn, &summary_id)? {
                    findings.push(SessionTreeLintFinding {
                        check_id: "stale_merge_summary",
                        session_id: destination.clone(),
                        message: format!(
                            "session tree merge {merge_id} summary record {summary_id} is missing or inactive"
                        ),
                    });
                }
            }
            "controlled_splice" => {
                if first.as_deref().is_none_or(str::is_empty)
                    || last.as_deref().is_none_or(str::is_empty)
                {
                    findings.push(SessionTreeLintFinding {
                        check_id: "inconsistent_branch_metadata",
                        session_id: destination.clone(),
                        message: format!(
                            "session tree merge {merge_id} controlled splice is missing turn bounds"
                        ),
                    });
                }
            }
            _ => {
                findings.push(SessionTreeLintFinding {
                    check_id: "inconsistent_branch_metadata",
                    session_id: destination.clone(),
                    message: format!(
                        "session tree merge {merge_id} has unknown strategy {strategy}"
                    ),
                });
            }
        }
    }
    Ok(())
}

fn exists(
    conn: &Connection,
    table: &'static str,
    column: &'static str,
    value: &str,
) -> Result<bool, StoreError> {
    let sql = format!("SELECT 1 FROM {table} WHERE {column} = ?1 LIMIT 1");
    conn.query_row(&sql, params![value], |_| Ok(()))
        .optional()
        .map(|row| row.is_some())
        .map_err(StoreError::from)
}

fn active_record_exists(conn: &Connection, record_id: &str) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT 1 FROM records WHERE record_id = ?1 AND active = 1 AND tombstoned = 0 LIMIT 1",
        params![record_id],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(StoreError::from)
}
