//! SQLite-backed lint queries for bitemporal entity edges.

use std::collections::BTreeMap;

use cairn_core::domain::graph::EdgeConfidence;
use cairn_core::generated::verbs::lint::{Finding, Kind, Severity};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::StoreError;
use crate::entity_graph::{ENTITY_EDGE_PRE_IMAGE_JSON, wal};

const ENTITY_EDGES_TABLE: &str = "entity_edges";
const WAL_OPS_TABLE: &str = "wal_ops";
const WAL_STEPS_TABLE: &str = "wal_steps";
const RESOLUTION_REASON: &str = "lint:contradiction_resolution";

type EdgeTriple = (String, String, String);
type EdgeGroups = BTreeMap<EdgeTriple, Vec<EdgeCandidate>>;

/// Report returned by `SQLite` edge lint operations.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeLintReport {
    /// Structured lint findings.
    pub findings: Vec<Finding>,
    /// Number of live contradictory edge pairs.
    pub contradictions: u64,
    /// Number of live ambiguous edges.
    pub ambiguous_edges: u64,
    /// Number of edges automatically invalidated by a fix operation.
    pub auto_resolved: u64,
}

/// Find live edge contradictions and ambiguous edges without mutating rows.
///
/// # Errors
///
/// Returns [`StoreError`] when required schema objects are missing or `SQLite`
/// rejects the read.
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

/// Invalidate duplicate live edges and record the repair through the WAL.
///
/// # Errors
///
/// Returns [`StoreError`] when required schema objects are missing, WAL state
/// transitions fail, or the edge table contains unsupported confidence values.
pub fn resolve_edge_contradictions(
    conn: &mut Connection,
    now_ms: i64,
    operation_id: &str,
) -> Result<EdgeLintReport, StoreError> {
    ensure_table(conn, ENTITY_EDGES_TABLE)?;
    ensure_table(conn, WAL_OPS_TABLE)?;
    ensure_table(conn, WAL_STEPS_TABLE)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let groups = live_edge_groups(&tx)?;
    let mut losers = Vec::new();

    for edges in groups.values() {
        if edges.len() <= 1 {
            continue;
        }

        if let Some(keeper) = choose_edge_keeper(edges) {
            losers.extend(edges.iter().filter(|edge| edge.id != keeper.id).cloned());
        }
    }

    if !losers.is_empty() {
        apply_lint_fix_wal(&tx, now_ms, operation_id, &losers)?;
    }

    tx.commit()?;

    let mut report = lint_edges(conn)?;
    report.auto_resolved = usize_to_u64(losers.len());
    Ok(report)
}

fn contradiction_findings(conn: &Connection) -> Result<Vec<Finding>, StoreError> {
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
        let edge_a: String = row.get(0)?;
        let edge_b: String = row.get(1)?;
        findings.push(Finding {
            kind: Kind::ContradictoryEdge,
            severity: Severity::Warning,
            message: "multiple live edges share the same source, target, and relation".to_owned(),
            entities: Some(vec![edge_a, edge_b]),
            suggested_fix: Some("run `cairn lint --fix` to keep the strongest edge".to_owned()),
            target: None,
            tracking_issue: Some(192),
        });
    }

    Ok(findings)
}

fn ambiguous_findings(conn: &Connection) -> Result<Vec<Finding>, StoreError> {
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
            findings.push(Finding {
                kind: Kind::AmbiguousEdge,
                severity: Severity::Info,
                message: "live edge is marked ambiguous".to_owned(),
                entities: Some(vec![edge_id]),
                suggested_fix: Some("review the edge confidence before relying on it".to_owned()),
                target: None,
                tracking_issue: Some(192),
            });
        }
    }

    Ok(findings)
}

fn live_edge_groups(conn: &Connection) -> Result<EdgeGroups, StoreError> {
    let pre_image = ENTITY_EDGE_PRE_IMAGE_JSON;
    let select = format!(
        "SELECT id, source_id, target_id, relation, confidence, confidence_score,
                valid_at, created_at, source_record_id, {pre_image}
         FROM entity_edges
         WHERE invalid_at IS NULL AND expired_at IS NULL
         ORDER BY source_id, target_id, relation, id"
    );
    let mut stmt = conn.prepare(&select)?;
    let mut rows = stmt.query([])?;
    let mut groups: EdgeGroups = BTreeMap::new();

    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let source_id: String = row.get(1)?;
        let target_id: String = row.get(2)?;
        let relation: String = row.get(3)?;
        let confidence_value: String = row.get(4)?;
        let confidence_score: f32 = row.get(5)?;
        let valid_at: i64 = row.get(6)?;
        let created_at: i64 = row.get(7)?;
        let source_record_id: Option<String> = row.get(8)?;
        let pre_image_json: String = row.get(9)?;
        let confidence = parse_confidence(&id, &confidence_value)?;

        groups
            .entry((source_id, target_id, relation))
            .or_default()
            .push(EdgeCandidate {
                id,
                confidence,
                confidence_score,
                valid_at,
                created_at,
                source_record_id,
                pre_image_json,
            });
    }

    Ok(groups)
}

fn apply_lint_fix_wal(
    tx: &Transaction<'_>,
    now_ms: i64,
    operation_id: &str,
    losers: &[EdgeCandidate],
) -> Result<(), StoreError> {
    issue_lint_fix_op(tx, operation_id)?;

    for (step_ord, loser) in losers.iter().enumerate() {
        let new_hash = edge_body_hash(
            loser.confidence.as_db_str(),
            loser.confidence_score,
            loser.valid_at,
            Some(now_ms),
            loser.created_at,
            loser.source_record_id.as_deref(),
        );
        tx.execute(
            "UPDATE entity_edges
             SET invalid_at = ?1, body_hash = ?2
             WHERE id = ?3",
            rusqlite::params![now_ms, &new_hash[..], &loser.id],
        )?;
        wal::write_step(
            tx,
            operation_id,
            i64::try_from(step_ord).unwrap_or(i64::MAX),
            "invalidate_edge",
            Some(loser.pre_image_json.as_bytes()),
        )?;
    }

    wal::commit_op(tx, operation_id)?;
    Ok(())
}

fn issue_lint_fix_op(tx: &Transaction<'_>, operation_id: &str) -> Result<(), StoreError> {
    let issued_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
        [],
        |r| r.get(0),
    )?;
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope,
         issuer, principal, target_hash, scope_json, expires_at, signature,
         issued_at, updated_at, reason) VALUES
         (?1, ?2, 'graph_contradict', 'ISSUED', ?3, 'cairn-store-sqlite',
          NULL, ?4, ?5, ?6, '', ?7, ?7, ?8)",
        rusqlite::params![
            operation_id,
            issued_seq,
            serde_json::json!({
                "version": 1,
                "kind": "lint_fix",
                "reason": RESOLUTION_REASON,
            })
            .to_string(),
            operation_id,
            serde_json::json!({"kind": "lint_fix"}).to_string(),
            now.saturating_add(24 * 60 * 60 * 1_000),
            now,
            RESOLUTION_REASON,
        ],
    )?;
    tx.execute(
        "UPDATE wal_ops SET state = 'PREPARED', updated_at = ?2 WHERE operation_id = ?1",
        rusqlite::params![operation_id, now],
    )?;
    Ok(())
}

fn ensure_table(conn: &Connection, table: &'static str) -> Result<(), StoreError> {
    let found = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get::<_, bool>(0),
    )?;
    if found {
        Ok(())
    } else {
        Err(StoreError::SchemaMissing { object: table })
    }
}

fn parse_confidence(edge_id: &str, value: &str) -> Result<EdgeConfidence, StoreError> {
    EdgeConfidence::from_db_str(value).ok_or_else(|| StoreError::InvalidConfidence {
        edge_id: edge_id.to_owned(),
        value: value.to_owned(),
    })
}

fn edge_body_hash(
    confidence_db_str: &str,
    confidence_score: f32,
    valid_at: i64,
    invalid_at: Option<i64>,
    created_at: i64,
    source_record_id: Option<&str>,
) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(confidence_db_str.as_bytes());
    h.update(&confidence_score.to_le_bytes());
    h.update(&valid_at.to_le_bytes());
    if let Some(t) = invalid_at {
        h.update(&[1]);
        h.update(&t.to_le_bytes());
    } else {
        h.update(&[0]);
    }
    h.update(&created_at.to_le_bytes());
    if let Some(rid) = source_record_id {
        h.update(b"|");
        h.update(rid.as_bytes());
    }
    *h.finalize().as_bytes()
}

fn choose_edge_keeper(edges: &[EdgeCandidate]) -> Option<&EdgeCandidate> {
    edges.iter().max_by(|left, right| {
        left.confidence_score
            .total_cmp(&right.confidence_score)
            .then_with(|| confidence_rank(left.confidence).cmp(&confidence_rank(right.confidence)))
            .then_with(|| right.id.cmp(&left.id))
    })
}

const fn confidence_rank(confidence: EdgeConfidence) -> u8 {
    match confidence {
        EdgeConfidence::Inferred => 1,
        EdgeConfidence::Extracted => 2,
        _ => 0,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq)]
struct EdgeCandidate {
    id: String,
    confidence: EdgeConfidence,
    confidence_score: f32,
    valid_at: i64,
    created_at: i64,
    source_record_id: Option<String>,
    pre_image_json: String,
}
