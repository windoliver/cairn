//! Repair helpers for legacy `consent_journal` rows that block migration 0021.

use rusqlite::Connection;
use serde::Serialize;

use crate::error::StoreError;

/// Reason a `consent_journal` row blocks migration 0021.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    /// Legacy row has `rowid <= 0`, which cannot be preserved after 0021.
    NonPositiveRowid,
    /// Legacy `decided_at` cannot be rendered as RFC3339 by SQLite.
    UnrenderableDecidedAt,
    /// Legacy `expires_at` cannot be rendered as RFC3339 by SQLite.
    UnrenderableExpiresAt,
    /// `kind IS NULL` row carries post-0009 event-shape fields.
    KindNullEventFieldDrift,
}

/// Legacy `consent_journal` row requiring operator triage.
#[derive(Debug, Clone, Serialize)]
pub struct ConsentJournalRepairRow {
    /// SQLite rowid, used by the mirror cursor and by the repair command.
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

/// Enumerate legacy rows that are known to block migration 0021.
///
/// # Errors
/// Returns [`StoreError`] for SQLite failures.
pub fn list_blockers(conn: &Connection) -> Result<Vec<ConsentJournalRepairRow>, StoreError> {
    apply_repair_pragmas(conn)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, consent_id, subject, scope, decision, granted_by, decided_at, expires_at, \
                op_id, kind, sensor_id, actor, payload_json, decided_at_iso, expires_at_iso, \
                strftime('%Y-%m-%dT%H:%M:%SZ', decided_at / 1000, 'unixepoch') IS NULL AS bad_decided, \
                (expires_at IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%SZ', expires_at / 1000, 'unixepoch') IS NULL) AS bad_expires \
         FROM consent_journal \
         WHERE kind IS NULL \
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([], |row| {
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
            item.blocker_codes
                .push(BlockerCode::UnrenderableDecidedAt);
        }
        let bad_expires: i64 = row.get("bad_expires")?;
        if bad_expires != 0 {
            item.blocker_codes
                .push(BlockerCode::UnrenderableExpiresAt);
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
    })?;

    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if !row.blocker_codes.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}

fn apply_repair_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
    Ok(())
}
