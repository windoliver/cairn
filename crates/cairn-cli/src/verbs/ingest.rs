//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Returns `Internal aborted` until the store is wired (issue #9).
//!
//! The `--resync <path>` flag re-ingests an out-of-band edited markdown
//! projection (brief §3.0, #43). The handler is fully implemented and
//! accepts `&dyn MemoryStore`; the real store is wired in #46.
//!
//! # Trust boundary (spec §3.5)
//!
//! `ingest` is an issuer-dependent verb: it will sign records on behalf of an
//! identity once the store is wired (#9).  The guard call below invokes
//! [`crate::identity::guard::refuse_if_degraded`] to enforce the
//! `VaultDegraded → EX_TEMPFAIL=75` contract even in the stub path.
//!
//! **Deferred**: full async wiring (calling [`open_for_signed_verb`] against the
//! resolved vault path) is deferred to issue #9 when this verb becomes async.
//! Until then the guard runs against a clean default report and always passes,
//! but the exit-code path is exercised by the unit tests in `guard.rs`.
//!
//! [`open_for_signed_verb`]: crate::identity::guard::open_for_signed_verb

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::projection::{
    ConflictOutcome, MarkdownProjector, PROJECTED_STANDARD_FIELDS, ResyncError,
};
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{Response, ResponseData, ResponsePolicyTrace};
use cairn_core::generated::verbs::ingest::IngestData;
use cairn_core::pipeline::extraction_cache::{
    ExtractionCacheEntry, ExtractionResult, cache_entry_path, cache_key_for_bytes,
    relative_path_for_cache,
};
use clap::ArgMatches;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{emit_json, human_error, new_operation_id, unimplemented_response};

#[derive(Default)]
struct CacheStats {
    files_processed: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_writes: u64,
}

/// Result of a successful `ingest --resync` operation.
#[must_use]
#[derive(Debug, serde::Serialize)]
pub struct ResyncResult {
    /// `"updated"` when the record was written; `"noop"` when the file
    /// was identical to the current store version.
    pub status: &'static str,
    /// Absolute path of the file that was resynced.
    pub path: std::path::PathBuf,
    /// Stable record identifier from the frontmatter `id` field.
    pub target_id: String,
    /// Version of the record as returned by the store after the upsert.
    pub version: u32,
}

/// Re-ingest a markdown projection file that has been edited out-of-band.
///
/// Steps:
/// 1. Read the file from `path`.
/// 2. Parse it with [`MarkdownProjector::parse`].
/// 3. Look up the current store record with [`MemoryStore::get`].
/// 4. Run [`MarkdownProjector::check_conflict`].
/// 5. On [`ConflictOutcome::Clean`]: upsert the updated record.
/// 6. On [`ConflictOutcome::Conflict`]: write a quarantine file to
///    `<vault_root>/.cairn/quarantine/<ts>-<id>.rejected` and return an error.
///
/// # Errors
///
/// Returns an error if the file cannot be read, fails to parse, the store
/// operation fails, or a conflict is detected.
#[allow(
    clippy::too_many_lines,
    reason = "CLI dispatcher: parse → load prior → conflict-check → upsert each step is linear and best read top-to-bottom"
)]
pub async fn resync_handler(
    store: &dyn MemoryStore,
    path: &Path,
    vault_root: &Path,
) -> anyhow::Result<ResyncResult> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("ingest --resync: {}", path.display()))?;

    let projector = MarkdownProjector;
    let parsed = projector.parse(&content).map_err(|e| match e {
        ResyncError::MissingId => {
            anyhow::anyhow!("ingest --resync: missing `id` field in {}", path.display())
        }
        ResyncError::ParseFailed(ref msg) => anyhow::anyhow!(
            "ingest --resync: parse error in {}: {msg}",
            path.display()
        ),
        // Conflict variant on parse should not occur; surface it defensively.
        ResyncError::Conflict { file_version, store_version, ref reason } => anyhow::anyhow!(
            "ingest --resync: unexpected conflict during parse (file={file_version}, store={store_version}): {reason}"
        ),
        _ => anyhow::anyhow!("ingest --resync: {e:?}"),
    })?;

    // Guard against ID misdirection: the filename encodes the record id (last `_`-delimited
    // segment of the stem). If the id in the frontmatter doesn't match the filename, the file
    // has been tampered with or mis-addressed and must not be applied.
    // Validate that the frontmatter id matches the id embedded in the filename.
    // project() encodes the record id as the last `_`-delimited segment of the stem.
    // Require the projected filename format `<kind>_<id>.md` and verify the id matches
    // frontmatter. Fail closed if the name doesn't match — arbitrary-path files are not
    // safe to resync without this check.
    let path_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|stem| stem.rsplit_once('_').map(|(_, id)| id.to_owned()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ingest --resync: filename does not match expected `<kind>_<id>.md` format: {}",
                path.display()
            )
        })?;
    if path_id != parsed.target_id.as_str() {
        anyhow::bail!(
            "ingest --resync: frontmatter id `{}` does not match filename id `{path_id}` in {}",
            parsed.target_id,
            path.display()
        );
    }

    let target = cairn_core::domain::TargetId::parse(parsed.target_id.clone())
        .with_context(|| format!("ingest --resync: invalid target_id `{}`", parsed.target_id))?;
    let current = store
        .get_active_by_target(&target)
        .await
        .map_err(anyhow::Error::msg)
        .context("store: get_active_by_target")?;

    let outcome = projector.check_conflict(&parsed, current.as_ref());

    // Separate standard projected fields from user-editable extras using the
    // same constant that project() uses, so the two sides always agree.
    let extra_frontmatter: std::collections::BTreeMap<String, serde_json::Value> = parsed
        .raw_frontmatter
        .iter()
        .filter(|(k, _)| !PROJECTED_STANDARD_FIELDS.contains(&k.as_str()))
        .filter_map(|(k, v)| serde_json::to_value(v).ok().map(|jv| (k.clone(), jv)))
        .collect();

    match outcome {
        ConflictOutcome::Clean => {
            if let Some(ref stored) = current {
                // Check if mutable fields are already up to date
                if stored.record.body == parsed.body
                    && stored.record.tags == parsed.tags
                    && stored.record.extra_frontmatter == extra_frontmatter
                {
                    return Ok(ResyncResult {
                        status: "noop",
                        path: path.to_path_buf(),
                        target_id: parsed.target_id,
                        version: stored.version,
                    });
                }
                // Merge mutable fields and upsert
                let mut r = stored.record.clone();
                r.body = parsed.body.clone();
                r.tags = parsed.tags.clone();
                r.extra_frontmatter = extra_frontmatter;
                let outcome = store
                    .upsert(&r)
                    .await
                    .map_err(anyhow::Error::msg)
                    .context("store: upsert")?;
                Ok(ResyncResult {
                    status: "updated",
                    path: path.to_path_buf(),
                    target_id: parsed.target_id,
                    version: outcome.version,
                })
            } else {
                // New record — build_record_from_parsed (deferred to #46)
                let record = build_record_from_parsed(&parsed)?;
                let outcome = store
                    .upsert(&record)
                    .await
                    .map_err(anyhow::Error::msg)
                    .context("store: upsert")?;
                Ok(ResyncResult {
                    status: "updated",
                    path: path.to_path_buf(),
                    target_id: parsed.target_id,
                    version: outcome.version,
                })
            }
        }
        ConflictOutcome::Conflict {
            ref marker,
            file_version,
            store_version,
        } => {
            let quarantine_dir = vault_root.join(".cairn/quarantine");
            tokio::fs::create_dir_all(&quarantine_dir)
                .await
                .with_context(|| format!("create quarantine dir {}", quarantine_dir.display()))?;
            write_quarantine(&quarantine_dir, &parsed.target_id, &content).await?;
            Err(anyhow::anyhow!(
                "conflict: file version {file_version}, store version {store_version}; {marker}; \
                 rejected content saved to .cairn/quarantine/"
            ))
        }
        // ConflictOutcome is #[non_exhaustive]; catch future variants.
        _ => Err(anyhow::anyhow!(
            "ingest --resync: unexpected conflict outcome"
        )),
    }
}

/// Write `content` to `.cairn/quarantine/<nanos>-<target_id>.rejected` using
/// create-new semantics so concurrent conflicts for the same record id never
/// overwrite each other's preserved content.
async fn write_quarantine(
    quarantine_dir: &Path,
    target_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut q_path = quarantine_dir.join(format!("{nanos}-{target_id}.rejected"));
    let mut retry: u32 = 0;
    loop {
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&q_path)
            .await
        {
            Ok(mut f) => {
                use tokio::io::AsyncWriteExt as _;
                f.write_all(content.as_bytes())
                    .await
                    .with_context(|| format!("write quarantine {}", q_path.display()))?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                retry += 1;
                q_path = quarantine_dir.join(format!("{nanos}-{target_id}-{retry}.rejected"));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "write quarantine {}: {e}",
                    q_path.display()
                ));
            }
        }
    }
}

/// Construct a minimal [`cairn_core::domain::record::MemoryRecord`] from a
/// [`cairn_core::domain::projection::ParsedProjection`] for the "new record"
/// branch of the resync path.
///
/// The "new record" path is a TODO(#46) stub — the real pipeline (WAL,
/// consent journal, signing) is not wired yet. Returns an error directing
/// the caller to use `cairn ingest` for brand-new records.
fn build_record_from_parsed(
    _parsed: &cairn_core::domain::projection::ParsedProjection,
) -> anyhow::Result<cairn_core::domain::record::MemoryRecord> {
    Err(anyhow::anyhow!(
        "ingest --resync: creating a brand-new record via resync requires the full ingest \
         pipeline (TODO #46); please run `cairn ingest` first to create the record, \
         then use --resync to re-ingest edits"
    ))
}

/// Run `cairn ingest`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // --resync <path>: re-ingest an out-of-band edited markdown projection.
    if let Some(resync_path) = sub.get_one::<std::path::PathBuf>("resync") {
        // TODO(#46): wire vault_root from resolved vault config.
        // For now: use CWD as vault_root placeholder.
        let resp = unimplemented_response(ResponseVerb::Ingest);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "ingest",
                "Internal",
                &format!(
                    "store not wired in this P0 build — --resync {} requires #46",
                    resync_path.display()
                ),
                &resp.operation_id,
            );
        }
        return ExitCode::FAILURE;
    }

    // --dry-run / --human-review: build a stub FlushPlan and either print
    // or persist it. Returns before the source-count validation so these
    // flags can be combined with a normal --kind/--body without tripping the
    // guard; the validation still runs for the non-flush path below.
    let dry_run = sub.get_flag("dry-run");
    let human_review = sub.get_flag("human-review");
    let no_diff = sub.get_flag("no-diff");
    if dry_run || human_review {
        let mode = if dry_run {
            cairn_core::domain::flush_plan::FlushMode::DryRun
        } else {
            cairn_core::domain::flush_plan::FlushMode::HumanReview
        };
        return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
    }

    // Enforce IDL exactly-one-of: body/file/folder/url (positional `source` counts as one).
    let source_count = ingest_source_count(sub);
    if source_count != 1 {
        eprintln!(
            "cairn ingest: exactly one of [source, --body, --file, --folder, --url] is required (got {source_count})"
        );
        return ExitCode::from(64);
    }

    if let Some(folder) = sub.get_one::<PathBuf>("folder") {
        return run_folder(sub, json, folder);
    }

    if let Some(folder) = positional_folder_source(sub) {
        return run_folder(sub, json, &folder);
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

    // §3.5 trust-boundary guard: refuse if the vault is degraded.
    // In this P0 stub the report is always clean (no store is open); full async
    // wiring against the resolved vault path is deferred to issue #9.
    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn ingest: VaultDegraded — {e}");
        return ExitCode::from(75); // EX_TEMPFAIL
    }

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

fn ingest_source_count(sub: &ArgMatches) -> u8 {
    u8::from(sub.get_one::<String>("source").is_some())
        + u8::from(sub.get_one::<String>("body").is_some())
        + u8::from(sub.get_one::<PathBuf>("file").is_some())
        + u8::from(sub.get_one::<PathBuf>("folder").is_some())
        + u8::from(sub.get_one::<String>("url").is_some())
}

fn positional_folder_source(sub: &ArgMatches) -> Option<PathBuf> {
    let source = sub.get_one::<String>("source")?;
    if source == "-" {
        return None;
    }
    let path = PathBuf::from(source);
    path.is_dir().then_some(path)
}

fn run_folder(sub: &ArgMatches, json: bool, folder: &Path) -> ExitCode {
    match run_folder_inner(sub, folder) {
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

fn run_folder_inner(sub: &ArgMatches, folder: &Path) -> Result<Response, FolderIngestError> {
    let vault_root = std::env::current_dir().map_err(FolderIngestError::CurrentDir)?;
    let folder_path = if folder.is_absolute() {
        folder.to_path_buf()
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
            plan_ref: None,
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
    let result = extraction_result_for_source_file(&key, &relative_path, body.len());
    let entry = ExtractionCacheEntry::new(key, relative_path, current_time_millis()?, result);
    save_cache_entry(&cache_path, &entry)?;
    stats.cache_writes += 1;
    Ok(())
}

fn extraction_result_for_source_file(
    key: &str,
    relative_path: &str,
    content_length_bytes: usize,
) -> ExtractionResult {
    ExtractionResult {
        nodes: vec![serde_json::json!({
            "id": format!("source:{key}"),
            "kind": "source_document",
            "source_path": relative_path,
            "content_length_bytes": content_length_bytes,
        })],
        edges: Vec::new(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::projection::MarkdownProjector;
    use cairn_test_fixtures::store::{FixtureStore, sample_stored_record};

    #[tokio::test]
    async fn resync_clean_upserts_record() {
        let store = FixtureStore::default();
        // Pre-populate store with version 1.
        let stored = sample_stored_record(1);
        store.upsert(&stored.record).await.unwrap();

        // Project to markdown, then modify the body so the resync is a real
        // edit (not a noop).  Version still matches → Clean → upsert.
        let proj = MarkdownProjector;
        let file = proj.project(&stored);
        // Append " edited" to the body so body != stored body → triggers upsert.
        let modified_content = file.content.replace(
            &stored.record.body,
            &format!("{} edited", stored.record.body),
        );
        let vault_root = tempfile::tempdir().unwrap();
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &modified_content)
            .await
            .unwrap();

        let result = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result.target_id, stored.record.id.as_str());
        // Store started at version 1 (one upsert above); resync does another
        // upsert → version 2.
        assert_eq!(result.version, 2);
        assert_eq!(result.status, "updated");
    }

    #[tokio::test]
    async fn resync_noop_when_content_unchanged() {
        let store = FixtureStore::default();
        // Pre-populate store with version 1.
        let stored = sample_stored_record(1);
        store.upsert(&stored.record).await.unwrap();

        // Project to markdown and resync it — body/tags are identical → noop.
        let proj = MarkdownProjector;
        let file = proj.project(&stored);
        let vault_root = tempfile::tempdir().unwrap();
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &file.content).await.unwrap();

        let result = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result.status, "noop");
        // Version should be unchanged at 1 — no upsert was performed.
        assert_eq!(result.version, 1);
        assert_eq!(result.target_id, stored.record.id.as_str());
    }

    #[tokio::test]
    async fn resync_conflict_writes_quarantine_file() {
        let store = FixtureStore::default();
        // Store a record and then upsert mutated bodies to advance the
        // version to 5, while keeping a v1 projected file. Upsert is
        // body-hash idempotent, so each call must mutate `body` to bump
        // the version.
        let base = sample_stored_record(1);
        for v in 1..=5_u32 {
            let mut r = base.record.clone();
            r.body = format!("{} v{v}", base.record.body);
            store.upsert(&r).await.unwrap();
        }

        // Write a file that claims to be at version 1 (stale).
        let proj = MarkdownProjector;
        let v1_stored = sample_stored_record(1);
        let file = proj.project(&v1_stored);
        let vault_root = tempfile::tempdir().unwrap();
        let quarantine_dir = vault_root.path().join(".cairn/quarantine");
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &file.content).await.unwrap();

        let err = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("conflict"),
            "error should mention conflict: {err}"
        );

        // Quarantine file should exist.
        let entries: Vec<_> = std::fs::read_dir(&quarantine_dir)
            .expect("quarantine dir should exist after conflict")
            .filter_map(std::result::Result::ok)
            .collect();
        assert!(
            !entries.is_empty(),
            "quarantine file should have been written"
        );
    }
}
