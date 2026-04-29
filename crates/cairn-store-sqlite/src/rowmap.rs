//! Row → domain mapping helpers.
//!
//! Concentrates JSON column handling so `store.rs` and `apply.rs` stay
//! focused on SQL. The `record_json` column stores the full serialized
//! `MemoryRecord` for lossless round-trips; per-column JSON values
//! (`scope`, `taxonomy`, etc.) exist for SQL-level filtering and rebac.

use cairn_core::contract::memory_store::{
    error::StoreError,
    types::{
        ChangeKind, HistoryEntry, OpId, PurgeMarker, RecordEvent, RecordId, RecordVersion, TargetId,
    },
};
use cairn_core::domain::{actor_ref::ActorRef, record::MemoryRecord, timestamp::Rfc3339Timestamp};
use rusqlite::Row;

/// Deserialize a `MemoryRecord` from the `record_json` column.
///
/// Falls back to `StoreError::Invariant` when the column is NULL or
/// the JSON fails to parse (should never happen for well-written rows;
/// old rows from before migration 0009 will have NULL).
pub fn row_to_record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let json: Option<String> = row.get("record_json")?;
    let json = json.ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(crate::error::SqliteStoreError::Rusqlite(
                rusqlite::Error::InvalidQuery,
            )),
        )
    })?;
    serde_json::from_str::<MemoryRecord>(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Build a `RecordVersion` from a row (for `version_history`).
///
/// Does not read `record_json`; sources only the version-lifecycle columns.
pub fn row_to_record_version(row: &Row<'_>) -> rusqlite::Result<RecordVersion> {
    let record_id: String = row.get("record_id")?;
    let target_id: String = row.get("target_id")?;
    let version: i64 = row.get("version")?;
    let active: i64 = row.get("active")?;
    let tombstoned: i64 = row.get("tombstoned")?;
    let tombstoned_at: Option<String> = row.get("tombstoned_at")?;
    let tombstoned_by: Option<String> = row.get("tombstoned_by")?;
    let expired_at: Option<String> = row.get("expired_at")?;
    let activated_at: Option<String> = row.get("activated_at")?;
    let activated_by: Option<String> = row.get("activated_by")?;
    // Sort keys (seconds since epoch) computed by SQLite's `unixepoch`
    // to avoid lexical comparison of RFC3339 strings — the latter is
    // unsafe once timezone offsets are involved (a `+02:00` instant
    // sorts after a `Z` instant earlier in real time).
    let activated_at_epoch: Option<i64> = row.get("activated_at_epoch").unwrap_or(None);
    let tombstoned_at_epoch: Option<i64> = row.get("tombstoned_at_epoch").unwrap_or(None);
    let expired_at_epoch: Option<i64> = row.get("expired_at_epoch").unwrap_or(None);

    // Emit an `Update` lifecycle event ONLY for versions that were
    // actually activated. A row that was staged but never activated has
    // `activated_at IS NULL` and contributes no Update event — staging
    // alone is not a lifecycle change worth surfacing through
    // `version_history`.
    let mut events: Vec<(RecordEvent, Option<i64>)> = Vec::new();
    if let (Some(at_str), Some(by_str)) = (activated_at, activated_by) {
        events.push((
            RecordEvent {
                kind: ChangeKind::Update,
                at: Rfc3339Timestamp::parse(&at_str).ok(),
                actor: Some(ActorRef::from_string(&by_str)),
            },
            activated_at_epoch,
        ));
    }

    // Emit a `Tombstone` event whenever the column is set, regardless of
    // current `active` state — historical events must not vanish when a
    // version is later superseded.
    if tombstoned == 1
        && let (Some(at_str), Some(by_str)) = (tombstoned_at, tombstoned_by)
    {
        events.push((
            RecordEvent {
                kind: ChangeKind::Tombstone,
                at: Rfc3339Timestamp::parse(&at_str).ok(),
                actor: Some(ActorRef::from_string(&by_str)),
            },
            tombstoned_at_epoch,
        ));
    }

    // Same for `Expire`: persist if the column is set, even when the row
    // has since been superseded.
    if let Some(at_str) = expired_at {
        events.push((
            RecordEvent {
                kind: ChangeKind::Expire,
                at: Rfc3339Timestamp::parse(&at_str).ok(),
                actor: None,
            },
            expired_at_epoch,
        ));
    }

    // The `RecordVersion` contract documents events as ascending by
    // timestamp. Sort by the SQLite-derived unix epoch (a real instant)
    // rather than the RFC3339 string, since lexical compare is wrong
    // once offsets are involved. Events whose epoch could not be parsed
    // (e.g. a corrupted column) sort last.
    events.sort_by(|a, b| match (a.1, b.1) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let events: Vec<RecordEvent> = events.into_iter().map(|(e, _)| e).collect();

    Ok(RecordVersion {
        record_id: RecordId(record_id),
        target_id: TargetId(target_id),
        version: u64::try_from(version).unwrap_or(0),
        active: active == 1,
        events,
    })
}

/// Build a `PurgeMarker` from a `record_purges` row.
pub fn row_to_purge_marker(row: &Row<'_>) -> rusqlite::Result<PurgeMarker> {
    let target_id: String = row.get("target_id")?;
    let op_id: String = row.get("op_id")?;
    let purged_at: String = row.get("purged_at")?;
    let purged_by: String = row.get("purged_by")?;
    let body_hash_salt: String = row.get("body_hash_salt")?;
    Ok(PurgeMarker {
        target_id: TargetId(target_id),
        op_id: OpId(op_id),
        event: RecordEvent {
            kind: ChangeKind::Purge,
            at: Rfc3339Timestamp::parse(&purged_at).ok(),
            actor: Some(ActorRef::from_string(&purged_by)),
        },
        body_hash_salt,
    })
}

/// Concatenate `Version` entries (ordered by version ASC, already filtered)
/// with `Purge` markers (ordered by `purged_at` ASC).
#[must_use]
pub fn into_history(versions: Vec<RecordVersion>, purges: Vec<PurgeMarker>) -> Vec<HistoryEntry> {
    let mut out: Vec<HistoryEntry> = versions.into_iter().map(HistoryEntry::Version).collect();
    out.extend(purges.into_iter().map(HistoryEntry::Purge));
    out
}

/// Convert a `rusqlite::Error` into the abstract `StoreError::Backend` variant.
#[must_use]
pub fn store_err(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(Box::new(crate::error::SqliteStoreError::Rusqlite(e)))
}
