//! Consent journal store helpers (brief §14, issue #94).
//!
//! Sync helpers over a `rusqlite::Connection` that map between the
//! body-free [`ConsentEvent`] domain type and the `consent_journal` row.
//! The async `.cairn/consent.log` materializer (in `cairn-workflows`)
//! tails this table by `rowid` — we therefore expose [`read_since_rowid`]
//! as the cursor primitive and [`append`] returns the new rowid so the
//! caller can advance any in-memory cursor it holds.
//!
//! All writes here go directly through `SQLite`'s per-statement transaction;
//! callers that need to couple a consent insert with a WAL state transition
//! should wrap the two in a single `BEGIN IMMEDIATE; … COMMIT;` per
//! brief §5.6.

use std::collections::HashSet;

use cairn_core::contract::{MalformedSourceForget, MalformedSourceForgetReason, SourceForget};
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, Rfc3339Timestamp, SensorLabel,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::error::StoreError;

/// Insert a `ConsentEvent` into `consent_journal`. Returns the new rowid
/// (so a caller advancing a cursor knows what to anchor on).
///
/// The `decided_at` INTEGER column is filled from the same RFC3339 string
/// the materializer reads, parsed to UNIX millis. Conversion failure
/// surfaces as [`StoreError::Sqlite`] via the underlying constraint.
///
/// # Errors
/// Returns [`StoreError`] if the insert fails — typically a CHECK trigger
/// firing (unknown kind, body-bearing forget payload, missing iso, …) or
/// the underlying `SQLite` layer reporting a constraint violation.
pub fn append(conn: &Connection, event: &ConsentEvent) -> Result<i64, StoreError> {
    append_inner(conn, event)
}

/// Insert a `ConsentEvent` into `consent_journal` inside an existing transaction.
///
/// # Errors
/// Returns [`StoreError`] if the insert fails.
pub fn append_in_tx(tx: &Transaction<'_>, event: &ConsentEvent) -> Result<i64, StoreError> {
    append_inner(tx, event)
}

fn append_inner(conn: &Connection, event: &ConsentEvent) -> Result<i64, StoreError> {
    event.validate()?;
    let payload_json =
        serde_json::to_string(&event.payload).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    let decided_millis = rfc3339_to_millis(&event.decided_at);
    let expires_millis = event.expires_at.as_ref().map(rfc3339_to_millis);

    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, reason, granted_by, \
           decided_at, expires_at, \
           op_id, kind, sensor_id, actor, payload_json, \
           decided_at_iso, expires_at_iso) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            event.consent_id,
            event.subject,
            event.scope,
            kind_to_decision(event.kind),
            Option::<String>::None,
            event.actor.as_str(),
            decided_millis,
            expires_millis,
            event.op_id,
            kind_wire(event.kind),
            event.sensor_id.as_ref().map(SensorLabel::as_str),
            event.actor.as_str(),
            payload_json,
            event.decided_at.as_str(),
            event.expires_at.as_ref().map(Rfc3339Timestamp::as_str),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// All event-kind rows whose `op_id` matches.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_by_op(conn: &Connection, op_id: &str) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "op_id = ?", params![op_id])
}

/// All event-kind rows authored by `actor`.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_by_actor(
    conn: &Connection,
    actor: &Identity,
) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "actor = ?", params![actor.as_str()])
}

/// All event-kind rows for a given sensor.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_by_sensor(
    conn: &Connection,
    sensor: &SensorLabel,
) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "sensor_id = ?", params![sensor.as_str()])
}

/// All event-kind rows for a given scope tuple wire form.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_by_scope(conn: &Connection, scope: &str) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "scope = ?", params![scope])
}

/// All event-kind rows for a given subject.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_by_subject(conn: &Connection, subject: &str) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "subject = ?", params![subject])
}

/// All `source_forget` event-kind rows, ordered by `rowid`.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn query_source_forgets(conn: &Connection) -> Result<Vec<ConsentEvent>, StoreError> {
    query_where(conn, "kind = ?", params!["source_forget"])
}

/// Mirror cursor primitive: every event-kind row with `rowid > since`,
/// in `rowid` order, paired with its rowid so the caller can advance the
/// cursor monotonically.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or row-decode errors.
pub fn read_since_rowid(
    conn: &Connection,
    since: i64,
) -> Result<Vec<(i64, ConsentEvent)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT rowid, consent_id, kind, actor, subject, scope, op_id, sensor_id, \
                payload_json, decided_at_iso, expires_at_iso \
         FROM consent_journal \
         WHERE rowid > ? \
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map(params![since], |row| {
        let rowid: i64 = row.get(0)?;
        Ok((rowid, decode_event(row)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (rowid, event) = row?;
        out.push((rowid, event?));
    }
    Ok(out)
}

/// Highest rowid currently visible in the journal. Useful for cursor
/// recovery: a rebuild starts from `0` and advances to this value.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures.
pub fn max_rowid(conn: &Connection) -> Result<i64, StoreError> {
    let value: Option<i64> = conn
        .query_row("SELECT MAX(rowid) FROM consent_journal", [], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(value.unwrap_or(0))
}

/// Every forgotten source-bytes hash currently persisted in the consent
/// journal.
///
/// Today this looks for future `source_forget` rows and returns an empty
/// set on current schemas where that kind is not yet written. Keeping the
/// query here lets CLI lint inject a real store-backed reader now without
/// teaching `cairn-core` about `SQLite`.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or malformed
/// `source_forget` payload JSON.
pub fn forgotten_source_bytes_hashes(conn: &Connection) -> Result<HashSet<String>, StoreError> {
    Ok(source_forget_snapshot(conn)?
        .rows
        .into_iter()
        .map(|row| row.source_bytes_hash)
        .collect())
}

/// Parsed well-formed `source_forget` rows currently visible in the journal.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or malformed JSON payloads.
pub fn forgotten_source_forgets(conn: &Connection) -> Result<Vec<SourceForget>, StoreError> {
    Ok(source_forget_snapshot(conn)?.rows)
}

/// Parsed malformed `source_forget` rows currently visible in the journal.
///
/// # Errors
/// Returns [`StoreError`] on `SQLite` failures or malformed JSON payloads.
pub fn malformed_source_forget_rows(
    conn: &Connection,
) -> Result<Vec<MalformedSourceForget>, StoreError> {
    Ok(source_forget_snapshot(conn)?.malformed)
}

#[derive(Default)]
struct SourceForgetSnapshot {
    rows: Vec<SourceForget>,
    malformed: Vec<MalformedSourceForget>,
}

fn source_forget_snapshot(conn: &Connection) -> Result<SourceForgetSnapshot, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT op_id, subject, payload_json \
         FROM consent_journal \
         WHERE kind = 'source_forget' \
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = SourceForgetSnapshot::default();
    for row in rows {
        let (op_id, subject, payload_json) = row?;
        let parsed = parse_source_forget_row(op_id, subject, payload_json)?;
        match parsed {
            ParsedSourceForget::WellFormed(row) => out.rows.push(row),
            ParsedSourceForget::Malformed(row) => out.malformed.push(row),
        }
    }
    Ok(out)
}

enum ParsedSourceForget {
    WellFormed(SourceForget),
    Malformed(MalformedSourceForget),
}

fn parse_source_forget_row(
    op_id: Option<String>,
    subject: Option<String>,
    payload_json: Option<String>,
) -> Result<ParsedSourceForget, StoreError> {
    let op_id = op_id.unwrap_or_default();
    let payload_json = payload_json.unwrap_or_else(|| "null".to_owned());
    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
    let source_id = payload
        .get("source_id")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| subject.unwrap_or_default(), ToOwned::to_owned);
    let source_bytes_hash = payload
        .get("source_bytes_hash")
        .or_else(|| payload.get("target_id_hash"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);

    let Some(source_bytes_hash) = source_bytes_hash else {
        return Ok(ParsedSourceForget::Malformed(MalformedSourceForget {
            op_id,
            source_id,
            source_bytes_hash: None,
            reason: MalformedSourceForgetReason::MalformedSourceBytesHashFormat,
        }));
    };

    if !looks_like_source_hash(&source_bytes_hash) {
        return Ok(ParsedSourceForget::Malformed(MalformedSourceForget {
            op_id,
            source_id,
            source_bytes_hash: None,
            reason: MalformedSourceForgetReason::MalformedSourceBytesHashFormat,
        }));
    }

    let target = if let Some(target) = payload.get("target").filter(|value| !value.is_null()) {
        let version = target.get("version").and_then(serde_json::Value::as_u64);
        let Some(version) = version else {
            return Ok(ParsedSourceForget::Malformed(MalformedSourceForget {
                op_id,
                source_id,
                source_bytes_hash: Some(source_bytes_hash),
                reason: MalformedSourceForgetReason::UnsupportedReplayHashVersion { version: 0 },
            }));
        };
        if version != 1 {
            return Ok(ParsedSourceForget::Malformed(MalformedSourceForget {
                op_id,
                source_id,
                source_bytes_hash: Some(source_bytes_hash),
                reason: MalformedSourceForgetReason::UnsupportedReplayHashVersion {
                    version: u32::try_from(version).unwrap_or(u32::MAX),
                },
            }));
        }
        let target_hash = target.get("hash").and_then(serde_json::Value::as_str);
        let Some(target_hash) = target_hash.filter(|h| looks_like_source_hash(h)) else {
            return Ok(ParsedSourceForget::Malformed(MalformedSourceForget {
                op_id,
                source_id,
                source_bytes_hash: Some(source_bytes_hash),
                reason: MalformedSourceForgetReason::MalformedReplayHashFormat,
            }));
        };
        Some(cairn_core::contract::TargetReplayKey {
            hash: target_hash.to_owned(),
            version: u32::try_from(version).unwrap_or(u32::MAX),
        })
    } else {
        None
    };

    Ok(ParsedSourceForget::WellFormed(SourceForget {
        op_id,
        source_id,
        source_bytes_hash,
        target,
    }))
}

fn looks_like_source_hash(raw: &str) -> bool {
    let Some((algo, hex)) = raw.split_once(':') else {
        return false;
    };
    let expected_len = match algo {
        "sha256" | "blake3" => 64,
        "sha512" => 128,
        _ => return false,
    };
    hex.len() == expected_len && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn query_where(
    conn: &Connection,
    pred: &str,
    p: &[&dyn rusqlite::ToSql],
) -> Result<Vec<ConsentEvent>, StoreError> {
    let sql = format!(
        "SELECT rowid, consent_id, kind, actor, subject, scope, op_id, sensor_id, \
                payload_json, decided_at_iso, expires_at_iso \
         FROM consent_journal \
         WHERE {pred} \
         ORDER BY rowid ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(p, decode_event)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}

fn decode_event(row: &Row<'_>) -> rusqlite::Result<Result<ConsentEvent, StoreError>> {
    let consent_id: String = row.get("consent_id")?;
    let kind_text: Option<String> = row.get("kind")?;
    let actor_text: Option<String> = row.get("actor")?;
    let subject: String = row.get("subject")?;
    let scope: String = row.get("scope")?;
    let op_id: Option<String> = row.get("op_id")?;
    let sensor_text: Option<String> = row.get("sensor_id")?;
    let payload_json: Option<String> = row.get("payload_json")?;
    let decided_iso: Option<String> = row.get("decided_at_iso")?;
    let expires_iso: Option<String> = row.get("expires_at_iso")?;

    Ok(decode_event_inner(
        consent_id,
        kind_text,
        actor_text,
        subject,
        scope,
        op_id,
        sensor_text,
        payload_json,
        decided_iso,
        expires_iso,
    ))
}

#[allow(clippy::too_many_arguments)]
fn decode_event_inner(
    consent_id: String,
    kind_text: Option<String>,
    actor_text: Option<String>,
    subject: String,
    scope: String,
    op_id: Option<String>,
    sensor_text: Option<String>,
    payload_json: Option<String>,
    decided_iso: Option<String>,
    expires_iso: Option<String>,
) -> Result<ConsentEvent, StoreError> {
    let kind_text = kind_text.ok_or_else(|| {
        StoreError::SchemaDrift(format!("consent_journal row {consent_id}: missing kind"))
    })?;
    let kind = parse_kind(&kind_text).ok_or_else(|| {
        StoreError::SchemaDrift(format!("unknown kind {kind_text:?} in row {consent_id}"))
    })?;
    let actor = actor_text
        .ok_or_else(|| StoreError::SchemaDrift(format!("row {consent_id}: missing actor")))?;
    let actor = Identity::parse(actor).map_err(|e| StoreError::SchemaDrift(e.to_string()))?;
    let payload_json = payload_json
        .ok_or_else(|| StoreError::SchemaDrift(format!("row {consent_id}: missing payload")))?;
    let payload: ConsentPayload = serde_json::from_str(&payload_json)
        .map_err(|e| StoreError::SchemaDrift(format!("row {consent_id}: payload: {e}")))?;
    let decided_at = decided_iso
        .ok_or_else(|| StoreError::SchemaDrift(format!("row {consent_id}: missing decided_iso")))
        .and_then(|s| {
            Rfc3339Timestamp::parse(s).map_err(|e| StoreError::SchemaDrift(e.to_string()))
        })?;
    let expires_at = expires_iso
        .map(|s| Rfc3339Timestamp::parse(s).map_err(|e| StoreError::SchemaDrift(e.to_string())))
        .transpose()?;
    let sensor_id = sensor_text
        .map(|s| SensorLabel::parse(s).map_err(|e| StoreError::SchemaDrift(e.to_string())))
        .transpose()?;

    let event = ConsentEvent {
        consent_id,
        kind,
        actor,
        subject,
        scope,
        op_id,
        sensor_id,
        payload,
        decided_at,
        expires_at,
    };
    // Defense in depth: ConsentEvent::validate() asserts the §14 metadata
    // domain (consent_id / scope / op_id / subject / payload / sensor_id)
    // on every read. Pre-0011 schema didn't enforce closed character classes
    // on those columns; legacy rows promoted by migration 0021 (after the
    // mirror cursor reset added in round 5) replay through this decode path
    // from rowid 0. Migration 0021 itself aborts on out-of-domain legacy
    // metadata, but we re-check here so any drift from a future schema
    // regression or direct-SQL writer fails closed at decode time.
    event.validate().map_err(|e| {
        StoreError::SchemaDrift(format!(
            "consent_journal row {consent_id} failed validate: {e}",
            consent_id = event.consent_id
        ))
    })?;
    Ok(event)
}

const fn kind_wire(kind: ConsentKind) -> &'static str {
    match kind {
        ConsentKind::SensorEnable => "sensor_enable",
        ConsentKind::SensorDisable => "sensor_disable",
        ConsentKind::PolicyChange => "policy_change",
        ConsentKind::RememberIntent => "remember_intent",
        ConsentKind::ForgetIntent => "forget_intent",
        ConsentKind::SourceForget => "source_forget",
        ConsentKind::Grant => "grant",
        ConsentKind::Revoke => "revoke",
        ConsentKind::PromoteReceipt => "promote_receipt",
        ConsentKind::FederationGrant => "federation_grant",
        ConsentKind::FederationAccept => "federation_accept",
        ConsentKind::FederationRevoke => "federation_revoke",
    }
}

fn parse_kind(s: &str) -> Option<ConsentKind> {
    Some(match s {
        "sensor_enable" => ConsentKind::SensorEnable,
        "sensor_disable" => ConsentKind::SensorDisable,
        "policy_change" => ConsentKind::PolicyChange,
        "remember_intent" => ConsentKind::RememberIntent,
        "forget_intent" => ConsentKind::ForgetIntent,
        "source_forget" => ConsentKind::SourceForget,
        "grant" => ConsentKind::Grant,
        "revoke" => ConsentKind::Revoke,
        "promote_receipt" => ConsentKind::PromoteReceipt,
        "federation_grant" => ConsentKind::FederationGrant,
        "federation_accept" => ConsentKind::FederationAccept,
        "federation_revoke" => ConsentKind::FederationRevoke,
        _ => return None,
    })
}

/// 0005's `decision` column is `CHECK(decision IN ('GRANT','REVOKE'))`.
/// All event-kind rows must still satisfy that constraint, so we map every
/// new kind onto a sensible decision proxy: revoke-shaped events count as
/// `'REVOKE'`, everything else as `'GRANT'`.
const fn kind_to_decision(kind: ConsentKind) -> &'static str {
    match kind {
        ConsentKind::SensorDisable
        | ConsentKind::Revoke
        | ConsentKind::ForgetIntent
        | ConsentKind::SourceForget => "REVOKE",
        _ => "GRANT",
    }
}

/// Best-effort RFC3339 → UNIX millis. Used only to populate the legacy
/// `decided_at` INTEGER column. The authoritative timestamp readers consume
/// is `decided_at_iso`.
fn rfc3339_to_millis(ts: &Rfc3339Timestamp) -> i64 {
    rfc3339_to_millis_str(ts.as_str()).unwrap_or(0)
}

fn rfc3339_to_millis_str(raw: &str) -> Option<i64> {
    // Hand-rolled — Rfc3339Timestamp validates structure already, so we
    // only need to compute the integer. Format: YYYY-MM-DDTHH:MM:SS[.f]TZ.
    let bytes = raw.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year: i64 = parse_int(&bytes[..4])?;
    let month: i64 = parse_int(&bytes[5..7])?;
    let day: i64 = parse_int(&bytes[8..10])?;
    let hour: i64 = parse_int(&bytes[11..13])?;
    let minute: i64 = parse_int(&bytes[14..16])?;
    let second: i64 = parse_int(&bytes[17..19])?;

    let mut idx: usize = 19;
    let mut milli: i64 = 0;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = &bytes[frac_start..idx];
        if !frac.is_empty() {
            // Take up to first 3 digits (millis).
            let take = frac.len().min(3);
            let mut v: i64 = 0;
            for &b in &frac[..take] {
                v = v * 10 + i64::from(b - b'0');
            }
            for _ in take..3 {
                v *= 10;
            }
            milli = v;
        }
    }

    // Offset
    let mut offset_minutes: i64 = 0;
    if idx < bytes.len() {
        match bytes[idx] {
            b'Z' | b'z' => {}
            b'+' | b'-' => {
                let sign: i64 = if bytes[idx] == b'+' { 1 } else { -1 };
                let oh: i64 = parse_int(&bytes[idx + 1..idx + 3])?;
                let om: i64 = parse_int(&bytes[idx + 4..idx + 6])?;
                offset_minutes = sign * (oh * 60 + om);
            }
            _ => return None,
        }
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(secs * 1000 + milli)
}

fn parse_int(bytes: &[u8]) -> Option<i64> {
    let mut v: i64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + i64::from(b - b'0');
    }
    Some(v)
}

/// Howard Hinnant's days-from-civil — RFC3339-only callsite, so y > 0
/// suffices. Returns days since 1970-01-01.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_epoch() {
        assert_eq!(rfc3339_to_millis_str("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn millis_known_instant() {
        // 2026-04-28T12:00:00Z → 1_777_377_600 seconds since UNIX epoch.
        assert_eq!(
            rfc3339_to_millis_str("2026-04-28T12:00:00Z"),
            Some(1_777_377_600_000),
        );
    }

    #[test]
    fn millis_with_offset() {
        // +02:00 means local clock is 2h ahead of UTC; subtract the offset.
        let utc = rfc3339_to_millis_str("2026-04-28T14:00:00Z").expect("ok");
        let off = rfc3339_to_millis_str("2026-04-28T16:00:00+02:00").expect("ok");
        assert_eq!(utc, off);
    }

    #[test]
    fn millis_with_fractional() {
        let a = rfc3339_to_millis_str("2026-04-28T12:00:00.500Z").expect("ok");
        let b = rfc3339_to_millis_str("2026-04-28T12:00:00Z").expect("ok");
        assert_eq!(a - b, 500);
    }
}
