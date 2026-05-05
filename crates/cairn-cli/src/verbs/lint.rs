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
/// The §6.4 `stale_schema` check reads the current host schema version
/// directly from `SchemaVersion::current()` (the same constant the store
/// stamps rows with), so this handler does **not** accept a
/// caller-supplied schema version. A second source of truth would let
/// callers pass `SchemaVersion::from_contract(CONTRACT_VERSION)` and
/// mark every freshly written row stale.
///
/// # Errors
///
/// Returns an error if the store cannot be queried or if writing the
/// report fails.
pub async fn lint_handler(
    store: &dyn cairn_core::contract::memory_store::MemoryStore,
    identity_registry: &dyn cairn_core::contract::identity_registry::IdentityRegistry,
    consent_lookup: Option<&dyn cairn_core::contract::consent_lookup::ConsentLookup>,
    config: &cairn_core::config::CairnConfig,
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
    let (index_stats, index_stats_skipped) = load_index_stats(store, stored.len()).await?;

    let supports_consent_model_gate = store.capabilities().per_record_consent_model;
    // Always observe `list_consent_models` so legacy-only adapters
    // (capability=false + explicit `Ok(empty)`) are distinguishable
    // from adapters that don't implement the escape hatch at all
    // (Err sentinel). Round 1 reorder: this is the documented
    // compatibility contract.
    let (consent_models, consent_models_unsupported) = load_consent_models(store).await?;

    // Keep `stored` borrowable for the post-run gap audit. Gaps in
    // `consent_models` (one-entry-per-active-row contract violation)
    // would otherwise be silently coerced to `LegacyEvent` below.
    let lint_records: Vec<LintRecord> = stored
        .iter()
        .map(|s| {
            let model = consent_models
                .get(&s.record.id)
                .copied()
                .unwrap_or(ConsentModel::LegacyEvent);
            LintRecord {
                stored: s.clone(),
                consent_model: model,
            }
        })
        .collect();

    let unresolvable_authors: std::collections::HashSet<cairn_core::domain::Identity> =
        prefetch_failures.keys().cloned().collect();
    // Round 4: prefer the store-provided consent_lookup so plugin-
    // registered adapters (which the registry only sees as
    // `Arc<dyn MemoryStore>`) can satisfy §6.5 without the caller
    // having to thread a side-channel reference. The explicit
    // `consent_lookup` parameter still wins when provided so tests
    // can inject a static fixture.
    let store_lookup = store.as_consent_lookup();
    let effective_lookup = consent_lookup.or(store_lookup);
    let inputs = LintInputs {
        records: &lint_records,
        config,
        index_stats,
        author_states: &author_states,
        unresolvable_authors: &unresolvable_authors,
        consent_lookup: effective_lookup,
    };
    let mut data = run_checks(&inputs).await;

    if index_stats_skipped {
        push_index_stats_skipped(&mut data);
    }

    // Build affected-record map per failed identity. Per-record ids
    // (stable ULIDs, not record bodies → safe under privacy invariant
    // §9) let operators identify exactly which rows lost §6.2
    // coverage. Without this, a partial outage leaves an arbitrary
    // number of records unclassified with no rows to quarantine or
    // retry.
    let affected_by_identity = affected_records_by_identity(&lint_records, &prefetch_failures);
    for (id, err) in &prefetch_failures {
        let affected = affected_by_identity
            .get(id)
            .map_or(&[][..], std::vec::Vec::as_slice);
        push_registry_unavailable(&mut data, Some(id), err, affected);
    }

    push_section_6_2_advisories(&mut data, &lint_records);

    push_consent_model_findings(
        &mut data,
        supports_consent_model_gate,
        consent_models_unsupported,
        &stored,
        &consent_models,
    );

    let has_error = data.findings.iter().any(|f| {
        matches!(
            f.severity,
            cairn_core::generated::verbs::lint::Severity::Error,
        )
    });

    let report_path = if write_report {
        Some(write_lint_report(&mut data, vault_root).await?)
    } else {
        None
    };

    Ok(LintHandlerResult {
        data,
        report_path,
        has_error,
    })
}

/// Render the lint report and persist it atomically under
/// `<vault_root>/.cairn/lint-report.md`. Pulled out of `lint_handler` so
/// the async shell stays under the workspace's `clippy::too_many_lines`
/// limit. Returns the vault-relative path that was written.
async fn write_lint_report(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    vault_root: &Path,
) -> anyhow::Result<PathBuf> {
    let body = cairn_core::verbs::lint::report::render(data);
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
    Ok(rel)
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
    use cairn_core::domain::IdentityKind;
    use cairn_core::domain::Rfc3339Timestamp;
    use cairn_core::pipeline::lint::author_lifecycle::AuthorLifecycle;
    use std::collections::{HashMap, HashSet};

    // Round-7 fix: skip sensor identities. The §6.2 leaf
    // short-circuits sensor-authored sensor_observation records
    // before consulting registry state — sensors aren't required to
    // be in IdentityRegistry. Looking them up unconditionally
    // creates a false failure mode: a registry hiccup on a sensor
    // identity would surface as a blocking DeferredCheck Error for
    // records the leaf wouldn't have classified through the
    // registry anyway.
    let mut unique: HashSet<Identity> = HashSet::new();
    for s in stored {
        if let Some(e) = s
            .record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            && e.identity.kind() != IdentityKind::Sensor
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
                let purge_requested_at = rec
                    .purge_requested_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok());
                let purged_at = rec
                    .purged_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok());
                map.insert(
                    id,
                    AuthorLifecycle {
                        state: rec.provisioning_state,
                        activated_at,
                        revoked_at,
                        purge_requested_at,
                        purged_at,
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

/// Group record ids by their chain author identity, restricted to
/// the set of identities whose registry lookup failed. Lets the cli
/// emit per-identity `DeferredCheck` Error findings that name the
/// exact rows whose §6.2 coverage was lost — operators need this to
/// quarantine or retry specific rows.
fn affected_records_by_identity(
    lint_records: &[cairn_core::verbs::lint::LintRecord],
    failures: &std::collections::HashMap<cairn_core::domain::Identity, String>,
) -> std::collections::HashMap<
    cairn_core::domain::Identity,
    Vec<cairn_core::domain::record::RecordId>,
> {
    use cairn_core::domain::ChainRole;
    let mut by_id: std::collections::HashMap<_, Vec<_>> =
        std::collections::HashMap::with_capacity(failures.len());
    for r in lint_records {
        if let Some(e) = r
            .stored
            .record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            && failures.contains_key(&e.identity)
        {
            by_id
                .entry(e.identity.clone())
                .or_default()
                .push(r.stored.record.id.clone());
        }
    }
    by_id
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
    affected_record_ids: &[cairn_core::domain::record::RecordId],
) {
    // Privacy §9: record ids are stable ULIDs, not bodies — safe to
    // surface so operators can identify the rows that lost §6.2
    // coverage. Cap inline ids at 16; the total count is always
    // shown. `target.record_id` carries the first id so single-row
    // outages render with a usable target field.
    const MAX_INLINE_IDS: usize = 16;
    let id_label = identity.map(|id| format!(" for {id}")).unwrap_or_default();
    let total = affected_record_ids.len();
    let affected_label = if total == 0 {
        String::new()
    } else if total <= MAX_INLINE_IDS {
        let joined = affected_record_ids
            .iter()
            .map(cairn_core::domain::record::RecordId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(" — {total} record(s) lost §6.2 coverage: [{joined}]")
    } else {
        let head = affected_record_ids
            .iter()
            .take(MAX_INLINE_IDS)
            .map(cairn_core::domain::record::RecordId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(" — {total} record(s) lost §6.2 coverage; first {MAX_INLINE_IDS}: [{head}, …]")
    };
    let target =
        affected_record_ids
            .first()
            .map(|rid| cairn_core::generated::verbs::lint::Target {
                operation_id: None,
                path: None,
                record_id: Some(cairn_core::generated::common::Ulid(rid.as_str().to_owned())),
            });
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: format!(
            "IdentityRegistry lookup failed{id_label}; §6.2 author-lifecycle classification deferred for records authored by this identity (synthetic MissingFromRegistry suppressed to avoid masking the real cause){affected_label}: {err}"
        ),
        severity: cairn_core::generated::verbs::lint::Severity::Error,
        suggested_fix: Some(
            "investigate the registry backend (transient I/O, schema drift, lock contention) and \
             re-run lint once it is reachable; the partial report still surfaces shape and \
             other-check findings"
                .to_owned(),
        ),
        target,
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

/// Load `index_stats`, swallowing the "not supported by this store
/// adapter" sentinel so the §6.7 `index_drift` check can downgrade to a
/// deferred-info finding instead of aborting the whole lint run. Real
/// operational failures from an adapter that *does* support
/// `index_stats` propagate.
async fn load_index_stats(
    store: &dyn cairn_core::contract::memory_store::MemoryStore,
    stored_len: usize,
) -> anyhow::Result<(cairn_core::contract::memory_store::IndexStats, bool)> {
    let stored_count = u64::try_from(stored_len).unwrap_or(u64::MAX);
    match store.index_stats().await {
        Ok(s) => Ok((s, false)),
        Err(e)
            if e.to_string()
                .contains("not supported by this store adapter") =>
        {
            Ok((
                cairn_core::contract::memory_store::IndexStats::new(stored_count, stored_count),
                true,
            ))
        }
        Err(e) => Err(anyhow::anyhow!("store: index_stats: {e}")).context("lint: index_stats"),
    }
}

/// Load per-record consent models, swallowing the "not supported by
/// this store adapter" sentinel so the caller can emit a finding rather
/// than aborting the whole lint run. Real I/O failures propagate.
async fn load_consent_models(
    store: &dyn cairn_core::contract::memory_store::MemoryStore,
) -> anyhow::Result<(
    std::collections::HashMap<
        cairn_core::domain::RecordId,
        cairn_core::domain::consent_timeline::ConsentModel,
    >,
    bool,
)> {
    match store.list_consent_models().await {
        Ok(m) => Ok((m, false)),
        Err(e)
            if e.to_string()
                .contains("not supported by this store adapter") =>
        {
            Ok((std::collections::HashMap::new(), true))
        }
        Err(e) => Err(anyhow::anyhow!("store: list_consent_models: {e}"))
            .context("lint: list_consent_models"),
    }
}

/// Append an error-severity finding noting that the store adapter has
/// not opted into per-record consent-model lookup. Issue #253 fails
/// closed: an adapter that returns the trait's "not supported" sentinel
/// cannot tell us whether any record carries `consent_model =
/// 'receipt_timeline'`, and silently coercing every row to legacy
/// semantics is a fail-open authorization hole. Adapters that
/// genuinely have no per-record metadata (test fixtures, legacy-only
/// stores) opt in by overriding `list_consent_models` to return
/// `Ok(HashMap::new())` — that path bypasses this finding entirely.
fn push_consent_models_unsupported(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::MissingProvenance,
        message: "store adapter does not implement list_consent_models; \
                  cannot enforce §6.5 per-record consent gating. Failing \
                  closed: lint refuses to certify a vault whose adapter \
                  cannot read the consent_model column."
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Error,
        suggested_fix: Some(
            "ship MemoryStore::list_consent_models on this adapter, or \
             explicitly opt into legacy-only by overriding to return \
             Ok(HashMap::new()) (acknowledges the adapter has no \
             receipt-timeline rows and silences this finding)"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(253),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.error += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("missing_provenance".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Dispatch the §6.5 adapter-capability findings (Issue #253). Pulled
/// out of `lint_handler` so the async shell stays under the workspace's
/// `clippy::too_many_lines` limit.
fn push_consent_model_findings(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    supports_gate: bool,
    consent_models_unsupported: bool,
    stored: &[cairn_core::contract::memory_store::StoredRecord],
    consent_models: &std::collections::HashMap<
        cairn_core::domain::RecordId,
        cairn_core::domain::consent_timeline::ConsentModel,
    >,
) {
    if !supports_gate {
        // Loop 3 round 1: capability=false on a non-empty vault is
        // fail-open territory regardless of whether
        // `list_consent_models` returned the Err sentinel or
        // Ok(empty). Both signals are advisory: a stale or rolled-back
        // adapter could return Ok(empty) while the underlying store
        // genuinely contains receipt_timeline rows. Lint can only
        // certify §6.5 when the adapter advertises capability=true
        // and provides authoritative per-row enumeration. Empty vault
        // remains a benign deferred-info coverage gap.
        if stored.is_empty() {
            push_consent_model_gate_deferred(data);
        } else {
            push_consent_model_gate_blocked_non_empty(data);
        }
        return;
    }
    if consent_models_unsupported {
        push_consent_models_unsupported(data);
        return;
    }
    // Capability says supported AND list_consent_models returned data:
    // result is authoritative. Validate completeness — any active
    // record missing from the map is corruption. Empty map on a
    // non-empty vault is fail-open territory; emit an explicit error
    // (Round 8).
    push_consent_model_gaps(data, stored, consent_models);
    if !stored.is_empty() && consent_models.is_empty() {
        push_consent_models_empty_with_records(data);
    }
}

/// Append an info-severity deferred-check finding for adapters that
/// declare `MemoryStoreCapabilities::per_record_consent_model = false`.
///
/// **Round 6 posture.** Until #255 ships the Phase-B privileged
/// transition writer, no `ReceiptTimeline` rows can be created through
/// supported APIs — every `upsert` forces `legacy_event` and the store
/// ignores caller-supplied `consent_model`. The §6.5 gate is therefore
/// not yet load-bearing, so a non-supporting adapter is a coverage gap
/// rather than a fail-open trust-boundary bypass. When #255 lands
/// writers, this branch flips to data-dependent error severity (the
/// round-2/10 posture). The trust boundary is held in this branch by
/// the read+write path forcing `legacy_event`, plus migration 0024's
/// `consent_timeline_grant_immutable` trigger preventing widening
/// inside any single `consent_ref`.
fn push_consent_model_gate_deferred(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: "store adapter declares \
                  capabilities.per_record_consent_model = false; the \
                  §6.5 per-row consent gate is deferred in this \
                  branch (no supported API can yet produce \
                  receipt_timeline rows — that lands in Phase-B, \
                  #255). When #255 ships, this finding flips to \
                  error severity for adapters that still cannot \
                  enumerate the column."
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Info,
        suggested_fix: Some(
            "if this adapter persists records in the consent_timeline \
             era, set capabilities.per_record_consent_model = true and \
             implement MemoryStore::list_consent_models"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(253),
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

/// Append an error finding when an adapter declares
/// `per_record_consent_model = false` on a non-empty vault. With no way
/// to enumerate per-row consent state, lint cannot prove that any row
/// is `legacy_event` — version skew, rollback, or out-of-band writes
/// could leave `receipt_timeline` rows the adapter is blind to, and
/// downgrading the gate to info would let those rows pass unchecked.
fn push_consent_model_gate_blocked_non_empty(
    data: &mut cairn_core::generated::verbs::lint::LintData,
) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::MissingProvenance,
        message: "store adapter declares \
                  capabilities.per_record_consent_model = false on a \
                  non-empty vault; refusing to certify §6.5 because \
                  the adapter cannot prove every row is legacy_event \
                  (rollback / version skew / out-of-band write could \
                  leave receipt_timeline rows unenumerable)."
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Error,
        suggested_fix: Some(
            "implement MemoryStore::list_consent_models and set \
             capabilities.per_record_consent_model = true on this \
             adapter, or run lint against an empty vault"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(253),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.error += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("missing_provenance".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Append an error finding when an adapter that *claims* to track
/// per-row `consent_model` returns an empty map for a non-empty vault.
/// Treating an empty result as authoritative would silently downgrade
/// every row to `legacy_event` (Round 8 review).
fn push_consent_models_empty_with_records(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        kind: cairn_core::generated::verbs::lint::Kind::MissingProvenance,
        message: "store adapter declares per_record_consent_model = true \
                  but list_consent_models returned an empty map for a \
                  non-empty vault. Refusing to coerce all rows to \
                  legacy_event — likely adapter bug or partial read."
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Error,
        suggested_fix: Some(
            "verify the adapter's list_consent_models query returns \
             every active record; investigate schema drift / partial reads"
                .to_owned(),
        ),
        target: None,
        tracking_issue: Some(253),
    };
    data.findings.push(f);
    data.summary.total += 1;
    data.summary.by_severity.error += 1;
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry("missing_provenance".to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
}

/// Append error findings for any active record that the adapter's
/// `list_consent_models` result omitted. The contract is one entry per
/// active row; a missing entry is corruption / adapter bug, not an
/// implicit `legacy_event` downgrade. Returns the set of `record_id`s
/// that *were* covered so the caller can branch on completeness.
fn push_consent_model_gaps(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    stored: &[cairn_core::contract::memory_store::StoredRecord],
    consent_models: &std::collections::HashMap<
        cairn_core::domain::RecordId,
        cairn_core::domain::consent_timeline::ConsentModel,
    >,
) {
    // Only validate completeness when the adapter returned at least one
    // entry — an entirely empty map is the explicit legacy-only opt-in
    // (see `push_consent_models_unsupported`'s suggested_fix), not a
    // gap to flag.
    if consent_models.is_empty() {
        return;
    }
    for s in stored {
        if !consent_models.contains_key(&s.record.id) {
            let f = cairn_core::generated::verbs::lint::Finding {
                kind: cairn_core::generated::verbs::lint::Kind::MissingProvenance,
                message: format!(
                    "active record {id} is missing from list_consent_models result; \
                     contract requires one entry per active row. Treating as adapter \
                     bug rather than implicit legacy_event downgrade.",
                    id = s.record.id.as_str(),
                ),
                severity: cairn_core::generated::verbs::lint::Severity::Error,
                suggested_fix: Some(
                    "verify the adapter's list_consent_models query returns every \
                     active record; investigate schema drift / partial reads"
                        .to_owned(),
                ),
                target: Some(cairn_core::generated::verbs::lint::Target {
                    operation_id: None,
                    path: None,
                    record_id: Some(cairn_core::generated::common::Ulid(
                        s.record.id.as_str().to_owned(),
                    )),
                }),
                tracking_issue: Some(253),
            };
            data.findings.push(f);
            data.summary.total += 1;
            data.summary.by_severity.error += 1;
            if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
                let entry = map
                    .entry("missing_provenance".to_owned())
                    .or_insert(serde_json::Value::from(0_u64));
                if let Some(n) = entry.as_u64() {
                    *entry = serde_json::Value::from(n.saturating_add(1));
                }
            }
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
        let result = lint_handler(&store, &registry, None, &cfg, true, vault.path())
            .await
            .expect("handler");

        // Post-rebase posture: §6.2 actor_chain is live. The empty
        // registry means the sample record's author resolves to
        // MissingFromRegistry → BrokenActorChain Error, tripping
        // has_error. §6.5 consent runs against FixtureStore (cap=true,
        // every record LegacyEvent) but FixtureStore does not
        // implement ConsentLookup, so consent.rs returns no findings.
        // Info findings: §6.3 + §6.4 + §6.6 deferred-info coverage
        // gaps + the §6.2 signature-verification-deferred advisory =
        // 4.
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
            info_count, 3,
            "expect §6.3 + §6.6 deferred-info findings + §6.2 signature-verification-deferred advisory (§6.4 schema check went live in #258)"
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

    /// §6.5 capability gate 2×2 matrix. Pins the four quadrants of
    /// `(per_record_consent_model: true|false) × (vault: empty|non-empty)`
    /// so the persistent fail-open/fail-closed oscillation gets caught
    /// the next time someone flips the gate posture.
    mod consent_model_gate_matrix {
        use super::*;
        use cairn_core::config::CairnConfig;
        use cairn_core::generated::verbs::lint::{Kind, Severity};
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        async fn run_lint(store: &FixtureStore) -> LintHandlerResult {
            use cairn_store_sqlite::SqliteIdentityRegistry;
            let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
            let cfg = CairnConfig::default();
            let vault = tempfile::tempdir().expect("tempdir");
            lint_handler(store, &registry, None, &cfg, false, vault.path())
                .await
                .expect("handler")
        }

        fn count_kind(result: &LintHandlerResult, kind: Kind, severity: Severity) -> usize {
            result
                .data
                .findings
                .iter()
                .filter(|f| f.kind == kind && f.severity == severity)
                .count()
        }

        // Quadrant 1: capability=true + vault=non-empty.
        // §6.5 sub-checks run; FixtureStore reports every record as
        // LegacyEvent so the covering-grant resolver is never engaged
        // and no §6.5 errors fire. The Phase-A "no receipt_timeline
        // records yet" deferred-info finding still emits.
        #[tokio::test]
        async fn cap_true_non_empty_runs_subchecks_no_errors() {
            let store = FixtureStore::default();
            store.upsert(&sample_record()).await.expect("upsert");
            let result = run_lint(&store).await;

            // §6.2 actor_chain is now a real check (post-rebase): the
            // empty stub registry will produce a BrokenActorChain
            // Error for the sample record's author, so result.has_error
            // is true regardless of §6.5. Scope the gate-matrix
            // invariant to §6.5-specific findings (tracking_issue=253).
            let consent_errors = result
                .data
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Error && f.tracking_issue == Some(253))
                .count();
            assert_eq!(
                consent_errors, 0,
                "LegacyEvent rows must not produce §6.5 errors"
            );
            // Gate findings (errors) for capability=false MUST NOT fire.
            let blocked = result
                .data
                .findings
                .iter()
                .filter(|f| {
                    f.kind == Kind::MissingProvenance
                        && f.severity == Severity::Error
                        && f.tracking_issue == Some(253)
                })
                .count();
            assert_eq!(blocked, 0, "no gate-blocked finding under cap=true");
        }

        // Quadrant 2: capability=true + vault=empty.
        // No records to check; gate is satisfied. Phase-A deferred-info
        // still fires for the §6.5 sub-checks coverage gap.
        #[tokio::test]
        async fn cap_true_empty_no_errors() {
            let store = FixtureStore::default();
            let result = run_lint(&store).await;

            assert!(
                !result.has_error,
                "empty vault must not error under cap=true"
            );
            let blocked = result
                .data
                .findings
                .iter()
                .filter(|f| {
                    f.kind == Kind::MissingProvenance
                        && f.severity == Severity::Error
                        && f.tracking_issue == Some(253)
                })
                .count();
            assert_eq!(blocked, 0);
        }

        // Quadrant 3: capability=false + vault=empty.
        // Coverage gap, not a trust-boundary breach. push_consent_model_gate_deferred
        // emits an Info finding with tracking_issue=253. Must not error.
        #[tokio::test]
        async fn cap_false_empty_emits_info_not_error() {
            let store = FixtureStore::default();
            store.set_legacy_only_override(true);
            let result = run_lint(&store).await;

            assert!(
                !result.has_error,
                "cap=false on empty vault is a coverage gap, not an error",
            );
            let deferred_gate = result
                .data
                .findings
                .iter()
                .filter(|f| {
                    f.kind == Kind::DeferredCheck
                        && f.severity == Severity::Info
                        && f.tracking_issue == Some(253)
                        && f.message.contains("per_record_consent_model = false")
                })
                .count();
            assert_eq!(
                deferred_gate, 1,
                "gate_deferred info must fire exactly once on cap=false empty vault",
            );
        }

        // Quadrant 4: capability=false + vault=non-empty.
        // Trust-boundary breach. push_consent_model_gate_blocked_non_empty
        // emits an Error: lint cannot prove every row is legacy_event when
        // the adapter is blind to the consent_model column.
        #[tokio::test]
        async fn cap_false_non_empty_emits_error() {
            let store = FixtureStore::default();
            store.upsert(&sample_record()).await.expect("upsert");
            store.set_legacy_only_override(true);
            let result = run_lint(&store).await;

            assert!(
                result.has_error,
                "cap=false on non-empty vault must fail closed",
            );
            let blocked = count_kind(&result, Kind::MissingProvenance, Severity::Error);
            assert!(
                blocked >= 1,
                "missing_provenance error from gate_blocked_non_empty must fire",
            );
            // And the deferred-info from quadrant 3 must NOT also fire.
            let deferred_gate = result
                .data
                .findings
                .iter()
                .filter(|f| {
                    f.kind == Kind::DeferredCheck
                        && f.severity == Severity::Info
                        && f.tracking_issue == Some(253)
                        && f.message.contains("per_record_consent_model = false")
                })
                .count();
            assert_eq!(
                deferred_gate, 0,
                "gate_deferred info must not fire on cap=false non-empty vault",
            );
        }
    }

    #[tokio::test]
    async fn lint_handler_flags_index_drift_with_error_severity() {
        use cairn_core::config::CairnConfig;
        use cairn_core::contract::memory_store::IndexStats;
        use cairn_store_sqlite::SqliteIdentityRegistry;
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        let store = FixtureStore::default();
        store.upsert(&sample_record()).await.expect("upsert");
        // Force a drift fixture: 5 active records but FTS reports 4.
        store.set_index_stats_override(IndexStats::new(5, 4));

        let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
        let cfg = CairnConfig::default();
        let vault = tempfile::tempdir().expect("tempdir");
        let result = lint_handler(&store, &registry, None, &cfg, false, vault.path())
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

    #[tokio::test]
    async fn lint_handler_skips_sensor_identities_in_registry_prefetch() {
        // Round-7 fix: sensor identities must not be looked up in
        // IdentityRegistry. The §6.2 leaf short-circuits sensor-
        // authored sensor_observation records via the carve-out
        // (kind == SensorObservation && author == source_sensor),
        // and sensors aren't required to live in the registry. A
        // registry hiccup on a sensor identity must not surface as a
        // blocking DeferredCheck Error for records the leaf wouldn't
        // have classified through the registry anyway.
        //
        // E2E shape: build a valid sensor-authored sensor_observation,
        // upsert it under an empty registry, run lint_handler. The
        // handler must produce:
        // - no BrokenActorChain finding (the sensor carve-out
        //   short-circuits before MissingFromRegistry can trip),
        // - no per-identity DeferredCheck Error (prefetch never
        //   tried to look the sensor up),
        // - the aggregate sensor-author advisory Info IS present.
        use cairn_core::config::CairnConfig;
        use cairn_core::domain::record::tests_export::sample_record;
        use cairn_core::domain::{
            ActorChainEntry, ChainRole, Identity, MemoryKind, Rfc3339Timestamp, ScopeTuple,
        };
        use cairn_store_sqlite::SqliteIdentityRegistry;
        use cairn_test_fixtures::store::FixtureStore;

        let store = FixtureStore::default();
        let mut r = sample_record();
        r.kind = MemoryKind::SensorObservation;
        let sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor identity");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.source_sensor = sensor.clone();
        r.provenance.originating_agent_id = sensor.clone();
        r.scope = ScopeTuple {
            entity: Some("camera-4".to_owned()),
            ..ScopeTuple::default()
        };
        r.validate().expect("valid sensor-authored record");
        store.upsert(&r).await.expect("upsert");

        // Empty registry — no rows for any identity. A non-sensor
        // author would emit a BrokenActorChain Error
        // (MissingFromRegistry); a sensor under the carve-out must
        // not.
        let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
        let cfg = CairnConfig::default();
        let vault = tempfile::tempdir().expect("tempdir");
        let result = lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("handler");

        let chain_findings: Vec<_> = result
            .data
            .findings
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    cairn_core::generated::verbs::lint::Kind::BrokenActorChain,
                )
            })
            .collect();
        assert!(
            chain_findings.is_empty(),
            "sensor carve-out must short-circuit before any BrokenActorChain finding: {chain_findings:?}",
        );

        // The §6.2 sensor-author advisory must surface (count == 1
        // record hit the carve-out).
        let sensor_advisory = result.data.findings.iter().find(|f| {
            matches!(
                f.kind,
                cairn_core::generated::verbs::lint::Kind::DeferredCheck
            ) && f.tracking_issue == Some(256)
                && f.message.contains("sensor-authored")
        });
        assert!(
            sensor_advisory.is_some(),
            "expected the §6.2 sensor-author DeferredCheck advisory: {:?}",
            result.data.findings,
        );

        // No per-identity registry-unavailable error: the sensor
        // identity was filtered out of prefetch entirely, so no
        // lookup happened, so no failure to surface.
        let registry_failures: Vec<_> = result
            .data
            .findings
            .iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    cairn_core::generated::verbs::lint::Kind::DeferredCheck,
                ) && matches!(
                    f.severity,
                    cairn_core::generated::verbs::lint::Severity::Error,
                )
            })
            .collect();
        assert!(
            registry_failures.is_empty(),
            "no per-identity DeferredCheck Error expected (sensor skipped from prefetch): {registry_failures:?}",
        );
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
        let r1 =
            cairn_core::domain::record::RecordId::parse("01HZZZZZZZZZZZZZZZZZZZZZZ1").expect("rid");
        let r2 =
            cairn_core::domain::record::RecordId::parse("01HZZZZZZZZZZZZZZZZZZZZZZ2").expect("rid");
        let affected = vec![r1.clone(), r2.clone()];
        super::push_registry_unavailable(
            &mut data,
            Some(&id),
            "boom: connection refused",
            &affected,
        );
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
        assert!(
            f.message.contains(r1.as_str()),
            "message must surface affected record ids: {}",
            f.message,
        );
        assert_eq!(
            f.target
                .as_ref()
                .and_then(|t| t.record_id.as_ref())
                .map(|u| u.0.as_str()),
            Some(r1.as_str()),
            "target.record_id must point at the first affected record"
        );
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
