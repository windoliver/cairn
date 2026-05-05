//! Repair helpers for legacy `consent_journal` rows that block migration 0021.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;

use crate::error::StoreError;

const BLOCKER_SELECT: &str = "\
SELECT rowid, consent_id, subject, scope, decision, granted_by, decided_at, expires_at, \
       op_id, kind, sensor_id, actor, payload_json, decided_at_iso, expires_at_iso, \
       strftime('%Y-%m-%dT%H:%M:%SZ', decided_at / 1000, 'unixepoch') IS NULL AS bad_decided, \
       (expires_at IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%SZ', expires_at / 1000, 'unixepoch') IS NULL) AS bad_expires \
FROM consent_journal ";

const REPAIR_AUDIT_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS consent_journal_repair_audit (
  repair_id        TEXT NOT NULL PRIMARY KEY,
  action           TEXT NOT NULL CHECK (action IN ('delete')),
  target_rowid     INTEGER NOT NULL,
  blocker_codes    TEXT NOT NULL CHECK (json_valid(blocker_codes) = 1),
  operator         TEXT NOT NULL,
  reason           TEXT NOT NULL,
  row_snapshot     TEXT NOT NULL CHECK (json_valid(row_snapshot) = 1),
  repaired_at      INTEGER NOT NULL
);

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_immutable
  BEFORE UPDATE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_no_delete
  BEFORE DELETE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;";

const DROP_CONSENT_JOURNAL_APPEND_ONLY_TRIGGERS: &str = "\
DROP TRIGGER IF EXISTS consent_journal_immutable;
DROP TRIGGER IF EXISTS consent_journal_no_delete;";

const CREATE_CONSENT_JOURNAL_APPEND_ONLY_TRIGGERS: &str = "\
CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only');
END;";

/// Reason a `consent_journal` row blocks migration 0021.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    /// Legacy row has `rowid <= 0`, which cannot be preserved after 0021.
    NonPositiveRowid,
    /// Legacy `decided_at` cannot be rendered as RFC3339 by `SQLite`.
    UnrenderableDecidedAt,
    /// Legacy `expires_at` cannot be rendered as RFC3339 by `SQLite`.
    UnrenderableExpiresAt,
    /// `kind IS NULL` row carries post-0009 event-shape fields.
    KindNullEventFieldDrift,
}

/// Legacy `consent_journal` row requiring operator triage.
#[derive(Debug, Clone, Serialize)]
pub struct ConsentJournalRepairRow {
    /// `SQLite` rowid, used by the mirror cursor and by the repair command.
    pub rowid: i64,
    /// Consent row primary key.
    pub consent_id: String,
    /// Legacy subject field.
    pub subject: String,
    /// Legacy scope field.
    pub scope: String,
    /// Legacy decision field (`GRANT` or `REVOKE`).
    pub decision: String,
    /// Legacy actor text from the 0005 schema.
    pub granted_by: String,
    /// Legacy UNIX timestamp in milliseconds.
    pub decided_at: i64,
    /// Optional legacy expiry timestamp in milliseconds.
    pub expires_at: Option<i64>,
    /// Optional operation id added by post-0009 schema.
    pub op_id: Option<String>,
    /// Optional consent event kind added by post-0009 schema.
    pub kind: Option<String>,
    /// Optional sensor id added by post-0009 schema.
    pub sensor_id: Option<String>,
    /// Optional event actor added by post-0009 schema.
    pub actor: Option<String>,
    /// Optional event payload added by post-0009 schema.
    pub payload_json: Option<String>,
    /// Optional event timestamp added by post-0009 schema.
    pub decided_at_iso: Option<String>,
    /// Optional event expiry added by post-0009 schema.
    pub expires_at_iso: Option<String>,
    /// Classifier outputs explaining why this row blocks 0021.
    pub blocker_codes: Vec<BlockerCode>,
}

/// Receipt for one operator-authorized repair.
#[derive(Debug, Clone, Serialize)]
pub struct ConsentJournalRepairReceipt {
    /// Unique id for the audit record.
    pub repair_id: String,
    /// Repaired `consent_journal` rowid.
    pub target_rowid: i64,
    /// Classifier outputs present when the row was deleted.
    pub blocker_codes: Vec<BlockerCode>,
    /// Operator identity recorded in the audit table.
    pub operator: String,
    /// Operator-supplied reason recorded in the audit table.
    pub reason: String,
    /// UNIX epoch milliseconds when the repair was applied.
    pub repaired_at: i64,
}

/// Enumerate legacy rows that are known to block migration 0021.
///
/// # Errors
/// Returns [`StoreError`] for `SQLite` failures.
pub fn list_blockers(conn: &Connection) -> Result<Vec<ConsentJournalRepairRow>, StoreError> {
    apply_repair_pragmas(conn)?;
    let query = format!("{BLOCKER_SELECT} WHERE kind IS NULL ORDER BY rowid ASC");
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], row_to_repair_row)?;

    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if !row.blocker_codes.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}

/// Delete one repair-eligible legacy row, preserving an append-only audit trail.
///
/// # Errors
/// Returns [`StoreError::RepairNotEligible`] when `rowid` is absent or is not a
/// row classified as a migration blocker. Other failures are `SQLite`, JSON codec,
/// or transaction errors.
pub fn delete_blocker(
    conn: &mut Connection,
    rowid: i64,
    reason: &str,
    operator: &str,
) -> Result<ConsentJournalRepairReceipt, StoreError> {
    apply_repair_pragmas(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_repair_audit_schema(&tx)?;

    let blocker =
        find_blocker_by_rowid(&tx, rowid)?.ok_or(StoreError::RepairNotEligible { rowid })?;
    let repair_id = ulid::Ulid::new().to_string();
    let repaired_at = chrono::Utc::now().timestamp_millis();
    let blocker_codes = blocker.blocker_codes.clone();
    let blocker_codes_json = serde_json::to_string(&blocker_codes)?;
    let row_snapshot = serde_json::to_string(&blocker)?;

    tx.execute(
        "INSERT INTO consent_journal_repair_audit \
           (repair_id, action, target_rowid, blocker_codes, operator, reason, row_snapshot, repaired_at) \
         VALUES (?1, 'delete', ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            repair_id,
            rowid,
            blocker_codes_json,
            operator,
            reason,
            row_snapshot,
            repaired_at
        ],
    )?;

    tx.execute_batch(DROP_CONSENT_JOURNAL_APPEND_ONLY_TRIGGERS)?;
    let deleted = tx.execute("DELETE FROM consent_journal WHERE rowid = ?1", [rowid])?;
    tx.execute_batch(CREATE_CONSENT_JOURNAL_APPEND_ONLY_TRIGGERS)?;
    if deleted != 1 {
        return Err(StoreError::RepairNotEligible { rowid });
    }

    mark_consent_mirror_reset_if_ready(&tx, repaired_at)?;

    let receipt = ConsentJournalRepairReceipt {
        repair_id,
        target_rowid: rowid,
        blocker_codes,
        operator: operator.to_owned(),
        reason: reason.to_owned(),
        repaired_at,
    };
    tx.commit()?;
    Ok(receipt)
}

fn apply_repair_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
    Ok(())
}

fn ensure_repair_audit_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(REPAIR_AUDIT_SCHEMA)?;
    Ok(())
}

fn find_blocker_by_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<ConsentJournalRepairRow>, StoreError> {
    let query = format!("{BLOCKER_SELECT} WHERE rowid = ?1 AND kind IS NULL");
    let mut stmt = conn.prepare(&query)?;
    let item = stmt
        .query_row(params![rowid], row_to_repair_row)
        .optional()?;
    Ok(item.filter(|row| !row.blocker_codes.is_empty()))
}

fn row_to_repair_row(row: &Row<'_>) -> rusqlite::Result<ConsentJournalRepairRow> {
    let mut item = ConsentJournalRepairRow {
        rowid: row.get("rowid")?,
        consent_id: row.get("consent_id")?,
        subject: row.get("subject")?,
        scope: row.get("scope")?,
        decision: row.get("decision")?,
        granted_by: row.get("granted_by")?,
        decided_at: row.get("decided_at")?,
        expires_at: row.get("expires_at")?,
        op_id: row.get("op_id")?,
        kind: row.get("kind")?,
        sensor_id: row.get("sensor_id")?,
        actor: row.get("actor")?,
        payload_json: row.get("payload_json")?,
        decided_at_iso: row.get("decided_at_iso")?,
        expires_at_iso: row.get("expires_at_iso")?,
        blocker_codes: Vec::new(),
    };

    if item.rowid <= 0 {
        item.blocker_codes.push(BlockerCode::NonPositiveRowid);
    }
    let bad_decided: i64 = row.get("bad_decided")?;
    if bad_decided != 0 {
        item.blocker_codes.push(BlockerCode::UnrenderableDecidedAt);
    }
    let bad_expires: i64 = row.get("bad_expires")?;
    if bad_expires != 0 {
        item.blocker_codes.push(BlockerCode::UnrenderableExpiresAt);
    }
    if item.actor.is_some()
        || item.payload_json.is_some()
        || item.decided_at_iso.is_some()
        || item.expires_at_iso.is_some()
        || item.op_id.is_some()
        || item.sensor_id.is_some()
    {
        item.blocker_codes
            .push(BlockerCode::KindNullEventFieldDrift);
    }
    Ok(item)
}

fn mark_consent_mirror_reset_if_ready(
    conn: &Connection,
    repaired_at: i64,
) -> Result<(), StoreError> {
    if !has_table(conn, "consent_mirror_resets")? || !has_migration(conn, 21)? {
        return Ok(());
    }

    conn.execute(
        "INSERT OR REPLACE INTO consent_mirror_resets \
           (migration_id, applied_at, consumed, db_nonce) \
         VALUES (21, ?1, 0, lower(hex(randomblob(16))))",
        [repaired_at],
    )?;
    Ok(())
}

fn has_table(conn: &Connection, name: &str) -> Result<bool, StoreError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(\
           SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1\
         )",
        [name],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

fn has_migration(conn: &Connection, migration_id: i64) -> Result<bool, StoreError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(\
           SELECT 1 FROM schema_migrations WHERE migration_id = ?1\
         )",
        [migration_id],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}
