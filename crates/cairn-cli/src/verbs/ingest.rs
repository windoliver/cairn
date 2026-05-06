//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Returns `Internal aborted` until the store is wired (issue #9).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{Response, ResponseData, ResponsePolicyTrace};
use cairn_core::generated::verbs::ingest::IngestData;
use cairn_core::pipeline::extraction_cache::{
    ExtractionCacheEntry, ExtractionResult, cache_entry_path, cache_key_for_bytes,
    relative_path_for_cache,
};
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, new_operation_id, unimplemented_response};

#[derive(Default)]
struct CacheStats {
    files_processed: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_writes: u64,
}

/// Run `cairn ingest`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // Enforce IDL exactly-one-of: body/file/url (positional `source` counts as one).
    let has_source = sub.get_one::<String>("source").is_some();
    let has_body = sub.get_one::<String>("body").is_some();
    let has_file = sub.get_one::<std::path::PathBuf>("file").is_some();
    let has_folder = sub.get_one::<std::path::PathBuf>("folder").is_some();
    let has_url = sub.get_one::<String>("url").is_some();
    let source_count = u8::from(has_source)
        + u8::from(has_body)
        + u8::from(has_file)
        + u8::from(has_folder)
        + u8::from(has_url);
    if source_count != 1 {
        eprintln!(
            "cairn ingest: exactly one of [source, --body, --file, --folder, --url] is required (got {source_count})"
        );
        return ExitCode::from(64);
    }

    if has_folder {
        return run_folder(sub, json);
    }

    if sub.get_flag("no_cache") {
        eprintln!("cairn ingest: --no-cache is only valid with --folder");
        return ExitCode::from(64);
    }

    // Resolve body: positional `source` wins if set; --body/--file/--url otherwise.
    let _body_resolved: Option<String> = if let Some(src) = sub.get_one::<String>("source") {
        if src == "-" {
            let mut buf = String::new();
            // Cap at 4 MiB to avoid unbounded allocation in the stubbed path.
            if std::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_string(&mut buf)
                .is_err()
            {
                let r = unimplemented_response(ResponseVerb::Ingest);
                if json {
                    emit_json(&r);
                } else {
                    human_error(
                        "ingest",
                        "Internal",
                        "failed to read stdin",
                        &r.operation_id,
                    );
                }
                return ExitCode::FAILURE;
            }
            Some(buf)
        } else {
            Some(src.clone())
        }
    } else {
        sub.get_one::<String>("body").cloned()
    };

    let resp = unimplemented_response(ResponseVerb::Ingest);
    if json {
        emit_json(&resp);
    } else {
        let op = resp.operation_id.clone();
        human_error(
            "ingest",
            "Internal",
            "store not wired in this P0 build",
            &op,
        );
    }
    ExitCode::FAILURE
}

fn run_folder(sub: &ArgMatches, json: bool) -> ExitCode {
    match run_folder_inner(sub) {
        Ok(resp) => {
            if json {
                emit_json(&resp);
            } else if let Some(ResponseData::Ingest(data)) = resp.data.as_ref() {
                println!(
                    "cairn ingest: processed {} file(s), cache hits {}, misses {}, writes {}",
                    data.files_processed.unwrap_or(0),
                    data.cache_hits.unwrap_or(0),
                    data.cache_misses.unwrap_or(0),
                    data.cache_writes.unwrap_or(0)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let message = e.to_string();
            let resp = internal_error_response(&message);
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "Internal", &message, &resp.operation_id);
            }
            ExitCode::FAILURE
        }
    }
}

fn run_folder_inner(sub: &ArgMatches) -> Result<Response, FolderIngestError> {
    let folder = sub
        .get_one::<PathBuf>("folder")
        .ok_or(FolderIngestError::MissingFolder)?;
    let vault_root = std::env::current_dir().map_err(FolderIngestError::CurrentDir)?;
    let folder_path = if folder.is_absolute() {
        folder.clone()
    } else {
        vault_root.join(folder)
    };

    let mut files = Vec::new();
    collect_files(&folder_path, &mut files)?;
    files.sort();

    let no_cache = sub.get_flag("no_cache");
    let mut stats = CacheStats::default();
    for file in files {
        process_folder_file(&vault_root, &file, no_cache, &mut stats)?;
    }

    let operation_id = new_operation_id();
    let session_id = sub
        .get_one::<String>("session_id")
        .cloned()
        .unwrap_or_else(|| "folder".to_owned());
    Ok(Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Ingest(IngestData {
            cache_hits: Some(stats.cache_hits),
            cache_misses: Some(stats.cache_misses),
            cache_writes: Some(stats.cache_writes),
            files_processed: Some(stats.files_processed),
            record_id: new_operation_id(),
            session_id,
        })),
        error: None,
        operation_id,
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: cairn_core::generated::envelope::ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Ingest,
    })
}

fn collect_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), FolderIngestError> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if !path.is_dir() {
        return Err(FolderIngestError::NotDirectory(path.to_path_buf()));
    }
    for entry in fs::read_dir(path).map_err(|source| FolderIngestError::ReadDir {
        path: path.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| FolderIngestError::ReadDir {
            path: path.to_path_buf(),
            source,
        })?;
        let child = entry.path();
        if child.file_name().is_some_and(|name| name == ".cairn") {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| FolderIngestError::ReadDir {
                path: path.to_path_buf(),
                source,
            })?;
        if file_type.is_dir() {
            collect_files(&child, out)?;
        } else if file_type.is_file() {
            out.push(child);
        }
    }
    Ok(())
}

fn process_folder_file(
    vault_root: &Path,
    file: &Path,
    no_cache: bool,
    stats: &mut CacheStats,
) -> Result<(), FolderIngestError> {
    stats.files_processed += 1;

    let body = fs::read(file).map_err(|source| FolderIngestError::ReadFile {
        path: file.to_path_buf(),
        source,
    })?;
    let key = cache_key_for_bytes(&body, file, vault_root).map_err(FolderIngestError::Key)?;
    let cache_path = cache_entry_path(vault_root, &key);

    if !no_cache && lookup_cache_entry(&cache_path, &key)?.is_some() {
        stats.cache_hits += 1;
        return Ok(());
    }

    stats.cache_misses += 1;
    let relative_path =
        relative_path_for_cache(file, vault_root).map_err(FolderIngestError::Key)?;
    let entry = ExtractionCacheEntry::new(
        key,
        relative_path,
        current_time_millis()?,
        ExtractionResult {
            nodes: Vec::new(),
            edges: Vec::new(),
        },
    );
    save_cache_entry(&cache_path, &entry)?;
    stats.cache_writes += 1;
    Ok(())
}

fn lookup_cache_entry(
    path: &Path,
    key: &str,
) -> Result<Option<ExtractionCacheEntry>, FolderIngestError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|source| FolderIngestError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let Ok(entry) = serde_json::from_slice::<ExtractionCacheEntry>(&bytes) else {
        return Ok(None);
    };
    Ok(entry.matches_key_and_schema(key).then_some(entry))
}

fn save_cache_entry(path: &Path, entry: &ExtractionCacheEntry) -> Result<(), FolderIngestError> {
    let parent = path
        .parent()
        .ok_or_else(|| FolderIngestError::MissingParent(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| FolderIngestError::CreateDir {
        path: parent.to_path_buf(),
        source,
    })?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FolderIngestError::MissingParent(path.to_path_buf()))?;
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", new_operation_id().0));
    let bytes = serde_json::to_vec_pretty(entry).map_err(FolderIngestError::SerializeCache)?;
    fs::write(&temp_path, bytes).map_err(|source| FolderIngestError::WriteFile {
        path: temp_path.clone(),
        source,
    })?;

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(_rename_error) if cfg!(windows) => {
            fs::copy(&temp_path, path).map_err(|source| FolderIngestError::WriteFile {
                path: path.to_path_buf(),
                source,
            })?;
            fs::remove_file(&temp_path).map_err(|source| FolderIngestError::WriteFile {
                path: temp_path,
                source,
            })?;
            Ok(())
        }
        Err(source) => {
            let _ = fs::remove_file(&temp_path);
            Err(FolderIngestError::WriteFile {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

fn current_time_millis() -> Result<u64, FolderIngestError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(FolderIngestError::Clock)?;
    u64::try_from(duration.as_millis()).map_err(|_| FolderIngestError::ClockOverflow)
}

fn internal_error_response(message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: cairn_core::generated::envelope::ResponseStatus::Aborted,
        target: None,
        verb: ResponseVerb::Ingest,
    }
}

#[derive(Debug)]
enum FolderIngestError {
    MissingFolder,
    MissingParent(PathBuf),
    NotDirectory(PathBuf),
    CurrentDir(std::io::Error),
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    Key(cairn_core::pipeline::extraction_cache::CacheKeyError),
    SerializeCache(serde_json::Error),
    Clock(std::time::SystemTimeError),
    ClockOverflow,
}

impl std::fmt::Display for FolderIngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFolder => f.write_str("--folder is required"),
            Self::MissingParent(path) => {
                write!(f, "cache path '{}' has no parent directory", path.display())
            }
            Self::NotDirectory(path) => write!(f, "'{}' is not a directory", path.display()),
            Self::CurrentDir(source) => write!(f, "failed to resolve current directory: {source}"),
            Self::ReadDir { path, source } => {
                write!(f, "failed to read directory '{}': {source}", path.display())
            }
            Self::ReadFile { path, source } => {
                write!(f, "failed to read file '{}': {source}", path.display())
            }
            Self::CreateDir { path, source } => {
                write!(
                    f,
                    "failed to create directory '{}': {source}",
                    path.display()
                )
            }
            Self::WriteFile { path, source } => {
                write!(f, "failed to write file '{}': {source}", path.display())
            }
            Self::Key(source) => write!(f, "failed to compute cache key: {source}"),
            Self::SerializeCache(source) => write!(f, "failed to serialize cache entry: {source}"),
            Self::Clock(source) => write!(f, "system clock is before Unix epoch: {source}"),
            Self::ClockOverflow => f.write_str("current timestamp does not fit in u64 millis"),
        }
    }
}

impl std::error::Error for FolderIngestError {}
