use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use cairn_core::domain::FlushMode;
use clap::ArgMatches;
use sha2::{Digest, Sha256};

use super::apply::{ApplyStats, apply_batch};
use super::cache::{CacheEntry, body_for_cache, cache_key, read_cache_entry, write_cache_entry};
use super::extract::extract_keyword_counts;
use super::patterns::{GlobPattern, PatternError, parse_pattern_list};
use super::planner::{FolderPlanBatch, PlannedFile, plan_batches};
use super::report::{FolderIngestSummary, render_human};
use super::scanner::{ScanEntry, ScanResult, scan_folder};
use crate::verbs::envelope::emit_json;

const DEFAULT_INCLUDE: &[&str] = &[
    "*.md", "*.txt", "*.rst", "*.rs", "*.py", "*.ts", "*.js", "*.go", "*.java",
];
const DEFAULT_EXCLUDE: &[&str] = &[".git", "node_modules", "target"];

#[derive(Debug, Clone)]
pub struct FolderIngestOptions {
    pub folder: PathBuf,
    pub vault_root: PathBuf,
    pub recursive: bool,
    pub include: Vec<GlobPattern>,
    pub exclude: Vec<GlobPattern>,
    pub mode: IngestMode,
    pub dry_run: bool,
    pub batch_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    Keyword,
    Semantic,
    Full,
}

#[derive(Debug)]
enum FolderIngestError {
    Usage(String),
    CapabilityUnavailable(String),
    Io(String),
}

pub fn run(sub: &ArgMatches, vault_root: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    match normalize_options(sub, vault_root).and_then(|options| run_with_options(&options)) {
        Ok(summary) => {
            if json {
                emit_json(&summary);
            } else {
                let folder = sub
                    .get_one::<PathBuf>("folder")
                    .map_or_else(String::new, |path| path.display().to_string());
                print!("{}", render_human(&folder, &summary));
            }
            ExitCode::SUCCESS
        }
        Err(FolderIngestError::Usage(message)) => {
            eprintln!("cairn ingest: {message}");
            ExitCode::from(64)
        }
        Err(FolderIngestError::CapabilityUnavailable(message)) => {
            if json {
                emit_json(&serde_json::json!({
                    "status": "aborted",
                    "error": {
                        "code": "CapabilityUnavailable",
                        "message": message,
                    },
                }));
            } else {
                eprintln!("cairn ingest: CapabilityUnavailable — {message}");
            }
            ExitCode::from(78)
        }
        Err(FolderIngestError::Io(message)) => {
            eprintln!("cairn ingest: {message}");
            ExitCode::FAILURE
        }
    }
}

fn normalize_options(
    sub: &ArgMatches,
    vault_root: PathBuf,
) -> Result<FolderIngestOptions, FolderIngestError> {
    let folder = sub
        .get_one::<PathBuf>("folder")
        .cloned()
        .ok_or_else(|| FolderIngestError::Usage("--folder is required".to_owned()))?;
    let has_body = sub.get_one::<String>("body").is_some();
    let has_file = sub.get_one::<PathBuf>("file").is_some();
    let has_url = sub.get_one::<String>("url").is_some();
    let has_source = sub.get_one::<String>("source").is_some();
    let source_conflicts =
        u8::from(has_body) + u8::from(has_file) + u8::from(has_url) + u8::from(has_source);
    if source_conflicts != 0 {
        return Err(FolderIngestError::Usage(
            "--folder is mutually exclusive with [source, --body, --file, --url]".to_owned(),
        ));
    }
    if sub.get_flag("human-review") {
        return Err(FolderIngestError::Usage(
            "--human-review is not supported with --folder".to_owned(),
        ));
    }
    if !folder.is_dir() {
        return Err(FolderIngestError::Usage(format!(
            "folder does not exist or is not a directory: {}",
            folder.display()
        )));
    }

    let include = parse_pattern_list(
        sub.get_many::<String>("include")
            .map(|values| values.cloned().collect()),
        DEFAULT_INCLUDE,
    )?;
    let exclude = parse_pattern_list(
        sub.get_many::<String>("exclude")
            .map(|values| values.cloned().collect()),
        DEFAULT_EXCLUDE,
    )?;
    let mode = match sub
        .get_one::<String>("mode")
        .map_or("keyword", String::as_str)
    {
        "keyword" => IngestMode::Keyword,
        "semantic" => IngestMode::Semantic,
        "full" => IngestMode::Full,
        other => {
            return Err(FolderIngestError::Usage(format!(
                "unknown folder ingest mode: {other}"
            )));
        }
    };
    let batch_size = sub.get_one::<u32>("batch_size").copied().unwrap_or(64);
    if batch_size == 0 {
        return Err(FolderIngestError::Usage(
            "--batch-size must be greater than zero".to_owned(),
        ));
    }

    Ok(FolderIngestOptions {
        folder,
        vault_root,
        recursive: true,
        include,
        exclude,
        mode,
        dry_run: sub.get_flag("dry-run"),
        batch_size,
    })
}

fn run_with_options(
    options: &FolderIngestOptions,
) -> Result<FolderIngestSummary, FolderIngestError> {
    if options.mode != IngestMode::Keyword {
        return Err(FolderIngestError::CapabilityUnavailable(
            "folder ingest currently supports keyword mode only".to_owned(),
        ));
    }

    let started = Instant::now();
    let scan = scan_options(options)?;
    let cache_root = options.vault_root.join(".cairn/cache");
    let mut summary = summary_for_scan(options, &scan);
    let planned_files = collect_planned_files(scan.entries, &cache_root, &mut summary)?;
    let batches = build_batches(options, planned_files)?;
    summary.plans = batches.len() as u64;
    summary.operation_ids = batches
        .iter()
        .map(|batch| batch.plan.operation_id.0.clone())
        .collect();

    if !options.dry_run {
        let apply_stats = apply_batches(options, &cache_root, &batches)?;
        summary.records_written = apply_stats.records_written;
    }
    summary.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(summary)
}

fn scan_options(options: &FolderIngestOptions) -> Result<ScanResult, FolderIngestError> {
    scan_folder(
        &options.folder,
        options.recursive,
        &options.include,
        &options.exclude,
    )
    .map_err(|err| {
        FolderIngestError::Io(format!(
            "failed to scan folder {}: {err}",
            options.folder.display()
        ))
    })
}

fn summary_for_scan(options: &FolderIngestOptions, scan: &ScanResult) -> FolderIngestSummary {
    FolderIngestSummary {
        scanned: scan.entries.len() as u64,
        cached: 0,
        processed: 0,
        skipped: scan.skipped,
        warnings: scan.warnings.broken_symlinks,
        entities_new: 0,
        entities_merged: 0,
        edges_new: 0,
        contradictions_resolved: 0,
        records_written: 0,
        plans: 0,
        batch_size: options.batch_size,
        operation_ids: Vec::new(),
        elapsed_ms: 0,
        dry_run: options.dry_run,
        mode: "keyword".to_owned(),
    }
}

fn collect_planned_files(
    entries: Vec<ScanEntry>,
    cache_root: &Path,
    summary: &mut FolderIngestSummary,
) -> Result<Vec<PlannedFile>, FolderIngestError> {
    let mut planned_files = Vec::new();
    for entry in entries {
        if !is_supported_keyword_file(&entry.relative_path) {
            summary.warnings = summary.warnings.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            continue;
        }
        let body = match std::fs::read_to_string(&entry.absolute_path) {
            Ok(body) => body,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                summary.warnings = summary.warnings.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                continue;
            }
            Err(err) => {
                return Err(FolderIngestError::Io(format!(
                    "failed to read {}: {err}",
                    entry.absolute_path.display()
                )));
            }
        };
        let body_for_hash = body_for_cache(&entry.relative_path, &body);
        let key = cache_key(&entry.relative_path, &body_for_hash);
        if read_cache_entry(cache_root, &key)
            .map_err(|err| {
                FolderIngestError::Io(format!(
                    "failed to read folder ingest cache for {}: {err}",
                    entry.relative_path.display()
                ))
            })?
            .is_some()
        {
            summary.cached = summary.cached.saturating_add(1);
            continue;
        }

        let counts = extract_keyword_counts(&entry.relative_path, &body);
        let body_hash = body_sha256(&body_for_hash);
        let (entities, wiki_edges) = extract_keyword_graph(&entry.relative_path, &body);
        summary.processed = summary.processed.saturating_add(1);
        summary.entities_new = summary.entities_new.saturating_add(counts.entities_new);
        summary.edges_new = summary.edges_new.saturating_add(counts.edges_new);
        planned_files.push(PlannedFile {
            absolute_path: entry.absolute_path,
            relative_path: entry.relative_path,
            body,
            body_hash,
            cache_key: key,
            counts,
            entities,
            wiki_edges,
        });
    }
    Ok(planned_files)
}

fn build_batches(
    options: &FolderIngestOptions,
    planned_files: Vec<PlannedFile>,
) -> Result<Vec<FolderPlanBatch>, FolderIngestError> {
    let flush_mode = if options.dry_run {
        FlushMode::DryRun
    } else {
        FlushMode::Autonomous
    };
    plan_batches(
        &options.folder,
        planned_files,
        options.batch_size as usize,
        flush_mode,
    )
    .map_err(|err| FolderIngestError::Io(format!("failed to plan folder ingest: {err}")))
}

fn apply_batches(
    options: &FolderIngestOptions,
    cache_root: &Path,
    batches: &[FolderPlanBatch],
) -> Result<ApplyStats, FolderIngestError> {
    if batches.is_empty() {
        return Ok(ApplyStats::default());
    }

    let db_path = options.vault_root.join(".cairn/cairn.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            FolderIngestError::Io(format!("failed to start folder ingest runtime: {err}"))
        })?;
    let store = runtime
        .block_on(cairn_store_sqlite::open(&db_path))
        .map_err(|err| {
            FolderIngestError::Io(format!("failed to open store {}: {err}", db_path.display()))
        })?;

    let mut totals = ApplyStats::default();
    for batch in batches {
        let stats = runtime
            .block_on(apply_batch(&store, batch))
            .map_err(|err| {
                FolderIngestError::Io(format!(
                    "failed to apply folder ingest batch {}: {err}",
                    batch.plan.operation_id.0
                ))
            })?;
        totals.records_written = totals.records_written.saturating_add(stats.records_written);
        totals.entities_written = totals
            .entities_written
            .saturating_add(stats.entities_written);
        totals.edges_written = totals.edges_written.saturating_add(stats.edges_written);
        write_batch_cache_entries(cache_root, batch)?;
    }

    Ok(totals)
}

fn write_batch_cache_entries(
    cache_root: &Path,
    batch: &FolderPlanBatch,
) -> Result<(), FolderIngestError> {
    for file in &batch.files {
        let cache_entry = CacheEntry {
            version: 1,
            relative_path: normalize_path(&file.relative_path),
            cache_key: file.cache_key.clone(),
            entities_new: file.counts.entities_new,
            edges_new: file.counts.edges_new,
        };
        write_cache_entry(cache_root, &cache_entry).map_err(|err| {
            FolderIngestError::Io(format!(
                "failed to write folder ingest cache for {}: {err}",
                file.relative_path.display()
            ))
        })?;
    }
    Ok(())
}

fn is_supported_keyword_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("md" | "txt" | "rst" | "rs" | "py" | "ts" | "js" | "go" | "java")
    )
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn body_sha256(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

fn extract_keyword_graph(relative_path: &Path, body: &str) -> (Vec<String>, Vec<(String, String)>) {
    let source = relative_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| normalize_path(relative_path), str::to_owned);
    let targets = extract_wiki_targets(body);
    let mut entity_set = BTreeSet::new();
    let mut edges = Vec::new();
    if !targets.is_empty() {
        entity_set.insert(source.clone());
    }
    for target in targets {
        if target.is_empty() {
            continue;
        }
        entity_set.insert(target.clone());
        edges.push((source.clone(), target));
    }

    (entity_set.into_iter().collect(), edges)
}

fn extract_wiki_targets(body: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("]]") else {
            break;
        };
        let raw = &after_start[..end];
        let target = raw
            .split('|')
            .next()
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default()
            .trim();
        if !target.is_empty() {
            targets.push(target.to_owned());
        }
        rest = &after_start[end + 2..];
    }
    targets
}

impl From<PatternError> for FolderIngestError {
    fn from(value: PatternError) -> Self {
        Self::Usage(value.to_string())
    }
}

impl std::fmt::Display for FolderIngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(message) | Self::CapabilityUnavailable(message) | Self::Io(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for FolderIngestError {}
