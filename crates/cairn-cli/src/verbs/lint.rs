//! `cairn lint` handler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::folder::{
    FolderPolicy, aggregate_folders, materialize_backlinks, parse_policy, project_index,
};
use cairn_core::domain::projection::MarkdownProjector;
use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Result of a `lint --fix-markdown` run.
#[derive(Debug, serde::Serialize)]
pub struct FixMarkdownResult {
    /// Vault-relative paths that were written or updated.
    pub written: Vec<PathBuf>,
    /// Number of files that were already up to date.
    pub already_current: usize,
}

/// Project all active records to markdown, writing files that are missing or stale.
///
/// `vault_root`: absolute path to the vault root (files written relative to this).
/// Returns a `FixMarkdownResult` on success.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, or if any file I/O fails.
pub async fn fix_markdown_handler(
    store: &dyn MemoryStore,
    vault_root: &Path,
) -> anyhow::Result<FixMarkdownResult> {
    let projector = MarkdownProjector;
    let records = store
        .list_active_stored(&cairn_core::contract::memory_store::ListArgs::default())
        .await
        .map_err(anyhow::Error::msg)
        .context("store: list_active_stored")?;
    let mut written = Vec::new();
    let mut already_current: usize = 0;

    for stored in records {
        let projected = projector.project(&stored);
        let abs_path = vault_root.join(&projected.path);

        let needs_write = match tokio::fs::read_to_string(&abs_path).await {
            Ok(existing) => existing != projected.content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(anyhow::anyhow!("cannot read {}: {e}", abs_path.display())),
        };

        if needs_write {
            if let Some(parent) = abs_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            // Write atomically via a unique temp file + rename. tempfile::Builder
            // assigns a random suffix so concurrent calls in the same process never
            // share a temp path; rename(2) is atomic for readers.
            let content = projected.content.clone();
            let dest = abs_path.clone();
            let parent_buf = abs_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            tokio::task::spawn_blocking(move || {
                use std::io::Write as _;
                let mut tmp = tempfile::Builder::new()
                    .suffix(".md.tmp")
                    .tempfile_in(&parent_buf)
                    .with_context(|| format!("create temp file in {}", parent_buf.display()))?;
                tmp.write_all(content.as_bytes())
                    .with_context(|| format!("write temp {}", tmp.path().display()))?;
                tmp.persist(&dest).map_err(|e| {
                    anyhow::anyhow!("persist temp -> {}: {}", dest.display(), e.error)
                })?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .with_context(|| format!("spawn_blocking write {}", abs_path.display()))??;
            written.push(projected.path);
        } else {
            already_current += 1;
        }
    }

    Ok(FixMarkdownResult {
        written,
        already_current,
    })
}

/// Result of a `lint --fix-folders` run.
#[derive(Debug, serde::Serialize)]
pub struct FixFoldersResult {
    /// Folder index files written or updated (vault-relative).
    pub written: Vec<PathBuf>,
    /// Number of indexes that already matched their projection.
    pub unchanged: usize,
    /// Per-policy parse failures; subtree was skipped.
    pub policy_errors: Vec<PolicyError>,
}

/// One `_policy.yaml` that failed to parse.
#[derive(Debug, serde::Serialize)]
pub struct PolicyError {
    /// Vault-relative path of the offending file.
    pub path: PathBuf,
    /// Human-readable reason.
    pub reason: String,
}

/// Walk the store, build folder states, project `_index.md` files, write
/// atomically. A bad `_policy.yaml` does not abort — that subtree is
/// skipped, the error is recorded.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, or if any non-policy
/// I/O fails.
pub async fn fix_folders_handler(
    store: &dyn MemoryStore,
    vault_root: &Path,
) -> anyhow::Result<FixFoldersResult> {
    let projector = MarkdownProjector;
    let records = store
        .list_active_stored(&cairn_core::contract::memory_store::ListArgs::default())
        .await
        .map_err(anyhow::Error::msg)
        .context("store: list_active_stored")?;

    // 1. Build record_paths from MarkdownProjector — same shape used by
    //    --fix-markdown, so callers get a coherent view.
    let mut record_paths: BTreeMap<cairn_core::domain::record::RecordId, PathBuf> = BTreeMap::new();
    for stored in &records {
        let pf = projector.project(stored);
        record_paths.insert(stored.record.id.clone(), pf.path);
    }

    // 2. Walk vault for files named `_policy.yaml`. Folders whose policy
    //    failed to parse are returned as `tainted_dirs`; per brief invariant 6
    //    (fail-closed) we drop every record under those subtrees so the
    //    handler does not silently fall back to default policy below a
    //    misconfigured folder.
    let (policies_by_dir, policy_errors, tainted_dirs) = collect_policies(vault_root).await?;

    if !tainted_dirs.is_empty() {
        record_paths.retain(|_, path| !tainted_dirs.iter().any(|bad| path.starts_with(bad)));
    }

    // 3. Reverse-map backlinks.
    let backlinks_by_target = materialize_backlinks(&records, &record_paths);

    // 4. Aggregate. `aggregate_folders` looks up each record's path in
    //    `record_paths` and silently skips records with no entry, so dropping
    //    tainted entries from the map is sufficient — no parallel filter on
    //    `records` is needed.
    let states = aggregate_folders(
        &records,
        &record_paths,
        &policies_by_dir,
        &backlinks_by_target,
    );

    // 5. Write each `_index.md` atomically. We delegate the symlink-rejection
    //    + atomic-persist sequence to `vault::bootstrap::write_once` (force=true)
    //    so the same lstat-then-rename guarantees protect both bootstrap and
    //    `lint --fix-folders`. The "unchanged" semantic (skip when content
    //    already matches) lives here, because `write_once` does not compare
    //    bytes — under `force=true` it always overwrites.
    let mut written = Vec::new();
    let mut unchanged = 0usize;
    for state in states {
        let projected = project_index(&state);
        let abs = vault_root.join(&projected.path);
        // No-follow validation BEFORE the read.  `read_to_string` follows
        // symlinks; if `_index.md` is a symlink to an external file with
        // matching content, the unchanged branch would silently bypass the
        // symlink rejection that lives inside `write_once`.  Run the same
        // lstat-based parent + target check up front, on every iteration.
        {
            let dest = abs.clone();
            let vroot = vault_root.to_path_buf();
            tokio::task::spawn_blocking(move || {
                crate::vault::bootstrap::check_write_safe(&vroot, &dest)
            })
            .await
            .with_context(|| format!("spawn_blocking validate {}", abs.display()))??;
        }
        // Byte-compare, NOT `read_to_string`.  A pre-existing `_index.md`
        // containing non-UTF-8 bytes (corruption, manual binary write,
        // partially-written tempfile that survived a crash) would otherwise
        // surface as `InvalidData` and abort the entire rebuild — exactly
        // the recovery case `lint --fix-folders` should handle.
        let needs_write = match tokio::fs::read(&abs).await {
            Ok(existing) => existing != projected.content.as_bytes(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(anyhow::anyhow!("cannot read {}: {e}", abs.display())),
        };
        if !needs_write {
            unchanged += 1;
            continue;
        }
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
        let content = projected.content.clone();
        let dest = abs.clone();
        let vroot = vault_root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            // `write_once` lstat-checks every ancestor + final target for
            // symlinks, writes a randomly-named tempfile in the same
            // directory, and atomically renames into place. The
            // created/skipped vecs are populated for bootstrap's receipt;
            // we discard them here because the surrounding read-and-compare
            // loop already classifies writes as `written` or `unchanged`.
            let mut created = Vec::new();
            let mut skipped = Vec::new();
            crate::vault::bootstrap::write_once(
                &vroot,
                &dest,
                &content,
                true,
                &mut created,
                &mut skipped,
            )
        })
        .await
        .with_context(|| format!("spawn_blocking write {}", abs.display()))??;
        written.push(projected.path);
    }

    Ok(FixFoldersResult {
        written,
        unchanged,
        policy_errors,
    })
}

/// Walk `vault_root` for `_policy.yaml` files and parse them.
///
/// Returns the parsed policies keyed by their containing directory, the list
/// of [`PolicyError`] entries for files that failed to parse, and a list of
/// tainted directories — folders whose `_policy.yaml` was unparseable. The
/// caller must skip every record under a tainted directory (brief invariant 6,
/// fail-closed): defaulting silently below a broken policy would let writes
/// land outside the configured `allowed_kinds`/`visibility_default`.
async fn collect_policies(
    vault_root: &Path,
) -> anyhow::Result<(
    BTreeMap<PathBuf, FolderPolicy>,
    Vec<PolicyError>,
    Vec<PathBuf>,
)> {
    let mut policies_by_dir: BTreeMap<PathBuf, FolderPolicy> = BTreeMap::new();
    let mut policy_errors: Vec<PolicyError> = Vec::new();
    let mut tainted_dirs: Vec<PathBuf> = Vec::new();
    // Skip hidden subdirectories (e.g. `.cairn/`, `.git/`) but never reject
    // the vault root itself — `tempfile::tempdir()` and similar tools
    // commonly produce dot-prefixed root paths.
    let walker = walkdir::WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden_dir(e));
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", vault_root.display()))?;
        // Match by name first.  `follow_links(false)` reports a symlinked
        // `_policy.yaml` as a non-file; if we filtered on `is_file()` first
        // we would silently skip it and fall back to inherited/default
        // policy below — exactly the fail-open hole brief invariant 6
        // forbids.  Treat any non-regular `_policy.yaml` (symlink, fifo,
        // directory) as a policy error and taint its containing folder.
        if entry.file_name() != "_policy.yaml" {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(vault_root)
            .with_context(|| format!("strip_prefix {}", abs.display()))?
            .to_path_buf();
        let dir = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        if !entry.file_type().is_file() {
            let kind = if entry.file_type().is_symlink() {
                "symlink"
            } else if entry.file_type().is_dir() {
                "directory"
            } else {
                "non-regular file"
            };
            policy_errors.push(PolicyError {
                path: rel,
                reason: format!("_policy.yaml is a {kind} — refusing to follow"),
            });
            tainted_dirs.push(dir);
            continue;
        }
        // Read raw bytes — `read_to_string` would return InvalidData on
        // non-UTF-8 and the `?` would abort the whole rebuild. Decode here
        // so a non-UTF-8 file taints only its own subtree (brief invariant 6,
        // fail-closed), exactly like a YAML parse failure.
        let bytes = tokio::fs::read(&abs)
            .await
            .with_context(|| format!("read {}", abs.display()))?;
        let text = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                policy_errors.push(PolicyError {
                    path: rel,
                    reason: format!("_policy.yaml is not valid UTF-8: {e}"),
                });
                tainted_dirs.push(dir);
                continue;
            }
        };
        match parse_policy(&text) {
            Ok(p) => {
                policies_by_dir.insert(dir, p);
            }
            // `FolderError` is `#[non_exhaustive]`; treat any current or
            // future variant as a non-fatal policy error so the run
            // continues and the offending subtree is skipped.
            Err(e) => {
                policy_errors.push(PolicyError {
                    path: rel,
                    reason: e.to_string(),
                });
                tainted_dirs.push(dir);
            }
        }
    }
    Ok((policies_by_dir, policy_errors, tainted_dirs))
}

fn is_hidden_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s != ".")
}

/// Result of a `lint` default-path run.
#[derive(Debug)]
pub struct LintHandlerResult {
    /// The structured findings + summary (ready for JSON serialization).
    pub data: cairn_core::generated::verbs::lint::LintData,
    /// Path of the written report (vault-relative), if `--write-report` was set.
    pub report_path: Option<PathBuf>,
    /// Whether any error-severity finding was emitted (drives exit code).
    pub has_error: bool,
}

/// Run `cairn lint` (default path — no `--fix-*`).
///
/// Builds a `LintInputs` snapshot from the store, runs the pure check
/// engine, and (when `write_report` is true) atomically writes
/// `.cairn/lint-report.md` under `vault_root`.
///
/// `schema_version` is the runtime's contract major.minor pair. Today
/// every record runs through the legacy `consent_model` gate (see
/// `cairn-core::verbs::lint::ConsentModel::LegacyEvent`); per-row gating
/// arrives with #253.
///
/// # Errors
///
/// Returns an error if the store cannot be queried or if writing the
/// report fails.
pub async fn lint_handler(
    store: &dyn cairn_core::contract::memory_store::MemoryStore,
    identity_registry: &dyn cairn_core::contract::identity_registry::IdentityRegistry,
    config: &cairn_core::config::CairnConfig,
    schema_version: cairn_core::verbs::lint::SchemaVersion,
    write_report: bool,
    vault_root: &Path,
) -> anyhow::Result<LintHandlerResult> {
    use cairn_core::contract::memory_store::ListArgs;
    use cairn_core::verbs::lint::{ConsentModel, LintInputs, LintRecord, run_checks};

    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .map_err(|e| anyhow::anyhow!("store: list_active_stored: {e}"))
        .context("lint: list_active_stored")?;

    // §6.2 author-lifecycle slice (issue #256). The registry is required
    // (no `Option`) so a caller cannot silently degrade lint into a
    // no-op against revoked / unknown / pending issuers. A registry
    // backend error is an infrastructure fault, not a per-record
    // finding — but aborting the whole lint run on one transient
    // hiccup also strips operators of every other check's coverage at
    // exactly the moment they need partial visibility. Degraded path:
    // per-identity isolation. One identity's lookup failure is
    // confined to that identity: it lands in `unresolvable_authors`
    // (the §6.2 leaf suppresses synthetic `MissingFromRegistry` for
    // those, treating their state as explicitly unavailable rather
    // than absent) and we emit a per-identity `DeferredCheck` Error
    // finding pinning the cause. Successfully fetched states still
    // drive lifecycle classification on every other record. This
    // prevents a single registry hiccup from poisoning the whole
    // vault into uniform false `BrokenActorChain` errors.
    let (author_states, prefetch_failures) =
        prefetch_author_states(identity_registry, &stored).await;

    // `index_stats` is opt-in for adapters: the default trait impl returns
    // an `Err` carrying the literal "not supported by this store adapter"
    // marker. We only swallow that exact case (so the §6.7 `index_drift`
    // check downgrades to a deferred-info finding instead of aborting the
    // whole lint run). Real operational failures from an adapter that
    // *does* support `index_stats` (DB I/O, missing FTS table, worker
    // crash) propagate, because hiding them would convert a real
    // index-corruption signal into a falsely clean run — exactly the
    // posture §6.7 is meant to expose.
    let stored_count = u64::try_from(stored.len()).unwrap_or(u64::MAX);
    let (index_stats, index_stats_skipped) = match store.index_stats().await {
        Ok(s) => (s, false),
        Err(e)
            if e.to_string()
                .contains("not supported by this store adapter") =>
        {
            (
                cairn_core::contract::memory_store::IndexStats::new(stored_count, stored_count),
                true,
            )
        }
        Err(e) => {
            return Err(anyhow::anyhow!("store: index_stats: {e}")).context("lint: index_stats");
        }
    };

    // PR-1: every row carries LegacyEvent. Per-record gating lands in #253.
    let lint_records: Vec<LintRecord> = stored
        .into_iter()
        .map(|s| LintRecord {
            stored: s,
            consent_model: ConsentModel::LegacyEvent,
        })
        .collect();

    let unresolvable_authors: std::collections::HashSet<cairn_core::domain::Identity> =
        prefetch_failures.keys().cloned().collect();
    let inputs = LintInputs {
        records: &lint_records,
        config,
        index_stats,
        schema_version,
        author_states: &author_states,
        unresolvable_authors: &unresolvable_authors,
    };
    let mut data = run_checks(&inputs);

    if index_stats_skipped {
        push_index_stats_skipped(&mut data);
    }

    for (id, err) in &prefetch_failures {
        push_registry_unavailable(&mut data, Some(id), err);
    }

    push_section_6_2_advisories(&mut data, &lint_records);

    let has_error = data.findings.iter().any(|f| {
        matches!(
            f.severity,
            cairn_core::generated::verbs::lint::Severity::Error,
        )
    });

    let report_path = if write_report {
        let body = cairn_core::verbs::lint::report::render(&data);
        let rel = PathBuf::from(".cairn/lint-report.md");
        let abs = vault_root.join(&rel);
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
        let parent = abs
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let dest = abs.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            use std::io::Write as _;
            let mut tmp = tempfile::Builder::new()
                .suffix(".md.tmp")
                .tempfile_in(&parent)
                .with_context(|| format!("create temp file in {}", parent.display()))?;
            tmp.write_all(body.as_bytes())
                .with_context(|| format!("write temp {}", tmp.path().display()))?;
            tmp.persist(&dest)
                .map_err(|e| anyhow::anyhow!("persist temp -> {}: {}", dest.display(), e.error))?;
            Ok(())
        })
        .await
        .with_context(|| format!("spawn_blocking write {}", abs.display()))??;
        data.report_path = Some(rel.display().to_string());
        Some(rel)
    } else {
        None
    };

    Ok(LintHandlerResult {
        data,
        report_path,
        has_error,
    })
}

/// Pre-fetch the `ProvisioningState` of every distinct chain-author
/// identity under `IdentityVisibility::Audit` so revoked / purged
/// states surface. Per-identity failure isolation: a backend error on
/// one identity must not contaminate every other record's verdict, so
/// failures land in the second map (`identity -> error string`)
/// instead of aborting prefetch. Successful lookups land in the first
/// map; identities the registry returns `None` for are omitted (the
/// §6.2 leaf treats absence as `MissingFromRegistry` →
/// `BrokenActorChain` Error). Identities in the failure map are fed
/// to the leaf via `LintInputs.unresolvable_authors` so it suppresses
/// the synthetic `MissingFromRegistry` finding for them — a
/// `DeferredCheck` Error gets emitted per-identity at the cli layer
/// instead, pinning the actual cause.
async fn prefetch_author_states(
    registry: &dyn cairn_core::contract::identity_registry::IdentityRegistry,
    stored: &[cairn_core::contract::memory_store::StoredRecord],
) -> (
    std::collections::HashMap<
        cairn_core::domain::Identity,
        cairn_core::pipeline::lint::author_lifecycle::AuthorLifecycle,
    >,
    std::collections::HashMap<cairn_core::domain::Identity, String>,
) {
    use cairn_core::contract::identity_registry::IdentityVisibility;
    use cairn_core::domain::ChainRole;
    use cairn_core::domain::Identity;
    use cairn_core::domain::Rfc3339Timestamp;
    use cairn_core::pipeline::lint::author_lifecycle::AuthorLifecycle;
    use std::collections::{HashMap, HashSet};

    let mut unique: HashSet<Identity> = HashSet::new();
    for s in stored {
        if let Some(e) = s
            .record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
        {
            unique.insert(e.identity.clone());
        }
    }
    let mut map: HashMap<Identity, AuthorLifecycle> = HashMap::with_capacity(unique.len());
    let mut failures: HashMap<Identity, String> = HashMap::new();
    for id in unique {
        match registry.get_identity(&id, IdentityVisibility::Audit).await {
            Ok(Some(rec)) => {
                // Convert chrono timestamps to the core's Rfc3339
                // newtype so the check stays in `cairn-core` without
                // a chrono dep. `to_rfc3339` always emits a valid
                // form so the parse cannot fail in practice; `.ok()`
                // keeps lint defensive.
                let activated_at = rec
                    .activated_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok());
                let revoked_at = rec
                    .revoked_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok());
                map.insert(
                    id,
                    AuthorLifecycle {
                        state: rec.provisioning_state,
                        activated_at,
                        revoked_at,
                    },
                );
            }
            Ok(None) => {} // genuinely absent — leaf surfaces MissingFromRegistry
            Err(e) => {
                failures.insert(id, format!("{e}"));
            }
        }
    }
    (map, failures)
}

/// Append a `deferred_check` info finding noting that `MemoryStore::index_stats`
/// is unavailable on this adapter, and keep all summary aggregates
/// (`total`, `by_severity.info`, `by_kind["deferred_check"]`) consistent.
fn push_index_stats_skipped(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: "store adapter does not implement index_stats; §6.7 index_drift skipped"
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Info,
        suggested_fix: Some(
            "ship MemoryStore::index_stats on this adapter to enable index_drift coverage"
                .to_owned(),
        ),
        target: None,
        tracking_issue: None,
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.info += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("deferred_check".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Emit the §6.2 honesty advisories pinning what the leaf's "clean"
/// verdict does *not* assert. The leaf validates chain-shape +
/// lifecycle classification — it does *not* verify
/// `record.signature` cryptographically and does *not* recompute the
/// body integrity hash. Anyone who can mutate a stored row can
/// rewrite the body while leaving `actor_chain` shape + an Active
/// author id intact and slip past §6.2. Real Ed25519 verification is
/// P1+ (needs `key_version` persistence + canonical-payload spec).
/// Two advisories: one always-on signature-verification deferral, one
/// conditional sensor-author-bypass deferral.
fn push_section_6_2_advisories(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    lint_records: &[cairn_core::verbs::lint::LintRecord],
) {
    if !lint_records.is_empty() {
        push_signature_verification_deferred(data, lint_records.len());
    }
    let sensor_authored_count = count_sensor_authored(lint_records);
    if sensor_authored_count > 0 {
        push_sensor_author_unverified(data, sensor_authored_count);
    }
}

/// Append a `deferred_check` Info finding pinning what §6.2's
/// "clean" verdict does *and does not* assert. The leaf currently
/// validates chain-shape + lifecycle classification — it does *not*
/// verify `record.signature` cryptographically and does *not*
/// recompute a body integrity hash. Without this advisory, an
/// operator could read a clean §6.2 as "the at-rest signature was
/// verified," and a tampered body under an active author would slip
/// past silently. Real Ed25519 verification is P1+.
fn push_signature_verification_deferred(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    record_count: usize,
) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: format!(
            "§6.2 ran chain-shape + lifecycle classification across {record_count} record(s); record.signature was NOT cryptographically verified and target_hash was NOT recomputed at P0 — a clean verdict means shape + author state pass, not that the at-rest body or signature is unforgeable"
        ),
        severity: cairn_core::generated::verbs::lint::Severity::Info,
        suggested_fix: Some(
            "ship Ed25519 signature verification + canonical-payload body-hash recompute in P1; \
             these require key_version persistence and a canonical-serialization spec, both \
             outside the PR-1 scope"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(256),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.info += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("deferred_check".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// §6.2 sensor-author carve-out: `sensor_observation` records authored
/// by their own `provenance.source_sensor` legitimately bypass the
/// human/agent lifecycle state machine (sensors are not in
/// `IdentityRegistry`). At P0 the binding rests on
/// `MemoryRecord::validate`'s shape-equality check — the at-rest
/// signature is *not* cryptographically verified to come from that
/// sensor's signing key. Count records that hit the carve-out so the
/// cli can surface the coverage gap as a single aggregate
/// `DeferredCheck` Info finding.
fn count_sensor_authored(lint_records: &[cairn_core::verbs::lint::LintRecord]) -> usize {
    use cairn_core::domain::{ChainRole, IdentityKind, MemoryKind};
    lint_records
        .iter()
        .filter(|r| {
            r.stored.record.kind == MemoryKind::SensorObservation
                && r.stored
                    .record
                    .actor_chain
                    .iter()
                    .find(|e| e.role == ChainRole::Author)
                    .is_some_and(|e| {
                        e.identity.kind() == IdentityKind::Sensor
                            && e.identity == r.stored.record.provenance.source_sensor
                    })
        })
        .count()
}

/// Append a `deferred_check` Info finding noting that N
/// sensor-authored `sensor_observation` records passed the §6.2
/// carve-out on shape-equality alone — the at-rest signature is *not*
/// cryptographically verified to come from that sensor's signing key.
/// P1+ ships real Ed25519 verification + per-sensor key attestation;
/// until then, operators must see this gap explicitly.
fn push_sensor_author_unverified(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    count: usize,
) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: format!(
            "{count} sensor-authored sensor_observation record(s) bypassed the §6.2 author-lifecycle classification on shape-equality (kind == sensor_observation && author == provenance.source_sensor); cryptographic binding of the sensor identity to its at-rest signature is P1+"
        ),
        severity: cairn_core::generated::verbs::lint::Severity::Info,
        suggested_fix: Some(
            "ship Ed25519 verification + per-sensor key attestation in P1; until then the sensor \
             carve-out trusts MemoryRecord::validate's shape check rather than a cryptographic \
             proof of origin"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(256),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.info += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("deferred_check".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Append a `deferred_check` Error finding noting that the
/// `IdentityRegistry` lookup failed for a specific identity. Per
/// identity isolation: one backend hiccup must not contaminate every
/// other record's §6.2 verdict, so the cli emits one finding per
/// failing identity (pinning the actual cause) and the §6.2 leaf
/// suppresses synthetic `MissingFromRegistry` for those identities.
/// `identity` is `None` only in legacy unit tests that pre-date
/// per-identity isolation; production paths always supply one.
fn push_registry_unavailable(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    identity: Option<&cairn_core::domain::Identity>,
    err: &str,
) {
    let id_label = identity.map(|id| format!(" for {id}")).unwrap_or_default();
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: format!(
            "IdentityRegistry lookup failed{id_label}; §6.2 author-lifecycle classification deferred for records authored by this identity (synthetic MissingFromRegistry suppressed to avoid masking the real cause): {err}"
        ),
        severity: cairn_core::generated::verbs::lint::Severity::Error,
        suggested_fix: Some(
            "investigate the registry backend (transient I/O, schema drift, lock contention) and \
             re-run lint once it is reachable; the partial report still surfaces shape and \
             other-check findings"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(256),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.error += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("deferred_check".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Run `cairn lint`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix_markdown = sub.get_flag("fix-markdown");
    let fix_folders = sub.get_flag("fix-folders");

    if fix_markdown || fix_folders {
        // TODO(#46): wire SQLite store. When live, dispatch should call:
        //   - fix_markdown_handler(&store, &vault_root) when fix_markdown
        //   - fix_folders_handler(&store, &vault_root) when fix_folders
        // Both handlers are already implemented and exercised via FixtureStore in
        // crates/cairn-cli/tests/{resync,lint_folders}.rs.
        let resp = unimplemented_response(ResponseVerb::Lint);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "lint",
                "Internal",
                "store not wired in this P0 build — --fix-folders requires #46",
                &resp.operation_id,
            );
        }
        return ExitCode::FAILURE;
    }

    let resp = unimplemented_response(ResponseVerb::Lint);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "lint",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_markdown_result_counts_written_and_current() {
        // written=2 means 2 files were written/updated
        let result = FixMarkdownResult {
            written: vec!["a.md".into(), "b.md".into()],
            already_current: 3,
        };
        assert_eq!(result.written.len(), 2);
        assert_eq!(result.already_current, 3);
    }

    #[test]
    fn fix_markdown_result_empty() {
        let result = FixMarkdownResult {
            written: vec![],
            already_current: 0,
        };
        assert!(result.written.is_empty());
        assert_eq!(result.already_current, 0);
    }

    #[tokio::test]
    async fn fix_markdown_handler_writes_missing_files() {
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        let store = FixtureStore::default();
        let record = sample_record();
        store.upsert(&record).await.unwrap();

        let vault_root = tempfile::tempdir().unwrap();
        let result = fix_markdown_handler(&store, vault_root.path())
            .await
            .unwrap();

        assert_eq!(result.written.len(), 1);
        assert_eq!(result.already_current, 0);

        // Running again should report already_current=1, written=0
        let result2 = fix_markdown_handler(&store, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result2.written.len(), 0);
        assert_eq!(result2.already_current, 1);
    }

    #[tokio::test]
    async fn lint_handler_writes_report_when_requested() {
        use cairn_core::config::CairnConfig;
        use cairn_core::verbs::lint::SchemaVersion;
        use cairn_store_sqlite::SqliteIdentityRegistry;
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        let store = FixtureStore::default();
        let r = sample_record();
        store.upsert(&r).await.expect("upsert");

        // Empty registry — the sample record's author is not registered,
        // so §6.2 emits a `BrokenActorChain` Error
        // (`MissingFromRegistry`). This test scopes to report-rendering
        // and the four §6.3–§6.6 deferred-info findings; the §6.2 Error
        // path is exercised in detail by the integration tests.
        let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
        let cfg = CairnConfig::default();
        let vault = tempfile::tempdir().expect("tempdir");
        let result = lint_handler(
            &store,
            &registry,
            &cfg,
            SchemaVersion { major: 0, minor: 1 },
            true,
            vault.path(),
        )
        .await
        .expect("handler");

        let info_count = result
            .data
            .findings
            .iter()
            .filter(|f| {
                matches!(
                    f.severity,
                    cairn_core::generated::verbs::lint::Severity::Info,
                )
            })
            .count();
        assert_eq!(
            info_count, 5,
            "expect §6.3 + §6.4 + §6.5 + §6.6 deferred-info findings + §6.2 signature-verification-deferred advisory"
        );
        assert!(
            result.has_error,
            "missing-from-registry author must trip has_error"
        );
        assert_eq!(
            result.report_path.as_deref(),
            Some(std::path::Path::new(".cairn/lint-report.md"))
        );
        let body = tokio::fs::read_to_string(vault.path().join(".cairn/lint-report.md"))
            .await
            .expect("read lint-report.md");
        assert!(body.contains("# Lint report"));
    }

    #[tokio::test]
    async fn lint_handler_flags_index_drift_with_error_severity() {
        use cairn_core::config::CairnConfig;
        use cairn_core::contract::memory_store::IndexStats;
        use cairn_core::verbs::lint::SchemaVersion;
        use cairn_store_sqlite::SqliteIdentityRegistry;
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        let store = FixtureStore::default();
        store.upsert(&sample_record()).await.expect("upsert");
        // Force a drift fixture: 5 active records but FTS reports 4.
        store.set_index_stats_override(IndexStats::new(5, 4));

        let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
        let cfg = CairnConfig::default();
        let vault = tempfile::tempdir().expect("tempdir");
        let result = lint_handler(
            &store,
            &registry,
            &cfg,
            SchemaVersion { major: 0, minor: 1 },
            false,
            vault.path(),
        )
        .await
        .expect("handler");

        assert!(result.has_error);
        let drifts: Vec<_> = result
            .data
            .findings
            .iter()
            .filter(|f| matches!(f.kind, cairn_core::generated::verbs::lint::Kind::IndexDrift,))
            .collect();
        assert_eq!(drifts.len(), 1);
    }

    #[test]
    fn push_registry_unavailable_emits_blocking_deferred_finding() {
        // Round-(this loop) fix: a registry-prefetch failure must
        // surface as a visible, blocking finding inside the report
        // instead of aborting the whole lint run. This unit pins:
        // - severity is Error (operator must see it),
        // - kind is DeferredCheck (categorically a coverage gap),
        // - tracking_issue is #256,
        // - the summary aggregates (total / by_severity / by_kind)
        //   stay consistent so the json/markdown render is correct.
        let mut data = cairn_core::generated::verbs::lint::LintData {
            findings: Vec::new(),
            summary: cairn_core::generated::verbs::lint::LintDataSummary {
                total: 0,
                by_severity: cairn_core::generated::verbs::lint::LintDataSummaryBySeverity {
                    error: 0,
                    warning: 0,
                    info: 0,
                },
                by_kind: serde_json::Value::Object(serde_json::Map::new()),
            },
            report_path: None,
        };
        let id = cairn_core::domain::Identity::parse("agt:test").expect("parse identity");
        super::push_registry_unavailable(&mut data, Some(&id), "boom: connection refused");
        assert_eq!(data.findings.len(), 1);
        let f = &data.findings[0];
        assert!(matches!(
            f.kind,
            cairn_core::generated::verbs::lint::Kind::DeferredCheck
        ));
        assert_eq!(
            f.severity,
            cairn_core::generated::verbs::lint::Severity::Error
        );
        assert_eq!(f.tracking_issue, Some(256));
        assert!(f.message.contains("boom: connection refused"));
        assert_eq!(data.summary.total, 1);
        assert_eq!(data.summary.by_severity.error, 1);
        if let serde_json::Value::Object(map) = &data.summary.by_kind {
            assert_eq!(
                map.get("deferred_check")
                    .and_then(serde_json::Value::as_u64),
                Some(1)
            );
        } else {
            panic!("by_kind must be an object");
        }
    }
}
