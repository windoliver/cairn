//! SQLite-backed lint queries for bitemporal entity edges.

use std::collections::BTreeMap;

use cairn_core::domain::{
    EdgeCandidate, EdgeConfidence, LintFinding, LintKind, Severity, choose_edge_keeper,
};
use rusqlite::Connection;

use crate::{StoreError, migrations::ensure_table};

const ENTITY_EDGES_TABLE: &str = "entity_edges";
const WAL_OPS_TABLE: &str = "wal_ops";
const REPLAY_LEDGER_TABLE: &str = "replay_ledger";
const RESOLUTION_REASON: &str = "lint:contradiction_resolution";

type EdgeTriple = (String, String, String);
type EdgeGroups = BTreeMap<EdgeTriple, Vec<EdgeCandidate>>;

/// Report returned by `SQLite` edge lint operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeLintReport {
    /// Structured lint findings.
    pub findings: Vec<LintFinding>,
    /// Number of live contradictory edge pairs.
    pub contradictions: u64,
    /// Number of live ambiguous edges.
    pub ambiguous_edges: u64,
    /// Number of edges automatically invalidated by a fix operation.
    pub auto_resolved: u64,
}

/// Find live edge contradictions and ambiguous edges without mutating rows.
pub fn lint_edges(conn: &Connection) -> Result<EdgeLintReport, StoreError> {
    ensure_table(conn, ENTITY_EDGES_TABLE)?;

    let mut findings = contradiction_findings(conn)?;
    let ambiguous_findings = ambiguous_findings(conn)?;
    let contradictions = usize_to_u64(findings.len());
    let ambiguous_edges = usize_to_u64(ambiguous_findings.len());
    findings.extend(ambiguous_findings);

    Ok(EdgeLintReport {
        findings,
        contradictions,
        ambiguous_edges,
        auto_resolved: 0,
    })
}

/// Invalidate duplicate live edges and record committed lint repair metadata.
pub fn resolve_edge_contradictions(
    conn: &mut Connection,
    now: i64,
    operation_id: &str,
) -> Result<EdgeLintReport, StoreError> {
    ensure_table(conn, ENTITY_EDGES_TABLE)?;
    ensure_table(conn, WAL_OPS_TABLE)?;
    ensure_table(conn, REPLAY_LEDGER_TABLE)?;

    let tx = conn.transaction()?;
    let groups = live_edge_groups(&tx)?;
    let mut losers = Vec::new();

    for edges in groups.values() {
        if edges.len() <= 1 {
            continue;
        }

        if let Some(keeper) = choose_edge_keeper(edges) {
            losers.extend(
                edges
                    .iter()
                    .filter(|edge| edge.id != keeper.id)
                    .map(|edge| edge.id.clone()),
            );
        }
    }

    for loser in &losers {
        tx.execute(
            "UPDATE entity_edges
             SET invalid_at = ?1
             WHERE id = ?2",
            (now, loser),
        )?;
    }

    if !losers.is_empty() {
        tx.execute(
            "INSERT INTO wal_ops (
                operation_id, state, kind, reason, envelope, issued_at, committed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                operation_id,
                "committed",
                "lint",
                RESOLUTION_REASON,
                "{}",
                now,
                now,
            ),
        )?;
        tx.execute(
            "INSERT INTO replay_ledger (operation_id, reason, committed_at)
             VALUES (?1, ?2, ?3)",
            (operation_id, RESOLUTION_REASON, now),
        )?;
    }

    tx.commit()?;

    let mut report = lint_edges(conn)?;
    report.auto_resolved = usize_to_u64(losers.len());
    Ok(report)
}

fn contradiction_findings(conn: &Connection) -> Result<Vec<LintFinding>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT a.id, b.id
         FROM entity_edges a
         JOIN entity_edges b
           ON a.source_id = b.source_id
          AND a.target_id = b.target_id
          AND a.relation = b.relation
          AND a.id < b.id
         WHERE a.invalid_at IS NULL
           AND a.expired_at IS NULL
           AND b.invalid_at IS NULL
           AND b.expired_at IS NULL
         ORDER BY a.id, b.id",
    )?;
    let mut rows = stmt.query([])?;
    let mut findings = Vec::new();

    while let Some(row) = rows.next()? {
        let edge_a = row.get(0)?;
        let edge_b = row.get(1)?;
        findings.push(LintFinding {
            kind: LintKind::ContradictoryEdge,
            severity: Severity::Warning,
            entities: vec![edge_a, edge_b],
            message: "multiple live edges share the same source, target, and relation".to_owned(),
            suggestion: Some("resolve by invalidating all but the strongest edge".to_owned()),
        });
    }

    Ok(findings)
}

fn ambiguous_findings(conn: &Connection) -> Result<Vec<LintFinding>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, confidence
         FROM entity_edges
         WHERE invalid_at IS NULL AND expired_at IS NULL
         ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut findings = Vec::new();

    while let Some(row) = rows.next()? {
        let edge_id: String = row.get(0)?;
        let confidence_value: String = row.get(1)?;
        let confidence = parse_confidence(&edge_id, &confidence_value)?;

        if confidence == EdgeConfidence::Ambiguous {
            findings.push(LintFinding {
                kind: LintKind::AmbiguousEdge,
                severity: Severity::Info,
                entities: vec![edge_id],
                message: "live edge is marked ambiguous".to_owned(),
                suggestion: Some("review the edge confidence before relying on it".to_owned()),
            });
        }
    }

    Ok(findings)
}

fn live_edge_groups(conn: &Connection) -> Result<EdgeGroups, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_id, relation, confidence, confidence_score
         FROM entity_edges
         WHERE invalid_at IS NULL AND expired_at IS NULL
         ORDER BY source_id, target_id, relation, id",
    )?;
    let mut rows = stmt.query([])?;
    let mut groups: EdgeGroups = BTreeMap::new();

    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let source_id: String = row.get(1)?;
        let target_id: String = row.get(2)?;
        let relation: String = row.get(3)?;
        let confidence_value: String = row.get(4)?;
        let confidence_score = row.get(5)?;
        let confidence = parse_confidence(&id, &confidence_value)?;

        groups
            .entry((source_id, target_id, relation))
            .or_default()
            .push(EdgeCandidate {
                id,
                confidence,
                confidence_score,
            });
    }

    Ok(groups)
}

fn parse_confidence(edge_id: &str, value: &str) -> Result<EdgeConfidence, StoreError> {
    EdgeConfidence::parse(value).map_err(|value| StoreError::InvalidConfidence {
        edge_id: edge_id.to_owned(),
        value,
    })
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
