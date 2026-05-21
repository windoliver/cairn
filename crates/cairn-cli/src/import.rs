//! Migration bridge entrypoints for legacy memory systems.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cairn_core::domain::flush_plan::PersistedPlan;
use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, FlushMode, FlushPlan, Identity, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, PlanReason, PlannedMutation, Provenance, RecordId,
    Rfc3339Timestamp, ScopeTuple, SourceId, SourceRef, TargetId,
};
use cairn_core::generated::common::Ulid;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use walkdir::WalkDir;

const KOI_IMPORT_AUTHOR: &str = "hmn:koi-import:v1";
const KOI_IMPORT_SENSOR: &str = "snr:koi-v1:import:local:v1";
const SOURCE_HASH_PREFIX: &str = "sha256:";

/// Build the `cairn import` CLI subcommand.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("import")
        .about("Create reviewed Cairn ingest plans from a legacy memory archive")
        .arg(
            clap::Arg::new("from")
                .long("from")
                .required(true)
                .value_name("SYSTEM")
                .value_parser(["koi-v1"])
                .help("Legacy memory system to import"),
        )
        .arg(
            clap::Arg::new("archive")
                .required(true)
                .value_name("PATH")
                .value_parser(clap::builder::PathBufValueParser::new())
                .help("Legacy archive root to scan"),
        )
        .arg(
            clap::Arg::new("batch_size")
                .long("batch-size")
                .value_name("U32")
                .value_parser(clap::value_parser!(u32))
                .default_value("64")
                .help("Maximum records per pending review plan"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON output"),
        )
}

/// Run `cairn import`.
#[must_use]
pub fn run(sub: &clap::ArgMatches, vault_root: &Path) -> std::process::ExitCode {
    let json = sub.get_flag("json");
    let Some(system) = sub.get_one::<String>("from") else {
        return usage_error(json, "from", "--from is required");
    };
    if system != "koi-v1" {
        return usage_error(json, "from", "only --from koi-v1 is supported");
    }
    let Some(archive) = sub.get_one::<PathBuf>("archive") else {
        return usage_error(json, "archive", "archive path is required");
    };
    let batch_size = sub
        .get_one::<u32>("batch_size")
        .copied()
        .unwrap_or(64)
        .try_into()
        .unwrap_or(64_usize);
    match map_koi_v1_archive(&KoiImportOptions {
        source: archive.clone(),
        batch_size,
        mode: FlushMode::HumanReview,
    })
    .and_then(|report| {
        let migration_report = MigrationReportSummary::from_report(&report);
        write_review_plans(vault_root, &report).map(|paths| ImportCliSummary {
            system: system.clone(),
            manifest: report.manifest,
            records: report.records.len(),
            ambiguities: report.ambiguities.len(),
            migration_report,
            plans: report.plans.len(),
            pending_dirs: paths,
        })
    }) {
        Ok(summary) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".to_owned())
                );
            } else {
                println!(
                    "cairn import: mapped {} records into {} pending review plan(s)",
                    summary.records, summary.plans
                );
                if summary.ambiguities > 0 {
                    println!(
                        "cairn import: {} ambiguous field(s) require review",
                        summary.ambiguities
                    );
                }
                if summary.migration_report.unsupported_fields > 0 {
                    println!(
                        "cairn import: {} unsupported field(s) require review",
                        summary.migration_report.unsupported_fields
                    );
                }
                if summary.migration_report.privacy_sensitive_fields > 0 {
                    println!(
                        "cairn import: {} privacy-sensitive field(s) require review",
                        summary.migration_report.privacy_sensitive_fields
                    );
                }
            }
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            if json {
                let response = serde_json::json!({
                    "status": "error",
                    "error": {
                        "code": "ImportError",
                        "message": err.to_string(),
                    }
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_owned())
                );
            } else {
                eprintln!("cairn import: {err}");
            }
            std::process::ExitCode::from(1)
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct ImportCliSummary {
    system: String,
    manifest: ExternalImportManifest,
    records: usize,
    ambiguities: usize,
    migration_report: MigrationReportSummary,
    plans: usize,
    pending_dirs: Vec<PathBuf>,
}

#[derive(Debug, serde::Serialize)]
struct MigrationReportSummary {
    ambiguous_fields: usize,
    unsupported_fields: usize,
    privacy_sensitive_fields: usize,
    findings: Vec<ExternalImportFinding>,
}

impl MigrationReportSummary {
    fn from_report(report: &KoiImportReport) -> Self {
        Self {
            ambiguous_fields: report
                .findings
                .iter()
                .filter(|finding| finding.kind == ExternalImportFindingKind::Ambiguous)
                .count(),
            unsupported_fields: report.manifest.unsupported_fields.len(),
            privacy_sensitive_fields: report.manifest.privacy_sensitive_fields.len(),
            findings: report.findings.clone(),
        }
    }
}

fn usage_error(json: bool, field: &str, message: &str) -> std::process::ExitCode {
    if json {
        let response = serde_json::json!({
            "status": "error",
            "error": {
                "code": "InvalidArgs",
                "field": field,
                "message": message,
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        eprintln!("cairn import: {field}: {message}");
    }
    std::process::ExitCode::from(64)
}

/// Options for mapping a Koi v1 archive into Cairn review plans.
#[derive(Debug, Clone)]
pub struct KoiImportOptions {
    /// Path to the Koi v1 archive root.
    pub source: PathBuf,
    /// Maximum records per generated review plan.
    pub batch_size: usize,
    /// Plan dispatch mode. Issue #120 uses [`FlushMode::HumanReview`].
    pub mode: FlushMode,
}

/// One ambiguous legacy field that fell back to a conservative Cairn value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImportAmbiguity {
    /// Source file where the ambiguity was observed.
    pub path: PathBuf,
    /// Legacy field name or inferred Cairn field.
    pub field: &'static str,
    /// Chosen fallback wire value.
    pub fallback: String,
    /// Human-readable reason for review.
    pub reason: String,
}

/// Neutral category for one imported legacy artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalImportItemKind {
    /// A Cairn memory record import candidate.
    Record,
    /// A legacy session descriptor discovered during import.
    Session,
    /// A legacy skill descriptor discovered during import.
    Skill,
}

/// Neutral review finding category emitted by a migration bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalImportFindingKind {
    /// A legacy field mapped through a conservative fallback.
    Ambiguous,
    /// A legacy field has no Cairn target yet.
    Unsupported,
    /// A legacy field name or payload looks privacy-sensitive.
    PrivacySensitive,
}

/// Provenance and source-hash metadata for one import manifest item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalImportProvenance {
    /// Import sensor identity used for the resulting Cairn record.
    pub source_sensor: String,
    /// Hash of the source body that will back the Cairn record.
    pub source_hash: String,
    /// Source references attached to the resulting Cairn provenance.
    pub source_refs: Vec<SourceRef>,
}

/// One importable item in the neutral external-memory manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalImportItem {
    /// Kind of legacy artifact represented by this item.
    pub kind: ExternalImportItemKind,
    /// Path to the source artifact relative to the archive root.
    pub source_path: PathBuf,
    /// Legacy system identifier, when present.
    pub legacy_id: Option<String>,
    /// Session ids referenced by the item.
    pub session_ids: Vec<String>,
    /// Skill ids referenced by the item.
    pub skill_ids: Vec<String>,
    /// Provenance and source-hash metadata for review.
    pub provenance: ExternalImportProvenance,
}

/// Neutral manifest shared by external-memory migration bridges.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalImportManifest {
    /// Legacy memory system identifier, such as `koi-v1`.
    pub system: String,
    /// Importable records, sessions, and skills discovered in the archive.
    pub items: Vec<ExternalImportItem>,
    /// Unsupported fields found while building the manifest.
    pub unsupported_fields: Vec<ExternalImportFinding>,
    /// Privacy-sensitive fields found while building the manifest.
    pub privacy_sensitive_fields: Vec<ExternalImportFinding>,
}

/// One migration report finding requiring human review.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalImportFinding {
    /// Source file where the finding was observed.
    pub path: PathBuf,
    /// Finding category.
    pub kind: ExternalImportFindingKind,
    /// Legacy field name or inferred Cairn field.
    pub field: String,
    /// Chosen fallback wire value, when a fallback exists.
    pub fallback: Option<String>,
    /// Human-readable reason for review.
    pub reason: String,
}

/// Koi import mapping output.
#[derive(Debug, Clone)]
pub struct KoiImportReport {
    /// Neutral manifest for records, sessions, skills, provenance, and hashes.
    pub manifest: ExternalImportManifest,
    /// Valid Cairn records derived from the archive.
    pub records: Vec<MemoryRecord>,
    /// Review notes for fields that could not be mapped confidently.
    pub ambiguities: Vec<ImportAmbiguity>,
    /// Neutral migration findings for human review.
    pub findings: Vec<ExternalImportFinding>,
    /// Review plans containing the mapped records as upsert mutations.
    pub plans: Vec<FlushPlan>,
}

/// Errors returned by the Koi v1 migration bridge.
#[derive(Debug, Error)]
pub enum ImportError {
    /// Source archive could not be read.
    #[error("koi import source `{}` is not a directory", .archive.display())]
    SourceNotDirectory {
        /// Requested archive root.
        archive: PathBuf,
    },
    /// Batch size must be nonzero.
    #[error("koi import batch size must be greater than zero")]
    InvalidBatchSize,
    /// File I/O failed.
    #[error("koi import I/O for `{path}`: {source}")]
    Io {
        /// File path being read or written.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A JSON source file was malformed.
    #[error("koi import JSON parse for `{path}`: {source}")]
    Json {
        /// Source file being parsed.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: serde_json::Error,
    },
    /// Existing source artifact bytes do not match the imported record.
    #[error(
        "koi import source artifact conflict for `{}`: existing hash `{existing}` does not match expected `{expected}`",
        .path.display()
    )]
    SourceArtifactConflict {
        /// Existing artifact path.
        path: PathBuf,
        /// Hash expected by the imported record.
        expected: String,
        /// Hash of the existing bytes.
        existing: String,
    },
    /// A mapped record failed domain validation.
    #[error("koi import record build for `{path}`: {source}")]
    Domain {
        /// Source path being mapped.
        path: PathBuf,
        /// Underlying domain error.
        #[source]
        source: cairn_core::domain::DomainError,
    },
    /// A review plan could not be serialized.
    #[error("koi import plan serialize for `{path}`: {source}")]
    Serialize {
        /// Plan path being written.
        path: PathBuf,
        /// Underlying serialization error.
        #[source]
        source: serde_json::Error,
    },
}

/// Map a Koi v1 archive into valid Cairn records and pending review plans.
///
/// This bridge performs no database writes. The resulting plans can be reviewed
/// and later applied through the ordinary flush path.
pub fn map_koi_v1_archive(opts: &KoiImportOptions) -> Result<KoiImportReport, ImportError> {
    if opts.batch_size == 0 {
        return Err(ImportError::InvalidBatchSize);
    }
    if !opts.source.is_dir() {
        return Err(ImportError::SourceNotDirectory {
            archive: opts.source.clone(),
        });
    }

    let mut records = Vec::new();
    let mut ambiguities = Vec::new();
    let mut findings = Vec::new();
    let mut items = Vec::new();
    for entry in WalkDir::new(&opts.source)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if !is_importable(path) {
            continue;
        }
        let raw = fs::read_to_string(path).map_err(|source| ImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mapped = map_file(path, &opts.source, &raw)?;
        ambiguities.extend(mapped.ambiguities);
        findings.extend(mapped.findings);
        items.extend(mapped.items);
        records.push(mapped.record);
    }

    let plans = plan_records(&opts.source, &records, opts.batch_size, opts.mode)?;
    let unsupported_fields = findings
        .iter()
        .filter(|finding| finding.kind == ExternalImportFindingKind::Unsupported)
        .cloned()
        .collect();
    let privacy_sensitive_fields = findings
        .iter()
        .filter(|finding| finding.kind == ExternalImportFindingKind::PrivacySensitive)
        .cloned()
        .collect();
    Ok(KoiImportReport {
        manifest: ExternalImportManifest {
            system: "koi-v1".to_owned(),
            items,
            unsupported_fields,
            privacy_sensitive_fields,
        },
        records,
        ambiguities,
        findings,
        plans,
    })
}

/// Persist mapped Koi records as source artifacts plus pending flush review plans.
pub fn write_review_plans(
    vault_root: &Path,
    report: &KoiImportReport,
) -> Result<Vec<PathBuf>, ImportError> {
    let pending = bucket_dir(vault_root, Bucket::Pending);
    fs::create_dir_all(&pending).map_err(|source| ImportError::Io {
        path: pending.clone(),
        source,
    })?;
    for record in &report.records {
        for source_id in &record.provenance.source_ids {
            let path = vault_root.join(source_id.as_str());
            if path.exists() {
                let bytes = fs::read(&path).map_err(|source| ImportError::Io {
                    path: path.clone(),
                    source,
                })?;
                let existing = sha256_wire(&bytes);
                if existing != record.provenance.source_hash {
                    return Err(ImportError::SourceArtifactConflict {
                        path,
                        expected: record.provenance.source_hash.clone(),
                        existing,
                    });
                }
            } else {
                fs::write(&path, record.body.as_bytes()).map_err(|source| ImportError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
        }
    }
    for plan in &report.plans {
        let path = plan_path(vault_root, Bucket::Pending, &plan.operation_id);
        let bytes =
            serde_json::to_vec_pretty(&PersistedPlan::pending(plan.clone())).map_err(|source| {
                ImportError::Serialize {
                    path: path.clone(),
                    source,
                }
            })?;
        fs::write(&path, bytes).map_err(|source| ImportError::Io { path, source })?;
    }
    Ok(vec![pending])
}

struct MappedFile {
    record: MemoryRecord,
    ambiguities: Vec<ImportAmbiguity>,
    findings: Vec<ExternalImportFinding>,
    items: Vec<ExternalImportItem>,
}

fn is_importable(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("json" | "md" | "txt")
    )
}

#[allow(clippy::too_many_lines)]
fn map_file(path: &Path, root: &Path, raw: &str) -> Result<MappedFile, ImportError> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let parsed_json = if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "json")
    {
        Some(
            serde_json::from_str::<Value>(raw).map_err(|source| ImportError::Json {
                path: relative.to_path_buf(),
                source,
            })?,
        )
    } else {
        None
    };
    let mut ambiguities = Vec::new();
    let body = parsed_json
        .as_ref()
        .and_then(extract_body)
        .unwrap_or_else(|| raw.trim().to_owned());
    let body_hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    let kind = parsed_json
        .as_ref()
        .and_then(|json| json.get("kind").and_then(Value::as_str))
        .and_then(|kind| MemoryKind::parse(kind).ok())
        .unwrap_or_else(|| {
            ambiguities.push(ImportAmbiguity {
                path: relative.to_path_buf(),
                field: "kind",
                fallback: MemoryKind::Reference.as_str().to_owned(),
                reason: "legacy Koi item did not declare a Cairn memory kind".to_owned(),
            });
            MemoryKind::Reference
        });
    let class = class_for_kind(kind);
    let issued_at = Rfc3339Timestamp::parse(
        parsed_json
            .as_ref()
            .and_then(|json| json.get("created_at").and_then(Value::as_str))
            .unwrap_or("2026-01-01T00:00:00Z")
            .to_owned(),
    )
    .map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let author = Identity::parse(KOI_IMPORT_AUTHOR).map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let scope = scope_from_json(parsed_json.as_ref());
    let mut extra_frontmatter = BTreeMap::new();
    extra_frontmatter.insert(
        "koi_v1_source_path".to_owned(),
        Value::String(slash_normalized_path(relative)),
    );
    extra_frontmatter.insert(
        "koi_v1_body_hash".to_owned(),
        Value::String(body_hash.clone()),
    );
    if let Some(id) = parsed_json
        .as_ref()
        .and_then(|json| json.get("id").and_then(Value::as_str))
    {
        extra_frontmatter.insert("koi_v1_id".to_owned(), Value::String(id.to_owned()));
    }
    if let Some(project) = parsed_json
        .as_ref()
        .and_then(|json| json.get("scope"))
        .and_then(|scope| scope_string(Some(scope), "project"))
    {
        extra_frontmatter.insert(
            "koi_v1_scope_project".to_owned(),
            Value::String(project.clone()),
        );
        ambiguities.push(ImportAmbiguity {
            path: relative.to_path_buf(),
            field: "scope.project",
            fallback: "extra_frontmatter.koi_v1_scope_project".to_owned(),
            reason: "Cairn records cannot yet use project as an addressable scope dimension"
                .to_owned(),
        });
    }
    let tags = parsed_json
        .as_ref()
        .and_then(|json| json.get("tags").and_then(Value::as_array))
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let identity_seed = format!("{}:{body_hash}", slash_normalized_path(relative));
    let source_id =
        SourceId::parse(deterministic_ulid(&identity_seed, "source").0).map_err(|source| {
            ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            }
        })?;
    let source_ref_id = source_id.as_str().to_owned();
    let source_hash = format!("{SOURCE_HASH_PREFIX}{body_hash}");
    let record = MemoryRecord {
        id: RecordId::parse(deterministic_ulid(&identity_seed, "record").0).map_err(|source| {
            ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            }
        })?,
        target_id: TargetId::parse(deterministic_ulid(&identity_seed, "target").0).map_err(
            |source| ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            },
        )?,
        kind,
        class,
        visibility: MemoryVisibility::Private,
        scope,
        body,
        source_ids: vec![source_id.clone()],
        provenance: Provenance {
            source_sensor: Identity::parse(KOI_IMPORT_SENSOR).map_err(|source| {
                ImportError::Domain {
                    path: relative.to_path_buf(),
                    source,
                }
            })?,
            created_at: issued_at.clone(),
            originating_agent_id: author.clone(),
            source_ids: vec![source_id],
            source_hash: source_hash.clone(),
            consent_ref: "consent:koi-v1-import".to_owned(),
            llm_id_if_any: None,
            source_refs: vec![SourceRef {
                id: source_ref_id,
                hash: source_hash,
            }],
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
            "b".repeat(128)
        ))
        .map_err(|source| ImportError::Domain {
            path: relative.to_path_buf(),
            source,
        })?,
        tags,
        extra_frontmatter,
        consent_model: None,
    };
    record.validate().map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let item = ExternalImportItem {
        kind: ExternalImportItemKind::Record,
        source_path: relative.to_path_buf(),
        legacy_id: legacy_id(parsed_json.as_ref()),
        session_ids: session_ids_from_json(parsed_json.as_ref()),
        skill_ids: skill_ids_from_json(parsed_json.as_ref()),
        provenance: ExternalImportProvenance {
            source_sensor: KOI_IMPORT_SENSOR.to_owned(),
            source_hash: record.provenance.source_hash.clone(),
            source_refs: record.provenance.source_refs.clone(),
        },
    };
    let mut items = vec![item.clone()];
    items.extend(
        item.session_ids
            .iter()
            .map(|session_id| ExternalImportItem {
                kind: ExternalImportItemKind::Session,
                source_path: item.source_path.clone(),
                legacy_id: Some(session_id.clone()),
                session_ids: vec![session_id.clone()],
                skill_ids: Vec::new(),
                provenance: item.provenance.clone(),
            }),
    );
    items.extend(item.skill_ids.iter().map(|skill_id| ExternalImportItem {
        kind: ExternalImportItemKind::Skill,
        source_path: item.source_path.clone(),
        legacy_id: Some(skill_id.clone()),
        session_ids: Vec::new(),
        skill_ids: vec![skill_id.clone()],
        provenance: item.provenance.clone(),
    }));
    let findings = findings_from_file(parsed_json.as_ref(), relative, &ambiguities);
    Ok(MappedFile {
        record,
        ambiguities,
        findings,
        items,
    })
}

fn extract_body(json: &Value) -> Option<String> {
    ["body", "text", "content", "memory"]
        .iter()
        .find_map(|key| json.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(ToOwned::to_owned)
}

fn legacy_id(json: Option<&Value>) -> Option<String> {
    json.and_then(|json| json.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

fn session_ids_from_json(json: Option<&Value>) -> Vec<String> {
    let mut ids = BTreeSet::new();
    if let Some(scope_id) = json
        .and_then(|json| json.get("scope"))
        .and_then(|scope| scope_string(Some(scope), "session_id"))
    {
        ids.insert(scope_id);
    }
    if let Some(session_id) = json.and_then(|json| json.get("session_id")) {
        ids.extend(id_values(session_id));
    }
    if let Some(session) = json.and_then(|json| json.get("session")) {
        ids.extend(id_values(session));
    }
    ids.into_iter().collect()
}

fn skill_ids_from_json(json: Option<&Value>) -> Vec<String> {
    let mut ids = BTreeSet::new();
    if let Some(skill) = json.and_then(|json| json.get("skill")) {
        ids.extend(id_values(skill));
    }
    if let Some(skills) = json.and_then(|json| json.get("skills")) {
        ids.extend(id_values(skills));
    }
    ids.into_iter().collect()
}

fn id_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(id) => normalized_id(id).into_iter().collect(),
        Value::Object(map) => map
            .get("id")
            .or_else(|| map.get("name"))
            .and_then(Value::as_str)
            .and_then(normalized_id)
            .into_iter()
            .collect(),
        Value::Array(values) => values.iter().flat_map(id_values).collect(),
        _ => Vec::new(),
    }
}

fn normalized_id(id: &str) -> Option<String> {
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

fn findings_from_file(
    json: Option<&Value>,
    relative: &Path,
    ambiguities: &[ImportAmbiguity],
) -> Vec<ExternalImportFinding> {
    let mut findings = ambiguities
        .iter()
        .map(|ambiguity| ExternalImportFinding {
            path: ambiguity.path.clone(),
            kind: ExternalImportFindingKind::Ambiguous,
            field: ambiguity.field.to_owned(),
            fallback: Some(ambiguity.fallback.clone()),
            reason: ambiguity.reason.clone(),
        })
        .collect::<Vec<_>>();

    let Some(Value::Object(map)) = json else {
        return findings;
    };
    for key in map.keys() {
        if is_supported_manifest_field(key) {
            continue;
        }
        let (kind, reason) = if is_privacy_sensitive_field(key) {
            (
                ExternalImportFindingKind::PrivacySensitive,
                "legacy field name looks privacy-sensitive and requires review".to_owned(),
            )
        } else {
            (
                ExternalImportFindingKind::Unsupported,
                "legacy field has no neutral Cairn import-manifest target".to_owned(),
            )
        };
        findings.push(ExternalImportFinding {
            path: relative.to_path_buf(),
            kind,
            field: key.clone(),
            fallback: None,
            reason,
        });
    }
    findings
}

fn is_supported_manifest_field(field: &str) -> bool {
    matches!(
        field,
        "id" | "body"
            | "text"
            | "content"
            | "memory"
            | "kind"
            | "tags"
            | "scope"
            | "created_at"
            | "session"
            | "session_id"
            | "skill"
            | "skills"
            | "provenance"
            | "source_hash"
    )
}

fn is_privacy_sensitive_field(field: &str) -> bool {
    let field = field.to_ascii_lowercase();
    [
        "api_key",
        "credential",
        "password",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| field.contains(marker))
}

fn scope_from_json(json: Option<&Value>) -> ScopeTuple {
    let scope = json.and_then(|json| json.get("scope"));
    ScopeTuple {
        tenant: scope_string(scope, "tenant"),
        workspace: scope_string(scope, "workspace"),
        project: None,
        session_id: scope_string(scope, "session_id"),
        entity: scope_string(scope, "entity"),
        user: scope
            .and_then(|scope| scope.get("user"))
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("hmn:"))
            .unwrap_or(KOI_IMPORT_AUTHOR)
            .to_owned()
            .into(),
        agent: scope
            .and_then(|scope| scope.get("agent"))
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("agt:"))
            .map(ToOwned::to_owned),
    }
}

fn scope_string(scope: Option<&Value>, key: &str) -> Option<String> {
    scope
        .and_then(|scope| scope.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

const fn class_for_kind(kind: MemoryKind) -> MemoryClass {
    match kind {
        MemoryKind::Event
        | MemoryKind::Feedback
        | MemoryKind::Reasoning
        | MemoryKind::SensorObservation
        | MemoryKind::Trace
        | MemoryKind::UserSignal => MemoryClass::Episodic,
        MemoryKind::Playbook
        | MemoryKind::Rule
        | MemoryKind::StrategyFailure
        | MemoryKind::StrategySuccess
        | MemoryKind::Workflow => MemoryClass::Procedural,
        _ => MemoryClass::Semantic,
    }
}

fn plan_records(
    source_root: &Path,
    records: &[MemoryRecord],
    batch_size: usize,
    mode: FlushMode,
) -> Result<Vec<FlushPlan>, ImportError> {
    let mut plans = Vec::new();
    let author = Identity::parse(KOI_IMPORT_AUTHOR).map_err(|source| ImportError::Domain {
        path: source_root.to_path_buf(),
        source,
    })?;
    for (idx, chunk) in records.chunks(batch_size).enumerate() {
        let issued_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        plans.push(FlushPlan {
            operation_id: deterministic_operation_id(source_root, idx, chunk),
            issued_at,
            issuer: author.clone(),
            principal: None,
            scope: ScopeTuple::default(),
            mode,
            mutations: chunk
                .iter()
                .cloned()
                .map(|record| PlannedMutation::Upsert {
                    record: Box::new(record),
                    prior_version: None,
                })
                .collect(),
            reason: PlanReason::UserIngest,
            source_events: Vec::new(),
            target_hashes: BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        });
    }
    Ok(plans)
}

fn deterministic_operation_id(
    source_root: &Path,
    batch_index: usize,
    records: &[MemoryRecord],
) -> Ulid {
    let mut hasher = Sha256::new();
    hasher.update(slash_normalized_path(source_root).as_bytes());
    hasher.update(b":koi-v1:");
    hasher.update(batch_index.to_be_bytes());
    for record in records {
        hasher.update(b":");
        hasher.update(record.id.as_str().as_bytes());
    }
    ulid_from_digest(hasher.finalize())
}

fn deterministic_ulid(seed: &str, suffix: &str) -> Ulid {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(b":");
    hasher.update(suffix.as_bytes());
    ulid_from_digest(hasher.finalize())
}

fn ulid_from_digest(digest: impl AsRef<[u8]>) -> Ulid {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    Ulid(ulid::Ulid::from_bytes(bytes).to_string())
}

fn slash_normalized_path(path: &Path) -> String {
    path.as_os_str().to_string_lossy().replace('\\', "/")
}

fn sha256_wire(bytes: &[u8]) -> String {
    format!("{SOURCE_HASH_PREFIX}{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use cairn_core::domain::{FlushMode, MemoryClass, MemoryKind, PlannedMutation};

    use super::{
        ExternalImportFindingKind, ExternalImportItemKind, KoiImportOptions, map_koi_v1_archive,
        write_review_plans,
    };

    #[test]
    fn koi_v1_json_maps_to_valid_records_and_ambiguity_report() {
        let archive = tempfile::tempdir().expect("archive");
        let memory_dir = archive.path().join("memory-fs");
        fs::create_dir_all(&memory_dir).expect("memory dir");
        fs::write(
            memory_dir.join("preference.json"),
            r#"{
              "id": "pref-tone",
              "text": "User prefers direct, compact updates.",
              "kind": "user",
              "tags": ["koi", "preference"],
              "scope": {
                "tenant": "default",
                "workspace": "my-vault",
                "project": "koi-project",
                "entity": "ingest",
                "user": "hmn:sophia"
              }
            }"#,
        )
        .expect("write fixture");
        fs::write(
            memory_dir.join("legacy.md"),
            "Koi kept an untyped markdown memory here.",
        )
        .expect("write md");

        let report = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 64,
            mode: FlushMode::HumanReview,
        })
        .expect("map archive");

        assert_eq!(report.records.len(), 2);
        let preference = report
            .records
            .iter()
            .find(|record| record.body.contains("direct, compact"))
            .expect("preference record");
        assert_eq!(preference.kind, MemoryKind::User);
        assert_eq!(preference.class, MemoryClass::Semantic);
        assert_eq!(preference.scope.tenant.as_deref(), Some("default"));
        assert_eq!(preference.scope.workspace.as_deref(), Some("my-vault"));
        assert_eq!(preference.scope.project, None);
        assert_eq!(preference.scope.entity.as_deref(), Some("ingest"));
        assert_eq!(preference.scope.user.as_deref(), Some("hmn:sophia"));
        assert_eq!(
            preference.extra_frontmatter["koi_v1_scope_project"],
            serde_json::json!("koi-project")
        );
        assert_eq!(
            preference.provenance.source_refs[0].id,
            preference.provenance.source_ids[0].as_str()
        );
        assert_eq!(
            preference.provenance.source_refs[0].hash,
            preference.provenance.source_hash
        );
        assert!(preference.tags.iter().any(|tag| tag == "koi"));
        preference.validate().expect("valid record");

        assert!(
            report
                .ambiguities
                .iter()
                .any(|ambiguity| ambiguity.field == "kind" && ambiguity.fallback == "reference"),
            "markdown import should report ambiguous kind fallback: {:?}",
            report.ambiguities
        );
        assert!(
            report
                .ambiguities
                .iter()
                .any(|ambiguity| ambiguity.field == "scope.project"
                    && ambiguity.fallback == "extra_frontmatter.koi_v1_scope_project"),
            "project scope fallback should be reported for review: {:?}",
            report.ambiguities
        );
    }

    #[test]
    fn koi_v1_report_exposes_neutral_manifest_and_review_findings() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("memory.json"),
            r#"{
              "id": "pref-tone",
              "text": "User prefers direct, compact updates.",
              "kind": "user",
              "created_at": "2026-02-03T04:05:06Z",
              "tags": ["koi", "preference"],
              "scope": {
                "tenant": "default",
                "workspace": "my-vault",
                "project": "koi-project",
                "session_id": "session-42",
                "entity": "ingest",
                "user": "hmn:sophia"
              },
              "skills": [
                {"id": "skill-tone", "name": "Tone Memory"}
              ],
              "legacy_embedding": [0.1, 0.2],
              "api_token": "redacted-before-review"
            }"#,
        )
        .expect("write fixture");

        let report = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 64,
            mode: FlushMode::HumanReview,
        })
        .expect("map archive");

        assert_eq!(report.manifest.system, "koi-v1");
        assert_eq!(report.manifest.items.len(), 3);
        let item = report
            .manifest
            .items
            .iter()
            .find(|item| item.kind == ExternalImportItemKind::Record)
            .expect("record item");
        assert_eq!(item.kind, ExternalImportItemKind::Record);
        assert_eq!(item.source_path, std::path::PathBuf::from("memory.json"));
        assert_eq!(item.legacy_id.as_deref(), Some("pref-tone"));
        assert_eq!(item.session_ids, vec!["session-42"]);
        assert_eq!(item.skill_ids, vec!["skill-tone"]);
        assert_eq!(item.provenance.source_sensor, "snr:koi-v1:import:local:v1");
        assert_eq!(
            item.provenance.source_hash,
            report.records[0].provenance.source_hash
        );
        assert_eq!(
            item.provenance.source_refs,
            report.records[0].provenance.source_refs
        );
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Session
                && item.legacy_id.as_deref() == Some("session-42")
        }));
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Skill
                && item.legacy_id.as_deref() == Some("skill-tone")
        }));

        assert!(
            report
                .plans
                .iter()
                .all(|plan| plan.mode == FlushMode::HumanReview)
        );
        assert!(report.plans.iter().all(|plan| {
            plan.mutations
                .iter()
                .all(|mutation| matches!(mutation, PlannedMutation::Upsert { .. }))
        }));

        assert!(
            report.findings.iter().any(|finding| {
                finding.kind == ExternalImportFindingKind::Ambiguous
                    && finding.field == "scope.project"
            }),
            "project scope fallback should be a neutral ambiguous finding: {:?}",
            report.findings
        );
        assert!(
            report.findings.iter().any(|finding| {
                finding.kind == ExternalImportFindingKind::Unsupported
                    && finding.field == "legacy_embedding"
            }),
            "unsupported fields should be reported for review: {:?}",
            report.findings
        );
        assert!(
            report.findings.iter().any(|finding| {
                finding.kind == ExternalImportFindingKind::PrivacySensitive
                    && finding.field == "api_token"
            }),
            "privacy-sensitive fields should be reported for review: {:?}",
            report.findings
        );
    }

    #[test]
    fn koi_v1_import_writes_pending_review_plans_and_sources_without_db_writes() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("memory.json"),
            r#"{"text":"Ship notes are stored in Koi memory-fs.","kind":"reference"}"#,
        )
        .expect("write fixture");
        let vault = tempfile::tempdir().expect("vault");

        let report = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 1,
            mode: FlushMode::HumanReview,
        })
        .expect("map archive");
        let written = write_review_plans(vault.path(), &report).expect("write plans");

        assert_eq!(written.len(), 1);
        assert!(written[0].ends_with(".cairn/flush/pending"));
        let source_id = report.records[0].provenance.source_ids[0].as_str();
        assert_eq!(
            fs::read_to_string(vault.path().join(source_id)).expect("source artifact"),
            report.records[0].body
        );
        let plan_path = written[0].join(format!("{}.plan.json", report.plans[0].operation_id.0));
        let raw = fs::read_to_string(plan_path).expect("plan json");
        let persisted: cairn_core::domain::flush_plan::PersistedPlan =
            serde_json::from_str(&raw).expect("persisted plan");
        assert!(matches!(
            persisted.status,
            cairn_core::domain::flush_plan::PlanStatus::Pending
        ));
        assert!(!persisted.plan.placeholder);
        assert_eq!(persisted.plan.mode, FlushMode::HumanReview);
        assert!(matches!(
            persisted.plan.mutations.first(),
            Some(PlannedMutation::Upsert { .. })
        ));
    }

    #[test]
    fn koi_v1_import_rejects_conflicting_existing_source_artifact() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("memory.json"),
            r#"{"text":"Ship notes are stored in Koi memory-fs.","kind":"reference"}"#,
        )
        .expect("write fixture");
        let vault = tempfile::tempdir().expect("vault");
        let report = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 1,
            mode: FlushMode::HumanReview,
        })
        .expect("map archive");
        let source_id = report.records[0].provenance.source_ids[0].as_str();
        fs::write(vault.path().join(source_id), "wrong source bytes").expect("seed conflict");

        let err = write_review_plans(vault.path(), &report).expect_err("conflict must fail");

        assert!(
            matches!(err, super::ImportError::SourceArtifactConflict { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn koi_v1_import_rejects_malformed_json_files() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("broken.json"),
            r#"{"text":"unterminated""#,
        )
        .expect("write fixture");

        let err = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 64,
            mode: FlushMode::HumanReview,
        })
        .expect_err("malformed json must fail closed");

        assert!(
            matches!(err, super::ImportError::Json { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn koi_v1_same_body_in_distinct_files_keeps_distinct_record_ids() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("one.json"),
            r#"{"id":"one","text":"same imported body","kind":"reference"}"#,
        )
        .expect("write one");
        fs::write(
            archive.path().join("two.json"),
            r#"{"id":"two","text":"same imported body","kind":"reference"}"#,
        )
        .expect("write two");

        let report = map_koi_v1_archive(&KoiImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 64,
            mode: FlushMode::HumanReview,
        })
        .expect("map archive");

        assert_eq!(report.records.len(), 2);
        assert_ne!(report.records[0].id, report.records[1].id);
        assert_ne!(report.records[0].target_id, report.records[1].target_id);
        assert_ne!(
            report.records[0].provenance.source_ids[0],
            report.records[1].provenance.source_ids[0]
        );
        assert_eq!(
            report.records[0].provenance.source_hash,
            report.records[1].provenance.source_hash
        );
    }
}
