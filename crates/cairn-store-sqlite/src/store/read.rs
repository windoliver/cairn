//! `MemoryStore::{get, list, versions}` impls.
//!
//! All three are read-only paths over the `records` table:
//!
//! - [`SqliteMemoryStore::do_get`] hydrates one record from `record_json`,
//!   suppressing tombstoned rows so the trait contract holds (`tombstoned`
//!   rows are not exposed via `get`).
//! - [`SqliteMemoryStore::do_list`] pages through active, non-tombstoned
//!   rows ordered `(updated_at DESC, record_id DESC)`. Optional kind /
//!   class / visibility filters are AND-combined; the visibility allowlist
//!   is empty-means-no-filter per the [`ListArgs`] contract.
//! - [`SqliteMemoryStore::do_versions`] returns full per-target history
//!   ordered oldest → newest, including inactive and tombstoned rows.
//!
//! The taxonomy column values written by [`crate::store::projection`] flow
//! from each enum's `as_str` accessor on the domain side; reading back the
//! same accessor here keeps the column ↔ enum mapping in lock-step without
//! re-stating the wire spelling in the `SQLite` adapter.

use std::collections::HashMap;

use cairn_core::contract::memory_store::{
    IndexStats, ListArgs, ListCursor, ListPage, RecordVersion, TombstoneReason,
};
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::{
    body_hash_from_str, record_from_json, record_id_from_str, target_id_from_str,
};

/// Hard upper bound on a single `list` page; defends against a caller
/// passing an unrealistically large `limit` and exhausting memory.
const LIST_LIMIT_MAX: usize = 1000;

impl SqliteMemoryStore {
    /// Inherent `get` implementation; the trait method
    /// [`MemoryStore::get`] guards `self.conn` then delegates here.
    ///
    /// Tombstoned and internal COW staging rows are filtered at the SQL
    /// boundary so callers see `Ok(None)` for missing, soft-deleted, and
    /// not-yet-activated rows.
    ///
    /// [`MemoryStore::get`]: cairn_core::contract::memory_store::MemoryStore::get
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails, [`StoreError::Sqlite`] for surfaced SQL errors, and
    /// [`StoreError::Codec`] when `record_json` cannot be deserialized.
    #[instrument(
        skip(self),
        err,
        fields(verb = "get", record_id = %id.as_str()),
    )]
    pub(crate) async fn do_get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        let conn = self.require_conn("get")?.clone();
        let key = id.as_str().to_owned();

        let record = conn
            .call(move |c| {
                let row: Option<(String, String)> = c
                    .query_row(
                        "SELECT record_json, consent_model FROM records \
                          WHERE record_id = ?1 AND tombstoned = 0 AND cow_staged = 0",
                        params![key],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                match row {
                    None => Ok::<_, tokio_rusqlite::Error>(None),
                    Some((json, consent_model)) => {
                        let mut r = record_from_json(&json)
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                        // Hydrate consent_model from the hot column —
                        // it's `#[serde(skip)]` on MemoryRecord so it
                        // doesn't ride through record_json (Issue #253).
                        r.consent_model = Some(
                            crate::store::upsert::parse_consent_model(&consent_model)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?,
                        );
                        Ok(Some(r))
                    }
                }
            })
            .await?;
        Ok(record)
    }

    /// Inherent `list` implementation; the trait method
    /// [`MemoryStore::list`] guards `self.conn` then delegates here.
    ///
    /// Pages active, non-tombstoned rows ordered `(updated_at DESC,
    /// record_id DESC)`. The `args.limit` is clamped to `[1, LIST_LIMIT_MAX]`;
    /// the worker over-fetches by one row to detect end-of-stream and emits
    /// `next_cursor = Some(_)` only when a further page exists.
    ///
    /// [`MemoryStore::list`]: cairn_core::contract::memory_store::MemoryStore::list
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] / [`StoreError::Sqlite`] for SQL
    /// failures, [`StoreError::Codec`] when a `record_json` payload cannot
    /// be hydrated, and [`StoreError::Invariant`] when a stored
    /// `record_id` cannot be parsed back into the typed newtype (corruption /
    /// schema drift signal).
    #[instrument(
        skip(self, args),
        err,
        fields(verb = "list", limit = args.limit),
    )]
    pub(crate) async fn do_list(&self, args: &ListArgs) -> Result<ListPage, StoreError> {
        let conn = self.require_conn("list")?.clone();
        let limit = args.limit.clamp(1, LIST_LIMIT_MAX);
        let kind = args.kind.map(|k| k.as_str().to_owned());
        let class = args.class.map(|c| c.as_str().to_owned());
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let cursor = args.cursor.clone();

        let page = conn
            .call(move |c| {
                let (sql, p) = build_list_query(
                    kind.as_deref(),
                    class.as_deref(),
                    &visibilities,
                    cursor.as_ref(),
                    limit,
                )
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

                let mut stmt = c.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(p.iter()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                let has_more = rows.len() > limit;
                let mut records = Vec::with_capacity(rows.len().min(limit));
                let mut last: Option<(i64, String)> = None;
                for (i, (json, updated_at, rid, consent_model)) in rows.into_iter().enumerate() {
                    if i >= limit {
                        break;
                    }
                    let mut r = record_from_json(&json)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    r.consent_model = Some(
                        crate::store::upsert::parse_consent_model(&consent_model)
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?,
                    );
                    records.push(r);
                    last = Some((updated_at, rid));
                }
                let next_cursor = if has_more {
                    match last {
                        Some((updated_at, rid)) => {
                            let record_id = record_id_from_str(&rid)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            Some(ListCursor {
                                updated_at,
                                record_id,
                            })
                        }
                        None => None,
                    }
                } else {
                    None
                };
                Ok::<_, tokio_rusqlite::Error>(ListPage {
                    records,
                    next_cursor,
                })
            })
            .await?;
        Ok(page)
    }

    /// Inherent `versions` implementation; the trait method
    /// [`MemoryStore::versions`] guards `self.conn` then delegates here.
    ///
    /// Returns the committed per-target history ordered `version ASC` (oldest
    /// first). Includes both active and inactive rows AND tombstoned rows,
    /// but excludes internal COW staging rows until activation commits them;
    /// the contract requires audit-grade visibility for committed lifecycle
    /// workflows (brief §10).
    ///
    /// [`MemoryStore::versions`]: cairn_core::contract::memory_store::MemoryStore::versions
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] / [`StoreError::Sqlite`] for SQL
    /// failures, [`StoreError::Codec`] when reading any structured column
    /// fails, and [`StoreError::Invariant`] when a stored id, body hash,
    /// or version counter fails the typed-newtype validation (corruption /
    /// schema drift), including `version` values that overflow `u32`.
    #[instrument(
        skip(self),
        err,
        fields(verb = "versions", target_id = %target.as_str()),
    )]
    pub(crate) async fn do_versions(
        &self,
        target: &TargetId,
    ) -> Result<Vec<RecordVersion>, StoreError> {
        let conn = self.require_conn("versions")?.clone();
        let key = target.as_str().to_owned();

        let history = conn
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT record_id, target_id, version, created_at, updated_at, \
                            active, tombstoned, tombstone_reason, body_hash, \
                            schema_version_major, schema_version_minor \
                       FROM records \
                      WHERE target_id = ?1 AND cow_staged = 0 \
                      ORDER BY version ASC",
                )?;
                let rows = stmt
                    .query_map(params![key], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, Option<i64>>(9)?,
                            row.get::<_, Option<i64>>(10)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let v = project_version_row(row)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    out.push(v);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(history)
    }

    /// Inherent `index_stats` implementation; the trait method
    /// [`MemoryStore::index_stats`] guards `self.conn` then delegates here.
    ///
    /// Runs two `COUNT(*)` queries inside a single deferred read transaction
    /// so both counts observe the same `SQLite` snapshot. Without the
    /// transaction, an ingest landing between the two `query_row` calls can
    /// surface as a phantom `index_drift` finding.
    ///
    /// [`MemoryStore::index_stats`]: cairn_core::contract::memory_store::MemoryStore::index_stats
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails, or [`StoreError::Sqlite`] for SQL errors.
    #[instrument(skip(self), err, fields(verb = "index_stats"))]
    pub(crate) async fn do_index_stats(&self) -> Result<IndexStats, StoreError> {
        let conn = self.require_conn("index_stats")?.clone();
        let stats = conn
            .call(move |c| {
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                let records_active: u64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .and_then(|v| {
                        u64::try_from(v).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, v))
                    })?;
                let fts5_rows: u64 = tx
                    .query_row("SELECT COUNT(*) FROM records_fts", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .and_then(|v| {
                        u64::try_from(v).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, v))
                    })?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(IndexStats::new(records_active, fts5_rows))
            })
            .await?;
        Ok(stats)
    }

    /// Inherent `list_consent_models` implementation; the trait method
    /// [`MemoryStore::list_consent_models`] guards `self.conn` then
    /// delegates here. Returns one entry per active, non-tombstoned record
    /// keyed by `record_id`, with the `consent_model` column projected to
    /// the typed [`ConsentModel`] enum.
    ///
    /// [`MemoryStore::list_consent_models`]: cairn_core::contract::memory_store::MemoryStore::list_consent_models
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] / [`StoreError::Sqlite`] for SQL
    /// failures, or [`StoreError::Invariant`] when a stored `record_id` or
    /// `consent_model` value is unrecognised (corruption / schema drift).
    #[instrument(skip(self), err, fields(verb = "list_consent_models"))]
    pub(crate) async fn do_list_consent_models(
        &self,
    ) -> Result<HashMap<RecordId, ConsentModel>, StoreError> {
        let conn = self.require_conn("list_consent_models")?.clone();
        let out = conn
            .call(|c| {
                let mut stmt = c.prepare(
                    "SELECT record_id, consent_model FROM records \
                      WHERE active = 1 AND tombstoned = 0",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                let mut out = HashMap::with_capacity(rows.len());
                for (id_s, model_s) in rows {
                    let id = record_id_from_str(&id_s)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let model = match model_s.as_str() {
                        "legacy_event" => ConsentModel::LegacyEvent,
                        "receipt_timeline" => ConsentModel::ReceiptTimeline,
                        other => {
                            return Err(tokio_rusqlite::Error::Other(Box::new(
                                StoreError::Invariant {
                                    what: format!("records.consent_model unknown variant: {other}"),
                                },
                            )));
                        }
                    };
                    out.insert(id, model);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(out)
    }
}

/// Tuple shape returned by the `versions` `query_map` row mapper. Pulled
/// out so [`project_version_row`] can take a single argument and keep the
/// `do_versions` async shell under the workspace's
/// `clippy::too_many_lines` limit.
type VersionRowTuple = (
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
);

/// Project one raw row of the `versions(target)` query into a
/// [`RecordVersion`]. The synchronous worker calls this for every row;
/// any [`StoreError`] is surfaced through `tokio_rusqlite::Error::Other`.
fn project_version_row(row: VersionRowTuple) -> Result<RecordVersion, StoreError> {
    let (
        record_id,
        target_id,
        version,
        created_at,
        updated_at,
        active,
        tombstoned,
        reason,
        hash,
        schema_major,
        schema_minor,
    ) = row;
    let rec_id = record_id_from_str(&record_id)?;
    let tgt = target_id_from_str(&target_id)?;
    let body_hash = body_hash_from_str(&hash)?;
    let version = u32::try_from(version).map_err(|_| StoreError::Invariant {
        what: format!("stored version overflows u32: {version}"),
    })?;
    // NULL → unstamped legacy row (pre-0041). Surface as `None` so
    // the §6.4 stale_schema lint can flag those rows for rewrite
    // instead of silently treating them as current.
    let schema_version = match (schema_major, schema_minor) {
        (Some(major), Some(minor)) => Some(cairn_core::contract::version::SchemaVersion {
            major: u32::try_from(major).map_err(|_| StoreError::Invariant {
                what: format!("schema_version_major out of u32 range: {major}"),
            })?,
            minor: u32::try_from(minor).map_err(|_| StoreError::Invariant {
                what: format!("schema_version_minor out of u32 range: {minor}"),
            })?,
        }),
        (None, None) => None,
        // Mixed NULL / non-NULL is corruption — both columns are written
        // together on every fresh row, so a half-NULL pair signals
        // manual SQL drift rather than legacy data.
        (major, minor) => {
            return Err(StoreError::Invariant {
                what: format!(
                    "records.schema_version_(major,minor) inconsistent: ({major:?}, {minor:?})"
                ),
            });
        }
    };
    Ok(RecordVersion {
        record_id: rec_id,
        target_id: tgt,
        version,
        created_at,
        updated_at,
        active: active != 0,
        tombstoned: tombstoned != 0,
        tombstone_reason: reason.as_deref().and_then(TombstoneReason::parse),
        body_hash,
        schema_version,
    })
}

/// Compose the SQL string + bound parameters for a single `list` page.
///
/// The query selects `record_json`, `updated_at`, and `record_id` from
/// active, non-tombstoned rows; AND-combines the optional kind/class
/// filters and the visibility allowlist (empty = no restriction); applies
/// the optional keyset cursor; and over-fetches by one row so the caller
/// can detect end-of-stream via `rows.len() > limit`.
fn build_list_query(
    kind: Option<&str>,
    class: Option<&str>,
    visibilities: &[String],
    cursor: Option<&ListCursor>,
    limit: usize,
) -> Result<(String, Vec<rusqlite::types::Value>), StoreError> {
    let mut sql = String::from(
        "SELECT record_json, updated_at, record_id, consent_model FROM records \
          WHERE active = 1 AND tombstoned = 0 AND cow_staged = 0",
    );
    let mut p: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(k) = kind {
        sql.push_str(" AND kind = ?");
        p.push(k.to_owned().into());
    }
    if let Some(cl) = class {
        sql.push_str(" AND class = ?");
        p.push(cl.to_owned().into());
    }
    if !visibilities.is_empty() {
        sql.push_str(" AND visibility IN (");
        sql.push_str(&vec!["?"; visibilities.len()].join(","));
        sql.push(')');
        for v in visibilities {
            p.push(v.clone().into());
        }
    }
    if let Some(cur) = cursor {
        sql.push_str(" AND (updated_at, record_id) < (?, ?)");
        p.push(cur.updated_at.into());
        p.push(cur.record_id.as_str().to_owned().into());
    }
    sql.push_str(" ORDER BY updated_at DESC, record_id DESC LIMIT ?");
    let plus_one = limit.checked_add(1).ok_or_else(|| StoreError::Invariant {
        what: format!("list limit + 1 overflows usize: {limit}"),
    })?;
    let bound = i64::try_from(plus_one).map_err(|_| StoreError::Invariant {
        what: format!("list limit + 1 overflows i64: {plus_one}"),
    })?;
    p.push(bound.into());
    Ok((sql, p))
}

#[cfg(test)]
mod tests {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::MemoryRecord;

    use crate::open_in_memory;

    fn base() -> MemoryRecord {
        cairn_core::domain::record::tests_export::sample_record()
    }

    #[tokio::test]
    async fn list_consent_models_returns_one_entry_per_active_record_with_correct_tag() {
        let store = open_in_memory().await.expect("open in-memory store");

        // Seed two active rows; both default to legacy_event under 0022.
        let r1 = base();
        let mut r2 = base();
        r2.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000002")
            .expect("invariant: valid ulid");
        r2.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000002")
            .expect("invariant: valid ulid for target_id");

        store.upsert(&r1).await.expect("upsert r1");
        store.upsert(&r2).await.expect("upsert r2");

        // Flip r2's consent_model to receipt_timeline via raw SQL.
        let conn = store
            .require_conn("test.flip")
            .expect("invariant: connected store")
            .clone();
        let r2_id = r2.id.as_str().to_owned();
        conn.call(move |c| {
            let n = c.execute(
                "UPDATE records SET consent_model = 'receipt_timeline' \
                  WHERE record_id = ?1",
                rusqlite::params![r2_id],
            )?;
            assert_eq!(n, 1, "expected exactly one row flipped");
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("flip consent_model");

        let map = super::SqliteMemoryStore::do_list_consent_models(&store)
            .await
            .expect("list_consent_models");
        assert_eq!(map.len(), 2, "one entry per active record");
        assert_eq!(
            map.get(&r1.id),
            Some(&super::ConsentModel::LegacyEvent),
            "r1 stays at the migration default",
        );
        assert_eq!(
            map.get(&r2.id),
            Some(&super::ConsentModel::ReceiptTimeline),
            "r2 reflects the flipped column value",
        );
    }
}
