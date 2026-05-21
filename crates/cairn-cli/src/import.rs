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
const OPENCLAW_IMPORT_AUTHOR: &str = "hmn:openclaw-import:v1";
const OPENCLAW_IMPORT_SENSOR: &str = "snr:openclaw:import:local:v1";
const ROWBOAT_IMPORT_AUTHOR: &str = "hmn:rowboat-import:v1";
const ROWBOAT_IMPORT_SENSOR: &str = "snr:rowboat:import:local:v1";
const OPENCODE_IMPORT_AUTHOR: &str = "hmn:opencode-import:v1";
const OPENCODE_IMPORT_SENSOR: &str = "snr:opencode:import:local:v1";
const HERMES_IMPORT_AUTHOR: &str = "hmn:hermes-import:v1";
const HERMES_IMPORT_SENSOR: &str = "snr:hermes-agent:import:local:v1";
const SOURCE_HASH_PREFIX: &str = "sha256:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportSystem {
    KoiV1,
    OpenClaw,
}

impl ImportSystem {
    const fn spec(self) -> ImportSpec {
        match self {
            Self::KoiV1 => ImportSpec {
                system: "koi-v1",
                author: KOI_IMPORT_AUTHOR,
                sensor: KOI_IMPORT_SENSOR,
                consent_ref: "consent:koi-v1-import",
                frontmatter_prefix: "koi_v1",
            },
            Self::OpenClaw => ImportSpec {
                system: "openclaw",
                author: OPENCLAW_IMPORT_AUTHOR,
                sensor: OPENCLAW_IMPORT_SENSOR,
                consent_ref: "consent:openclaw-import",
                frontmatter_prefix: "openclaw",
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ImportSpec {
    system: &'static str,
    author: &'static str,
    sensor: &'static str,
    consent_ref: &'static str,
    frontmatter_prefix: &'static str,
}

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
                .value_parser(["koi-v1", "openclaw", "rowboat", "opencode", "hermes-agent"])
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
    let Some(archive) = sub.get_one::<PathBuf>("archive") else {
        return usage_error(json, "archive", "archive path is required");
    };
    let batch_size = sub
        .get_one::<u32>("batch_size")
        .copied()
        .unwrap_or(64)
        .try_into()
        .unwrap_or(64_usize);
    let default_workspace = match configured_default_workspace(vault_root, json) {
        Ok(default_workspace) => default_workspace,
        Err(code) => return code,
    };
    let Some(report) = create_import_report(system, archive, batch_size, &default_workspace) else {
        return usage_error(json, "from", "unsupported import system");
    };
    match report.and_then(|report| {
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

fn create_import_report(
    system: &str,
    archive: &Path,
    batch_size: usize,
    default_workspace: &str,
) -> Option<Result<KoiImportReport, ImportError>> {
    let source = archive.to_path_buf();
    Some(match system {
        "koi-v1" => map_archive(
            ImportSystem::KoiV1,
            &KoiImportOptions {
                source,
                batch_size,
                mode: FlushMode::HumanReview,
            },
            default_workspace,
        ),
        "openclaw" => map_archive(
            ImportSystem::OpenClaw,
            &KoiImportOptions {
                source,
                batch_size,
                mode: FlushMode::HumanReview,
            },
            default_workspace,
        ),
        "rowboat" => map_rowboat_archive(&RowboatImportOptions {
            source,
            batch_size,
            mode: FlushMode::HumanReview,
        }),
        "opencode" => map_opencode_archive(&OpenCodeImportOptions {
            source,
            batch_size,
            mode: FlushMode::HumanReview,
        }),
        "hermes-agent" => map_hermes_agent_archive(&KoiImportOptions {
            source,
            batch_size,
            mode: FlushMode::HumanReview,
        }),
        _ => return None,
    })
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

fn configured_default_workspace(
    vault_root: &Path,
    json: bool,
) -> Result<String, std::process::ExitCode> {
    crate::config::load(vault_root, &crate::config::CliOverrides::default())
        .map(|config| config.vault.name)
        .map_err(|err| {
            if json {
                let response = serde_json::json!({
                    "status": "error",
                    "error": {
                        "code": "ConfigError",
                        "message": err.to_string(),
                    }
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_owned())
                );
            } else {
                eprintln!("cairn import: config error — {err:#}");
            }
            std::process::ExitCode::from(78)
        })
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

/// Options for mapping a Rowboat workspace into Cairn review plans.
#[derive(Debug, Clone)]
pub struct RowboatImportOptions {
    /// Path to the Rowboat work directory.
    pub source: PathBuf,
    /// Maximum records per generated review plan.
    pub batch_size: usize,
    /// Plan dispatch mode. Issue #155 uses [`FlushMode::HumanReview`].
    pub mode: FlushMode,
}

/// Options for mapping an `OpenCode` archive into Cairn review plans.
#[derive(Debug, Clone)]
pub struct OpenCodeImportOptions {
    /// Path to an `OpenCode` project/session archive root.
    pub source: PathBuf,
    /// Maximum records per generated review plan.
    pub batch_size: usize,
    /// Plan dispatch mode. Issue #156 uses [`FlushMode::HumanReview`].
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
    #[error("import source `{}` is not a directory", .archive.display())]
    SourceNotDirectory {
        /// Requested archive root.
        archive: PathBuf,
    },
    /// Batch size must be nonzero.
    #[error("import batch size must be greater than zero")]
    InvalidBatchSize,
    /// File I/O failed.
    #[error("import I/O for `{path}`: {source}")]
    Io {
        /// File path being read or written.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A JSON source file was malformed.
    #[error("import JSON parse for `{path}`: {source}")]
    Json {
        /// Source file being parsed.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: serde_json::Error,
    },
    /// Existing source artifact bytes do not match the imported record.
    #[error(
        "import source artifact conflict for `{}`: existing hash `{existing}` does not match expected `{expected}`",
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
    #[error("import record build for `{path}`: {source}")]
    Domain {
        /// Source path being mapped.
        path: PathBuf,
        /// Underlying domain error.
        #[source]
        source: cairn_core::domain::DomainError,
    },
    /// A review plan could not be serialized.
    #[error("import plan serialize for `{path}`: {source}")]
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
    map_archive(ImportSystem::KoiV1, opts, "my-vault")
}

fn map_archive(
    system: ImportSystem,
    opts: &KoiImportOptions,
    default_workspace: &str,
) -> Result<KoiImportReport, ImportError> {
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
    let spec = system.spec();
    for entry in WalkDir::new(&opts.source)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if !is_importable(system, path, &opts.source) {
            continue;
        }
        let raw = fs::read_to_string(path).map_err(|source| ImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mapped = map_file(path, &opts.source, &raw, spec, default_workspace)?;
        ambiguities.extend(mapped.ambiguities);
        findings.extend(mapped.findings);
        items.extend(mapped.items);
        records.push(mapped.record);
    }

    let plans = plan_records(
        spec.system,
        spec.author,
        &opts.source,
        &records,
        opts.batch_size,
        opts.mode,
    )?;
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
            system: spec.system.to_owned(),
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

/// Map a Rowboat work directory into valid Cairn records and pending review plans.
///
/// This bridge imports knowledge markdown notes only. Rowboat sync state is
/// preserved as manifest context and migration-report findings for review.
pub fn map_rowboat_archive(opts: &RowboatImportOptions) -> Result<KoiImportReport, ImportError> {
    if opts.batch_size == 0 {
        return Err(ImportError::InvalidBatchSize);
    }
    if !opts.source.is_dir() {
        return Err(ImportError::SourceNotDirectory {
            archive: opts.source.clone(),
        });
    }

    let state = read_rowboat_state(&opts.source)?;
    let mut records = Vec::new();
    let mut ambiguities = Vec::new();
    let mut findings = Vec::new();
    let mut items = Vec::new();
    let knowledge_root = opts.source.join("knowledge");
    if knowledge_root.is_dir() {
        for entry in WalkDir::new(&knowledge_root)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let raw = fs::read_to_string(path).map_err(|source| ImportError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let mapped = map_rowboat_note(path, &opts.source, &raw, &state.workflow_ids)?;
            ambiguities.extend(mapped.ambiguities);
            findings.extend(mapped.findings);
            items.extend(mapped.items);
            records.push(mapped.record);
        }
    }
    findings.extend(state.findings);
    items.extend(state.workflow_ids.into_iter().map(|workflow_id| {
        let provenance = records.first().map_or_else(
            || ExternalImportProvenance {
                source_sensor: ROWBOAT_IMPORT_SENSOR.to_owned(),
                source_hash: format!("{SOURCE_HASH_PREFIX}{:x}", Sha256::digest([])),
                source_refs: Vec::new(),
            },
            |record| ExternalImportProvenance {
                source_sensor: ROWBOAT_IMPORT_SENSOR.to_owned(),
                source_hash: record.provenance.source_hash.clone(),
                source_refs: record.provenance.source_refs.clone(),
            },
        );
        ExternalImportItem {
            kind: ExternalImportItemKind::Skill,
            source_path: PathBuf::from("agent_notes_state.json"),
            legacy_id: Some(workflow_id.clone()),
            session_ids: Vec::new(),
            skill_ids: vec![workflow_id],
            provenance,
        }
    }));

    let plans = plan_records(
        "rowboat",
        ROWBOAT_IMPORT_AUTHOR,
        &opts.source,
        &records,
        opts.batch_size,
        opts.mode,
    )?;
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
            system: "rowboat".to_owned(),
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

/// Map `OpenCode` project instructions, session parts, and compaction summaries
/// into valid Cairn records and pending review plans.
///
/// This bridge performs no database writes. The resulting plans can be reviewed
/// and later applied through the ordinary flush path.
pub fn map_opencode_archive(opts: &OpenCodeImportOptions) -> Result<KoiImportReport, ImportError> {
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
        let relative = path.strip_prefix(&opts.source).unwrap_or(path);
        let is_instruction = is_opencode_instruction_file(path);
        let is_json = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "json");
        if !is_instruction && !is_json {
            continue;
        }
        let raw = fs::read_to_string(path).map_err(|source| ImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mapped = if is_instruction {
            let kind = match path.file_name().and_then(|name| name.to_str()) {
                Some("AGENTS.md") => MemoryKind::Project,
                Some("CLAUDE.md" | "CONTEXT.md") => MemoryKind::Rule,
                _ => MemoryKind::Reference,
            };
            vec![map_opencode_record(
                relative,
                "instruction",
                raw.trim().to_owned(),
                kind,
                None,
                None,
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                None,
                None,
                None,
                None,
                None,
                None,
            )?]
        } else {
            map_opencode_json(path, relative, &raw)?
        };

        for mapped_file in mapped {
            ambiguities.extend(mapped_file.ambiguities);
            findings.extend(mapped_file.findings);
            items.extend(mapped_file.items);
            records.push(mapped_file.record);
        }
    }

    let plans = plan_records_for_system(
        &opts.source,
        &records,
        opts.batch_size,
        opts.mode,
        OPENCODE_IMPORT_AUTHOR,
        "opencode",
    )?;
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
            system: "opencode".to_owned(),
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

struct RowboatState {
    workflow_ids: Vec<String>,
    findings: Vec<ExternalImportFinding>,
}

struct MappedFile {
    record: MemoryRecord,
    ambiguities: Vec<ImportAmbiguity>,
    findings: Vec<ExternalImportFinding>,
    items: Vec<ExternalImportItem>,
}

fn read_rowboat_state(root: &Path) -> Result<RowboatState, ImportError> {
    let path = root.join("agent_notes_state.json");
    if !path.exists() {
        return Ok(RowboatState {
            workflow_ids: Vec::new(),
            findings: Vec::new(),
        });
    }
    let raw = fs::read_to_string(&path).map_err(|source| ImportError::Io {
        path: path.clone(),
        source,
    })?;
    let json = serde_json::from_str::<Value>(&raw).map_err(|source| ImportError::Json {
        path: PathBuf::from("agent_notes_state.json"),
        source,
    })?;
    let workflow_ids = json
        .get("workflows")
        .map(id_values)
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let findings = findings_from_rowboat_json_object(
        json.as_object(),
        Path::new("agent_notes_state.json"),
        &["last_sync_at", "workflows"],
    );
    Ok(RowboatState {
        workflow_ids,
        findings,
    })
}

fn map_rowboat_note(
    path: &Path,
    root: &Path,
    raw: &str,
    workflow_ids: &[String],
) -> Result<MappedFile, ImportError> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let (frontmatter, body) = split_markdown_frontmatter(raw);
    let note_type = frontmatter
        .get("type")
        .or_else(|| frontmatter.get("note_type"))
        .map(String::as_str);
    let kind = rowboat_kind(note_type);
    let class = class_for_kind(kind);
    let issued_at = Rfc3339Timestamp::parse(
        frontmatter
            .get("created_at")
            .or_else(|| frontmatter.get("updated_at"))
            .cloned()
            .unwrap_or_else(|| "2026-01-01T00:00:00Z".to_owned()),
    )
    .map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let author = Identity::parse(ROWBOAT_IMPORT_AUTHOR).map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let body = body.trim().to_owned();
    let body_hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    let extra_frontmatter = rowboat_extra_frontmatter(
        relative,
        &frontmatter,
        note_type,
        &body,
        &body_hash,
        workflow_ids,
    );
    let tags = note_type
        .into_iter()
        .map(|value| format!("rowboat:{value}"))
        .chain(std::iter::once("rowboat".to_owned()))
        .collect::<Vec<_>>();
    let identity_seed = format!("{}:{body_hash}", slash_normalized_path(relative));
    let record = build_rowboat_record(RowboatRecordBuild {
        relative,
        identity_seed: &identity_seed,
        kind,
        class,
        body,
        source_hash: format!("{SOURCE_HASH_PREFIX}{body_hash}"),
        issued_at,
        author,
        tags,
        extra_frontmatter,
    })?;
    record.validate().map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let item = ExternalImportItem {
        kind: ExternalImportItemKind::Record,
        source_path: relative.to_path_buf(),
        legacy_id: rowboat_legacy_id(&frontmatter),
        session_ids: Vec::new(),
        skill_ids: workflow_ids.to_vec(),
        provenance: ExternalImportProvenance {
            source_sensor: ROWBOAT_IMPORT_SENSOR.to_owned(),
            source_hash: record.provenance.source_hash.clone(),
            source_refs: record.provenance.source_refs.clone(),
        },
    };
    let findings = findings_from_rowboat_object(
        Some(&frontmatter),
        relative,
        &[
            "type",
            "note_type",
            "source",
            "rowboat_id",
            "id",
            "created_at",
            "updated_at",
        ],
    );
    Ok(MappedFile {
        record,
        ambiguities: Vec::new(),
        findings,
        items: vec![item],
    })
}

struct RowboatRecordBuild<'a> {
    relative: &'a Path,
    identity_seed: &'a str,
    kind: MemoryKind,
    class: MemoryClass,
    body: String,
    source_hash: String,
    issued_at: Rfc3339Timestamp,
    author: Identity,
    tags: Vec<String>,
    extra_frontmatter: BTreeMap<String, Value>,
}

fn rowboat_extra_frontmatter(
    relative: &Path,
    frontmatter: &BTreeMap<String, String>,
    note_type: Option<&str>,
    body: &str,
    body_hash: &str,
    workflow_ids: &[String],
) -> BTreeMap<String, Value> {
    let mut extra = BTreeMap::from([
        (
            "rowboat_source_path".to_owned(),
            Value::String(slash_normalized_path(relative)),
        ),
        (
            "rowboat_body_hash".to_owned(),
            Value::String(body_hash.to_owned()),
        ),
    ]);
    if let Some(note_type) = note_type {
        extra.insert(
            "rowboat_note_type".to_owned(),
            Value::String(note_type.to_owned()),
        );
    }
    if let Some(source) = frontmatter.get("source") {
        extra.insert("rowboat_source".to_owned(), Value::String(source.clone()));
    }
    if let Some(rowboat_id) = rowboat_legacy_id(frontmatter) {
        extra.insert("rowboat_id".to_owned(), Value::String(rowboat_id));
    }
    let wikilinks = rowboat_wikilinks(body);
    if !wikilinks.is_empty() {
        extra.insert("rowboat_wikilinks".to_owned(), serde_json::json!(wikilinks));
    }
    if !workflow_ids.is_empty() {
        extra.insert(
            "rowboat_workflow_ids".to_owned(),
            serde_json::json!(workflow_ids),
        );
    }
    extra
}

fn build_rowboat_record(args: RowboatRecordBuild<'_>) -> Result<MemoryRecord, ImportError> {
    let source_id =
        SourceId::parse(deterministic_ulid(args.identity_seed, "source").0).map_err(|source| {
            ImportError::Domain {
                path: args.relative.to_path_buf(),
                source,
            }
        })?;
    let source_ref_id = source_id.as_str().to_owned();
    let source_hash = args.source_hash.clone();
    let provenance = rowboat_provenance(source_id.clone(), source_ref_id, source_hash, &args)?;
    Ok(MemoryRecord {
        id: RecordId::parse(deterministic_ulid(args.identity_seed, "record").0).map_err(
            |source| ImportError::Domain {
                path: args.relative.to_path_buf(),
                source,
            },
        )?,
        target_id: TargetId::parse(deterministic_ulid(args.identity_seed, "target").0).map_err(
            |source| ImportError::Domain {
                path: args.relative.to_path_buf(),
                source,
            },
        )?,
        kind: args.kind,
        class: args.class,
        visibility: MemoryVisibility::Private,
        scope: rowboat_scope(),
        body: args.body,
        source_ids: vec![source_id.clone()],
        provenance,
        updated_at: args.issued_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: args.author,
            at: args.issued_at,
        }],
        signature: placeholder_import_signature(args.relative)?,
        tags: args.tags,
        extra_frontmatter: args.extra_frontmatter,
        consent_model: None,
    })
}

fn rowboat_scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("my-vault".to_owned()),
        entity: Some("ingest".to_owned()),
        user: ROWBOAT_IMPORT_AUTHOR.to_owned().into(),
        ..ScopeTuple::default()
    }
}

fn rowboat_provenance(
    source_id: SourceId,
    source_ref_id: String,
    source_hash: String,
    args: &RowboatRecordBuild<'_>,
) -> Result<Provenance, ImportError> {
    Ok(Provenance {
        source_sensor: Identity::parse(ROWBOAT_IMPORT_SENSOR).map_err(|source| {
            ImportError::Domain {
                path: args.relative.to_path_buf(),
                source,
            }
        })?,
        created_at: args.issued_at.clone(),
        originating_agent_id: args.author.clone(),
        source_ids: vec![source_id],
        source_hash: source_hash.clone(),
        consent_ref: "consent:rowboat-import".to_owned(),
        llm_id_if_any: None,
        source_refs: vec![SourceRef {
            id: source_ref_id,
            hash: source_hash,
        }],
    })
}

fn placeholder_import_signature(
    relative: &Path,
) -> Result<cairn_core::domain::record::Ed25519Signature, ImportError> {
    cairn_core::domain::record::Ed25519Signature::parse(format!("ed25519:{}", "b".repeat(128)))
        .map_err(|source| ImportError::Domain {
            path: relative.to_path_buf(),
            source,
        })
}

fn rowboat_legacy_id(frontmatter: &BTreeMap<String, String>) -> Option<String> {
    frontmatter
        .get("rowboat_id")
        .or_else(|| frontmatter.get("id"))
        .cloned()
}

fn split_markdown_frontmatter(raw: &str) -> (BTreeMap<String, String>, &str) {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return (BTreeMap::new(), raw);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (BTreeMap::new(), raw);
    };
    let frontmatter_raw = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    let mut frontmatter = BTreeMap::new();
    for line in frontmatter_raw.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        if !key.is_empty() && !value.is_empty() {
            frontmatter.insert(key.to_owned(), value.to_owned());
        }
    }
    (frontmatter, body)
}

fn rowboat_kind(note_type: Option<&str>) -> MemoryKind {
    match note_type {
        Some("People" | "Organizations" | "Projects") => MemoryKind::Entity,
        _ => MemoryKind::Reference,
    }
}

fn rowboat_wikilinks(body: &str) -> Vec<String> {
    let mut links = BTreeSet::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            break;
        };
        let link = after_start[..end].trim();
        if !link.is_empty() {
            links.insert(link.to_owned());
        }
        rest = &after_start[end + 2..];
    }
    links.into_iter().collect()
}

fn findings_from_rowboat_object(
    map: Option<&BTreeMap<String, String>>,
    relative: &Path,
    supported: &[&str],
) -> Vec<ExternalImportFinding> {
    let Some(map) = map else {
        return Vec::new();
    };
    map.keys()
        .filter(|key| !supported.contains(&key.as_str()))
        .map(|key| {
            let (kind, reason) = if is_privacy_sensitive_field(key) {
                (
                    ExternalImportFindingKind::PrivacySensitive,
                    "legacy field name looks privacy-sensitive and requires review".to_owned(),
                )
            } else {
                (
                    ExternalImportFindingKind::Unsupported,
                    "Rowboat field has no neutral Cairn import-manifest target".to_owned(),
                )
            };
            ExternalImportFinding {
                path: relative.to_path_buf(),
                kind,
                field: key.clone(),
                fallback: None,
                reason,
            }
        })
        .collect()
}

fn findings_from_rowboat_json_object(
    map: Option<&serde_json::Map<String, Value>>,
    relative: &Path,
    supported: &[&str],
) -> Vec<ExternalImportFinding> {
    let Some(map) = map else {
        return Vec::new();
    };
    map.keys()
        .filter(|key| !supported.contains(&key.as_str()))
        .map(|key| {
            let (kind, reason) = if is_privacy_sensitive_field(key) {
                (
                    ExternalImportFindingKind::PrivacySensitive,
                    "legacy field name looks privacy-sensitive and requires review".to_owned(),
                )
            } else {
                (
                    ExternalImportFindingKind::Unsupported,
                    "Rowboat state field has no neutral Cairn import-manifest target".to_owned(),
                )
            };
            ExternalImportFinding {
                path: relative.to_path_buf(),
                kind,
                field: key.clone(),
                fallback: None,
                reason,
            }
        })
        .collect()
}

fn is_importable(system: ImportSystem, path: &Path, root: &Path) -> bool {
    if !has_importable_extension(path) {
        return false;
    }
    if system == ImportSystem::KoiV1 {
        return true;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    matches!(
        (components.next(), components.next()),
        (Some("MEMORY.md" | "SOUL.md"), None)
            | (Some("memory" | "sessions" | "transcripts"), Some(_))
    )
}

fn has_importable_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("json" | "md" | "txt")
    )
}

fn is_opencode_instruction_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("AGENTS.md" | "CLAUDE.md" | "CONTEXT.md")
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "OpenCode JSON mapping keeps summary, parts, fallback records, and findings together"
)]
fn map_opencode_json(
    _path: &Path,
    relative: &Path,
    raw: &str,
) -> Result<Vec<MappedFile>, ImportError> {
    let json = serde_json::from_str::<Value>(raw).map_err(|source| ImportError::Json {
        path: relative.to_path_buf(),
        source,
    })?;
    let session_id = session_ids_from_json(Some(&json)).into_iter().next();
    let scope_tenant = scope_string(json.get("scope"), "tenant");
    let scope_workspace = scope_string(json.get("scope"), "workspace");
    let scope_entity = scope_string(json.get("scope"), "entity");
    let scope_user = scope_string(json.get("scope"), "user");
    let scope_agent = scope_string(json.get("scope"), "agent");
    let created_at = json
        .get("created_at")
        .or_else(|| json.get("updated_at"))
        .and_then(Value::as_str);
    let mut mapped = Vec::new();
    if let Some(summary) = json
        .get("summary")
        .or_else(|| json.get("compaction_summary"))
    {
        mapped.extend(map_opencode_summary(
            relative,
            summary,
            session_id.as_deref(),
            created_at,
            scope_tenant.as_deref(),
            scope_workspace.as_deref(),
            scope_entity.as_deref(),
            scope_user.as_deref(),
            scope_agent.as_deref(),
        )?);
    }
    if let Some(parts) = json.get("parts").and_then(Value::as_array) {
        for (idx, part) in parts.iter().enumerate() {
            let body = part_text(part)
                .or_else(|| serde_json::to_string(part).ok())
                .unwrap_or_default();
            if body.trim().is_empty() {
                continue;
            }
            let mut extra = BTreeMap::new();
            extra.insert(
                "opencode_part_order".to_owned(),
                serde_json::json!(idx as u64),
            );
            if let Some(part_type) = part
                .get("type")
                .or_else(|| part.get("role"))
                .and_then(Value::as_str)
            {
                extra.insert(
                    "opencode_part_type".to_owned(),
                    Value::String(part_type.to_owned()),
                );
            }
            if let Some(tool) = part.get("tool").and_then(Value::as_str) {
                extra.insert("opencode_tool".to_owned(), Value::String(tool.to_owned()));
            }
            mapped.push(map_opencode_record(
                relative,
                &format!("part-{idx}"),
                body.trim().to_owned(),
                MemoryKind::Trace,
                created_at,
                session_id.as_deref(),
                Vec::new(),
                opencode_skill_ids(part),
                extra,
                scope_tenant.as_deref(),
                scope_workspace.as_deref(),
                scope_entity.as_deref(),
                scope_user.as_deref(),
                scope_agent.as_deref(),
                None,
            )?);
        }
    }
    if mapped.is_empty() {
        let kind = json
            .get("kind")
            .and_then(Value::as_str)
            .and_then(|kind| MemoryKind::parse(kind).ok())
            .unwrap_or(MemoryKind::Reference);
        mapped.push(map_opencode_record(
            relative,
            "json",
            extract_body(&json).unwrap_or_else(|| raw.trim().to_owned()),
            kind,
            created_at,
            session_id.as_deref(),
            tags_from_json(Some(&json)),
            skill_ids_from_json(Some(&json)),
            BTreeMap::new(),
            scope_tenant.as_deref(),
            scope_workspace.as_deref(),
            scope_entity.as_deref(),
            scope_user.as_deref(),
            scope_agent.as_deref(),
            legacy_id(Some(&json)),
        )?);
    }
    let file_findings = findings_from_file(Some(&json), relative, &[]);
    if let Some(first_mapped_file) = mapped.first_mut() {
        first_mapped_file.findings.extend(file_findings);
    }
    Ok(mapped)
}

#[allow(
    clippy::too_many_arguments,
    reason = "OpenCode scope fields map one-for-one"
)]
fn map_opencode_summary(
    relative: &Path,
    summary: &Value,
    session_id: Option<&str>,
    created_at: Option<&str>,
    scope_tenant: Option<&str>,
    scope_workspace: Option<&str>,
    scope_entity: Option<&str>,
    scope_user: Option<&str>,
    scope_agent: Option<&str>,
) -> Result<Vec<MappedFile>, ImportError> {
    let Some(map) = summary.as_object() else {
        return Ok(Vec::new());
    };
    let fields = [
        ("Goal", MemoryKind::User, "summary-goal"),
        ("goal", MemoryKind::User, "summary-goal"),
        ("Constraints", MemoryKind::Rule, "summary-constraints"),
        ("constraints", MemoryKind::Rule, "summary-constraints"),
        ("Progress", MemoryKind::StrategySuccess, "summary-progress"),
        ("progress", MemoryKind::StrategySuccess, "summary-progress"),
        (
            "Decisions",
            MemoryKind::StrategySuccess,
            "summary-decisions",
        ),
        (
            "decisions",
            MemoryKind::StrategySuccess,
            "summary-decisions",
        ),
    ];
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for (field, kind, role) in fields {
        if seen.contains(role) {
            continue;
        }
        let Some(value) = map.get(field) else {
            continue;
        };
        let Some(body) = summary_field_text(value) else {
            continue;
        };
        seen.insert(role);
        let mut extra = BTreeMap::new();
        extra.insert(
            "opencode_summary_field".to_owned(),
            Value::String(field.to_owned()),
        );
        records.push(map_opencode_record(
            relative,
            role,
            body,
            kind,
            created_at,
            session_id,
            Vec::new(),
            Vec::new(),
            extra,
            scope_tenant,
            scope_workspace,
            scope_entity,
            scope_user,
            scope_agent,
            None,
        )?);
    }
    Ok(records)
}

fn part_text(part: &Value) -> Option<String> {
    ["text", "content", "body", "message"]
        .iter()
        .find_map(|key| part.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(ToOwned::to_owned)
}

fn summary_field_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        Value::Array(items) => {
            let lines = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>();
            (!lines.is_empty()).then(|| lines.join("\n"))
        }
        _ => None,
    }
}

fn opencode_skill_ids(part: &Value) -> Vec<String> {
    let mut ids = BTreeSet::new();
    if part
        .get("tool")
        .and_then(Value::as_str)
        .is_some_and(|tool| tool == "skill")
    {
        ids.insert("skill".to_owned());
    }
    if let Some(skills) = part.get("skills") {
        ids.extend(id_values(skills));
    }
    ids.into_iter().collect()
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "OpenCode record mapping keeps provenance, scope, and metadata together"
)]
fn map_opencode_record(
    relative: &Path,
    role: &str,
    body: String,
    kind: MemoryKind,
    created_at: Option<&str>,
    session_id: Option<&str>,
    tags: Vec<String>,
    skill_ids: Vec<String>,
    mut extra_frontmatter: BTreeMap<String, Value>,
    scope_tenant: Option<&str>,
    scope_workspace: Option<&str>,
    scope_entity: Option<&str>,
    scope_user: Option<&str>,
    scope_agent: Option<&str>,
    legacy_id: Option<String>,
) -> Result<MappedFile, ImportError> {
    let body_hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    extra_frontmatter.insert(
        "opencode_source_path".to_owned(),
        Value::String(slash_normalized_path(relative)),
    );
    extra_frontmatter.insert(
        "opencode_body_hash".to_owned(),
        Value::String(body_hash.clone()),
    );
    extra_frontmatter.insert(
        "opencode_record_role".to_owned(),
        Value::String(role.to_owned()),
    );
    if let Some(session_id) = session_id {
        extra_frontmatter.insert(
            "opencode_session_id".to_owned(),
            Value::String(session_id.to_owned()),
        );
    }
    let issued_at = Rfc3339Timestamp::parse(
        created_at
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("2026-01-01T00:00:00Z")
            .to_owned(),
    )
    .map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let author = Identity::parse(OPENCODE_IMPORT_AUTHOR).map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let identity_seed = format!(
        "opencode:{}:{role}:{body_hash}",
        slash_normalized_path(relative)
    );
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
        class: class_for_kind(kind),
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            tenant: scope_tenant.map(ToOwned::to_owned),
            workspace: scope_workspace.map(ToOwned::to_owned),
            entity: scope_entity.map(ToOwned::to_owned),
            session_id: session_id.map(ToOwned::to_owned),
            user: scope_user
                .filter(|user| user.starts_with("hmn:"))
                .map(ToOwned::to_owned)
                .or_else(|| Some(OPENCODE_IMPORT_AUTHOR.to_owned())),
            agent: scope_agent
                .filter(|agent| agent.starts_with("agt:"))
                .map(ToOwned::to_owned),
            ..ScopeTuple::default()
        },
        body,
        source_ids: vec![source_id.clone()],
        provenance: Provenance {
            source_sensor: Identity::parse(OPENCODE_IMPORT_SENSOR).map_err(|source| {
                ImportError::Domain {
                    path: relative.to_path_buf(),
                    source,
                }
            })?,
            created_at: issued_at.clone(),
            originating_agent_id: author.clone(),
            source_ids: vec![source_id],
            source_hash: source_hash.clone(),
            consent_ref: "consent:opencode-import".to_owned(),
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
            "c".repeat(128)
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
        legacy_id: legacy_id.or_else(|| Some(format!("opencode:{role}"))),
        session_ids: session_id.into_iter().map(ToOwned::to_owned).collect(),
        skill_ids,
        provenance: ExternalImportProvenance {
            source_sensor: OPENCODE_IMPORT_SENSOR.to_owned(),
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
    Ok(MappedFile {
        record,
        ambiguities: Vec::new(),
        findings: Vec::new(),
        items,
    })
}

#[allow(clippy::too_many_lines)]
fn map_file(
    path: &Path,
    root: &Path,
    raw: &str,
    spec: ImportSpec,
    default_workspace: &str,
) -> Result<MappedFile, ImportError> {
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
        .or_else(|| kind_hint_for_path(spec, relative))
        .unwrap_or_else(|| {
            ambiguities.push(ImportAmbiguity {
                path: relative.to_path_buf(),
                field: "kind",
                fallback: MemoryKind::Reference.as_str().to_owned(),
                reason: format!(
                    "legacy {} item did not declare a Cairn memory kind",
                    spec.system
                ),
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
    let author = Identity::parse(spec.author).map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    let scope = scope_from_json(parsed_json.as_ref(), spec, default_workspace);
    let mut extra_frontmatter = BTreeMap::new();
    extra_frontmatter.insert(
        format!("{}_source_path", spec.frontmatter_prefix),
        Value::String(slash_normalized_path(relative)),
    );
    extra_frontmatter.insert(
        format!("{}_body_hash", spec.frontmatter_prefix),
        Value::String(body_hash.clone()),
    );
    if let Some(id) = parsed_json
        .as_ref()
        .and_then(|json| json.get("id").and_then(Value::as_str))
    {
        extra_frontmatter.insert(
            format!("{}_id", spec.frontmatter_prefix),
            Value::String(id.to_owned()),
        );
    }
    if let Some(project) = parsed_json
        .as_ref()
        .and_then(|json| json.get("scope"))
        .and_then(|scope| scope_string(Some(scope), "project"))
    {
        extra_frontmatter.insert(
            format!("{}_scope_project", spec.frontmatter_prefix),
            Value::String(project.clone()),
        );
        ambiguities.push(ImportAmbiguity {
            path: relative.to_path_buf(),
            field: "scope.project",
            fallback: format!(
                "extra_frontmatter.{}_scope_project",
                spec.frontmatter_prefix
            ),
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
            source_sensor: Identity::parse(spec.sensor).map_err(|source| ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            })?,
            created_at: issued_at.clone(),
            originating_agent_id: author.clone(),
            source_ids: vec![source_id],
            source_hash: source_hash.clone(),
            consent_ref: spec.consent_ref.to_owned(),
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
            source_sensor: spec.sensor.to_owned(),
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

/// Map a Hermes Agent archive into valid Cairn records and pending review plans.
///
/// Hermes stores builtin memory as markdown files with `§` entry delimiters.
/// This bridge treats each entry as one reviewable Cairn record and preserves
/// skills as playbook candidates.
pub fn map_hermes_agent_archive(opts: &KoiImportOptions) -> Result<KoiImportReport, ImportError> {
    if opts.batch_size == 0 {
        return Err(ImportError::InvalidBatchSize);
    }
    let source = hermes_archive_root(&opts.source);
    if !source.is_dir() {
        return Err(ImportError::SourceNotDirectory {
            archive: opts.source.clone(),
        });
    }

    let mut records = Vec::new();
    let mut items = Vec::new();
    for spec in hermes_builtin_specs(&source) {
        if !spec.path.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&spec.path).map_err(|source| ImportError::Io {
            path: spec.path.clone(),
            source,
        })?;
        let mapped = map_hermes_sections(&source, &spec.path, &raw, spec.kind, None)?;
        items.extend(mapped.items);
        records.extend(mapped.records);
    }

    let skills = source.join("skills");
    if skills.is_dir() {
        for entry in WalkDir::new(&skills)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if !has_importable_extension(path) {
                continue;
            }
            let raw = fs::read_to_string(path).map_err(|source| ImportError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let skill_id = hermes_skill_id(&skills, path);
            let mapped =
                map_hermes_sections(&source, path, &raw, MemoryKind::Playbook, Some(&skill_id))?;
            items.extend(mapped.items);
            records.extend(mapped.records);
        }
    }

    let plans = plan_records(
        "hermes-agent",
        HERMES_IMPORT_AUTHOR,
        &source,
        &records,
        opts.batch_size,
        opts.mode,
    )?;
    Ok(KoiImportReport {
        manifest: ExternalImportManifest {
            system: "hermes-agent".to_owned(),
            items,
            unsupported_fields: Vec::new(),
            privacy_sensitive_fields: Vec::new(),
        },
        records,
        ambiguities: Vec::new(),
        findings: Vec::new(),
        plans,
    })
}

struct HermesBuiltinSpec {
    path: PathBuf,
    kind: MemoryKind,
}

struct MappedHermesFile {
    records: Vec<MemoryRecord>,
    items: Vec<ExternalImportItem>,
}

fn hermes_archive_root(source: &Path) -> PathBuf {
    let nested = source.join(".hermes");
    if nested.is_dir() {
        nested
    } else {
        source.to_path_buf()
    }
}

fn hermes_builtin_specs(source: &Path) -> Vec<HermesBuiltinSpec> {
    vec![
        HermesBuiltinSpec {
            path: source.join("memories/MEMORY.md"),
            kind: MemoryKind::Reference,
        },
        HermesBuiltinSpec {
            path: source.join("memories/USER.md"),
            kind: MemoryKind::User,
        },
        HermesBuiltinSpec {
            path: source.join("SOUL.md"),
            kind: MemoryKind::Rule,
        },
    ]
}

fn map_hermes_sections(
    root: &Path,
    path: &Path,
    raw: &str,
    default_kind: MemoryKind,
    skill_id: Option<&str>,
) -> Result<MappedHermesFile, ImportError> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let sections = hermes_sections(raw);
    let mut records = Vec::new();
    let mut items = Vec::new();
    let mut skill_item = None;
    for (idx, body) in sections.into_iter().enumerate() {
        let kind = hermes_kind(default_kind, &body);
        let record = hermes_record(relative, idx, &body, kind, skill_id)?;
        let item = ExternalImportItem {
            kind: ExternalImportItemKind::Record,
            source_path: relative.to_path_buf(),
            legacy_id: Some(hermes_legacy_id(relative, idx)),
            session_ids: Vec::new(),
            skill_ids: skill_id.into_iter().map(ToOwned::to_owned).collect(),
            provenance: ExternalImportProvenance {
                source_sensor: HERMES_IMPORT_SENSOR.to_owned(),
                source_hash: record.provenance.source_hash.clone(),
                source_refs: record.provenance.source_refs.clone(),
            },
        };
        if let Some(skill_id) = skill_id {
            skill_item.get_or_insert_with(|| ExternalImportItem {
                kind: ExternalImportItemKind::Skill,
                source_path: relative.to_path_buf(),
                legacy_id: Some(skill_id.to_owned()),
                session_ids: Vec::new(),
                skill_ids: vec![skill_id.to_owned()],
                provenance: item.provenance.clone(),
            });
        }
        items.push(item);
        records.push(record);
    }
    if let Some(skill_item) = skill_item {
        items.push(skill_item);
    }
    Ok(MappedHermesFile { records, items })
}

fn hermes_sections(raw: &str) -> Vec<String> {
    raw.split('§')
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn hermes_kind(default_kind: MemoryKind, body: &str) -> MemoryKind {
    if default_kind == MemoryKind::Reference && body.to_ascii_lowercase().contains("trajectory") {
        MemoryKind::Trace
    } else {
        default_kind
    }
}

fn hermes_record(
    relative: &Path,
    section_index: usize,
    body: &str,
    kind: MemoryKind,
    skill_id: Option<&str>,
) -> Result<MemoryRecord, ImportError> {
    let body_hash = format!("{:x}", Sha256::digest(body.as_bytes()));
    let identity_seed = format!(
        "hermes-agent:{}:{section_index}:{body_hash}",
        slash_normalized_path(relative)
    );
    let source_id =
        SourceId::parse(deterministic_ulid(&identity_seed, "source").0).map_err(|source| {
            ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            }
        })?;
    let source_ref_id = source_id.as_str().to_owned();
    let source_hash = format!("{SOURCE_HASH_PREFIX}{body_hash}");
    let issued_at =
        Rfc3339Timestamp::parse("2026-01-01T00:00:00Z".to_owned()).map_err(|source| {
            ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            }
        })?;
    let author = Identity::parse(HERMES_IMPORT_AUTHOR).map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
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
        class: class_for_kind(kind),
        visibility: MemoryVisibility::Private,
        scope: hermes_scope(),
        body: body.to_owned(),
        source_ids: vec![source_id.clone()],
        provenance: hermes_provenance(
            relative,
            &issued_at,
            author.clone(),
            source_id,
            source_ref_id,
            source_hash,
        )?,
        updated_at: issued_at.clone(),
        evidence: EvidenceVector::default(),
        salience: if kind == MemoryKind::Playbook {
            0.7
        } else {
            0.5
        },
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
        tags: hermes_tags(skill_id),
        extra_frontmatter: hermes_extra_frontmatter(relative, section_index, body_hash, skill_id),
        consent_model: None,
    };
    record.validate().map_err(|source| ImportError::Domain {
        path: relative.to_path_buf(),
        source,
    })?;
    Ok(record)
}

fn hermes_scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("my-vault".to_owned()),
        entity: Some("ingest".to_owned()),
        user: Some(HERMES_IMPORT_AUTHOR.to_owned()),
        ..ScopeTuple::default()
    }
}

fn hermes_provenance(
    relative: &Path,
    issued_at: &Rfc3339Timestamp,
    author: Identity,
    source_id: SourceId,
    source_ref_id: String,
    source_hash: String,
) -> Result<Provenance, ImportError> {
    Ok(Provenance {
        source_sensor: Identity::parse(HERMES_IMPORT_SENSOR).map_err(|source| {
            ImportError::Domain {
                path: relative.to_path_buf(),
                source,
            }
        })?,
        created_at: issued_at.clone(),
        originating_agent_id: author,
        source_ids: vec![source_id],
        source_hash: source_hash.clone(),
        consent_ref: "consent:hermes-agent-import".to_owned(),
        llm_id_if_any: None,
        source_refs: vec![SourceRef {
            id: source_ref_id,
            hash: source_hash,
        }],
    })
}

fn hermes_extra_frontmatter(
    relative: &Path,
    section_index: usize,
    body_hash: String,
    skill_id: Option<&str>,
) -> BTreeMap<String, Value> {
    let mut extra_frontmatter = BTreeMap::new();
    extra_frontmatter.insert(
        "hermes_agent_source_path".to_owned(),
        Value::String(slash_normalized_path(relative)),
    );
    extra_frontmatter.insert(
        "hermes_agent_section_index".to_owned(),
        Value::Number(section_index.into()),
    );
    extra_frontmatter.insert(
        "hermes_agent_body_hash".to_owned(),
        Value::String(body_hash),
    );
    if let Some(skill_id) = skill_id {
        extra_frontmatter.insert(
            "hermes_agent_skill_id".to_owned(),
            Value::String(skill_id.to_owned()),
        );
    }
    extra_frontmatter
}

fn hermes_tags(skill_id: Option<&str>) -> Vec<String> {
    skill_id.map_or_else(
        || vec!["hermes-agent".to_owned()],
        |skill_id| vec!["hermes-agent".to_owned(), format!("skill:{skill_id}")],
    )
}

fn hermes_legacy_id(relative: &Path, section_index: usize) -> String {
    format!("{}#{section_index}", slash_normalized_path(relative))
}

fn hermes_skill_id(skills_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(skills_root).unwrap_or(path);
    let without_extension = relative.with_extension("");
    let skill_id = slash_normalized_path(&without_extension);
    let skill_id = skill_id.trim_matches('/').trim();
    if skill_id.is_empty() {
        "skill".to_owned()
    } else {
        skill_id.to_owned()
    }
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

fn tags_from_json(json: Option<&Value>) -> Vec<String> {
    json.and_then(|json| json.get("tags").and_then(Value::as_array))
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
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
            | "summary"
            | "compaction_summary"
            | "parts"
            | "created_at"
            | "updated_at"
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

fn kind_hint_for_path(spec: ImportSpec, relative: &Path) -> Option<MemoryKind> {
    if spec.system != "openclaw" {
        return None;
    }
    let relative = slash_normalized_path(relative);
    match relative.as_str() {
        "MEMORY.md" => Some(MemoryKind::User),
        "SOUL.md" => Some(MemoryKind::Rule),
        _ => relative
            .strip_prefix("memory/")
            .and_then(|path| path.rsplit('/').next())
            .and_then(|file_name| file_name.rsplit_once('.').map(|(stem, _)| stem))
            .and_then(|stem| MemoryKind::parse(stem).ok()),
    }
}

fn scope_from_json(json: Option<&Value>, spec: ImportSpec, default_workspace: &str) -> ScopeTuple {
    let scope = json.and_then(|json| json.get("scope"));
    ScopeTuple {
        tenant: scope_string(scope, "tenant").or_else(|| Some("default".to_owned())),
        workspace: scope_string(scope, "workspace").or_else(|| Some(default_workspace.to_owned())),
        project: None,
        session_id: scope_string(scope, "session_id"),
        entity: scope_string(scope, "entity").or_else(|| Some("ingest".to_owned())),
        user: scope
            .and_then(|scope| scope.get("user"))
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("hmn:"))
            .map(ToOwned::to_owned)
            .or_else(|| (spec.system == "koi-v1").then(|| KOI_IMPORT_AUTHOR.to_owned())),
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
    system: &str,
    author_identity: &str,
    source_root: &Path,
    records: &[MemoryRecord],
    batch_size: usize,
    mode: FlushMode,
) -> Result<Vec<FlushPlan>, ImportError> {
    plan_records_for_system(
        source_root,
        records,
        batch_size,
        mode,
        author_identity,
        system,
    )
}

fn plan_records_for_system(
    source_root: &Path,
    records: &[MemoryRecord],
    batch_size: usize,
    mode: FlushMode,
    author_id: &str,
    system: &str,
) -> Result<Vec<FlushPlan>, ImportError> {
    let mut plans = Vec::new();
    let author = Identity::parse(author_id).map_err(|source| ImportError::Domain {
        path: source_root.to_path_buf(),
        source,
    })?;
    for (idx, chunk) in records.chunks(batch_size).enumerate() {
        let issued_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        plans.push(FlushPlan {
            operation_id: deterministic_operation_id(source_root, system, idx, chunk),
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
    system: &str,
    batch_index: usize,
    records: &[MemoryRecord],
) -> Ulid {
    let mut hasher = Sha256::new();
    hasher.update(slash_normalized_path(source_root).as_bytes());
    hasher.update(b":");
    hasher.update(system.as_bytes());
    hasher.update(b":");
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
        ExternalImportFindingKind, ExternalImportItemKind, ImportSystem, KoiImportOptions,
        map_archive, map_koi_v1_archive, write_review_plans,
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
    fn koi_v1_import_uses_configured_default_workspace_when_scope_omits_it() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("memory.json"),
            r#"{"text":"Scoped by vault config.","kind":"reference"}"#,
        )
        .expect("write fixture");

        let report = map_archive(
            ImportSystem::KoiV1,
            &KoiImportOptions {
                source: archive.path().to_path_buf(),
                batch_size: 64,
                mode: FlushMode::HumanReview,
            },
            "research-vault",
        )
        .expect("map archive");

        assert_eq!(report.records.len(), 1);
        assert_eq!(
            report.records[0].scope.workspace.as_deref(),
            Some("research-vault")
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "fixture covers the importer mapping matrix"
    )]
    fn opencode_import_maps_instructions_summaries_and_session_parts() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("AGENTS.md"),
            "# Project Memory\n\nUse deterministic import review plans.",
        )
        .expect("write agents");
        fs::write(
            archive.path().join("CLAUDE.md"),
            "# Rules\n\nNever write directly to the memory store during migration.",
        )
        .expect("write claude");
        fs::write(archive.path().join("screenshot.bin"), [0, 159, 146, 150])
            .expect("write ignored binary");
        fs::write(
            archive.path().join("memory.json"),
            r#"{
              "id": "json-memory-01",
              "kind": "rule",
              "text": "OpenCode fallback JSON preserves typed metadata.",
              "tags": ["opencode", "fallback"],
              "skills": [{"id": "json-skill"}],
              "scope": {
                "tenant": "default",
                "workspace": "metadata",
                "entity": "import",
                "user": "hmn:tafeng"
              }
            }"#,
        )
        .expect("write fallback json");
        fs::create_dir_all(archive.path().join("sessions")).expect("sessions dir");
        fs::write(
            archive.path().join("sessions").join("session.json"),
            r#"{
              "id": "ses_01",
              "session_id": "ses_01",
              "created_at": "2026-04-01T10:00:00Z",
              "summary": {
                "Goal": "Build an OpenCode bridge.",
                "Constraints": ["Preserve ordering.", "No direct DB writes."],
                "Progress": "Mapped session parts into trace records.",
                "Decisions": ["Use reviewable FlushPlans."]
              },
              "parts": [
                {"id": "p1", "type": "user", "text": "Please import OpenCode memory."},
                {"id": "p2", "type": "tool", "tool": "skill", "text": "Loaded migration skill."},
                {"id": "p3", "type": "assistant", "text": "Drafted an import plan."}
              ],
              "unsupported_private_state": true,
              "api_token": "redacted-before-review"
            }"#,
        )
        .expect("write session");

        let report = super::map_opencode_archive(&super::OpenCodeImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 64,
            mode: FlushMode::HumanReview,
        })
        .expect("map opencode archive");

        assert_eq!(report.manifest.system, "opencode");
        assert_eq!(report.records.len(), 10);
        assert!(
            report
                .records
                .iter()
                .all(|record| record.validate().is_ok())
        );
        assert!(report.records.iter().any(|record| {
            record.kind == MemoryKind::Project && record.body.contains("Project Memory")
        }));
        assert!(report.records.iter().any(|record| {
            record.kind == MemoryKind::Rule && record.body.contains("Never write directly")
        }));
        let json_record = report
            .records
            .iter()
            .find(|record| record.body.contains("fallback JSON preserves"))
            .expect("fallback JSON record");
        assert_eq!(json_record.kind, MemoryKind::Rule);
        assert_eq!(json_record.scope.user.as_deref(), Some("hmn:tafeng"));
        assert!(json_record.tags.iter().any(|tag| tag == "fallback"));
        assert!(report.records.iter().any(|record| {
            record.kind == MemoryKind::User && record.body.contains("Build an OpenCode bridge")
        }));
        assert!(report.records.iter().any(|record| {
            record.kind == MemoryKind::StrategySuccess
                && record.body.contains("Use reviewable FlushPlans")
        }));
        let trace_orders = report
            .records
            .iter()
            .filter(|record| record.kind == MemoryKind::Trace)
            .map(|record| {
                record.extra_frontmatter["opencode_part_order"]
                    .as_u64()
                    .expect("part order")
            })
            .collect::<Vec<_>>();
        assert_eq!(trace_orders, vec![0, 1, 2]);
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Session
                && item.legacy_id.as_deref() == Some("ses_01")
        }));
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Skill && item.legacy_id.as_deref() == Some("skill")
        }));
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Record
                && item.legacy_id.as_deref() == Some("json-memory-01")
                && item.skill_ids == vec!["json-skill"]
        }));
        assert!(report.manifest.items.iter().any(|item| {
            item.kind == ExternalImportItemKind::Skill
                && item.legacy_id.as_deref() == Some("json-skill")
        }));
        assert!(
            report.findings.iter().any(|finding| {
                finding.kind == ExternalImportFindingKind::Unsupported
                    && finding.field == "unsupported_private_state"
            }),
            "unsupported OpenCode fields should be review findings: {:?}",
            report.findings
        );
        assert!(
            report.findings.iter().any(|finding| {
                finding.kind == ExternalImportFindingKind::PrivacySensitive
                    && finding.field == "api_token"
            }),
            "privacy fields should be review findings: {:?}",
            report.findings
        );
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|finding| finding.field == "api_token")
                .count(),
            1,
            "file-level findings should not be duplicated for every mapped session record"
        );
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.field != "summary" && finding.field != "parts"),
            "OpenCode summary and parts are handled fields, not unsupported findings: {:?}",
            report.findings
        );
        assert!(
            report
                .plans
                .iter()
                .all(|plan| plan.mode == FlushMode::HumanReview)
        );
    }

    #[test]
    fn opencode_cli_writes_pending_review_plans_and_source_artifacts() {
        let archive = tempfile::tempdir().expect("archive");
        fs::write(
            archive.path().join("AGENTS.md"),
            "Project purpose from OpenCode.",
        )
        .expect("write agents");
        let vault = tempfile::tempdir().expect("vault");

        let report = super::map_opencode_archive(&super::OpenCodeImportOptions {
            source: archive.path().to_path_buf(),
            batch_size: 1,
            mode: FlushMode::HumanReview,
        })
        .expect("map opencode archive");
        let written = write_review_plans(vault.path(), &report).expect("write plans");

        assert_eq!(written.len(), 1);
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
        assert!(matches!(
            persisted.plan.mutations.first(),
            Some(PlannedMutation::Upsert { .. })
        ));
    }
}
