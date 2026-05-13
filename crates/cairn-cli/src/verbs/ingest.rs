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

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::canonical::{
    canonical_bytes_signed_intent, canonical_bytes_signed_payload,
};
use cairn_core::domain::identity::{
    keys::{IdentityRevision, SecretHandle},
    provision::ProvisionInput,
};
use cairn_core::domain::projection::{
    ConflictOutcome, MarkdownProjector, PROJECTED_STANDARD_FIELDS, ResyncError,
};
use cairn_core::domain::{Identity, MemoryKind, SignedAdmission, WalActionKind};
use cairn_core::generated::common::{Ed25519Signature, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{
    RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace, ResponseStatus,
    SignedIntent, SignedIntentScope, SignedIntentScopeTier,
};
use cairn_core::generated::verbs::ingest::{IngestArgs, IngestData, IngestMode};
use cairn_core::policy_trace::{PolicyErrorCode, PolicyGate, PolicyTraceEntry, to_wire};
use clap::ArgMatches;
use sha2::Digest as _;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{
    emit_json, human_error, invalid_args_response, new_operation_id, not_found_response,
};
use super::status;

const DEFAULT_INGEST_ISSUER: &str = "agt:cairn-cli:default:writer:v1";
const DEFAULT_TENANT: &str = "default";
const INGEST_ENTITY: &str = "ingest";
const INGEST_CHALLENGE_TTL_MS: i64 = 5 * 60 * 1_000;
const STDIN_LIMIT_BYTES: u64 = 4 * 1024 * 1024;
const SESSION_LOCK_TENANT: &str = "default";
const SESSION_LOCK_WORKSPACE: &str = "default";
const UNSCOPED_LOCK_COMPONENT: &str = "__none__";
const SESSION_LOCK_TTL: Duration = Duration::from_secs(30);

mod apply;
mod cache;
mod extract;
mod folder;
mod patterns;
mod planner;
pub mod report;
mod scanner;

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
pub fn run(
    sub: &ArgMatches,
    vault_root: PathBuf,
    config: cairn_core::config::CairnConfig,
) -> ExitCode {
    let json = sub.get_flag("json");
    if let Some(requested_folder) = sub.get_one::<PathBuf>("folder") {
        return folder::run(sub, vault_root.clone(), requested_folder.as_path(), false);
    }
    if let Some(requested_folder) = positional_folder_source(sub) {
        return folder::run(sub, vault_root.clone(), requested_folder.as_path(), true);
    }

    // --resync <path>: re-ingest an out-of-band edited markdown projection.
    if let Some(resync_path) = sub.get_one::<std::path::PathBuf>("resync") {
        return run_resync(sub, json, resync_path, vault_root.as_path());
    }

    // --jsonl <path>: harness transcript backfill (issue #311, brief §5.5).
    if let Some(jsonl_path) = sub.get_one::<std::path::PathBuf>("jsonl") {
        return crate::verbs::ingest_jsonl::run(sub, json, jsonl_path, &vault_root);
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

    if sub.get_flag("no_cache") {
        eprintln!("cairn ingest: --no-cache is only valid with --folder");
        return ExitCode::from(64);
    }

    // Enforce IDL exactly-one-of: body/file/folder/url (positional `source` counts as one).
    let source_count = ingest_source_count(sub);
    if source_count != 1 {
        eprintln!(
            "cairn ingest: exactly one of [source, --body, --file, --folder, --url] is required (got {source_count})"
        );
        return ExitCode::from(64);
    }

    let _kind = match parse_kind(sub, json) {
        Ok(kind) => kind,
        Err(code) => return code,
    };

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

    if let Some(exit) = require_bound_vault(json, vault_root.as_path()) {
        return exit;
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp = super::envelope::internal_error_response(
                ResponseVerb::Ingest,
                &format!("runtime build: {e}"),
            );
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "Internal", &format!("{e}"), &resp.operation_id);
            }
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(run_async(sub, vault_root, config, json, resolved.body))
}

struct ResolvedBody {
    body: String,
    // Retained for the source-artifact pipeline (issue #325 follow-up); the
    // signed-ingest path does not yet stage `sources/` files.
    #[allow(
        dead_code,
        reason = "wired in upcoming source-artifact persistence task"
    )]
    source_subdir: &'static str,
    #[allow(
        dead_code,
        reason = "wired in upcoming source-artifact persistence task"
    )]
    source_extension: String,
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
            Ok(ResolvedBody {
                body: buf,
                source_subdir: "chat",
                source_extension: "txt".to_owned(),
            })
        } else {
            let path = PathBuf::from(src);
            if path.is_file() {
                fs::read_to_string(&path)
                    .map(|body| ResolvedBody {
                        body,
                        source_subdir: "documents",
                        source_extension: source_extension(&path),
                    })
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))
            } else {
                Ok(ResolvedBody {
                    body: src.clone(),
                    source_subdir: "chat",
                    source_extension: "txt".to_owned(),
                })
            }
        };
    }
    if let Some(body) = sub.get_one::<String>("body") {
        return Ok(ResolvedBody {
            body: body.clone(),
            source_subdir: "chat",
            source_extension: "txt".to_owned(),
        });
    }
    if let Some(path) = sub.get_one::<PathBuf>("file") {
        return fs::read_to_string(path)
            .map(|body| ResolvedBody {
                body,
                source_subdir: "documents",
                source_extension: source_extension(path),
            })
            .map_err(|e| format!("failed to read {}: {e}", path.display()));
    }
    if let Some(url) = sub.get_one::<String>("url") {
        return Ok(ResolvedBody {
            body: url.clone(),
            source_subdir: "articles",
            source_extension: "txt".to_owned(),
        });
    }
    Err("exactly one source is required".to_owned())
}

fn source_extension(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .map_or_else(|| "txt".to_owned(), str::to_owned)
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
                    jsonl_summary: None,
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

#[allow(
    clippy::too_many_lines,
    reason = "signed ingest CLI flow is guard, prepare, authorize, persist, and emit in source order"
)]
async fn run_async(
    sub: &ArgMatches,
    vault_root: PathBuf,
    config: cairn_core::config::CairnConfig,
    json: bool,
    body: String,
) -> ExitCode {
    let args = ingest_args_from_matches(sub, body);
    if args.session_id.is_some() {
        let reason = "`--session` ingest is not supported until signed intents carry a session scope dimension";
        let resp = super::envelope::invalid_args_response(ResponseVerb::Ingest, "session", reason);
        if json {
            emit_json(&resp);
        } else {
            human_error("ingest", "InvalidArgs", reason, &resp.operation_id);
        }
        return ExitCode::from(64);
    }
    let ctx = match super::signed::open_context(ResponseVerb::Ingest, &vault_root, config).await {
        Ok(ctx) => ctx,
        Err(resp) => {
            if json {
                emit_json(&resp);
            }
            return ExitCode::from(78);
        }
    };
    let issuer_wire =
        std::env::var("CAIRN_ISSUER").unwrap_or_else(|_| DEFAULT_INGEST_ISSUER.to_owned());
    let prepared = match cairn_core::verbs::ingest::prepare_ingest_body(&args, &issuer_wire) {
        Ok(p) => p,
        Err(e) => {
            let resp = super::signed::rejected_from_domain(ResponseVerb::Ingest, e);
            if json {
                emit_json(&resp);
            }
            return ExitCode::from(64);
        }
    };
    match prepared {
        cairn_core::verbs::ingest::PreparedIngest::Rejected { policy_trace, .. } => {
            let mut resp = super::signed::rejected_from_domain(
                ResponseVerb::Ingest,
                cairn_core::domain::DomainError::Unauthorized {
                    message: "ingest rejected by filter".to_owned(),
                },
            );
            resp.policy_trace = policy_trace;
            if json {
                emit_json(&resp);
            }
            ExitCode::from(65)
        }
        cairn_core::verbs::ingest::PreparedIngest::Proceed {
            record,
            policy_trace,
            ..
        } => {
            let mut record = *record;
            let issuer = match Identity::parse(issuer_wire) {
                Ok(issuer) => issuer,
                Err(e) => {
                    let resp = super::signed::rejected_from_domain(ResponseVerb::Ingest, e);
                    if json {
                        emit_json(&resp);
                    }
                    return ExitCode::from(64);
                }
            };
            if let Err(resp) = ensure_ingest_issuer(&ctx, &issuer).await {
                if json {
                    emit_json(&resp);
                }
                return response_exit_code(&resp);
            }
            bind_record_scope_to_ingest_context(&mut record, &ctx);
            let record_id = cairn_core::generated::common::Ulid(record.id.as_str().to_owned());
            let session_id = args
                .session_id
                .clone()
                .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
            let op = new_operation_id();
            let payload = match canonical_bytes_signed_payload(&record) {
                Ok(payload) => payload,
                Err(e) => {
                    let resp = super::signed::rejected_from_domain(ResponseVerb::Ingest, e);
                    if json {
                        emit_json(&resp);
                    }
                    return ExitCode::from(64);
                }
            };
            let admission =
                match signed_ingest_admission(&ctx, &issuer, op.clone(), &args, &record, &payload)
                    .await
                {
                    Ok(admission) => admission,
                    Err(resp) => {
                        if json {
                            emit_json(&resp);
                        }
                        return response_exit_code(&resp);
                    }
                };
            let result = ctx
                .store
                .with_tx(move |tx| {
                    tx.prepare_wal_with_replay(&admission).map_err(|e| {
                        cairn_store_sqlite::StoreError::Invariant {
                            what: format!("wal admission: {e}"),
                        }
                    })?;
                    tx.upsert(&record)?;
                    tx.commit_prepared_wal(&admission)?;
                    Ok::<_, cairn_store_sqlite::StoreError>(())
                })
                .await;
            match result {
                Ok(()) => {
                    let data = IngestData {
                        cache_hits: None,
                        cache_misses: None,
                        cache_writes: None,
                        files_processed: None,
                        plan_ref: None,
                        record_id,
                        session_id,
                        jsonl_summary: None,
                    };
                    let resp = super::signed::committed(
                        ResponseVerb::Ingest,
                        op,
                        ResponseData::Ingest(data),
                        policy_trace,
                    );
                    if json {
                        emit_json(&resp);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    let resp =
                        super::signed::aborted(ResponseVerb::Ingest, format!("store upsert: {e}"));
                    if json {
                        emit_json(&resp);
                    }
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn bind_record_scope_to_ingest_context(
    record: &mut cairn_core::domain::record::MemoryRecord,
    ctx: &super::signed::OpenedVerbContext,
) {
    record.scope.tenant = Some(DEFAULT_TENANT.to_owned());
    record.scope.workspace = Some(ctx.config.vault.name.clone());
    record.scope.entity = Some(INGEST_ENTITY.to_owned());
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        cairn_core::generated::envelope::ResponseStatus::Rejected => ExitCode::from(64),
        cairn_core::generated::envelope::ResponseStatus::Aborted => ExitCode::from(78),
        cairn_core::generated::envelope::ResponseStatus::Committed => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

async fn ensure_ingest_issuer(
    ctx: &super::signed::OpenedVerbContext,
    issuer: &Identity,
) -> Result<(), Response> {
    let existing = ctx
        .identity
        .registry
        .get_identity(issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Ingest, format!("identity lookup: {e}"))
        })?;
    if existing.is_some() {
        return Ok(());
    }
    if issuer.as_str() != DEFAULT_INGEST_ISSUER {
        return Err(super::signed::rejected_from_domain(
            ResponseVerb::Ingest,
            cairn_core::domain::DomainError::Unauthorized {
                message: format!("issuer {issuer} is not active in this vault"),
            },
        ));
    }

    let mut rng = rand_core::OsRng;
    let input = ProvisionInput {
        vault_id: ctx.identity.vault_id.clone(),
        id: issuer.clone(),
        kind: issuer.kind(),
        revision: IdentityRevision::FIRST,
    };
    ctx.identity
        .provision(issuer.kind(), input, &mut rng)
        .await
        .map_err(|e| {
            super::signed::aborted(
                ResponseVerb::Ingest,
                format!("default issuer provision: {e}"),
            )
        })?;
    Ok(())
}

async fn signed_ingest_admission(
    ctx: &super::signed::OpenedVerbContext,
    issuer: &Identity,
    operation_id: cairn_core::generated::common::Ulid,
    args: &IngestArgs,
    record: &cairn_core::domain::record::MemoryRecord,
    payload: &[u8],
) -> Result<SignedAdmission, Response> {
    let active = ctx
        .identity
        .registry
        .get_identity(issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| super::signed::aborted(ResponseVerb::Ingest, format!("identity lookup: {e}")))?
        .ok_or_else(|| {
            super::signed::rejected_from_domain(
                ResponseVerb::Ingest,
                cairn_core::domain::DomainError::Unauthorized {
                    message: format!("issuer {issuer} is not active in this vault"),
                },
            )
        })?;
    let handle = SecretHandle::for_identity(
        ctx.identity.vault_id.clone(),
        issuer.clone(),
        active.current_key_version,
    );
    let signing_key = ctx
        .identity
        .keystore
        .load_signing_key(&handle)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Ingest, format!("issuer key load: {e}"))
        })?;
    let challenge = mint_ingest_challenge(ctx, issuer).await?;
    let mut intent = unsigned_ingest_intent(
        issuer,
        active.current_key_version.as_u32(),
        operation_id,
        ctx.config.vault.name.clone(),
        challenge,
        payload,
    );
    let intent_bytes = canonical_bytes_signed_intent(&intent)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Ingest, e))?;
    intent.signature = Ed25519Signature(format!(
        "ed25519:{}",
        hex_lower(&signing_key.sign(&intent_bytes).to_bytes())
    ));
    let request = super::signed::request(
        RequestVerb::Ingest,
        RequestArgs::Ingest(args.clone()),
        intent,
    );
    let verified = super::signed::verify_request(ctx, request).await?;
    record
        .validate_against_intent(&verified)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Ingest, e))?;
    SignedAdmission::new(verified, WalActionKind::Upsert, None, payload)
        .map_err(|e| super::signed::aborted(ResponseVerb::Ingest, format!("signed admission: {e}")))
}

async fn mint_ingest_challenge(
    ctx: &super::signed::OpenedVerbContext,
    issuer: &Identity,
) -> Result<Nonce16Base64, Response> {
    let issuer = issuer.as_str().to_owned();
    let now_ms = unix_time_millis_i64();
    let minted = ctx
        .store
        .with_tx(move |tx| tx.mint_challenge(&issuer, now_ms, INGEST_CHALLENGE_TTL_MS))
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Ingest, format!("challenge mint: {e}"))
        })?;
    Ok(Nonce16Base64(minted.nonce_b64))
}

fn unsigned_ingest_intent(
    issuer: &Identity,
    key_version: u32,
    operation_id: cairn_core::generated::common::Ulid,
    workspace: String,
    server_challenge: Nonce16Base64,
    payload: &[u8],
) -> SignedIntent {
    let issued_at = chrono::Utc::now();
    let expires_at = issued_at + chrono::Duration::minutes(5);
    SignedIntent {
        chain_parents: vec![],
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issued_at: issued_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issuer: cairn_core::generated::common::Identity(issuer.as_str().to_owned()),
        key_version: i64::from(key_version),
        nonce: super::envelope::new_nonce(),
        operation_id,
        scope: SignedIntentScope {
            tenant: DEFAULT_TENANT.to_owned(),
            workspace,
            entity: INGEST_ENTITY.to_owned(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: None,
        server_challenge: Some(server_challenge),
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: sha256_wire(payload),
    }
}

fn ingest_args_from_matches(sub: &ArgMatches, body: String) -> IngestArgs {
    IngestArgs {
        batch_size: sub.get_one::<u32>("batch_size").map(|v| i64::from(*v)),
        body: Some(body),
        dry_run: Some(sub.get_flag("dry-run")).filter(|b| *b),
        exclude: sub
            .get_many::<String>("exclude")
            .map(|vals| vals.cloned().collect()),
        file: None,
        folder: None,
        frontmatter: None,
        human_review: Some(sub.get_flag("human-review")).filter(|b| *b),
        include: sub
            .get_many::<String>("include")
            .map(|vals| vals.cloned().collect()),
        kind: sub
            .get_one::<String>("kind")
            .cloned()
            .unwrap_or_else(|| "reference".to_owned()),
        mode: sub
            .get_one::<String>("mode")
            .and_then(|mode| match mode.as_str() {
                "keyword" => Some(IngestMode::Keyword),
                "semantic" => Some(IngestMode::Semantic),
                "full" => Some(IngestMode::Full),
                _ => None,
            }),
        no_cache: Some(sub.get_flag("no_cache")).filter(|b| *b),
        no_diff: Some(sub.get_flag("no-diff")).filter(|b| *b),
        recursive: Some(sub.get_flag("recursive")).filter(|b| *b),
        session_id: sub.get_one::<String>("session_id").cloned(),
        tags: sub
            .get_many::<String>("tags")
            .map(|vals| vals.cloned().collect()),
        url: None,
        jsonl: sub
            .get_one::<PathBuf>("jsonl")
            .map(|p| p.to_string_lossy().into_owned()),
        harness: sub.get_one::<String>("harness").cloned(),
        session_id_from: sub.get_one::<String>("session_id_from").cloned(),
        limit: sub.get_one::<u32>("limit").map(|v| i64::from(*v)),
    }
}

fn sha256_wire(payload: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(payload);
    format!("sha256:{}", hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn unix_time_millis_i64() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
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
            UNSCOPED_LOCK_COMPONENT,
            UNSCOPED_LOCK_COMPONENT,
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

        let err = acquire_session_shared_lock(&store, None, None, "sess-42")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("lock held"),
            "shared ingest lock should fail behind a live exclusive session holder: {err:#}"
        );

        exclusive.release().await.unwrap();
    }

    #[tokio::test]
    async fn session_namespace_shared_lock_rejects_while_exclusive_holder_is_live() {
        let store = cairn_store_sqlite::open_in_memory().await.unwrap();
        let conn = std::sync::Arc::clone(store.raw_conn_for_admin().unwrap());
        let incarnation = store.incarnation().cloned().unwrap();
        let resource = cairn_store_sqlite::locks::ResourceKey::session_namespace("sess-42");
        let exclusive = cairn_store_sqlite::locks::acquire(
            &conn,
            &resource,
            cairn_store_sqlite::locks::LockMode::Exclusive,
            "test-exclusive-holder",
            SESSION_LOCK_TTL,
            &incarnation,
            "forget --session namespace test",
        )
        .await
        .unwrap();

        let err = acquire_session_namespace_shared_lock(&store, "sess-42")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("lock held"),
            "shared ingest namespace lock should fail behind a live exclusive holder: {err:#}"
        );

        exclusive.release().await.unwrap();
    }
}
