use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, FlushMode, FlushPlan, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, PlanReason, PlannedMutation, Provenance, RecordId,
    Rfc3339Timestamp, ScopeTuple, TargetId,
};
use cairn_core::generated::common::Ulid;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::extract::ExtractionCounts;

const FOLDER_INGEST_HUMAN: &str = "hmn:folder-ingest";
const FOLDER_INGEST_SENSOR: &str = "snr:local:folder-ingest:v1";
const SOURCE_HASH_PREFIX: &str = "sha256:";

/// A scanned and extracted folder file ready to be planned as a store upsert.
#[allow(
    dead_code,
    reason = "Task 5/6 consume the full planned-file payload when apply and folder routing are wired"
)]
#[derive(Debug, Clone)]
pub struct PlannedFile {
    /// Absolute source path used for diagnostics and later apply/report stages.
    pub absolute_path: PathBuf,
    /// Source path relative to the ingested folder.
    pub relative_path: PathBuf,
    /// Extracted markdown/text body that will become the record body.
    pub body: String,
    /// Lowercase SHA-256 hex hash for the source body.
    pub body_hash: String,
    /// Cache key produced by the folder ingest cache layer.
    pub cache_key: String,
    /// Keyword extraction counts for reporting/frontmatter.
    pub counts: ExtractionCounts,
    /// Extracted entity labels.
    pub entities: Vec<String>,
    /// Extracted wiki-link edges.
    pub wiki_edges: Vec<(String, String)>,
}

/// One folder ingest flush plan plus the files represented in that batch.
#[allow(
    dead_code,
    reason = "Task 5/6 consume batch file metadata when apply and folder routing are wired"
)]
#[derive(Debug, Clone)]
pub struct FolderPlanBatch {
    /// Flush plan containing one upsert mutation per file.
    pub plan: FlushPlan,
    /// Files represented by `plan.mutations`, in deterministic input order.
    pub files: Vec<PlannedFile>,
}

/// Errors produced while planning folder ingest batches.
#[derive(Debug, Error)]
pub enum PlannerError {
    /// `batch_size` must be greater than zero.
    #[error("folder ingest planner batch_size must be greater than zero")]
    InvalidBatchSize,
    /// Body hashes must be lowercase SHA-256 hex strings.
    #[error(
        "invalid body hash `{hash}` for `{path}`: expected 64 lowercase SHA-256 hex characters"
    )]
    InvalidBodyHash {
        /// Source path whose body hash was invalid.
        path: PathBuf,
        /// Offending body hash.
        hash: String,
    },
    /// Plan expiry timestamp overflowed while adding the five-minute TTL.
    #[error("planner expires_at overflow for issued_at `{issued_at}`")]
    TimestampOutOfRange {
        /// The issued timestamp that could not be advanced.
        issued_at: String,
    },
    /// A generated domain value failed current-main validation rules.
    #[error(transparent)]
    Domain(#[from] cairn_core::domain::DomainError),
    /// A file could not be converted into a valid record for a batch.
    #[error(
        "failed to build record for `{path}` in batch {batch_index} ({operation_id}): {source}"
    )]
    RecordBuild {
        /// Source path whose record could not be built.
        path: PathBuf,
        /// Index of the planned batch.
        batch_index: usize,
        /// Operation id for the containing batch.
        operation_id: String,
        /// Underlying planner error.
        #[source]
        source: Box<PlannerError>,
    },
}

/// Build a deterministic folder-ingest operation id from folder, batch index,
/// and the batch's sorted body hashes.
#[allow(
    clippy::unnecessary_wraps,
    reason = "Task 4 API is fallible for planner parity; digest-to-ULID conversion is currently infallible"
)]
pub fn deterministic_operation_id(
    folder: &Path,
    batch_index: usize,
    sorted_hashes: &[String],
) -> Result<Ulid, PlannerError> {
    let mut hasher = Sha256::new();
    hasher.update(slash_normalized_path(folder).as_bytes());
    hasher.update(b"\0");
    hasher.update(batch_index.to_be_bytes());
    for hash in sorted_hashes {
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
    }

    Ok(ulid_from_digest(hasher.finalize()))
}

/// Build one flush plan per `batch_size` files.
pub fn plan_batches(
    folder: &Path,
    files: Vec<PlannedFile>,
    batch_size: usize,
    mode: FlushMode,
) -> Result<Vec<FolderPlanBatch>, PlannerError> {
    if batch_size == 0 {
        return Err(PlannerError::InvalidBatchSize);
    }

    let mut batches = Vec::new();
    let mut file_iter = files.into_iter();
    for batch_index in 0.. {
        let chunk = file_iter.by_ref().take(batch_size).collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        let mut sorted_hashes = chunk
            .iter()
            .map(|file| file.body_hash.clone())
            .collect::<Vec<_>>();
        sorted_hashes.sort();

        let operation_id = deterministic_operation_id(folder, batch_index, &sorted_hashes)?;
        let operation_id_for_errors = operation_id.0.clone();
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339(&issued_at)?;
        let mutations = chunk
            .iter()
            .map(|file| {
                build_record_for_file(file, &issued_at)
                    .map(|record| PlannedMutation::Upsert {
                        record: Box::new(record),
                        prior_version: None,
                    })
                    .map_err(|source| PlannerError::RecordBuild {
                        path: file.relative_path.clone(),
                        batch_index,
                        operation_id: operation_id_for_errors.clone(),
                        source: Box::new(source),
                    })
            })
            .collect::<Result<Vec<_>, PlannerError>>()?;

        let plan = FlushPlan {
            operation_id,
            issued_at,
            issuer: folder_ingest_identity()?,
            principal: None,
            scope: folder_ingest_scope(),
            mode,
            mutations,
            reason: PlanReason::UserIngest,
            source_events: Vec::new(),
            target_hashes: BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        };

        batches.push(FolderPlanBatch { plan, files: chunk });
    }

    Ok(batches)
}

fn build_record_for_file(
    file: &PlannedFile,
    issued_at: &str,
) -> Result<MemoryRecord, PlannerError> {
    validate_body_hash(&file.relative_path, &file.body_hash)?;
    let issued_at = Rfc3339Timestamp::parse(issued_at.to_owned())?;
    let author = folder_ingest_identity()?;
    let mut extra_frontmatter = BTreeMap::new();
    extra_frontmatter.insert(
        "folder_relative_path".to_owned(),
        serde_json::Value::String(slash_normalized_path(&file.relative_path)),
    );
    extra_frontmatter.insert(
        "folder_cache_key".to_owned(),
        serde_json::Value::String(file.cache_key.clone()),
    );
    extra_frontmatter.insert(
        "folder_entities_new".to_owned(),
        serde_json::json!(file.counts.entities_new),
    );
    extra_frontmatter.insert(
        "folder_edges_new".to_owned(),
        serde_json::json!(file.counts.edges_new),
    );

    let record = MemoryRecord {
        id: RecordId::parse(deterministic_record_ulid(&file.cache_key, "record").0)?,
        target_id: TargetId::parse(deterministic_record_ulid(&file.cache_key, "target").0)?,
        kind: MemoryKind::Reference,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: folder_ingest_scope(),
        body: file.body.clone(),
        provenance: Provenance {
            source_sensor: Identity::parse(FOLDER_INGEST_SENSOR)?,
            created_at: issued_at.clone(),
            originating_agent_id: author.clone(),
            source_hash: format!("{SOURCE_HASH_PREFIX}{}", file.body_hash),
            consent_ref: "consent:folder-ingest".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: issued_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: author,
            at: issued_at,
        }],
        signature: cairn_core::domain::record::Ed25519Signature::parse(format!(
            "ed25519:{}",
            "a".repeat(128)
        ))?,
        tags: Vec::new(),
        extra_frontmatter,
        consent_model: None,
    };

    record.validate()?;
    Ok(record)
}

fn deterministic_record_ulid(cache_key: &str, suffix: &str) -> Ulid {
    let mut hasher = Sha256::new();
    hasher.update(cache_key.as_bytes());
    hasher.update(b":");
    hasher.update(suffix.as_bytes());
    ulid_from_digest(hasher.finalize())
}

fn ulid_from_digest(digest: impl AsRef<[u8]>) -> Ulid {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    Ulid(ulid::Ulid::from_bytes(bytes).to_string())
}

fn validate_body_hash(path: &Path, hash: &str) -> Result<(), PlannerError> {
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(());
    }

    Err(PlannerError::InvalidBodyHash {
        path: path.to_path_buf(),
        hash: hash.to_owned(),
    })
}

fn folder_ingest_identity() -> Result<Identity, PlannerError> {
    Ok(Identity::parse(FOLDER_INGEST_HUMAN)?)
}

fn folder_ingest_scope() -> ScopeTuple {
    ScopeTuple {
        user: Some(FOLDER_INGEST_HUMAN.to_owned()),
        ..ScopeTuple::default()
    }
}

fn slash_normalized_path(path: &Path) -> String {
    path.as_os_str().to_string_lossy().replace('\\', "/")
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn expires_at_rfc3339(issued_at: &str) -> Result<String, PlannerError> {
    let raw_issued_at = issued_at.to_owned();
    let issued_at = chrono::DateTime::parse_from_rfc3339(issued_at)
        .map_err(|_| cairn_core::domain::DomainError::InvalidTimestamp {
            message: format!("planner issued_at `{issued_at}` did not parse as RFC3339"),
        })?
        .with_timezone(&chrono::Utc);
    let expires_at = issued_at
        .checked_add_signed(chrono::Duration::minutes(5))
        .ok_or(PlannerError::TimestampOutOfRange {
            issued_at: raw_issued_at,
        })?;
    Ok(expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::ingest::extract::ExtractionCounts;
    use std::path::PathBuf;

    fn item(path: &str, hash: &str) -> PlannedFile {
        PlannedFile {
            absolute_path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            body: format!("body for {path}"),
            body_hash: hash.to_owned(),
            cache_key: hash.to_owned(),
            counts: ExtractionCounts {
                entities_new: 1,
                edges_new: 0,
            },
            entities: vec!["Alpha".to_owned()],
            wiki_edges: vec![],
        }
    }

    #[test]
    fn batch_size_two_over_five_files_produces_three_plans() {
        let files = vec![
            item("a.md", &"a".repeat(64)),
            item("b.md", &"b".repeat(64)),
            item("c.md", &"c".repeat(64)),
            item("d.md", &"d".repeat(64)),
            item("e.md", &"e".repeat(64)),
        ];

        let plans = plan_batches(
            Path::new("/tmp/project"),
            files,
            2,
            cairn_core::domain::flush_plan::FlushMode::DryRun,
        )
        .expect("plans");

        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].plan.mutations.len(), 2);
        assert_eq!(plans[1].plan.mutations.len(), 2);
        assert_eq!(plans[2].plan.mutations.len(), 1);
    }

    #[test]
    fn deterministic_operation_id_is_stable_for_same_batch() {
        let hashes = vec!["a".repeat(64), "b".repeat(64)];
        let first = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        let second = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn deterministic_operation_id_changes_with_path_body_or_batch() {
        let hashes = vec!["a".repeat(64), "b".repeat(64)];
        let base = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        let changed_path = deterministic_operation_id(Path::new("/tmp/other"), 0, &hashes).unwrap();
        let changed_batch =
            deterministic_operation_id(Path::new("/tmp/project"), 1, &hashes).unwrap();
        let changed_hash = deterministic_operation_id(
            Path::new("/tmp/project"),
            0,
            &["a".repeat(64), "c".repeat(64)],
        )
        .unwrap();

        assert_ne!(base, changed_path);
        assert_ne!(base, changed_batch);
        assert_ne!(base, changed_hash);
    }
}
