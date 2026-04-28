//! `MemoryStore` (read-only) implementation on `SqliteMemoryStore`.
//!
//! Every read dispatches to `tokio::task::spawn_blocking`, acquires the
//! connection mutex synchronously (via `blocking_lock`), and runs pure SQL.
//! Rows the principal cannot read are dropped before return; the count is
//! surfaced via `ListResult::hidden` (brief lines 2557/3287/4136).

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    MemoryStore, MemoryStoreCapabilities,
    error::StoreError,
    types::{HistoryEntry, ListQuery, ListResult, PurgeMarker, RecordVersion, TargetId},
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{principal::Principal, record::MemoryRecord};
use rusqlite::params;

use crate::rebac::principal_can_read;
use crate::rowmap::{
    into_history, row_to_purge_marker, row_to_record, row_to_record_version, store_err,
};

/// Columns needed for a full `MemoryRecord` round-trip plus rebac.
const SELECT_RECORD_COLS: &str = "record_id, target_id, version, active, tombstoned, \
     created_at, created_by, tombstoned_at, tombstoned_by, expired_at, \
     scope, taxonomy, record_json";

/// Capabilities advertised after Task 3: FTS, graph edges, transactions enabled;
/// vector deferred to #48.
static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
    fts: true,
    vector: false,
    graph_edges: true,
    transactions: true,
};

#[async_trait]
impl MemoryStore for crate::SqliteMemoryStore {
    fn name(&self) -> &str {
        crate::PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        // Issue-46 contract is `0.2.0`. Bump together with
        // cairn_core::contract::memory_store::CONTRACT_VERSION.
        VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0))
    }

    async fn get(
        &self,
        principal: &Principal,
        target_id: &TargetId,
    ) -> Result<Option<MemoryRecord>, StoreError> {
        let conn = self.conn.clone();
        let principal = principal.clone();
        let target_id = target_id.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {SELECT_RECORD_COLS} FROM records \
                     WHERE target_id = ?1 \
                       AND active = 1 \
                       AND tombstoned = 0 \
                       AND (expired_at IS NULL OR unixepoch(expired_at) > unixepoch('now'))"
                ))
                .map_err(store_err)?;
            let mut rows = stmt.query(params![target_id.as_str()]).map_err(store_err)?;
            if let Some(row) = rows.next().map_err(store_err)? {
                let scope_json: String = row.get("scope").map_err(store_err)?;
                let taxonomy_json: String = row.get("taxonomy").map_err(store_err)?;
                if !principal_can_read(&principal, &scope_json, &taxonomy_json) {
                    return Ok(None);
                }
                // Treat legacy rows whose `record_json` is NULL or
                // unparseable as effectively missing rather than
                // surfacing a hard error. The migration runner logs
                // legacy rows; this keeps reads alive after upgrade.
                if let Ok(rec) = row_to_record(row) {
                    Ok(Some(rec))
                } else {
                    tracing::warn!(
                        "get: legacy row with unreadable record_json; treating as missing"
                    );
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
    }

    async fn list(&self, query: &ListQuery) -> Result<ListResult, StoreError> {
        let conn = self.conn.clone();
        let q = query.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // Build SQL dynamically based on forensic toggles.
            // Always filter to active rows unless include_tombstoned/include_expired
            // are set (used by WAL recovery and audit paths).
            let mut sql = format!("SELECT {SELECT_RECORD_COLS} FROM records WHERE 1=1");
            if !q.include_tombstoned {
                sql.push_str(" AND tombstoned = 0");
            }
            if !q.include_expired {
                sql.push_str(
                    " AND (expired_at IS NULL OR unixepoch(expired_at) > unixepoch('now'))",
                );
            }
            // Always restrict to active versions for normal reads.
            sql.push_str(" AND active = 1");

            // Optional target_id prefix filter (caller-supplied).
            let mut bind_prefix = false;
            if q.target_prefix.is_some() {
                sql.push_str(" AND target_id LIKE ?1 ESCAPE '\\'");
                bind_prefix = true;
            }

            sql.push_str(" ORDER BY target_id, version");
            // `max_results` documents a *visible*-row cap, so it must be
            // enforced after rebac filtering. Pushing LIMIT into SQL
            // would let hidden rows displace readable ones from the
            // result page.

            let mut stmt = conn.prepare(&sql).map_err(store_err)?;

            // Execute with or without prefix parameter.
            let rows_iter: Vec<(String, String, Option<String>)> = if bind_prefix {
                // bind_prefix is only set when target_prefix is Some.
                let Some(prefix) = q.target_prefix.as_ref() else {
                    return Err(StoreError::Invariant(
                        "list: bind_prefix set but target_prefix is None",
                    ));
                };
                // Escape LIKE metacharacters in the prefix, then append %.
                let escaped = prefix.as_str().replace('%', "\\%").replace('_', "\\_");
                let pattern = format!("{escaped}%");
                stmt.query_map(params![pattern], |row| {
                    let scope: String = row.get("scope")?;
                    let taxonomy: String = row.get("taxonomy")?;
                    let json: Option<String> = row.get("record_json")?;
                    Ok((scope, taxonomy, json))
                })
                .map_err(store_err)?
                .collect::<Result<_, _>>()
                .map_err(store_err)?
            } else {
                stmt.query_map([], |row| {
                    let scope: String = row.get("scope")?;
                    let taxonomy: String = row.get("taxonomy")?;
                    let json: Option<String> = row.get("record_json")?;
                    Ok((scope, taxonomy, json))
                })
                .map_err(store_err)?
                .collect::<Result<_, _>>()
                .map_err(store_err)?
            };

            let mut out = Vec::new();
            let mut hidden = 0usize;

            for (scope_json, taxonomy_json, record_json_opt) in rows_iter {
                if let Some(want_kind) = q.kind_filter.as_deref() {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&taxonomy_json).unwrap_or(serde_json::Value::Null);
                    let row_kind = parsed
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if row_kind != want_kind {
                        // Kind mismatches are not "hidden" rebac drops;
                        // skip silently so callers see only requested kind.
                        continue;
                    }
                }
                if !principal_can_read(&q.principal, &scope_json, &taxonomy_json) {
                    hidden += 1;
                    continue;
                }
                // Cap visible-row count, but keep iterating so the
                // `hidden` count remains accurate over the full
                // candidate set.
                if let Some(limit) = q.max_results
                    && out.len() >= limit
                {
                    continue;
                }
                // Skip rows with missing or unparseable `record_json`
                // (e.g. legacy rows that 0009's best-effort backfill
                // could not reconstruct into a valid `MemoryRecord`).
                // Treat them as not-yet-migrated rather than poisoning
                // the entire result with `StoreError::Invariant`.
                let Some(json) = record_json_opt else {
                    tracing::warn!("list: skipping row with NULL record_json (legacy/pre-0009)");
                    continue;
                };
                let Ok(rec) = serde_json::from_str::<MemoryRecord>(&json) else {
                    tracing::warn!("list: skipping row with unparseable record_json");
                    continue;
                };
                out.push(rec);
            }

            Ok(ListResult { rows: out, hidden })
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
    }

    async fn version_history(
        &self,
        principal: &Principal,
        target_id: &TargetId,
    ) -> Result<Vec<HistoryEntry>, StoreError> {
        let conn = self.conn.clone();
        let principal = principal.clone();
        let target_id = target_id.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // Fetch all versions (active and superseded) for the target.
            let mut stmt = conn
                .prepare(
                    "SELECT record_id, target_id, version, active, tombstoned, \
                     created_at, created_by, tombstoned_at, tombstoned_by, expired_at, \
                     scope, taxonomy \
                     FROM records WHERE target_id = ?1 ORDER BY version ASC",
                )
                .map_err(store_err)?;

            let all_versions: Vec<(RecordVersion, String, String)> = stmt
                .query_map(params![target_id.as_str()], |row| {
                    let rv = row_to_record_version(row)?;
                    let scope: String = row.get("scope")?;
                    let taxonomy: String = row.get("taxonomy")?;
                    Ok((rv, scope, taxonomy))
                })
                .map_err(store_err)?
                .collect::<Result<_, _>>()
                .map_err(store_err)?;

            // Rebac filter: drop versions the principal cannot read.
            let visible: Vec<RecordVersion> = all_versions
                .into_iter()
                .filter(|(_, scope, taxonomy)| principal_can_read(&principal, scope, taxonomy))
                .map(|(rv, _, _)| rv)
                .collect();

            // Purge markers fail closed to system principals only. A
            // single snapshot of scope/taxonomy taken at purge time
            // cannot prove the caller could read every historical
            // version — visibility may have changed across versions.
            // Per-version visibility persistence is a follow-up; until
            // then, expose purge audit only to system/audit callers.
            // The 0010 snapshot columns remain populated for that
            // future work but are not consulted on read.
            let purges: Vec<PurgeMarker> = if principal.is_system() {
                let mut p = conn
                    .prepare(
                        "SELECT target_id, op_id, purged_at, purged_by, body_hash_salt \
                         FROM record_purges WHERE target_id = ?1 ORDER BY purged_at",
                    )
                    .map_err(store_err)?;
                p.query_map(params![target_id.as_str()], row_to_purge_marker)
                    .map_err(store_err)?
                    .collect::<Result<_, _>>()
                    .map_err(store_err)?
            } else {
                Vec::new()
            };

            Ok(into_history(visible, purges))
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
    }
}
