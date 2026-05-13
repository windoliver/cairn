//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Body/file/stdin/url ingest now runs the local filter/classify path and
//! writes accepted records through the configured `MemoryStore`.
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

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::projection::{
    ConflictOutcome, MarkdownProjector, PROJECTED_STANDARD_FIELDS, ResyncError,
};
use cairn_core::domain::record::{Ed25519Signature, RecordId};
use cairn_core::domain::{
    ActorChainEntry, CaptureMode, ChainRole, EvidenceVector, Identity, IdentityKind, MemoryClass,
    MemoryKind, MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple,
    SourceFamily, SourceId, TargetId,
};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus,
};
use cairn_core::generated::verbs::ingest::IngestData;
use cairn_core::pipeline::extraction_cache::{
    ExtractionCacheEntry, ExtractionResult, cache_entry_path, cache_key_for_bytes,
    relative_path_for_cache,
};
use cairn_core::pipeline::filter::{
    Decision, FilterInputs, VisibilityPolicy, default_visibility, fence, redact, should_memorize,
};
use cairn_core::policy_trace::{
    PolicyDetail, PolicyErrorCode, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire,
};
use clap::ArgMatches;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{
    emit_json, human_error, invalid_args_response, new_operation_id, not_found_response,
};
use super::status;

const CLI_AUTHOR_ID: &str = "agt:cairn-cli:p0:v1";
const CLI_SENSOR_ID: &str = "snr:local:cli:p0:v1";
const STDIN_LIMIT_BYTES: u64 = 4 * 1024 * 1024;
const SESSION_LOCK_TENANT: &str = "default";
const SESSION_LOCK_WORKSPACE: &str = "default";
const SESSION_LOCK_TTL: Duration = Duration::from_secs(30);

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
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = sub.get_flag("json");

    // --resync <path>: re-ingest an out-of-band edited markdown projection.
    if let Some(resync_path) = sub.get_one::<std::path::PathBuf>("resync") {
        return run_resync(sub, json, resync_path, vault_root);
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

    let kind = match parse_kind(sub, json) {
        Ok(kind) => kind,
        Err(code) => return code,
    };

    if let Some(folder) = sub.get_one::<PathBuf>("folder") {
        return run_folder(sub, json, folder, vault_root);
    }

    if let Some(folder) = positional_folder_source(sub) {
        return run_folder(sub, json, &folder, vault_root);
    }

    if sub.get_flag("no_cache") {
        eprintln!("cairn ingest: --no-cache is only valid with --folder");
        return ExitCode::from(64);
    }

    // Resolve body: positional `source` wins if set; --body/--file/--url otherwise.
    let resolved = match resolve_body(sub) {
        Ok(resolved) => resolved,
        Err(reason) => {
            let resp = invalid_args_response(ResponseVerb::Ingest, "source", &reason);
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "InvalidArgs", &reason, &resp.operation_id);
            }
            return ExitCode::from(64);
        }
    };
    if resolved.body.trim().is_empty() {
        let resp = invalid_args_response(ResponseVerb::Ingest, "body", "must not be empty");
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "ingest",
                "InvalidArgs",
                "body: must not be empty",
                &resp.operation_id,
            );
        }
        return ExitCode::from(64);
    }

    // §3.5 trust-boundary guard: refuse if the vault is degraded.
    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn ingest: VaultDegraded — {e}");
        return ExitCode::from(75); // EX_TEMPFAIL
    }

    if let Some(exit) = require_bound_vault(json, vault_root) {
        return exit;
    }

    run_body_ingest(sub, json, vault_root, kind, resolved)
}

struct ResolvedBody {
    body: String,
}

fn parse_kind(sub: &ArgMatches, json: bool) -> Result<MemoryKind, ExitCode> {
    let kind_raw = sub
        .get_one::<String>("kind")
        .map(String::as_str)
        .unwrap_or_default();
    match MemoryKind::parse(kind_raw) {
        Ok(kind) => Ok(kind),
        Err(e) => {
            let reason = e.to_string();
            let mut resp = invalid_args_response(ResponseVerb::Ingest, "kind", &reason);
            resp.policy_trace = to_wire(&[PolicyTraceEntry::error(
                PolicyGate::ScopeCheck,
                PolicyErrorCode::from_static("invalid_kind"),
            )]);
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "InvalidArgs", &reason, &resp.operation_id);
            }
            Err(ExitCode::from(64))
        }
    }
}

fn resolve_body(sub: &ArgMatches) -> Result<ResolvedBody, String> {
    if let Some(src) = sub.get_one::<String>("source") {
        return if src == "-" {
            let mut buf = String::new();
            std::io::stdin()
                .take(STDIN_LIMIT_BYTES)
                .read_to_string(&mut buf)
                .map_err(|e| format!("failed to read stdin: {e}"))?;
            Ok(ResolvedBody { body: buf })
        } else {
            let path = PathBuf::from(src);
            if path.is_file() {
                fs::read_to_string(&path)
                    .map(|body| ResolvedBody { body })
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))
            } else {
                Ok(ResolvedBody { body: src.clone() })
            }
        };
    }
    if let Some(body) = sub.get_one::<String>("body") {
        return Ok(ResolvedBody { body: body.clone() });
    }
    if let Some(path) = sub.get_one::<PathBuf>("file") {
        return fs::read_to_string(path)
            .map(|body| ResolvedBody { body })
            .map_err(|e| format!("failed to read {}: {e}", path.display()));
    }
    if let Some(url) = sub.get_one::<String>("url") {
        return Ok(ResolvedBody { body: url.clone() });
    }
    Err("exactly one source is required".to_owned())
}

fn require_bound_vault(json: bool, vault_root: &Path) -> Option<ExitCode> {
    match status::probe_vault_binding(vault_root) {
        status::VaultBinding::Bound => None,
        status::VaultBinding::Unbound => {
            let msg = format!(
                "no Cairn vault at {} — run `cairn bootstrap` first",
                vault_root.display()
            );
            let resp = not_found_response(ResponseVerb::Ingest, "vault", &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "NotFound", &msg, &resp.operation_id);
            }
            Some(ExitCode::from(78))
        }
        status::VaultBinding::Invalid(reason) => {
            let msg = format!("vault binding error — {reason}");
            let resp = internal_error_response(&msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "Internal", &msg, &resp.operation_id);
            }
            Some(ExitCode::from(78))
        }
    }
}

fn run_resync(sub: &ArgMatches, json: bool, resync_path: &Path, vault_root: &Path) -> ExitCode {
    if let Some(exit) = require_bound_vault(json, vault_root) {
        return exit;
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return emit_internal(json, &format!("runtime build: {e}"), Vec::new()),
    };

    let db_path = vault_root.join(".cairn").join("cairn.db");
    let result = rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path).await?;
        resync_handler(&store, resync_path, vault_root).await
    });

    match result {
        Ok(result) => {
            let session_id = sub
                .get_one::<String>("session_id")
                .cloned()
                .unwrap_or_else(|| "resync".to_owned());
            let resp = Response {
                contract: "cairn.mcp.v1".to_owned(),
                data: Some(ResponseData::Ingest(IngestData {
                    cache_hits: None,
                    cache_misses: None,
                    cache_writes: None,
                    files_processed: None,
                    plan_ref: None,
                    record_id: Ulid(result.target_id),
                    session_id,
                })),
                error: None,
                operation_id: new_operation_id(),
                policy_trace: Vec::<ResponsePolicyTrace>::new(),
                status: ResponseStatus::Committed,
                target: None,
                verb: ResponseVerb::Ingest,
            };
            if json {
                emit_json(&resp);
            } else if let Some(ResponseData::Ingest(data)) = resp.data.as_ref() {
                println!(
                    "cairn ingest --resync: committed record {}",
                    data.record_id.0
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => emit_internal(json, &format!("{e:#}"), Vec::new()),
    }
}

fn run_body_ingest(
    sub: &ArgMatches,
    json: bool,
    vault_root: &Path,
    kind: MemoryKind,
    resolved: ResolvedBody,
) -> ExitCode {
    let ResolvedBody { body } = resolved;
    let redacted = redact(&body);
    let fenced = fence(&redacted.text);
    let decision = should_memorize(&FilterInputs::new(&redacted, &fenced));
    let visibility = default_visibility(
        IdentityKind::Agent,
        CaptureMode::Explicit,
        SourceFamily::Cli,
        &VisibilityPolicy::default(),
    );
    let trace = policy_trace_for_ingest(&redacted, &fenced, decision, visibility);
    let class = default_class_for_kind(kind);
    let session_id = sub
        .get_one::<String>("session_id")
        .cloned()
        .unwrap_or_else(|| new_operation_id().0);
    let scope = ScopeTuple {
        session_id: Some(session_id.clone()),
        agent: Some(CLI_AUTHOR_ID.to_owned()),
        ..ScopeTuple::default()
    };

    if let Decision::Discard(reason) = decision {
        let metric = IngestMetricRow::discarded(kind, class, visibility, &scope, reason.as_str());
        if let Err(e) = append_metric(vault_root, &metric) {
            return emit_internal(json, &format!("write metrics: {e:#}"), trace);
        }
        let mut resp = invalid_args_response(
            ResponseVerb::Ingest,
            "body",
            &format!("discarded by filter: {}", reason.as_str()),
        );
        resp.policy_trace = trace;
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "ingest",
                "InvalidArgs",
                &format!("body discarded by filter: {}", reason.as_str()),
                &resp.operation_id,
            );
        }
        return ExitCode::from(64);
    }

    let record = match build_record(kind, class, visibility, scope.clone(), &fenced.text) {
        Ok(record) => record,
        Err(e) => return emit_internal(json, &format!("build record: {e:#}"), trace),
    };
    let db_path = vault_root.join(".cairn").join("cairn.db");

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return emit_internal(json, &format!("runtime build: {e}"), trace),
    };

    let outcome = match rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path)
            .await
            .map_err(|e| format!("open store: {e}"))?;
        let lock = acquire_session_shared_lock(&store, &session_id)
            .await
            .map_err(|e| format!("acquire session lock: {e:#}"))?;
        if source_hash_is_forgotten(&db_path, &record.provenance.source_hash) {
            return Err(format!(
                "source hash `{}` has a prior source-forget receipt and cannot be re-ingested",
                record.provenance.source_hash
            ));
        }
        write_source_artifact(vault_root, &record.provenance.source_ids[0], &fenced.text)
            .map_err(|e| format!("write source artifact: {e:#}"))?;
        let outcome = store
            .upsert(&record)
            .await
            .map_err(|e| format!("store upsert: {e}"))?;
        lock.release()
            .await
            .map_err(|e| format!("release session lock: {e}"))?;
        Ok(outcome)
    }) {
        Ok(outcome) => outcome,
        Err(reason) if reason.contains("prior source-forget receipt") => {
            let mut resp = invalid_args_response(ResponseVerb::Ingest, "body", &reason);
            resp.policy_trace = trace;
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "InvalidArgs", &reason, &resp.operation_id);
            }
            return ExitCode::from(64);
        }
        Err(reason) => return emit_internal(json, &reason, trace),
    };

    let metric = IngestMetricRow::accepted(
        outcome.record_id.as_str(),
        kind,
        class,
        visibility,
        &scope,
        1,
    );
    if let Err(e) = append_metric(vault_root, &metric) {
        return emit_internal(json, &format!("write metrics: {e:#}"), trace);
    }

    let resp = Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Ingest(IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            plan_ref: None,
            record_id: Ulid(outcome.record_id.as_str().to_owned()),
            session_id,
        })),
        error: None,
        operation_id: new_operation_id(),
        policy_trace: trace,
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Ingest,
    };
    if json {
        emit_json(&resp);
    } else if let Some(ResponseData::Ingest(data)) = resp.data.as_ref() {
        println!("cairn ingest: committed record {}", data.record_id.0);
    }
    ExitCode::SUCCESS
}

fn source_hash_is_forgotten(db_path: &Path, source_hash: &str) -> bool {
    let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let Ok(events) = cairn_store_sqlite::consent::query_by_subject(&conn, source_hash) else {
        return false;
    };
    events.into_iter().any(|event| {
        event.kind == cairn_core::domain::ConsentKind::ForgetIntent
            && matches!(
                event.payload,
                cairn_core::domain::ConsentPayload::IntentReceipt { reason_code, .. }
                    if reason_code.starts_with("source_forget")
            )
    })
}

async fn acquire_session_shared_lock(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    session_id: &str,
) -> anyhow::Result<cairn_store_sqlite::locks::LockHandle> {
    let conn = store
        .raw_conn_for_admin()
        .ok_or_else(|| anyhow::anyhow!("store lock path unavailable"))?;
    let incarnation = store
        .incarnation()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("store incarnation unavailable"))?;
    let resource = cairn_store_sqlite::locks::ResourceKey::session(
        SESSION_LOCK_TENANT,
        SESSION_LOCK_WORKSPACE,
        session_id,
    );
    let holder_id = format!("pid={}-{}", std::process::id(), ulid::Ulid::new());
    cairn_store_sqlite::locks::acquire(
        conn,
        &resource,
        cairn_store_sqlite::locks::LockMode::Shared,
        &holder_id,
        SESSION_LOCK_TTL,
        &incarnation,
        "ingest",
    )
    .await
    .map_err(|e| anyhow::anyhow!("acquire session lock: {e}"))
}

fn policy_trace_for_ingest(
    redacted: &cairn_core::pipeline::filter::RedactedPayload,
    fenced: &cairn_core::pipeline::filter::FencedPayload,
    decision: Decision,
    visibility: MemoryVisibility,
) -> Vec<ResponsePolicyTrace> {
    let mut entries = vec![
        PolicyTraceEntry::from(redacted),
        PolicyTraceEntry::from(fenced),
        PolicyTraceEntry::from(&decision),
    ];
    if matches!(decision, Decision::Proceed) {
        entries.push(PolicyTraceEntry::new(
            PolicyGate::VisibilityFloor,
            PolicyOutcome::Pass,
            PolicyDetail::VisibilityFloor(visibility),
        ));
        entries.push(PolicyTraceEntry::pass(PolicyGate::ScopeCheck));
    }
    to_wire(&entries)
}

#[derive(serde::Serialize)]
struct IngestMetricRow<'a> {
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_id: Option<&'a str>,
    kind: &'static str,
    class: &'static str,
    visibility: &'static str,
    scope: &'a ScopeTuple,
    source_family: &'static str,
    capture_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    discard_reason: Option<&'a str>,
}

impl<'a> IngestMetricRow<'a> {
    fn accepted(
        record_id: &'a str,
        kind: MemoryKind,
        class: MemoryClass,
        visibility: MemoryVisibility,
        scope: &'a ScopeTuple,
        rank: u32,
    ) -> Self {
        Self {
            event: "accepted",
            record_id: Some(record_id),
            kind: kind.as_str(),
            class: class.as_str(),
            visibility: visibility.as_str(),
            scope,
            source_family: SourceFamily::Cli.as_str(),
            capture_mode: CaptureMode::Explicit.as_str(),
            rank: Some(rank),
            discard_reason: None,
        }
    }

    fn discarded(
        kind: MemoryKind,
        class: MemoryClass,
        visibility: MemoryVisibility,
        scope: &'a ScopeTuple,
        reason: &'a str,
    ) -> Self {
        Self {
            event: "discarded",
            record_id: None,
            kind: kind.as_str(),
            class: class.as_str(),
            visibility: visibility.as_str(),
            scope,
            source_family: SourceFamily::Cli.as_str(),
            capture_mode: CaptureMode::Explicit.as_str(),
            rank: None,
            discard_reason: Some(reason),
        }
    }
}

fn append_metric(vault_root: &Path, row: &IngestMetricRow<'_>) -> anyhow::Result<()> {
    let cairn_dir = vault_root.join(".cairn");
    fs::create_dir_all(&cairn_dir)
        .with_context(|| format!("create metrics dir {}", cairn_dir.display()))?;
    let metrics_path = cairn_dir.join("metrics.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&metrics_path)
        .with_context(|| format!("open {}", metrics_path.display()))?;
    serde_json::to_writer(&mut file, row).context("serialize metric row")?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", metrics_path.display()))?;
    Ok(())
}

fn build_record(
    kind: MemoryKind,
    class: MemoryClass,
    visibility: MemoryVisibility,
    scope: ScopeTuple,
    body: &str,
) -> anyhow::Result<MemoryRecord> {
    let id_text = new_operation_id().0;
    let id = RecordId::parse(id_text.clone()).map_err(anyhow::Error::msg)?;
    let target_id = TargetId::parse(id_text).map_err(anyhow::Error::msg)?;
    let author = Identity::parse(CLI_AUTHOR_ID).map_err(anyhow::Error::msg)?;
    let now = now_timestamp()?;
    let source_hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
    let source_id =
        SourceId::parse(format!("sources/cli/{}.txt", id.as_str())).map_err(anyhow::Error::msg)?;
    let record = MemoryRecord {
        id,
        target_id,
        kind,
        class,
        visibility,
        scope,
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse(CLI_SENSOR_ID).map_err(anyhow::Error::msg)?,
            created_at: now.clone(),
            originating_agent_id: author.clone(),
            source_ids: vec![source_id],
            source_hash,
            consent_ref: "consent:cli:p0".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: now.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: author,
            at: now,
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
            .map_err(anyhow::Error::msg)?,
        tags: Vec::new(),
        extra_frontmatter: BTreeMap::new(),
        consent_model: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}

fn write_source_artifact(
    vault_root: &Path,
    source_id: &SourceId,
    body: &str,
) -> anyhow::Result<()> {
    let path = vault_root.join(source_id.as_str());
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("source artifact missing parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create source dir {}", parent.display()))?;
    fs::write(&path, body).with_context(|| format!("write source artifact {}", path.display()))?;
    Ok(())
}

fn now_timestamp() -> anyhow::Result<Rfc3339Timestamp> {
    let raw = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Rfc3339Timestamp::parse(raw).map_err(anyhow::Error::msg)
}

fn default_class_for_kind(kind: MemoryKind) -> MemoryClass {
    match kind {
        MemoryKind::Event
        | MemoryKind::Trace
        | MemoryKind::Reasoning
        | MemoryKind::SensorObservation
        | MemoryKind::UserSignal
        | MemoryKind::Feedback => MemoryClass::Episodic,
        MemoryKind::Workflow
        | MemoryKind::StrategySuccess
        | MemoryKind::StrategyFailure
        | MemoryKind::Playbook => MemoryClass::Procedural,
        _ => MemoryClass::Semantic,
    }
}

fn emit_internal(json: bool, message: &str, policy_trace: Vec<ResponsePolicyTrace>) -> ExitCode {
    let mut resp = internal_error_response(message);
    resp.policy_trace = policy_trace;
    if json {
        emit_json(&resp);
    } else {
        human_error("ingest", "Internal", message, &resp.operation_id);
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

fn run_folder(sub: &ArgMatches, json: bool, folder: &Path, vault_root: &Path) -> ExitCode {
    match run_folder_inner(sub, folder, vault_root) {
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

fn run_folder_inner(
    sub: &ArgMatches,
    folder: &Path,
    vault_root: &Path,
) -> Result<Response, FolderIngestError> {
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
        process_folder_file(vault_root, &file, no_cache, &mut stats)?;
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

    #[tokio::test]
    async fn session_shared_lock_rejects_while_exclusive_holder_is_live() {
        let store = cairn_store_sqlite::open_in_memory().await.unwrap();
        let conn = std::sync::Arc::clone(store.raw_conn_for_admin().unwrap());
        let incarnation = store.incarnation().cloned().unwrap();
        let resource = cairn_store_sqlite::locks::ResourceKey::session(
            SESSION_LOCK_TENANT,
            SESSION_LOCK_WORKSPACE,
            "sess-42",
        );
        let exclusive = cairn_store_sqlite::locks::acquire(
            &conn,
            &resource,
            cairn_store_sqlite::locks::LockMode::Exclusive,
            "test-exclusive-holder",
            SESSION_LOCK_TTL,
            &incarnation,
            "forget --session test",
        )
        .await
        .unwrap();

        let err = acquire_session_shared_lock(&store, "sess-42")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("lock held"),
            "shared ingest lock should fail behind a live exclusive session holder: {err:#}"
        );

        exclusive.release().await.unwrap();
    }
}
