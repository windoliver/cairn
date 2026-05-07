//! Store-backed folder ingest plan application.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cairn_core::contract::memory_store::{MemoryStore, StoreError};
use cairn_core::domain::PlannedMutation;
use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode};
use cairn_core::pipeline::entity_resolve::normalize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::planner::{FolderPlanBatch, PlannedFile};

const WIKI_LINK_RELATION: &str = "wiki_link";

/// Counts returned by applying a folder ingest batch.
#[allow(
    clippy::struct_field_names,
    reason = "Task 5 API exposes explicit *_written counters"
)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyStats {
    /// Canonical records whose content changed in the store.
    pub records_written: u64,
    /// Entity nodes submitted to a graph-capable store.
    pub entities_written: u64,
    /// Entity edges whose store upsert was not an idempotent no-op.
    pub edges_written: u64,
}

/// Errors produced while applying a folder ingest plan batch.
#[derive(Debug, Error)]
pub enum ApplyError {
    /// A planned mutation did not have the corresponding planned file payload.
    #[error(
        "failed to apply batch {operation_id}: plan has {mutations} mutations but {files} files"
    )]
    PlanFileMismatch {
        /// Operation id for the containing batch.
        operation_id: String,
        /// Number of mutations in the flush plan.
        mutations: usize,
        /// Number of planned files attached to the batch.
        files: usize,
    },
    /// Folder apply currently accepts only file upserts.
    #[error(
        "failed to apply `{path}` in batch {operation_id}: mutation {mutation_index} is not a file upsert"
    )]
    UnsupportedMutation {
        /// Source file attached to the unsupported mutation.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Index of the unsupported mutation.
        mutation_index: usize,
    },
    /// A store record upsert failed.
    #[error("failed to upsert record for `{path}` in batch {operation_id}: {source}")]
    RecordUpsert {
        /// Source file being applied.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Underlying store error.
        #[source]
        source: StoreError,
    },
    /// The plan timestamp could not be converted into graph event time.
    #[error("invalid issued_at `{issued_at}` for batch {operation_id}: {source}")]
    InvalidIssuedAt {
        /// Operation id for the containing batch.
        operation_id: String,
        /// Offending timestamp string.
        issued_at: String,
        /// Underlying parse error.
        #[source]
        source: chrono::ParseError,
    },
    /// A graph entity name normalized to the empty key.
    #[error(
        "failed to upsert graph entity `{entity}` for `{path}` in batch {operation_id}: normalized name is empty"
    )]
    EmptyEntityName {
        /// Source file being applied.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Original extracted entity name.
        entity: String,
    },
    /// A graph entity upsert failed.
    #[error(
        "failed to upsert graph entity `{entity}` for `{path}` in batch {operation_id}: {source}"
    )]
    EntityUpsert {
        /// Source file being applied.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Entity label being written.
        entity: String,
        /// Underlying store error.
        #[source]
        source: StoreError,
    },
    /// A graph entity edge upsert failed.
    #[error(
        "failed to upsert graph edge `{source_entity}` to `{target_entity}` for `{path}` in batch {operation_id}: {source}"
    )]
    EntityEdgeUpsert {
        /// Source file being applied.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Edge source entity label.
        source_entity: String,
        /// Edge target entity label.
        target_entity: String,
        /// Underlying store error.
        #[source]
        source: StoreError,
    },
    /// An edge endpoint was not available after entity upserts.
    #[error(
        "failed to upsert graph edge `{source_entity}` to `{target_entity}` for `{path}` in batch {operation_id}: missing entity endpoint `{missing_entity}`"
    )]
    MissingEdgeEndpoint {
        /// Source file being applied.
        path: PathBuf,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Edge source entity label.
        source_entity: String,
        /// Edge target entity label.
        target_entity: String,
        /// Entity label missing from the local endpoint map.
        missing_entity: String,
    },
}

/// Apply one planned folder-ingest batch to a [`MemoryStore`].
///
/// Record upserts always run. Knowledge-graph writes run only when the store
/// advertises `graph_edges` support.
pub async fn apply_batch(
    store: &dyn MemoryStore,
    batch: &FolderPlanBatch,
) -> Result<ApplyStats, ApplyError> {
    let operation_id = operation_id(batch);
    if batch.plan.mutations.len() != batch.files.len() {
        return Err(ApplyError::PlanFileMismatch {
            operation_id,
            mutations: batch.plan.mutations.len(),
            files: batch.files.len(),
        });
    }

    let mut stats = ApplyStats::default();
    let mut record_ids = Vec::with_capacity(batch.files.len());
    for (mutation_index, (mutation, file)) in batch
        .plan
        .mutations
        .iter()
        .zip(batch.files.iter())
        .enumerate()
    {
        let PlannedMutation::Upsert { record, .. } = mutation else {
            return Err(ApplyError::UnsupportedMutation {
                path: file.absolute_path.clone(),
                operation_id: operation_id.clone(),
                mutation_index,
            });
        };

        let outcome = store
            .upsert(record)
            .await
            .map_err(|source| ApplyError::RecordUpsert {
                path: file.absolute_path.clone(),
                operation_id: operation_id.clone(),
                source,
            })?;
        if outcome.content_changed {
            stats.records_written += 1;
        }
        record_ids.push(outcome.record_id);
    }

    if store.capabilities().graph_edges {
        let event_time_ms = plan_issued_at_ms(batch)?;
        for (file, record_id) in batch.files.iter().zip(record_ids.iter()) {
            let graph_stats =
                apply_graph_for_file(store, file, record_id, event_time_ms, batch).await?;
            stats.entities_written += graph_stats.entities_written;
            stats.edges_written += graph_stats.edges_written;
        }
    }

    Ok(stats)
}

async fn apply_graph_for_file(
    store: &dyn MemoryStore,
    file: &PlannedFile,
    record_id: &cairn_core::domain::RecordId,
    event_time_ms: i64,
    batch: &FolderPlanBatch,
) -> Result<ApplyStats, ApplyError> {
    let mut stats = ApplyStats::default();
    let mut entity_names = BTreeMap::<String, String>::new();
    for entity in &file.entities {
        let name_norm = normalized_entity_name(file, batch, entity)?;
        entity_names
            .entry(name_norm)
            .or_insert_with(|| entity.clone());
    }
    for (source, target) in &file.wiki_edges {
        let source_norm = normalized_entity_name(file, batch, source)?;
        entity_names
            .entry(source_norm)
            .or_insert_with(|| source.clone());
        let target_norm = normalized_entity_name(file, batch, target)?;
        entity_names
            .entry(target_norm)
            .or_insert_with(|| target.clone());
    }

    let mut entity_ids = BTreeMap::<String, EntityId>::new();
    for (name_norm, name) in entity_names {
        let node = EntityNode {
            id: EntityId::from(deterministic_graph_ulid(
                &file.cache_key,
                &["entity", name.as_str()],
            )),
            name: name.clone(),
            name_norm: name_norm.clone(),
            summary: None,
            created_at: event_time_ms,
            embedding_id: None,
        };
        let id = store
            .upsert_entity(&node)
            .await
            .map_err(|source| ApplyError::EntityUpsert {
                path: file.absolute_path.clone(),
                operation_id: operation_id(batch),
                entity: name,
                source,
            })?;
        entity_ids.insert(name_norm, id);
        stats.entities_written += 1;
    }

    for (source_entity, target_entity) in &file.wiki_edges {
        let source_norm = normalized_entity_name(file, batch, source_entity)?;
        let target_norm = normalized_entity_name(file, batch, target_entity)?;
        let Some(source_id) = entity_ids.get(&source_norm).cloned() else {
            return Err(ApplyError::MissingEdgeEndpoint {
                path: file.absolute_path.clone(),
                operation_id: operation_id(batch),
                source_entity: source_entity.clone(),
                target_entity: target_entity.clone(),
                missing_entity: source_entity.clone(),
            });
        };
        let Some(target_id) = entity_ids.get(&target_norm).cloned() else {
            return Err(ApplyError::MissingEdgeEndpoint {
                path: file.absolute_path.clone(),
                operation_id: operation_id(batch),
                source_entity: source_entity.clone(),
                target_entity: target_entity.clone(),
                missing_entity: target_entity.clone(),
            });
        };
        let edge = EntityEdge {
            id: EntityEdgeId::from(deterministic_graph_ulid(
                &file.cache_key,
                &[
                    "edge",
                    source_entity.as_str(),
                    target_entity.as_str(),
                    WIKI_LINK_RELATION,
                ],
            )),
            source_id,
            target_id,
            relation: WIKI_LINK_RELATION.to_owned(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: event_time_ms,
            invalid_at: None,
            created_at: event_time_ms,
            source_record_id: Some(record_id.clone()),
        };
        let outcome = store.upsert_entity_edge(&edge).await.map_err(|source| {
            ApplyError::EntityEdgeUpsert {
                path: file.absolute_path.clone(),
                operation_id: operation_id(batch),
                source_entity: source_entity.clone(),
                target_entity: target_entity.clone(),
                source,
            }
        })?;
        if !outcome.body_was_unchanged {
            stats.edges_written += 1;
        }
    }

    Ok(stats)
}

fn normalized_entity_name(
    file: &PlannedFile,
    batch: &FolderPlanBatch,
    entity: &str,
) -> Result<String, ApplyError> {
    let name_norm = normalize(entity);
    if name_norm.is_empty() {
        return Err(ApplyError::EmptyEntityName {
            path: file.absolute_path.clone(),
            operation_id: operation_id(batch),
            entity: entity.to_owned(),
        });
    }
    Ok(name_norm)
}

fn plan_issued_at_ms(batch: &FolderPlanBatch) -> Result<i64, ApplyError> {
    chrono::DateTime::parse_from_rfc3339(&batch.plan.issued_at)
        .map(|timestamp| timestamp.timestamp_millis())
        .map_err(|source| ApplyError::InvalidIssuedAt {
            operation_id: operation_id(batch),
            issued_at: batch.plan.issued_at.clone(),
            source,
        })
}

fn deterministic_graph_ulid(cache_key: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cache_key.as_bytes());
    for part in parts {
        hasher.update(b"\0");
        hasher.update(part.as_bytes());
    }

    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ulid::Ulid::from_bytes(bytes).to_string()
}

fn operation_id(batch: &FolderPlanBatch) -> String {
    batch.plan.operation_id.0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::ingest::extract::ExtractionCounts;
    use crate::verbs::ingest::planner::{PlannedFile, plan_batches};
    use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
    use cairn_core::domain::flush_plan::FlushMode;
    use cairn_test_fixtures::store::FixtureStore;
    use std::path::{Path, PathBuf};

    fn file(path: &str, hash_char: char) -> PlannedFile {
        PlannedFile {
            absolute_path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            body: format!("# {path}\n[[Entity]]\n"),
            body_hash: hash_char.to_string().repeat(64),
            cache_key: hash_char.to_string().repeat(64),
            counts: ExtractionCounts {
                entities_new: 2,
                edges_new: 1,
            },
            entities: vec!["Entity".to_owned()],
            wiki_edges: vec![("file".to_owned(), "Entity".to_owned())],
        }
    }

    #[tokio::test]
    async fn apply_plan_upserts_records_and_reports_written() {
        let store = FixtureStore::new();
        let batches = plan_batches(
            Path::new("/tmp/project"),
            vec![file("a.md", 'a'), file("b.md", 'b')],
            64,
            FlushMode::Autonomous,
        )
        .unwrap();

        let stats = apply_batch(&store, &batches[0]).await.unwrap();

        assert_eq!(stats.records_written, 2);
        let page = store.list(&ListArgs::default()).await.unwrap();
        assert_eq!(page.records.len(), 2);
    }

    #[tokio::test]
    async fn apply_plan_is_idempotent_for_same_records() {
        let store = FixtureStore::new();
        let batches = plan_batches(
            Path::new("/tmp/project"),
            vec![file("a.md", 'a')],
            64,
            FlushMode::Autonomous,
        )
        .unwrap();

        let first = apply_batch(&store, &batches[0]).await.unwrap();
        let second = apply_batch(&store, &batches[0]).await.unwrap();

        assert_eq!(first.records_written, 1);
        assert_eq!(second.records_written, 0);
    }

    #[tokio::test]
    async fn unsupported_mutation_error_includes_file_context() {
        let store = FixtureStore::new();
        let mut batches = plan_batches(
            Path::new("/tmp/project"),
            vec![file("a.md", 'a')],
            64,
            FlushMode::Autonomous,
        )
        .unwrap();
        let target = match &batches[0].plan.mutations[0] {
            PlannedMutation::Upsert { record, .. } => record.target_id.clone(),
            _ => panic!("fixture starts with upsert"),
        };
        batches[0].plan.mutations[0] = PlannedMutation::Delete {
            target,
            prior_version: 1,
        };

        let err = apply_batch(&store, &batches[0]).await.unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("a.md"),
            "error lacked file path: {message}"
        );
        assert!(
            message.contains(&batches[0].plan.operation_id.0),
            "error lacked operation id: {message}"
        );
    }
}
