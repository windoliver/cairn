//! `cairn forget` handler.
//!
//! # Trust boundary (spec §3.5)
//!
//! `forget` is an issuer-dependent verb: it produces a signed tombstone record
//! through the signed-verb context before mutating the selected vault store.
//!
//! Record-mode forget is wired end-to-end for P0. After the WAL-driven
//! tombstone commits, the verb additionally appends body-free
//! `source_forget` consent events keyed by `provenance.source_hash` and,
//! when `config.source.redact_on_forget` is set, rewrites any matching
//! files under `sources/` to metadata stubs (issue #327).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload, RecordId, Rfc3339Timestamp};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus};
use cairn_core::generated::verbs::forget::ForgetData;
use clap::ArgMatches;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::envelope::{
    EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error, not_found_response,
};

fn requested_capability(sub: &ArgMatches) -> &'static str {
    if sub.get_one::<String>("session_id").is_some() {
        "cairn.mcp.v1.forget.session"
    } else if sub.get_one::<String>("scope").is_some() {
        "cairn.mcp.v1.forget.scope"
    } else {
        "cairn.mcp.v1.forget.record"
    }
}

/// Run `cairn forget`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
    if !requires_vault_context(sub) {
        return run_without_context(sub);
    }

    let json = sub.get_flag("json");

    let Some(record_id) = sub.get_one::<String>("record_id") else {
        return run_without_context(sub);
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp = super::signed::aborted(ResponseVerb::Forget, format!("runtime build: {e}"));
            emit_response(&resp, json, record_id);
            return ExitCode::FAILURE;
        }
    };
    let resp = rt.block_on(run_record(record_id.clone(), vault_root, config));
    emit_response(&resp, json, record_id);
    response_exit_code(&resp)
}

/// Whether this invocation needs the resolved vault path and config.
#[must_use]
pub fn requires_vault_context(sub: &ArgMatches) -> bool {
    !sub.get_flag("dry-run")
        && !sub.get_flag("human-review")
        && sub.get_one::<String>("record_id").is_some()
}

/// Run `cairn forget` modes that do not open the vault store.
#[must_use]
pub fn run_without_context(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

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

    let capability = requested_capability(sub);
    let resp = capability_unavailable_response(ResponseVerb::Forget, capability);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "forget",
            "CapabilityUnavailable",
            "capability is not advertised in this build",
            &resp.operation_id,
        );
    }
    ExitCode::from(EX_UNAVAILABLE)
}

async fn run_record(record_id_raw: String, vault_root: PathBuf, config: CairnConfig) -> Response {
    let record_id = match RecordId::parse(record_id_raw.clone()) {
        Ok(record_id) => record_id,
        Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Forget, e),
    };

    // Best-effort recovery of any crash-stranded source redactions left
    // by a prior `forget --record`. Failures here are logged but do not
    // block the new request — recovery is idempotent and will be retried
    // on the next invocation.
    if let Err(e) = reconcile_pending_source_redactions(&vault_root) {
        tracing::warn!(
            error = %e,
            vault = %vault_root.display(),
            "forget: pending source-redaction recovery failed"
        );
    }

    let ctx = match super::signed::open_context(ResponseVerb::Forget, &vault_root, config.clone())
        .await
    {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };

    // Snapshot every version's source_hash + originating_agent BEFORE
    // the WAL tombstone runs — after `forget_record` returns, the rows
    // may already be drained from the primary table by the
    // `primary.purge` step.
    let source_event_seeds = match collect_source_event_seeds(&ctx.store, &record_id).await {
        Ok(seeds) => seeds,
        Err(e) => {
            return super::signed::aborted(
                ResponseVerb::Forget,
                format!("collect source seeds: {e}"),
            );
        }
    };

    // Prepare source-file redactions (and stage backups for crash
    // recovery) BEFORE the WAL commits. If the WAL fails, we restore
    // the originals from the staged manifest.
    let source_hashes: BTreeSet<String> = source_event_seeds
        .iter()
        .map(|seed| seed.source_hash.clone())
        .collect();
    let source_root = vault_root.join(&config.vault.layout.sources);
    let prepared_redactions = if config.source.redact_on_forget {
        match prepare_redactions(&source_root, &source_hashes) {
            Ok(prepared) => prepared,
            Err(e) => {
                return super::signed::aborted(
                    ResponseVerb::Forget,
                    format!("prepare source redactions: {e}"),
                );
            }
        }
    } else {
        Vec::new()
    };

    match ctx.store.forget_record(&record_id).await {
        Ok(outcome) => {
            let operation_id = match response_operation_id(&outcome.operation_id) {
                Ok(operation_id) => operation_id,
                Err(message) => return super::signed::aborted(ResponseVerb::Forget, message),
            };
            let op_id_str = operation_id.0.clone();

            // Stage backups + apply redactions before the consent
            // append; cleanup happens after the events commit.
            let manifest_dir = if config.source.redact_on_forget {
                match stage_pending_redactions(
                    &vault_root,
                    &op_id_str,
                    outcome.target_hash_for_manifest(),
                    source_event_seeds.len(),
                    &prepared_redactions,
                ) {
                    Ok(dir) => dir,
                    Err(e) => {
                        return super::signed::aborted(
                            ResponseVerb::Forget,
                            format!("stage source-redaction manifest: {e}"),
                        );
                    }
                }
            } else {
                None
            };
            if config.source.redact_on_forget {
                if let Err(e) = apply_redactions(&prepared_redactions) {
                    if let Some(dir) = manifest_dir.as_ref() {
                        let _ = restore_pending_redactions(&vault_root, dir);
                    }
                    return super::signed::aborted(
                        ResponseVerb::Forget,
                        format!("apply source redactions: {e}"),
                    );
                }
            }

            // Append body-free `source_forget` consent events inside a
            // single transaction so the journal is consistent.
            let events = match build_source_forget_events(&source_event_seeds, &op_id_str) {
                Ok(events) => events,
                Err(e) => {
                    if let Some(dir) = manifest_dir.as_ref() {
                        let _ = restore_pending_redactions(&vault_root, dir);
                    }
                    return super::signed::aborted(
                        ResponseVerb::Forget,
                        format!("build source events: {e}"),
                    );
                }
            };
            if !events.is_empty() {
                let events_for_tx = events.clone();
                let tx_result = ctx
                    .store
                    .with_tx(move |tx| {
                        for event in &events_for_tx {
                            tx.append_consent_event(event)?;
                        }
                        Ok(())
                    })
                    .await;
                if let Err(e) = tx_result {
                    if let Some(dir) = manifest_dir.as_ref() {
                        let _ = restore_pending_redactions(&vault_root, dir);
                    }
                    return super::signed::aborted(
                        ResponseVerb::Forget,
                        format!("append source_forget events: {e}"),
                    );
                }
            }

            if let Some(dir) = manifest_dir.as_ref() {
                if let Err(e) = cleanup_pending_redactions(dir) {
                    tracing::warn!(
                        error = %e,
                        dir = %dir.display(),
                        "forget: cleanup of staged source-redaction manifest failed"
                    );
                }
            }

            let data = ForgetData {
                deleted_count: outcome.deleted_count,
                plan_ref: None,
                tombstones: Some(
                    outcome
                        .tombstones
                        .into_iter()
                        .map(|id| Ulid(id.as_str().to_owned()))
                        .collect(),
                ),
            };
            super::signed::committed(
                ResponseVerb::Forget,
                operation_id,
                ResponseData::Forget(data),
                Vec::new(),
            )
        }
        Err(cairn_store_sqlite::StoreError::NotFound { id }) => not_found_response(
            ResponseVerb::Forget,
            "record",
            &format!("record not found: {id}"),
        ),
        Err(e) => super::signed::aborted(ResponseVerb::Forget, format!("store forget: {e}")),
    }
}

#[derive(Debug, Clone)]
struct SourceEventSeed {
    source_hash: String,
    originating_agent: cairn_core::domain::Identity,
    visibility: cairn_core::domain::MemoryVisibility,
    visibility_wire: String,
}

async fn collect_source_event_seeds(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    record_id: &RecordId,
) -> anyhow::Result<Vec<SourceEventSeed>> {
    let Some(record) = store.get(record_id).await.map_err(anyhow::Error::msg)? else {
        return Ok(Vec::new());
    };
    let version_rows = store
        .versions(&record.target_id)
        .await
        .map_err(anyhow::Error::msg)?;
    let mut seen = BTreeSet::new();
    let mut seeds = Vec::new();
    for version in &version_rows {
        let Some(rec) = store
            .get(&version.record_id)
            .await
            .map_err(anyhow::Error::msg)?
        else {
            continue;
        };
        if !seen.insert(rec.provenance.source_hash.clone()) {
            continue;
        }
        seeds.push(SourceEventSeed {
            source_hash: rec.provenance.source_hash.clone(),
            originating_agent: rec.provenance.originating_agent_id.clone(),
            visibility: rec.visibility,
            visibility_wire: rec.visibility.as_str().to_owned(),
        });
    }
    Ok(seeds)
}

fn build_source_forget_events(
    seeds: &[SourceEventSeed],
    operation_id: &str,
) -> anyhow::Result<Vec<ConsentEvent>> {
    let decided_at = Rfc3339Timestamp::parse(cairn_core::time::now_rfc3339_seconds())
        .map_err(anyhow::Error::msg)?;
    let mut events = Vec::with_capacity(seeds.len());
    for (idx, seed) in seeds.iter().enumerate() {
        events.push(ConsentEvent {
            consent_id: format!("source-{operation_id}-{idx}"),
            kind: ConsentKind::SourceForget,
            actor: seed.originating_agent.clone(),
            subject: seed.source_hash.clone(),
            scope: seed.visibility_wire.clone(),
            op_id: Some(operation_id.to_owned()),
            sensor_id: None,
            payload: ConsentPayload::IntentReceipt {
                target_id_hash: seed.source_hash.clone(),
                scope_tier: seed.visibility,
                reason_code: "record_forget".to_owned(),
            },
            decided_at: decided_at.clone(),
            expires_at: None,
        });
    }
    Ok(events)
}

#[derive(Debug)]
struct PreparedRedaction {
    path: PathBuf,
    original_bytes: Vec<u8>,
    replacement: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingSourceRedactionManifest {
    op_id: String,
    target_hash: String,
    expected_event_count: usize,
    files: Vec<PendingSourceRedactionFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingSourceRedactionFile {
    source_rel: String,
    backup_rel: String,
}

fn prepare_redactions(
    source_root: &Path,
    source_hashes: &BTreeSet<String>,
) -> anyhow::Result<Vec<PreparedRedaction>> {
    let mut out = Vec::new();
    if source_hashes.is_empty() || !source_root.exists() {
        return Ok(out);
    }
    collect_matching_sources(source_root, source_root, source_hashes, &mut out)?;
    Ok(out)
}

fn collect_matching_sources(
    source_root: &Path,
    dir: &Path,
    source_hashes: &BTreeSet<String>,
    out: &mut Vec<PreparedRedaction>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_matching_sources(source_root, &path, source_hashes, out)?;
        } else if path.is_file() {
            let bytes = fs::read(&path)?;
            let hash = format!("sha256:{:x}", Sha256::digest(&bytes));
            if source_hashes.contains(&hash) {
                let replacement = redaction_stub(source_root, &path, &hash, bytes.len());
                out.push(PreparedRedaction {
                    path,
                    original_bytes: bytes,
                    replacement,
                });
            }
        }
    }
    Ok(())
}

fn redaction_stub(source_root: &Path, path: &Path, source_hash: &str, size: usize) -> String {
    let relative = path
        .strip_prefix(source_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    format!(
        "source_hash: {source_hash}\nsource_id: {relative}\nsize: {size}\nmime_type: {}\nredacted_at: {}\n",
        guess_mime_type(path),
        cairn_core::time::now_rfc3339_seconds()
    )
}

fn guess_mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("md" | "txt" | "json" | "yaml" | "yml" | "csv") => "text/plain",
        _ => "application/octet-stream",
    }
}

fn redaction_recovery_root(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("source-redactions")
}

fn stage_pending_redactions(
    vault_root: &Path,
    operation_id: &str,
    target_hash: &str,
    expected_event_count: usize,
    redactions: &[PreparedRedaction],
) -> anyhow::Result<Option<PathBuf>> {
    if redactions.is_empty() {
        return Ok(None);
    }
    let op_dir = redaction_recovery_root(vault_root).join(operation_id);
    let backup_dir = op_dir.join("backups");
    fs::create_dir_all(&backup_dir)?;

    let mut files = Vec::with_capacity(redactions.len());
    for (idx, redaction) in redactions.iter().enumerate() {
        let backup_name = format!("{idx}.bak");
        let backup_path = backup_dir.join(&backup_name);
        fs::write(&backup_path, &redaction.original_bytes)?;
        let source_rel = redaction
            .path
            .strip_prefix(vault_root)
            .unwrap_or(&redaction.path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(PendingSourceRedactionFile {
            source_rel,
            backup_rel: format!("backups/{backup_name}"),
        });
    }

    let manifest = PendingSourceRedactionManifest {
        op_id: operation_id.to_owned(),
        target_hash: target_hash.to_owned(),
        expected_event_count,
        files,
    };
    fs::write(
        op_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(Some(op_dir))
}

fn apply_redactions(redactions: &[PreparedRedaction]) -> anyhow::Result<()> {
    for redaction in redactions {
        fs::write(&redaction.path, &redaction.replacement)?;
    }
    Ok(())
}

fn restore_pending_redactions(vault_root: &Path, op_dir: &Path) -> anyhow::Result<()> {
    let manifest: PendingSourceRedactionManifest =
        serde_json::from_slice(&fs::read(op_dir.join("manifest.json"))?)?;
    for file in &manifest.files {
        let source_path = vault_root.join(&file.source_rel);
        let backup_path = op_dir.join(&file.backup_rel);
        fs::write(source_path, fs::read(backup_path)?)?;
    }
    cleanup_pending_redactions(op_dir)
}

fn cleanup_pending_redactions(op_dir: &Path) -> anyhow::Result<()> {
    if op_dir.exists() {
        fs::remove_dir_all(op_dir)?;
    }
    Ok(())
}

/// Recover any crash-stranded strict source-redaction work for `vault_root`.
///
/// If a previous `forget --record` rewrote source files but died before the
/// matching `source_forget` consent events committed, this restores the
/// originals from the staged backups. If the events committed and only
/// cleanup was interrupted, this removes the stale recovery directory.
///
/// # Errors
///
/// Returns an error if the recovery directory exists but cannot be read,
/// or if a manifest is malformed.
pub fn reconcile_pending_source_redactions(vault_root: &Path) -> anyhow::Result<()> {
    let recovery_root = redaction_recovery_root(vault_root);
    if !recovery_root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&recovery_root)? {
        let entry = entry?;
        let op_dir = entry.path();
        if !op_dir.is_dir() {
            continue;
        }
        let manifest_path = op_dir.join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: PendingSourceRedactionManifest =
            serde_json::from_slice(&fs::read(&manifest_path)?)?;
        if redaction_commit_persisted(vault_root, &manifest)? {
            cleanup_pending_redactions(&op_dir)?;
        } else {
            restore_pending_redactions(vault_root, &op_dir)?;
        }
    }
    Ok(())
}

fn redaction_commit_persisted(
    vault_root: &Path,
    manifest: &PendingSourceRedactionManifest,
) -> anyhow::Result<bool> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    if !db_path.is_file() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open(db_path)?;
    let source_forget_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM consent_journal WHERE op_id = ?1 AND kind = 'source_forget'",
        params![manifest.op_id],
        |row| row.get(0),
    )?;
    let expected = i64::try_from(manifest.expected_event_count).map_err(anyhow::Error::msg)?;
    Ok(source_forget_rows == expected)
}

fn response_operation_id(operation_id: &cairn_core::wal::OperationId) -> Result<Ulid, String> {
    const PREFIX: &str = "forget_record-";
    let raw = operation_id.as_str();
    let Some(ulid) = raw.strip_prefix(PREFIX) else {
        return Err(format!("unexpected forget wal operation id: {raw}"));
    };
    RecordId::parse(ulid.to_owned())
        .map_err(|e| format!("invalid forget wal operation id `{raw}`: {e}"))?;
    Ok(Ulid(ulid.to_owned()))
}

fn emit_response(resp: &Response, json: bool, requested_record_id: &str) {
    if json {
        emit_json(resp);
        return;
    }
    match resp.status {
        ResponseStatus::Committed => {
            if let Some(ResponseData::Forget(data)) = resp.data.as_ref() {
                println!(
                    "cairn forget: committed record {requested_record_id} (deleted {})",
                    data.deleted_count
                );
            } else {
                println!("cairn forget: committed record {requested_record_id}");
            }
        }
        ResponseStatus::Rejected | ResponseStatus::Aborted => {
            let code = super::signed::response_error_code(resp).unwrap_or("Internal");
            let message = resp
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("forget failed");
            human_error("forget", code, message, &resp.operation_id);
        }
        _ => human_error(
            "forget",
            "Internal",
            "unknown response status",
            &resp.operation_id,
        ),
    }
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => ExitCode::from(64),
        _ => ExitCode::FAILURE,
    }
}

trait ForgetOutcomeExt {
    fn target_hash_for_manifest(&self) -> &str;
}

impl ForgetOutcomeExt for cairn_store_sqlite::record_wal::forget::ForgetOutcome {
    fn target_hash_for_manifest(&self) -> &str {
        // ForgetOutcome on main carries operation_id but not target_hash;
        // we only need an opaque label for the manifest. Reuse the
        // operation_id's str slice — guaranteed stable across the manifest
        // lifetime and never displayed to users.
        self.operation_id.as_str()
    }
}
