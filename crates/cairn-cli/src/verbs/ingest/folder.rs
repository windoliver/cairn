use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::ArgMatches;

use super::cache::{CacheEntry, body_for_cache, cache_key, read_cache_entry, write_cache_entry};
use super::extract::extract_keyword_counts;
use super::patterns::{GlobPattern, PatternError, parse_pattern_list};
use super::report::{FolderIngestSummary, render_human};
use super::scanner::scan_folder;
use crate::verbs::envelope::emit_json;

const DEFAULT_INCLUDE: &[&str] = &["*.md", "*.txt", "*.rs", "*.py", "*.ts", "*.js", "*.go"];
const DEFAULT_EXCLUDE: &[&str] = &[".git", "node_modules", "target"];

#[derive(Debug, Clone)]
pub struct FolderIngestOptions {
    pub folder: PathBuf,
    pub recursive: bool,
    pub include: Vec<GlobPattern>,
    pub exclude: Vec<GlobPattern>,
    pub mode: IngestMode,
    pub dry_run: bool,
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

pub fn run(sub: &ArgMatches, json: bool) -> ExitCode {
    match normalize_options(sub).and_then(|options| run_with_options(&options)) {
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

fn normalize_options(sub: &ArgMatches) -> Result<FolderIngestOptions, FolderIngestError> {
    let folder = sub
        .get_one::<PathBuf>("folder")
        .cloned()
        .ok_or_else(|| FolderIngestError::Usage("--folder is required".to_owned()))?;
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

    Ok(FolderIngestOptions {
        folder,
        recursive: true,
        include,
        exclude,
        mode,
        dry_run: sub.get_flag("dry_run"),
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
    let scan = scan_folder(
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
    })?;
    let cache_root = Path::new(".cairn").join("cache");
    let mut summary = FolderIngestSummary {
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
        elapsed_ms: 0,
        dry_run: options.dry_run,
        mode: "keyword".to_owned(),
    };

    for entry in scan.entries {
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
        if read_cache_entry(&cache_root, &key)
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
        summary.processed = summary.processed.saturating_add(1);
        summary.entities_new = summary.entities_new.saturating_add(counts.entities_new);
        summary.edges_new = summary.edges_new.saturating_add(counts.edges_new);

        if !options.dry_run {
            let cache_entry = CacheEntry {
                version: 1,
                relative_path: normalize_path(&entry.relative_path),
                cache_key: key,
                entities_new: counts.entities_new,
                edges_new: counts.edges_new,
            };
            write_cache_entry(&cache_root, &cache_entry).map_err(|err| {
                FolderIngestError::Io(format!(
                    "failed to write folder ingest cache for {}: {err}",
                    entry.relative_path.display()
                ))
            })?;
        }
    }

    summary.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(summary)
}

fn is_supported_keyword_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("md" | "txt" | "rst" | "rs" | "py" | "ts" | "js" | "go")
    )
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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
