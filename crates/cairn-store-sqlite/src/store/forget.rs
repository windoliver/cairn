//! Store-facing forget helpers.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cairn_core::domain::projection::MarkdownProjector;
use cairn_core::domain::{RecordId, TargetId};
use cairn_core::wal::{OpState, OperationId};
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::record_wal::forget::{ForgetOutcome, apply_forget_record};
use crate::record_wal::ops::finalize;
use crate::store::SqliteMemoryStore;
use crate::store::current_unix_ms;

impl SqliteMemoryStore {
    /// Forget one public record id by deleting the full target lineage.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the record id is not present, WAL setup fails,
    /// lock fencing fails, or a Phase B step exhausts retries.
    pub async fn forget_record(&self, record_id: &RecordId) -> Result<ForgetOutcome, StoreError> {
        apply_forget_record(self, record_id).await
    }

    /// Forget every record target in one session.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the session is missing, ambiguous across
    /// partitions, lock acquisition fails, or the purge transaction fails.
    pub async fn forget_session(
        &self,
        session_id: &str,
    ) -> Result<ForgetSessionOutcome, StoreError> {
        apply_forget_session(self, session_id).await
    }
}

/// Result of a session-wide forget operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetSessionOutcome {
    /// Number of record rows physically removed.
    pub deleted_count: u64,
    /// Record ids covered by the session fan-out.
    pub tombstones: Vec<RecordId>,
    /// WAL operation id for the session forget.
    pub operation_id: OperationId,
    /// Vault-relative markdown projections covered by the fan-out.
    pub projection_paths: Vec<PathBuf>,
}

struct SessionScopePartition {
    tenant: Option<String>,
    workspace: Option<String>,
}

async fn apply_forget_session(
    store: &SqliteMemoryStore,
    session_id: &str,
) -> Result<ForgetSessionOutcome, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_session")?);
    let incarnation = store
        .incarnation()
        .cloned()
        .ok_or_else(|| StoreError::Invariant {
            what: "forget_session requires daemon incarnation".to_owned(),
        })?;
    let op_id = new_forget_session_operation_id()?;

    let namespace_lock = crate::locks::acquire(
        &conn,
        &crate::locks::ResourceKey::session_namespace(session_id),
        crate::locks::LockMode::Exclusive,
        &format!("{}:session_namespace", op_id.as_str()),
        Duration::from_secs(30),
        &incarnation,
        "forget_session",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let result = async {
        let partition = session_scope_partition(store, session_id).await?;
        let session_lock = crate::locks::acquire(
            &conn,
            &crate::locks::ResourceKey::session(
                session_lock_component(partition.tenant.as_deref()),
                session_lock_component(partition.workspace.as_deref()),
                session_id,
            ),
            crate::locks::LockMode::Exclusive,
            &format!("{}:session", op_id.as_str()),
            Duration::from_secs(30),
            &incarnation,
            "forget_session",
        )
        .await
        .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

        let body_result = commit_forget_session(store, session_id, &op_id, partition).await;

        let release_result = session_lock
            .release()
            .await
            .map_err(|e| StoreError::RecordWalLock(Box::new(e)));
        match (body_result, release_result) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Err(error), Ok(())) | (_, Err(error)) => Err(error),
        }
    }
    .await;

    let release_result = namespace_lock
        .release()
        .await
        .map_err(|e| StoreError::RecordWalLock(Box::new(e)));
    match (result, release_result) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) | (_, Err(error)) => Err(error),
    }
}

async fn session_scope_partition(
    store: &SqliteMemoryStore,
    session_id: &str,
) -> Result<SessionScopePartition, StoreError> {
    let partitions = {
        let session = session_id.to_owned();
        store
            .with_tx(move |tx| tx.list_session_scope_partitions(&session))
            .await?
    };
    let [(tenant, workspace)] = partitions.as_slice() else {
        return if partitions.is_empty() {
            Err(StoreError::NotFound {
                id: session_id.to_owned(),
            })
        } else {
            Err(StoreError::Invariant {
                what: format!("forget_session `{session_id}` spans multiple scope partitions"),
            })
        };
    };
    Ok(SessionScopePartition {
        tenant: tenant.clone(),
        workspace: workspace.clone(),
    })
}

async fn commit_forget_session(
    store: &SqliteMemoryStore,
    session_id: &str,
    op_id: &OperationId,
    partition: SessionScopePartition,
) -> Result<ForgetSessionOutcome, StoreError> {
    let session = session_id.to_owned();
    let tenant = partition.tenant;
    let workspace = partition.workspace;
    let op_for_tx = op_id.clone();
    store
        .with_tx(move |tx| {
            let mut target_ids = tx.list_target_ids_for_session_scope(
                &session,
                tenant.as_deref(),
                workspace.as_deref(),
            )?;
            if target_ids.is_empty() {
                return Err(StoreError::NotFound { id: session });
            }
            let source_record_ids = tx.record_ids_for_targets(&target_ids)?;
            target_ids.extend(tx.summary_targets_for_source_records(&source_record_ids)?);
            target_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            target_ids.dedup_by(|left, right| left.as_str() == right.as_str());
            let tombstones = tx.record_ids_for_targets(&target_ids)?;
            let projection_paths = tx.projection_paths_for_targets(&target_ids)?;
            issue_forget_session_prepared(
                &tx.tx,
                &op_for_tx,
                &session,
                tenant.as_deref(),
                workspace.as_deref(),
                &target_ids,
                tombstones.len(),
            )?;
            let fence_resource =
                session_fence_resource(tenant.as_deref(), workspace.as_deref(), &session);
            insert_session_reader_fence(&tx.tx, &op_for_tx, &fence_resource)?;
            let deleted_count = tx.purge_targets_with_indexes(&target_ids)?;
            clear_session_reader_fence(&tx.tx, &op_for_tx, &fence_resource)?;
            finalize(&tx.tx, &op_for_tx, OpState::Committed, "applied")?;
            Ok(ForgetSessionOutcome {
                deleted_count,
                tombstones,
                operation_id: op_for_tx,
                projection_paths,
            })
        })
        .await
}

fn new_forget_session_operation_id() -> Result<OperationId, StoreError> {
    OperationId::parse(format!("forget_session-{}", ulid::Ulid::new())).map_err(|e| {
        StoreError::Invariant {
            what: format!("generated invalid operation id: {e}"),
        }
    })
}

fn session_lock_component(value: Option<&str>) -> &str {
    match value {
        Some("") | None => "default",
        Some(value) => value,
    }
}

fn session_fence_resource(
    tenant: Option<&str>,
    workspace: Option<&str>,
    session_id: &str,
) -> String {
    crate::locks::ResourceKey::session(
        session_lock_component(tenant),
        session_lock_component(workspace),
        session_id,
    )
    .as_resource_str()
}

fn issue_forget_session_prepared(
    conn: &rusqlite::Connection,
    op_id: &OperationId,
    session_id: &str,
    tenant: Option<&str>,
    workspace: Option<&str>,
    target_ids: &[TargetId],
    tombstone_count: usize,
) -> Result<(), StoreError> {
    let now = current_unix_ms();
    let target_hashes = target_ids
        .iter()
        .map(|target| hash_receipt_component(target.as_str()))
        .collect::<Vec<_>>();
    let scope_json = serde_json::json!({
        "mode": "session",
        "session_id": session_id,
        "tenant": tenant,
        "workspace": workspace,
        "audit_receipt": {
            "version": 1,
            "target_count": target_ids.len(),
            "tombstone_count": tombstone_count,
            "deleted_count": tombstone_count,
            "target_hashes": target_hashes,
        },
    })
    .to_string();
    conn.execute(
        "INSERT INTO wal_ops \
           (operation_id, issued_seq, kind, state, envelope, issuer, principal, \
            target_hash, scope_json, plan_ref, expires_at, signature, issued_at, updated_at) \
         VALUES (?1, COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0) + 1, \
            'forget_session', 'ISSUED', '{}', 'cairn-store-sqlite', NULL, ?2, ?3, \
            NULL, 0, 'local', ?4, ?4)",
        params![
            op_id.as_str(),
            format!("session:{session_id}"),
            scope_json,
            now
        ],
    )?;
    conn.execute(
        "UPDATE wal_ops SET state = 'PREPARED', updated_at = ?1 WHERE operation_id = ?2",
        params![now, op_id.as_str()],
    )?;
    Ok(())
}

fn hash_receipt_component(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn insert_session_reader_fence(
    conn: &rusqlite::Connection,
    op_id: &OperationId,
    resource: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO reader_fence(resource, operation_id, state, created_at) \
         VALUES (?1, ?2, 'PENDING', ?3)",
        params![resource, op_id.as_str(), current_unix_ms()],
    )?;
    Ok(())
}

fn clear_session_reader_fence(
    conn: &rusqlite::Connection,
    op_id: &OperationId,
    resource: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE reader_fence \
            SET state = 'CLEARED', cleared_at = ?3 \
          WHERE resource = ?1 AND operation_id = ?2 AND state = 'PENDING'",
        params![resource, op_id.as_str(), current_unix_ms()],
    )?;
    Ok(())
}

impl crate::store::tx::StoreTx<'_> {
    fn record_ids_for_targets(&self, targets: &[TargetId]) -> Result<Vec<RecordId>, StoreError> {
        let mut tombstones = Vec::new();
        for target in targets {
            let mut stmt = self.tx.prepare(
                "SELECT record_id FROM records WHERE target_id = ?1 ORDER BY version, record_id",
            )?;
            let rows = stmt.query_map(params![target.as_str()], |row| row.get::<_, String>(0))?;
            for raw in rows {
                let raw = raw?;
                let record_id =
                    RecordId::parse(raw.clone()).map_err(|e| StoreError::Invariant {
                        what: format!("invalid record_id `{raw}` in session forget: {e}"),
                    })?;
                tombstones.push(record_id);
            }
        }
        Ok(tombstones)
    }

    fn purge_targets_with_indexes(&self, targets: &[TargetId]) -> Result<u64, StoreError> {
        let mut deleted_count = 0_u64;
        for target in targets {
            self.expire_entity_edges_for_target(target)?;
            self.tx.execute(
                "DELETE FROM entity_episodes \
                  WHERE episode_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
                params![target.as_str()],
            )?;
            self.tx.execute(
                "DELETE FROM record_vectors \
                  WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
                params![target.as_str()],
            )?;
            self.tx.execute(
                "DELETE FROM pending_embeddings \
                  WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
                params![target.as_str()],
            )?;
            self.tx.execute(
                "DELETE FROM records_fts \
                  WHERE rowid IN (SELECT rowid FROM records WHERE target_id = ?1)",
                params![target.as_str()],
            )?;
            deleted_count = deleted_count.saturating_add(self.purge_target(target)?);
        }
        Ok(deleted_count)
    }

    fn projection_paths_for_targets(
        &self,
        targets: &[TargetId],
    ) -> Result<Vec<PathBuf>, StoreError> {
        let projector = MarkdownProjector;
        let mut paths = BTreeSet::new();
        for target in targets {
            let mut stmt = self.tx.prepare(
                "SELECT DISTINCT path \
                   FROM record_projection_links \
                  WHERE target_id = ?1 AND projection_kind = 'markdown' \
                  ORDER BY path",
            )?;
            let rows = stmt.query_map(params![target.as_str()], |row| row.get::<_, String>(0))?;
            for raw in rows {
                paths.insert(PathBuf::from(raw?));
            }
            if let Some(stored) = self.get_active_by_target(target)? {
                paths.insert(projector.project(&stored).path);
            }
        }
        Ok(paths.into_iter().collect())
    }

    fn summary_targets_for_source_records(
        &self,
        source_record_ids: &[RecordId],
    ) -> Result<Vec<TargetId>, StoreError> {
        let mut target_ids = BTreeSet::new();
        for record_id in source_record_ids {
            let mut stmt = self.tx.prepare(
                "SELECT DISTINCT target_id
                   FROM (
                        SELECT r.target_id
                          FROM record_summary_links AS links
                          JOIN records AS r
                            ON r.record_id = links.summary_record_id
                         WHERE links.source_record_id = ?1
                           AND r.active = 1
                           AND r.tombstoned = 0
                        UNION
                        SELECT target_id
                          FROM records
                         WHERE active = 1 AND tombstoned = 0
                           AND json_extract(extra_frontmatter, '$.consolidation') IS NOT NULL
                           AND EXISTS (
                               SELECT 1
                                 FROM json_each(json_extract(extra_frontmatter,
                                                  '$.consolidation.source_record_ids'))
                                WHERE value = ?1
                           )
                   )
                  ORDER BY target_id",
            )?;
            let rows =
                stmt.query_map(params![record_id.as_str()], |row| row.get::<_, String>(0))?;
            for raw in rows {
                let raw = raw?;
                target_ids.insert(TargetId::parse(raw.clone()).map_err(|e| {
                    StoreError::Invariant {
                        what: format!("invalid summary target_id `{raw}` in session forget: {e}"),
                    }
                })?);
            }
        }
        Ok(target_ids.into_iter().collect())
    }

    fn expire_entity_edges_for_target(&self, target: &TargetId) -> Result<(), StoreError> {
        let edges = {
            let mut stmt = self.tx.prepare(
                "SELECT id, confidence, confidence_score, valid_at, invalid_at, \
                        created_at, expired_at, tombstone_reason \
                   FROM entity_edges \
                  WHERE source_record_id IN ( \
                        SELECT record_id FROM records WHERE target_id = ?1 \
                  )",
            )?;
            let rows = stmt.query_map(params![target.as_str()], |row| {
                Ok(ForgetSessionEntityEdge {
                    id: row.get(0)?,
                    confidence: row.get(1)?,
                    confidence_score: row.get(2)?,
                    valid_at: row.get(3)?,
                    invalid_at: row.get(4)?,
                    created_at: row.get(5)?,
                    expired_at: row.get(6)?,
                    tombstone_reason: row.get(7)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let now_ms = current_unix_ms();
        for edge in edges {
            let hash = crate::entity_graph::entity_edge_body_hash_components(
                &edge.confidence,
                edge.confidence_score,
                edge.valid_at,
                edge.invalid_at,
                edge.created_at,
                None,
            );
            self.tx.execute(
                "UPDATE entity_edges \
                    SET expired_at = ?1, \
                        tombstone_reason = ?2, \
                        source_record_id = NULL, \
                        body_hash = ?3 \
                  WHERE id = ?4",
                params![
                    edge.expired_at.unwrap_or(now_ms),
                    edge.tombstone_reason.unwrap_or_else(|| "forget".to_owned()),
                    &hash[..],
                    edge.id,
                ],
            )?;
        }
        Ok(())
    }
}

struct ForgetSessionEntityEdge {
    id: String,
    confidence: String,
    confidence_score: f32,
    valid_at: i64,
    invalid_at: Option<i64>,
    created_at: i64,
    expired_at: Option<i64>,
    tombstone_reason: Option<String>,
}
