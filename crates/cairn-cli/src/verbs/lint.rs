//! `cairn lint` handler.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::contract::source_resolver::{SourceResolver, SourceResolverError};
use cairn_core::domain::SourceId;
use cairn_core::domain::folder::{
    FolderPolicy, aggregate_folders, materialize_backlinks, parse_policy, project_index,
};
use cairn_core::domain::projection::MarkdownProjector;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::lint::{
    Finding, Kind, LintData, LintDataSummary, LintDataSummaryBySeverity, Severity,
};
use cairn_store_sqlite::{
    EdgeLintReport, SqliteConsentJournalReader, SqliteWorkflowJobsReader, StoreError, lint_edges,
    resolve_edge_contradictions,
};
use clap::ArgMatches;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

use super::envelope::{emit_json, human_error, new_operation_id, unimplemented_response};

struct VaultFsSourceResolver {
    vault_root: PathBuf,
}

impl VaultFsSourceResolver {
    fn new(vault_root: &Path) -> Self {
        Self {
            vault_root: vault_root.to_path_buf(),
        }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.vault_root.join(id)
    }

    /// Resolve `id` to a filesystem path and refuse to traverse outside
    /// `vault_root`. `SourceRef.id` is a logical key validated at the
    /// domain layer (no leading `/`, no `..` segments, no NUL); even
    /// so, lexical join can still escape via symlinks. Canonicalize
    /// once, verify containment, and treat any escape attempt as
    /// `NotFound` to fail closed.
    fn safe_path_for(&self, id: &str) -> Result<PathBuf, SourceResolverError> {
        let candidate = self.path_for(id);
        let canon_root =
            std::fs::canonicalize(&self.vault_root).map_err(|e| SourceResolverError::Io {
                detail: format!("canonicalize vault_root: {e}"),
            })?;
        let canon_candidate = match std::fs::canonicalize(&candidate) {
            Ok(p) => p,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(SourceResolverError::NotFound);
            }
            Err(err) => {
                return Err(SourceResolverError::Io {
                    detail: err.to_string(),
                });
            }
        };
        if !canon_candidate.starts_with(&canon_root) {
            return Err(SourceResolverError::NotFound);
        }
        Ok(canon_candidate)
    }
}

impl SourceResolver for VaultFsSourceResolver {
    fn exists(&self, id: &str) -> bool {
        self.safe_path_for(id).is_ok_and(|p| p.is_file())
    }

    fn read(&self, id: &str) -> Result<Vec<u8>, SourceResolverError> {
        let path = self.safe_path_for(id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(SourceResolverError::NotFound)
            }
            Err(err) => Err(SourceResolverError::Io {
                detail: err.to_string(),
            }),
        }
    }

    fn locator(&self, id: &str) -> String {
        // Diagnostic-only — still surface the join-form to operators
        // even when canonicalization fails, so finding messages name
        // the path the operator would expect.
        self.path_for(id).display().to_string()
    }
}

/// Sentinel error type used to thread `LockLost` through the
/// `anyhow::Error` path of [`fix_markdown_handler_with_fence`]. Carrying
/// a typed marker (vs. matching on a string) lets the wrapper distinguish
/// fence-trigger from a real I/O failure.
#[derive(Debug, thiserror::Error)]
#[error("lint.lock_lost (fence)")]
struct LockLostMarker;

/// Errors from [`fix_markdown_with_lock`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LintFixError {
    /// Another `--fix-markdown` run currently holds the repair lock.
    #[error("lint.fix_in_progress")]
    FixInProgress,
    /// The lint-repair lease was reclaimed by another caller while this
    /// run was in flight (machine sleep, scheduler stall, etc). The
    /// markdown rewrites that did happen are durable; the WAL op is
    /// aborted so the next run can pick up cleanly.
    #[error("lint.lock_lost")]
    LockLost,
    /// Lock-table error (not a simple Held conflict).
    ///
    /// `LockError` is large (several hundred bytes for the structured
    /// `Held` / `Fenced` variants) so we box it here to keep
    /// `LintFixError` itself small — same rationale as
    /// `StoreError::LockInit`.
    #[error("lock error")]
    Lock(#[source] Box<cairn_store_sqlite::locks::LockError>),
    /// WAL state-machine error.
    #[error("wal error")]
    Wal(#[source] cairn_store_sqlite::wal::lint_repair::LintRepairWalError),
    /// The underlying `fix_markdown_handler` failed.
    #[error("handler error")]
    Handler(#[source] anyhow::Error),
}

/// Derive a stable vault identifier from its filesystem path.
///
/// Canonicalizes the path (falls back to the raw path on I/O error) and
/// returns the lowercase hex blake3 digest of the string representation.
/// This produces a fixed-length, filesystem-safe key suitable for use as a
/// lock-table `scope_key`.
fn vault_id(vault_root: &Path) -> Result<String, LintFixError> {
    // Fail closed on canonicalization errors. Two invocations
    // targeting the same live vault MUST resolve to the same lock
    // scope, otherwise both runs would bypass `FixInProgress` and
    // race on the same files. The only safe response to "the OS
    // can't tell us the canonical path" is to refuse to acquire
    // the lock at all — TOCTOU-grade ambiguity on the scope key
    // is worse than no repair.
    let canonical = vault_root.canonicalize().map_err(|e| {
        LintFixError::Lock(Box::new(cairn_store_sqlite::locks::LockError::Db(
            tokio_rusqlite::Error::Other(
                format!(
                    "vault canonicalization failed for {}: {e}",
                    vault_root.display()
                )
                .into(),
            ),
        )))
    })?;
    Ok(cairn_core::domain::projection::body_hash(
        &canonical.to_string_lossy(),
    ))
}

/// Acquire an EXCLUSIVE lint-repair lock, open a `lint_repair` WAL op,
/// call `fix_markdown_handler`, then commit or abort the WAL op.
///
/// # Errors
/// - [`LintFixError::FixInProgress`] if another caller holds the lock.
/// - [`LintFixError::Lock`] on any other lock-table error.
/// - [`LintFixError::Wal`] if the WAL state-machine rejects a transition.
/// - [`LintFixError::Handler`] if `fix_markdown_handler` fails.
// Single sequence: acquire-lock + open-WAL + fence-callbacks + finalize.
// Splitting it for the line counter would force the lock + WAL + fence
// trio across helper signatures without making the flow easier to follow.
#[allow(clippy::too_many_lines)]
pub async fn fix_markdown_with_lock(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    vault_root: &Path,
    ttl: Duration,
) -> Result<FixMarkdownResult, LintFixError> {
    let conn = Arc::clone(store.raw_conn_for_admin().ok_or_else(|| {
        LintFixError::Lock(Box::new(cairn_store_sqlite::locks::LockError::Db(
            tokio_rusqlite::Error::Other("store not initialized".into()),
        )))
    })?);
    let vid = vault_id(vault_root)?;
    // Use a ULID for the holder id — unique per process+call, no uuid dep needed.
    let holder_id = format!("pid={}-{}", std::process::id(), ulid::Ulid::new());

    // The typed `acquire` takes the daemon incarnation Arc so the per-holder
    // fencing CAS can match on `(acquired_epoch, owner_incarnation)` rather
    // than the legacy `acquired_at` timestamp. `Store::open` mints this in
    // Task 8; an unconnected (registry-stub) Store would surface
    // `NoIncarnation` here, mirroring the `store not initialized` branch.
    let inc = store.incarnation().cloned().ok_or_else(|| {
        LintFixError::Lock(Box::new(
            cairn_store_sqlite::locks::LockError::NoIncarnation,
        ))
    })?;
    let resource = cairn_store_sqlite::locks::ResourceKey::vault(&vid);

    let lock = match cairn_store_sqlite::locks::acquire(
        &conn,
        &resource,
        cairn_store_sqlite::locks::LockMode::Exclusive,
        &holder_id,
        ttl,
        &inc,
        "lint --fix-markdown",
    )
    .await
    {
        Ok(h) => h,
        Err(cairn_store_sqlite::locks::LockError::Held { .. }) => {
            return Err(LintFixError::FixInProgress);
        }
        Err(e) => return Err(LintFixError::Lock(Box::new(e))),
    };

    // Holding the exclusive repair lock means no other repair is in
    // flight for this vault, so any PREPARED `lint_repair` op left over
    // from a previous run is stale: either a post-write `commit` failed,
    // or a `wal::abort` failed after a handler error. Markdown rewrites
    // are idempotent + the DB is unchanged across a `lint_repair` op, so
    // the safe + correct recovery is to abort the stale op. The next
    // `--fix-markdown` (this one) will re-derive the projection from the
    // DB and write whatever the vault actually needs.
    reconcile_stale_lint_repair_ops(&conn, &vid).await?;

    let op_id = cairn_store_sqlite::wal::lint_repair::begin(&conn, &vid, &holder_id, ttl)
        .await
        .map_err(LintFixError::Wal)?;
    // Once `begin` succeeds an `ISSUED` row exists. If `prepare`
    // fails, the op is still in ISSUED — and ISSUED → ABORTED is
    // illegal in the FSM, so calling `abort()` here would just
    // return IllegalTransition. Use the FSM-legal terminal
    // transition for a never-prepared op: REJECTED.
    if let Err(e) = cairn_store_sqlite::wal::lint_repair::prepare(&conn, &op_id).await {
        let _ = cairn_store_sqlite::wal::lint_repair::reject(
            &conn,
            &op_id,
            "prepare failed before any markdown rewrite",
        )
        .await;
        return Err(LintFixError::Wal(e));
    }

    // Heartbeat: refresh the lock holder's expires_at every `ttl/3` so a
    // repair longer than `ttl` does not get reclaimed mid-run by a second
    // caller. The heartbeat owns its own oneshot cancel; we shut it down
    // before releasing the lock or returning, so it never outlives the
    // repair.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let heartbeat = spawn_lock_heartbeat(
        Arc::clone(&conn),
        lock.acquisition_ulid().to_owned(),
        ttl,
        cancel_rx,
    );

    // Fence callback invoked before every destructive write inside the
    // handler. `is_still_held` returns false once a second caller has
    // reclaimed our holder row (the `(acquired_epoch, owner_incarnation)`
    // pair no longer matches); we surface that as a typed error so the
    // loop stops *before* publishing more stale files instead of
    // overwriting a winner's newer projections.
    let lock_for_fence = &lock;
    let outcome = fix_markdown_handler_with_fence(store, vault_root, move || {
        let lock = lock_for_fence;
        async move {
            if lock
                .is_still_held()
                .await
                .map_err(|e| anyhow::anyhow!("lock fence query: {e}"))?
            {
                Ok(())
            } else {
                Err(anyhow::Error::new(LockLostMarker))
            }
        }
    })
    .await;

    // Stop the heartbeat before touching the WAL or the lock so the next
    // statements are the only ones racing for connection access.
    let _ = cancel_tx.send(());
    let _ = heartbeat.await;

    // Final fence (paranoia): even if the handler returned Ok, recheck
    // before WAL commit. The handler's per-write fence catches the
    // common case; this catches the no-write run plus any window after
    // the last write.
    let still_ours = lock
        .is_still_held()
        .await
        .map_err(|e| LintFixError::Lock(Box::new(e)))?;
    if !still_ours {
        let _ = cairn_store_sqlite::wal::lint_repair::abort(
            &conn,
            &op_id,
            "fencing check failed: lock_holders row missing or reclaimed",
        )
        .await;
        // The lock row no longer belongs to us, so dropping the handle
        // would race against the new owner. Forget the handle; TTL +
        // reclaim semantics on the new owner's row stand on their own.
        std::mem::forget(lock);
        return Err(LintFixError::LockLost);
    }

    // Map the fence-trigger error from the handler to LockLost.
    let outcome = match outcome {
        Ok(r) => Ok(r),
        Err(e) if e.is::<LockLostMarker>() => {
            let _ = cairn_store_sqlite::wal::lint_repair::abort(
                &conn,
                &op_id,
                "lock_lost during fix_markdown_handler",
            )
            .await;
            std::mem::forget(lock);
            return Err(LintFixError::LockLost);
        }
        Err(e) => Err(e),
    };

    finalize_outcome(&conn, &op_id, &resource, lock, outcome).await
}

/// Commit-or-abort the WAL op according to handler `outcome`, then
/// release the lock. Post-commit lock-release failures are demoted to
/// `tracing::warn!` so a successful repair is never reported as failure.
async fn finalize_outcome(
    conn: &Arc<tokio_rusqlite::Connection>,
    op_id: &str,
    resource: &cairn_store_sqlite::locks::ResourceKey,
    lock: cairn_store_sqlite::locks::LockHandle,
    outcome: Result<FixMarkdownResult, anyhow::Error>,
) -> Result<FixMarkdownResult, LintFixError> {
    match outcome {
        Ok(r) => {
            cairn_store_sqlite::wal::lint_repair::commit(conn, op_id)
                .await
                .map_err(LintFixError::Wal)?;
            // Post-commit cleanup must not turn success into failure
            // (the WAL op is durable, files are on disk). But a stuck
            // holder row blocks the next `--fix-markdown` for the
            // remainder of TTL — bad UX. Try once, briefly back off,
            // try again; if it still fails, demote to a warning and
            // count on TTL reclaim. Two attempts cover transient
            // connection-busy errors without prolonging shutdown.
            let acq_ulid = lock.acquisition_ulid().to_owned();
            if let Err(first) = lock.release().await {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                if let Err(retry) =
                    cairn_store_sqlite::locks::release_by_holder(conn, &acq_ulid).await
                {
                    tracing::warn!(
                        first_error = %first,
                        retry_error = %retry,
                        resource = %resource,
                        "lint --fix-markdown: lock release failed twice post-commit; \
                         row will be reclaimed on TTL expiry",
                    );
                }
            }
            Ok(r)
        }
        Err(handler_err) => {
            let reason = format!("fix_markdown_handler: {handler_err:?}");
            let _ = cairn_store_sqlite::wal::lint_repair::abort(conn, op_id, &reason).await;
            let _ = lock.release().await;
            Err(LintFixError::Handler(handler_err))
        }
    }
}

/// Spawn a tokio task that refreshes `lock_holders.expires_at` for the
/// `(resource, holder_id, acquired_epoch, owner_incarnation)` row every
/// `ttl / 3` until cancellation. The `(acquired_epoch, owner_incarnation)`
/// predicate is the fencing token: once a second caller has reclaimed the
/// row (which bumps `locks.epoch` and inserts a new holder row), our
/// renewals stop matching and drop on the floor instead of resurrecting a
/// row that no longer belongs to us.
///
/// The refresh is best-effort: a transient DB error simply causes the next
/// tick to retry. Cancellation drains the task before the surrounding flow
/// releases the lock.
fn spawn_lock_heartbeat(
    conn: Arc<tokio_rusqlite::Connection>,
    acquisition_ulid: String,
    ttl: Duration,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    // ttl/3 gives two refreshes within each TTL window so a transient
    // failure on one tick still leaves a valid renewal before expiry.
    let interval = ttl / 3;
    let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut cancel => return,
                () = tokio::time::sleep(interval) => {}
            }
            // The heartbeat must NOT revive an already-expired lease.
            // Compute now_ms inside the DB call (after queueing settles)
            // and gate the UPDATE on `expires_at > now_ms`. If 0 rows
            // are updated, the lease was lost — bail out. `acquisition_ulid`
            // is per-acquisition unique so we can't accidentally extend
            // a different acquisition's row.
            let acq = acquisition_ulid.clone();
            let updated = conn
                .call(move |c| {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(i64::MAX, |d| {
                            i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
                        });
                    let expires = now_ms.saturating_add(ttl_ms);
                    let n = c.execute(
                        "UPDATE lock_holders \
                            SET expires_at = ?1 \
                          WHERE acquisition_ulid = ?2 \
                            AND expires_at > ?3",
                        rusqlite::params![expires, acq, now_ms],
                    )?;
                    Ok::<usize, tokio_rusqlite::Error>(n)
                })
                .await;
            if matches!(updated, Ok(0)) {
                // Lease lost — either expired between ticks or was
                // reclaimed. Stop heartbeating; the surrounding flow's
                // pre-commit fence (`is_still_held` / `with_fencing`)
                // will surface the loss.
                tracing::warn!(
                    "lint --fix-markdown heartbeat: lease lost (0 rows updated); \
                     exiting heartbeat task — caller's fence check will fail closed"
                );
                return;
            }
        }
    })
}

/// Abort every non-terminal `lint_repair` WAL op whose envelope names
/// this vault. Caller must hold the EXCLUSIVE `lint_repair` lock for
/// `vault_id` before invoking — that's what makes any leftover ISSUED
/// or PREPARED row definitively stale (no live writer can be using it).
///
/// Sweeps both ISSUED and PREPARED states. ISSUED rows can be left
/// behind by a `begin()`-success / `prepare()`-failure path; PREPARED
/// rows by a post-write `commit()` failure or a handler-error path
/// where `abort()` itself failed. Recovery treats both the same way:
/// abort and let the next run re-derive the projection from the DB.
///
/// Errors during the abort UPDATE are surfaced; an op that has already
/// terminated since the SELECT is treated as a benign race (the abort
/// returns `IllegalTransition`, which we swallow).
async fn reconcile_stale_lint_repair_ops(
    conn: &Arc<tokio_rusqlite::Connection>,
    vault_id: &str,
) -> Result<(), LintFixError> {
    // Pull state along with the op id so we know which terminal
    // transition is FSM-legal: ISSUED → REJECTED, PREPARED →
    // ABORTED. The previous round's pass selected both states but
    // called only `abort()`, which is gated on PREPARED — every
    // ISSUED row therefore came back as IllegalTransition and was
    // silently swallowed, leaving permanent non-terminal pollution.
    let vault_id_q = vault_id.to_owned();
    let stale: Vec<(String, String)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT operation_id, state FROM wal_ops \
                  WHERE kind = 'lint_repair' \
                    AND state IN ('ISSUED', 'PREPARED') \
                    AND scope_json LIKE '%' || ?1 || '%'",
            )?;
            let rows = stmt
                .query_map([vault_id_q], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| LintFixError::Lock(Box::new(cairn_store_sqlite::locks::LockError::Db(e))))?;

    for (op_id, state) in stale {
        // IllegalTransition (already-terminal between SELECT and
        // UPDATE) is benign; anything else is a real recovery
        // failure and must surface so the caller bails before
        // touching files.
        let result = match state.as_str() {
            "ISSUED" => {
                cairn_store_sqlite::wal::lint_repair::reject(
                    conn,
                    &op_id,
                    "stale issued op reclaimed under lint_repair lock",
                )
                .await
            }
            // PREPARED — fall through to abort.
            _ => {
                cairn_store_sqlite::wal::lint_repair::abort(
                    conn,
                    &op_id,
                    "stale prepared op reclaimed under lint_repair lock",
                )
                .await
            }
        };
        match result {
            Ok(())
            | Err(cairn_store_sqlite::wal::lint_repair::LintRepairWalError::IllegalTransition(_)) =>
                {}
            Err(e) => return Err(LintFixError::Wal(e)),
        }
    }
    Ok(())
}

/// Result of a `lint --fix-markdown` run.
#[derive(Debug, serde::Serialize)]
pub struct FixMarkdownResult {
    /// Vault-relative paths that were written or updated.
    pub written: Vec<PathBuf>,
    /// Number of files that were already up to date.
    pub already_current: usize,
    /// Vault-relative paths the run could NOT auto-repair: symlinked
    /// projection paths, non-regular files at the destination, or
    /// projections whose existing on-disk file failed to read with
    /// anything other than `NotFound`. Operators must triage these
    /// manually before re-running. The presence of any entry forces
    /// the surrounding command to a non-zero exit so automation
    /// cannot mistake a partial repair for a successful one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<BlockedProjection>,
}

/// One projection that `--fix-markdown` refused to auto-repair.
#[derive(Debug, serde::Serialize)]
pub struct BlockedProjection {
    /// Vault-relative projection path.
    pub path: PathBuf,
    /// Why the path was skipped — `unsafe-path`, `read-error`, etc.
    pub reason: String,
    /// Operator-readable detail.
    pub detail: String,
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
    fix_markdown_handler_with_fence(store, vault_root, || async { Ok(()) }).await
}

/// Variant of [`fix_markdown_handler`] that calls `fence` before every
/// destructive write. The fence is the lock-fencing recheck used by
/// [`fix_markdown_with_lock`]: a paused / stalled run that has lost its
/// lease must stop publishing files immediately, not after the loop ends.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, the fence reports lock
/// loss, or any file I/O fails.
// Single-loop body bundling fence callbacks, spawn_blocking stages,
// and the final persist into one cohesive sequence; splitting it for
// the line counter would force the F: FnMut closure to thread through
// extra helper signatures without making the flow easier to follow.
#[allow(clippy::too_many_lines)]
pub async fn fix_markdown_handler_with_fence<F, Fut>(
    store: &dyn MemoryStore,
    vault_root: &Path,
    mut fence: F,
) -> anyhow::Result<FixMarkdownResult>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    use std::collections::HashMap;

    let projector = MarkdownProjector;
    let records = store
        .list_active_stored(&cairn_core::contract::memory_store::ListArgs::default())
        .await
        .map_err(anyhow::Error::msg)
        .context("store: list_active_stored")?;
    // target_id -> record_id we projected this iteration. After the
    // loop completes, we re-list and any target whose active
    // record_id differs (or is gone) was mutated mid-run; the
    // on-disk projection is now stale relative to the DB even though
    // we did write a file for it. Recording record_id (not target_id
    // alone) is what catches updates to *existing* records — a
    // target-only HashSet would miss them.
    let mut projected_at: HashMap<String, String> = HashMap::with_capacity(records.len());
    let mut written = Vec::new();
    let mut already_current: usize = 0;
    let mut blocked: Vec<BlockedProjection> = Vec::new();

    for snapshot in records {
        // Re-fetch per record immediately before projection. The lock
        // serialises other `lint_repair` runs but does NOT fence
        // concurrent ingest/upsert/tombstone — the snapshot from
        // `list_active_stored` may be seconds out of date by the time
        // we reach this iteration. Always project from the freshest
        // store state so we never publish a stale projection on top of
        // newer authoritative data.
        // Record was tombstoned or otherwise deactivated since the
        // snapshot. Leave the on-disk projection untouched — its
        // removal is the projection-cleanup workflow's job, not
        // `--fix-markdown`'s.
        let Some(stored) = store
            .get_active_by_target(&snapshot.record.target_id)
            .await
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!(
                    "store: get_active_by_target({})",
                    snapshot.record.target_id.as_str()
                )
            })?
        else {
            continue;
        };
        projected_at.insert(
            stored.record.target_id.as_str().to_owned(),
            stored.record.id.as_str().to_owned(),
        );
        let projected = projector.project(&stored);
        let abs_path = vault_root.join(&projected.path);

        // No-follow validation BEFORE the read. `read_to_string` follows
        // symlinks; without this guard, a symlinked `raw/<id>.md` (or any
        // intermediate ancestor swapped for a symlink) would be read through
        // to its target and a "matching" comparison would silently bypass
        // the symlink rejection that `write_once` enforces.
        //
        // An unsafe path (symlink, non-regular file) is NOT a fixable
        // condition by `--fix-markdown` alone — the operator must
        // remove the symlink first. Skip the iteration with a warning
        // instead of aborting the whole run; the read-only `lint`
        // pass already surfaces it as a Drift finding so the user
        // sees the offending path.
        let safe = {
            let vroot = vault_root.to_path_buf();
            let dest = abs_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::vault::bootstrap::check_write_safe(&vroot, &dest)
            })
            .await
            .with_context(|| format!("spawn_blocking validate {}", abs_path.display()))?
        };
        if let Err(unsafe_err) = safe {
            // Unsafe projection path (symlink / non-regular file). The
            // run cannot rewrite this file safely; record it as blocked
            // so the caller fails non-zero. Other records still proceed
            // — the lint command stays useful for the rest of the
            // vault — but the overall outcome is not "success".
            tracing::warn!(
                path = %abs_path.display(),
                error = %unsafe_err,
                "lint --fix-markdown: skipping unsafe projection path; \
                 manual cleanup required before this projection can be repaired",
            );
            blocked.push(BlockedProjection {
                path: projected.path.clone(),
                reason: "unsafe-path".to_owned(),
                detail: format!("{unsafe_err}"),
            });
            continue;
        }

        // Use the same normalized comparison the read-only drift pass
        // uses, so `--fix-markdown` is a no-op exactly when `lint` reports
        // no drift. Raw `!=` would treat CRLF / trailing-newline
        // differences as drift and rewrite files that lint already
        // considers matching.
        // Read-only `lint` reports an unreadable projection (permission
        // denied, invalid UTF-8, transient I/O error) as `ProjectionDrift`
        // and the finding builder unconditionally suggests
        // `cairn lint --fix-markdown`. Honour that contract here: treat
        // a non-NotFound read failure the same as Missing — overwrite
        // with the canonical projection rather than aborting the entire
        // repair run on the first unreadable file.
        // Auto-repair only for `NotFound` (file gone) and clean drift
        // (file present but different from canonical). For any other
        // read failure (permission denied, invalid UTF-8, transient
        // I/O, hardware error) DO NOT overwrite — the existing file
        // is the only forensic evidence of what went wrong. Record
        // the case as blocked and move on; the operator triages.
        let needs_write = match tokio::fs::read_to_string(&abs_path).await {
            Ok(existing) => !matches!(
                cairn_core::domain::projection::compare_projection(
                    &projected.content,
                    Some(&existing),
                ),
                cairn_core::domain::projection::ProjectionStatus::Match
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                tracing::warn!(
                    path = %abs_path.display(),
                    error = %e,
                    "lint --fix-markdown: refusing to overwrite unreadable \
                     projection; manual triage required",
                );
                blocked.push(BlockedProjection {
                    path: projected.path.clone(),
                    reason: "read-error".to_owned(),
                    detail: format!("{e}"),
                });
                continue;
            }
        };

        if needs_write {
            // Pre-write fence: a stalled run that lost its lease must
            // refuse to publish more files even if its in-memory snapshot
            // is older than what a concurrent winner already wrote.
            fence().await?;

            // Refuse to call `create_dir_all` on an unchecked path —
            // that helper follows symlinks. If a missing ancestor is
            // swapped to a symlink between the top-of-iteration
            // check and this point, `create_dir_all` would create
            // directories outside the vault as a side effect.
            //
            // Bootstrap creates `raw/` (and the rest of the vault
            // tree) up front; the projector emits `raw/<id>.md` and
            // similar shallow paths. If the parent does NOT exist,
            // someone has tampered with the vault outside the
            // documented projection layout — record it as blocked
            // and skip rather than spelunk a possibly-malicious
            // path with `create_dir_all`.
            if let Some(parent) = abs_path.parent() {
                let parent_meta = tokio::fs::symlink_metadata(parent).await;
                let parent_ok = matches!(
                    parent_meta.as_ref(),
                    Ok(m) if m.file_type().is_dir(),
                );
                if !parent_ok {
                    let detail = match parent_meta {
                        Err(e) => format!("parent {} is unreachable: {e}", parent.display()),
                        Ok(_) => {
                            format!("parent {} is a symlink or non-directory", parent.display())
                        }
                    };
                    tracing::warn!(
                        parent = %parent.display(),
                        "lint --fix-markdown: parent must be a real directory; \
                         refusing to mkdir on an unchecked path",
                    );
                    blocked.push(BlockedProjection {
                        path: projected.path.clone(),
                        reason: "unsafe-parent".to_owned(),
                        detail,
                    });
                    continue;
                }
            }

            // Stage 1: build the temp file fully on disk (write + fsync)
            // before the publish fence. The temp lives in the destination
            // directory, so `persist()` is a same-filesystem rename(2).
            let content = projected.content.clone();
            let parent_buf = abs_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            let staged: tempfile::NamedTempFile = tokio::task::spawn_blocking(move || {
                use std::io::Write as _;
                let mut tmp = tempfile::Builder::new()
                    .suffix(".md.tmp")
                    .tempfile_in(&parent_buf)
                    .with_context(|| format!("create temp file in {}", parent_buf.display()))?;
                tmp.write_all(content.as_bytes())
                    .with_context(|| format!("write temp {}", tmp.path().display()))?;
                tmp.as_file()
                    .sync_all()
                    .with_context(|| format!("fsync temp {}", tmp.path().display()))?;
                Ok::<_, anyhow::Error>(tmp)
            })
            .await
            .with_context(|| format!("spawn_blocking stage {}", abs_path.display()))??;

            // Publish fence: re-check ownership immediately before the
            // rename. Combined with the pre-write fence above, the only
            // window in which a stale writer can publish is the time
            // between this check and the rename(2) syscall — a single
            // spawn_blocking schedule + `persist()` call. Drop the
            // staged temp file on lease loss so it never reaches the
            // canonical path.
            fence().await?;

            // Late-bound TOCTOU defense: re-validate path safety
            // immediately before publish. The first `check_write_safe`
            // at the top of this iteration runs before stage; a
            // concurrent actor that swapped an ancestor for a symlink
            // between then and here would otherwise let `persist()`
            // rename through the symlink. This second check shrinks
            // the unsafe window to the single rename(2) syscall —
            // not zero, but bounded.
            let vroot = vault_root.to_path_buf();
            let dest_for_check = abs_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::vault::bootstrap::check_write_safe(&vroot, &dest_for_check)
            })
            .await
            .with_context(|| format!("spawn_blocking revalidate {}", abs_path.display()))??;

            // Persist + parent directory fsync. POSIX `rename(2)`
            // updates the dirent but the change can be lost on a
            // crash unless the *containing directory* is fsync'd.
            // Without this, the WAL op can reach COMMITTED while
            // the on-disk projection silently reverts to the
            // pre-rename name on power loss — exactly the partial-
            // write recovery case `--fix-markdown` exists to
            // repair, but worse: now the WAL says the repair
            // already happened. Sync the directory before this
            // iteration counts as written.
            let dest = abs_path.clone();
            let parent_for_sync = abs_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            tokio::task::spawn_blocking(move || {
                staged.persist(&dest).map_err(|e| {
                    anyhow::anyhow!("persist temp -> {}: {}", dest.display(), e.error)
                })?;
                let dir = std::fs::File::open(&parent_for_sync).with_context(|| {
                    format!("open parent for fsync {}", parent_for_sync.display())
                })?;
                dir.sync_all()
                    .with_context(|| format!("fsync parent dir {}", parent_for_sync.display()))?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .with_context(|| format!("spawn_blocking publish {}", abs_path.display()))??;
            written.push(projected.path);
        } else {
            already_current += 1;
        }
    }

    // Convergence check: re-list active records and detect drift the
    // run cannot have repaired. The `lint_repair` lock does not fence
    // concurrent ingest/upsert/tombstone, so the DB can change
    // between our per-record `get_active_by_target` and the end of
    // the run. Two distinct cases need reporting:
    //   1. New target_id appeared after our snapshot — never
    //      projected by this run.
    //   2. Existing target_id we did project, but the active
    //      record_id is now different — our on-disk projection
    //      reflects a superseded version.
    // Either case means a successful WAL commit would leave the
    // vault stale; surface them as `blocked` so the caller fails
    // non-zero and the operator re-runs.
    let post = store
        .list_active_stored(&cairn_core::contract::memory_store::ListArgs::default())
        .await
        .map_err(anyhow::Error::msg)
        .context("store: list_active_stored (post)")?;
    for s in &post {
        let tid = s.record.target_id.as_str();
        let now_rid = s.record.id.as_str();
        match projected_at.get(tid) {
            None => {
                blocked.push(BlockedProjection {
                    path: PathBuf::from(format!("(target {tid})")),
                    reason: "post-snapshot-record".to_owned(),
                    detail: "record ingested during repair; re-run cairn lint --fix-markdown \
                             to converge"
                        .to_owned(),
                });
            }
            Some(seen_rid) if seen_rid != now_rid => {
                blocked.push(BlockedProjection {
                    path: PathBuf::from(format!("(target {tid})")),
                    reason: "post-write-update".to_owned(),
                    detail: format!(
                        "record updated during repair (projected {seen_rid}, now active {now_rid}); \
                         re-run cairn lint --fix-markdown to converge"
                    ),
                });
            }
            Some(_) => {}
        }
    }

    Ok(FixMarkdownResult {
        written,
        already_current,
        blocked,
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
#[allow(clippy::too_many_lines)] // dispatcher wires multiple subsystems; extraction deferred
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
    let source_artifacts = build_source_artifacts(vault_root, &lint_records).await;
    let source_forgets = build_source_forgets(vault_root)
        .with_context(|| format!("lint: source-forget snapshot from {}", vault_root.display()))?;

    let unresolvable_authors: std::collections::HashSet<cairn_core::domain::Identity> =
        prefetch_failures.keys().cloned().collect();
    let source_resolver = VaultFsSourceResolver::new(vault_root);
    let (consent_journal, consent_journal_unavailable) = open_consent_journal(vault_root)?;
    let hot_body_loader = |step| super::assemble_hot::lint_step_body_sync(vault_root, config, step);
    // Issue #92, spec §4.8/§4.12: open a read-only handle to the vault
    // DB so the `workflow_health` lint check can surface dead-letter /
    // stuck / stale / overdue findings. Missing or unreadable DB
    // degrades to `None`, matching the consent-journal degraded path.
    let workflow_jobs_reader = open_workflow_jobs_reader(vault_root);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let inputs = LintInputs {
        records: &lint_records,
        config,
        index_stats,
        author_states: &author_states,
        unresolvable_authors: &unresolvable_authors,
        consent_lookup,
        source_artifacts: &source_artifacts,
        source_forgets: &source_forgets,
        vault_root: Some(vault_root),
        hot_body_loader: Some(&hot_body_loader),
        source_resolver: &source_resolver,
        consent_journal: &consent_journal,
        workflow_jobs: workflow_jobs_reader
            .as_ref()
            .map(|r| r as &dyn cairn_core::contract::workflow_jobs::WorkflowJobsReader),
        now_ms,
    };
    let mut data = run_checks(&inputs).await;

    if index_stats_skipped {
        push_index_stats_skipped(&mut data);
    }

    if consent_journal_unavailable && !lint_records.is_empty() {
        push_consent_journal_unavailable(&mut data);
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

    // Projection-drift pass: read-only, Warning-severity only. Extracted
    // to keep lint_handler within the line-count limit.
    append_projection_drift_findings(store, vault_root, &mut data).await?;

    let has_error = data.findings.iter().any(|f| {
        matches!(
            f.severity,
            cairn_core::generated::verbs::lint::Severity::Error,
        )
    });

    let report_path = if write_report {
        Some(write_lint_report(vault_root, &mut data).await?)
    } else {
        None
    };

    Ok(LintHandlerResult {
        data,
        report_path,
        has_error,
    })
}

async fn build_source_artifacts(
    vault_root: &Path,
    lint_records: &[cairn_core::verbs::lint::LintRecord],
) -> HashMap<SourceId, cairn_core::verbs::lint::SourceArtifact> {
    let mut source_ids: Vec<SourceId> = lint_records
        .iter()
        .flat_map(|record| record.stored.record.provenance.source_ids.iter().cloned())
        .collect();
    source_ids.sort();
    source_ids.dedup();

    let mut artifacts = HashMap::with_capacity(source_ids.len());
    for source_id in source_ids {
        let path = source_id.as_str().to_owned();
        let abs = vault_root.join(&path);
        let state = match tokio::fs::read(&abs).await {
            Ok(bytes) => match parse_redaction_marker(&bytes) {
                Some(original_sha256) => {
                    cairn_core::verbs::lint::SourceArtifactState::Redacted { original_sha256 }
                }
                None => cairn_core::verbs::lint::SourceArtifactState::Present {
                    sha256: format!("sha256:{:x}", Sha256::digest(&bytes)),
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cairn_core::verbs::lint::SourceArtifactState::Missing
            }
            Err(error) => cairn_core::verbs::lint::SourceArtifactState::Unreadable {
                message: error.to_string(),
            },
        };
        artifacts.insert(
            source_id,
            cairn_core::verbs::lint::SourceArtifact { path, state },
        );
    }

    artifacts
}

fn build_source_forgets(
    vault_root: &Path,
) -> anyhow::Result<HashMap<String, cairn_core::verbs::lint::SourceForgetLedger>> {
    // Lint may run against a vault without a bootstrapped store
    // (fixture-only tests, freshly-created vault). Treat the missing DB
    // as "no source-forget receipts yet" rather than a hard failure.
    let db_path = vault_root.join(".cairn/cairn.db");
    if !db_path.exists() {
        return Ok(HashMap::new());
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut ledgers: HashMap<String, cairn_core::verbs::lint::SourceForgetLedger> = HashMap::new();
    for (_rowid, event) in cairn_store_sqlite::consent::read_since_rowid(&conn, 0)? {
        let cairn_core::domain::ConsentPayload::IntentReceipt {
            target_id_hash,
            reason_code,
            ..
        } = event.payload
        else {
            continue;
        };
        if event.kind != cairn_core::domain::ConsentKind::ForgetIntent
            || !reason_code.starts_with("source_forget")
        {
            continue;
        }
        ledgers
            .entry(event.subject)
            .or_default()
            .forgotten_target_hashes
            .insert(target_id_hash);
    }
    Ok(ledgers)
}

fn parse_redaction_marker(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "cairn:redacted-source:v1" {
        return None;
    }
    lines.find_map(|line| line.strip_prefix("source_hash=").map(str::to_owned))
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

/// Render the lint report markdown and write it atomically under
/// `.cairn/lint-report.md` inside `vault_root`. Returns the
/// vault-relative path stamped onto `data.report_path`.
async fn write_lint_report(
    vault_root: &Path,
    data: &mut cairn_core::generated::verbs::lint::LintData,
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

/// Open the `SQLite` consent-journal snapshot at the vault's standard
/// `.cairn/cairn.db` path. Returns `(reader, unavailable)` — when the
/// DB file is absent (and the vault is empty, fresh-init case) the
/// unavailable flag drives a `DeferredCheck` finding rather than
/// silently substituting an empty reader. Substituting silently turns
/// every `source_after_forget` / `source_redact_skipped` check into a
/// false negative when the store backend doesn't live at the default
/// path — exactly the privacy regression to avoid.
fn open_consent_journal(vault_root: &Path) -> anyhow::Result<(SqliteConsentJournalReader, bool)> {
    let db_path = vault_root.join(".cairn/cairn.db");
    if db_path.is_file() {
        let reader = SqliteConsentJournalReader::open(&db_path)
            .map_err(|e| anyhow::anyhow!("store: consent_journal: {e}"))
            .context("lint: consent_journal")?;
        Ok((reader, false))
    } else {
        Ok((SqliteConsentJournalReader::default(), true))
    }
}

/// Open a read-only `workflow_jobs` reader for the lint dispatch path
/// (issue #92, spec §4.8/§4.10/§4.12). Returns `None` when the vault DB
/// is missing or the connection cannot be opened — the
/// `workflow_health` check stays on its no-op path, matching the
/// consent-journal degraded behaviour. Errors are not propagated to the
/// caller because workflow health is advisory; a missing reader means
/// "lint cannot see workflow jobs", not "lint must abort".
fn open_workflow_jobs_reader(vault_root: &Path) -> Option<SqliteWorkflowJobsReader> {
    let db_path = vault_root.join(".cairn/cairn.db");
    if !db_path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    Some(SqliteWorkflowJobsReader::new(conn))
}

/// Append a `deferred_check` info finding noting that the
/// `consent_journal` snapshot is unavailable — forget-related rules
/// degrade to false negatives until the backend can provide a real
/// journal. Mirror the index-stats degraded path so summary
/// aggregates stay consistent.
fn push_consent_journal_unavailable(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        entities: None,
        kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
        message: "consent_journal snapshot unavailable; source_after_forget / source_redact_skipped checks skipped"
            .to_owned(),
        severity: cairn_core::generated::verbs::lint::Severity::Info,
        suggested_fix: Some(
            "configure a backend that surfaces consent_journal rows (default vault layout creates .cairn/cairn.db)"
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

/// Append a `deferred_check` info finding noting that `MemoryStore::index_stats`
/// is unavailable on this adapter, and keep all summary aggregates
/// (`total`, `by_severity.info`, `by_kind["deferred_check"]`) consistent.
fn push_index_stats_skipped(data: &mut cairn_core::generated::verbs::lint::LintData) {
    let f = cairn_core::generated::verbs::lint::Finding {
        entities: None,
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
        entities: None,
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
        entities: None,
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
        entities: None,
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

/// Walk every active record, compare its canonical markdown projection
/// against what is on disk, and append any `ProjectionDrift` or
/// `ProjectionMissing` findings to `data`. Read-only — never writes to the
/// vault. Findings are Warning-severity only and do not affect `has_error`.
///
/// Extracted from `lint_handler` to keep that function within the 100-line
/// clippy limit while preserving full test coverage.
///
/// # Errors
///
/// Returns an error if the store cannot be queried or if a file read fails
/// for any reason other than `NotFound`.
async fn append_projection_drift_findings(
    store: &dyn cairn_core::contract::memory_store::MemoryStore,
    vault_root: &Path,
    data: &mut cairn_core::generated::verbs::lint::LintData,
) -> anyhow::Result<()> {
    use cairn_core::contract::memory_store::ListArgs;

    let active = store
        .list_active_stored(&ListArgs::default())
        .await
        .map_err(anyhow::Error::msg)
        .context("lint: list_active_stored for projection drift")?;

    let projector = MarkdownProjector;
    for stored in &active {
        let projected = projector.project(stored);
        let abs = vault_root.join(&projected.path);

        // No-follow validation BEFORE the read. `read_to_string` follows
        // symlinks; without this guard, a symlinked projection could be
        // read through to a target whose content matches the canonical
        // projection — `compare_projection` would return Match and lint
        // would emit no finding, while `--fix-markdown` would refuse to
        // write through the same path. Surface the unsafe path as a Drift
        // finding instead of silently blessing it.
        let safe = {
            let vroot = vault_root.to_path_buf();
            let dest = abs.clone();
            tokio::task::spawn_blocking(move || {
                crate::vault::bootstrap::check_write_safe(&vroot, &dest)
            })
            .await
            .with_context(|| format!("spawn_blocking validate {}", abs.display()))?
        };
        if let Err(symlink_err) = safe {
            // Treat any symlinked / non-regular projection as drift —
            // `--fix-markdown` would refuse the same path, so lint must
            // not bless it. Encode the symlink reason in a synthetic
            // body hash so the finding still carries actionable detail.
            let status = cairn_core::domain::projection::ProjectionStatus::Drift {
                expected_body_hash: cairn_core::domain::projection::body_hash(&projected.content),
                actual_body_hash: format!("unsafe-projection-path: {symlink_err}"),
            };
            if let Some(f) = cairn_core::verbs::lint::checks::projection::finding_for(
                stored.record.id.as_str(),
                projected.path.to_string_lossy().as_ref(),
                &status,
            ) {
                push_projection_finding(data, f);
            }
            continue;
        }

        let actual = match tokio::fs::read_to_string(&abs).await {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                // A single unreadable projection (permission denied, I/O
                // error, invalid UTF-8) must NOT abort the whole lint —
                // the read-only `lint` is a survey, not a transaction.
                // Surface the failure as a per-record Drift finding so
                // the user sees actionable detail and continues scanning
                // the rest of the vault.
                let status = cairn_core::domain::projection::ProjectionStatus::Drift {
                    expected_body_hash: cairn_core::domain::projection::body_hash(
                        &projected.content,
                    ),
                    actual_body_hash: format!("read-error: {e}"),
                };
                if let Some(f) = cairn_core::verbs::lint::checks::projection::finding_for(
                    stored.record.id.as_str(),
                    projected.path.to_string_lossy().as_ref(),
                    &status,
                ) {
                    push_projection_finding(data, f);
                }
                continue;
            }
        };
        let status = cairn_core::domain::projection::compare_projection(
            &projected.content,
            actual.as_deref(),
        );
        if let Some(f) = cairn_core::verbs::lint::checks::projection::finding_for(
            stored.record.id.as_str(),
            projected.path.to_string_lossy().as_ref(),
            &status,
        ) {
            push_projection_finding(data, f);
        }
    }
    Ok(())
}

/// Append a projection-drift or projection-missing finding and keep
/// `data.summary` consistent. Severity is taken from the finding —
/// auto-repairable cases stay Warning, but unsafe-path / read-error
/// cases that `--fix-markdown` cannot resolve are emitted as Error so
/// `has_error` flips and `cairn lint` exits non-zero.
fn push_projection_finding(
    data: &mut cairn_core::generated::verbs::lint::LintData,
    f: cairn_core::generated::verbs::lint::Finding,
) {
    let kind_key = match f.kind {
        cairn_core::generated::verbs::lint::Kind::ProjectionDrift => "projection_drift",
        cairn_core::generated::verbs::lint::Kind::ProjectionMissing => "projection_missing",
        // Safety: this helper is only called with projection finding kinds.
        _ => "unknown",
    };
    data.summary.total += 1;
    // The `LintResult.has_error` flag is computed downstream by
    // walking `data.findings` for any Error-severity entry, so we
    // only need to keep the per-severity counter in sync here.
    match f.severity {
        cairn_core::generated::verbs::lint::Severity::Error => {
            data.summary.by_severity.error += 1;
        }
        cairn_core::generated::verbs::lint::Severity::Warning => {
            data.summary.by_severity.warning += 1;
        }
        cairn_core::generated::verbs::lint::Severity::Info => {
            data.summary.by_severity.info += 1;
        }
        // `Severity` is `#[non_exhaustive]`; an unrecognised variant
        // gets counted as a warning so the total stays consistent.
        _ => data.summary.by_severity.warning += 1,
    }
    if let serde_json::Value::Object(map) = &mut data.summary.by_kind {
        let entry = map
            .entry(kind_key.to_owned())
            .or_insert(serde_json::Value::from(0_u64));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::from(n.saturating_add(1));
        }
    }
    data.findings.push(f);
}

/// Run `cairn lint`.
///
/// `vault_root` is the already-resolved vault root from `main.rs` —
/// `cairn_cli::vault::resolve_vault` runs the registry lookup, the
/// walk-up search, and the `CAIRN_VAULT` fallback before this verb
/// is dispatched. Passing the raw selector string here would re-do
/// (and frequently mis-do) that resolution: registry names would
/// be treated as relative paths, and a subdirectory inside a vault
/// would look for `.cairn/cairn.db` under the subdirectory. `None`
/// falls back to cwd, which only happens when the top-level guard
/// tolerated a `NoneResolved` outcome.
#[must_use]
#[allow(clippy::too_many_lines)] // dispatcher fans to --fix, vault-level, and edge-level paths
pub fn run(sub: &ArgMatches, vault_root: Option<&Path>) -> ExitCode {
    let json = sub.get_flag("json");
    let fix = sub.get_flag("fix");
    let fix_markdown_flag = sub.get_flag("fix-markdown");
    let fix_folders_flag = sub.get_flag("fix-folders");
    let write_report = sub.get_flag("write_report");
    let plan_id = sub.get_one::<String>("plan").cloned();
    let operation_id = new_operation_id();

    if let Some(plan_id) = plan_id {
        return run_plan_lint(json, vault_root, &plan_id);
    }

    if fix_markdown_flag {
        return run_fix_markdown(json, vault_root);
    }

    if fix_folders_flag {
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

    // --fix resolves WAL edge contradictions via raw rusqlite (no store
    // migration path); keep that path unchanged.
    if fix {
        match run_edge_lint(true, vault_root, &operation_id) {
            Ok(report) => {
                let has_blocking_findings = report.findings.iter().any(has_warning_or_error);
                let data = lint_data(report);
                let response = committed_response(operation_id, data);
                if json {
                    emit_json(&response);
                } else if let Some(ResponseData::Lint(data)) = response.data.as_ref() {
                    emit_human(data, &response.operation_id);
                }
                if has_blocking_findings {
                    return ExitCode::FAILURE;
                }
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                emit_aborted(json, operation_id, &err.to_string());
                return ExitCode::FAILURE;
            }
        }
    }

    // Default (read-only) path: dispatch through lint_handler so the
    // hot-memory walker (BrokenSourceLink, MissingSummary,
    // StaleProfileLine) and all other vault-level checks run.
    let vault_root = vault_root.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let db_path = vault_root.join(".cairn").join("cairn.db");

    if let Some(exit) = require_existing_vault(json, &vault_root, &db_path) {
        return exit;
    }

    let config = match crate::config::load(&vault_root, &crate::config::CliOverrides::default()) {
        Ok(c) => c,
        Err(e) => {
            emit_aborted(json, operation_id, &format!("config: {e:#}"));
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            emit_aborted(json, operation_id, &format!("tokio init: {e}"));
            return ExitCode::FAILURE;
        }
    };

    let outcome = rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path)
            .await
            .map_err(|e| anyhow::anyhow!("open store: {e}"))?;
        let registry = cairn_store_sqlite::SqliteIdentityRegistry::open(&db_path)
            .map_err(|e| anyhow::anyhow!("open registry: {e}"))?;
        lint_handler(&store, &registry, None, &config, write_report, &vault_root).await
    });

    // Helper: run edge lint, merge its findings into `data`, recompute
    // summary, emit response. Returns the correct `ExitCode`.
    let emit_merged = |mut data: cairn_core::generated::verbs::lint::LintData| {
        // Also run the edge-integrity pass (read-only) so WAL
        // contradiction findings are surfaced alongside vault-level
        // findings. run_edge_lint uses its own rusqlite connection
        // (read-only flags), so it is safe to call after the async
        // store is closed. Failures are non-fatal: if the edge schema
        // is missing the check is skipped rather than aborting an
        // otherwise clean lint run.
        if let Ok(edge_report) = run_edge_lint(false, Some(&vault_root), &operation_id) {
            data.findings.extend(edge_report.findings);
        }
        // Recompute summary to include merged edge findings.
        let total = usize_to_u64(data.findings.len());
        data.summary = edge_summary(&data.findings, total, 0);
        let has_error_or_warning = data.findings.iter().any(has_warning_or_error);
        let response = committed_response(operation_id, data);
        if json {
            emit_json(&response);
        } else if let Some(ResponseData::Lint(d)) = response.data.as_ref() {
            emit_human(d, &response.operation_id);
        }
        if has_error_or_warning {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    };

    match outcome {
        Ok(result) => emit_merged(result.data),
        Err(store_err) => {
            // `cairn_store_sqlite::open` rejected the vault (e.g. schema
            // fingerprint mismatch on a manually-modified DB). Degrade
            // gracefully: run the edge-only pass so contradiction findings
            // still surface, and emit a committed response rather than
            // aborting. This is intentionally lenient: schema verification
            // is a store concern; the lint verb should not abort on
            // validator disagreements that the user could resolve with
            // `cairn lint --fix`.
            //
            // Codex review round 1 finding 5: do NOT silently emit empty
            // findings. Surface the store failure as a `DeferredCheck`
            // Error so operators see explicitly that vault-level checks
            // (including the hot-memory walker) did not run.
            let store_err_msg = store_err.to_string();
            let deferred = cairn_core::generated::verbs::lint::Finding {
                entities: None,
                kind: cairn_core::generated::verbs::lint::Kind::DeferredCheck,
                message: format!(
                    "lint vault-level checks did not run: store open failed ({store_err_msg}); \
                     edge-integrity pass is the only coverage in this response"
                ),
                severity: cairn_core::generated::verbs::lint::Severity::Error,
                suggested_fix: Some(
                    "inspect .cairn/cairn.db schema state; try `cairn lint --fix` to repair \
                     WAL edge contradictions, then re-run `cairn lint`"
                        .to_owned(),
                ),
                target: None,
                tracking_issue: Some(83),
            };
            let degraded_data = cairn_core::generated::verbs::lint::LintData {
                findings: vec![deferred],
                report_path: None,
                summary: cairn_core::generated::verbs::lint::LintDataSummary {
                    auto_resolved: Some(0),
                    by_kind: serde_json::Value::Object(serde_json::Map::new()),
                    by_severity: LintDataSummaryBySeverity {
                        error: 1,
                        warning: 0,
                        info: 0,
                    },
                    total: 1,
                },
            };
            emit_merged(degraded_data)
        }
    }
}

/// `cairn lint --plan <ULID>` (issue #289): structurally lint a pending
/// `FlushPlan`, then check rename mutations against the live store. Surfaces
/// rename-target collisions before `flush apply` consumes the plan.
#[allow(
    clippy::too_many_lines,
    reason = "linear load → walk-mutations → live-check → emit; splitting would scatter \
              error-emission boilerplate (json vs human) across helpers without simplifying flow"
)]
fn run_plan_lint(json: bool, vault_root: Option<&Path>, plan_id: &str) -> ExitCode {
    use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
    use cairn_core::domain::flush_plan::{PersistedPlan, PlannedMutation};

    let op = new_operation_id();
    // Reject non-canonical ULIDs before constructing the filesystem path.
    // `plan_path` interpolates the id into the path; without this gate a
    // caller could traverse via `..` or absolute-path components and point
    // the lint at arbitrary `.plan.json` files outside `.cairn/flush/pending`.
    if !super::flush::is_valid_ulid_str(plan_id) {
        let msg =
            format!("lint --plan: invalid ULID `{plan_id}` (expected 26-char Crockford base32)");
        if json {
            emit_json(&serde_json::json!({
                "code": "InvalidArgument",
                "message": msg,
                "operation_id": op.0,
            }));
        } else {
            human_error("lint", "InvalidArgument", &msg, &op);
        }
        return ExitCode::from(64);
    }
    let Some(vault_root) = vault_root else {
        let msg = "lint --plan requires a resolved vault root: pass --vault NAME_OR_PATH \
                   or set CAIRN_VAULT";
        if json {
            emit_json(&serde_json::json!({
                "code": "Internal",
                "message": msg,
                "operation_id": op.0,
            }));
        } else {
            human_error("lint", "Internal", msg, &op);
        }
        return ExitCode::from(78);
    };

    let ulid = cairn_core::generated::common::Ulid(plan_id.to_owned());
    let path = plan_path(vault_root, Bucket::Pending, &ulid);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("read pending plan {plan_id}: {e}");
            if json {
                emit_json(&serde_json::json!({
                    "code": "NotFound",
                    "message": msg,
                    "operation_id": op.0,
                }));
            } else {
                human_error("lint", "NotFound", &msg, &op);
            }
            return ExitCode::from(66);
        }
    };
    let persisted: PersistedPlan = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("parse pending plan {plan_id}: {e}");
            if json {
                emit_json(&serde_json::json!({
                    "code": "Internal",
                    "message": msg,
                    "operation_id": op.0,
                }));
            } else {
                human_error("lint", "Internal", &msg, &op);
            }
            return ExitCode::from(65);
        }
    };

    let mut findings: Vec<String> = Vec::new();
    let mut seen_new_ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut renames: Vec<(cairn_core::domain::TargetId, cairn_core::domain::TargetId)> = Vec::new();

    for (idx, mutation) in persisted.plan.mutations.iter().enumerate() {
        if let PlannedMutation::Rename { record_id, new_id } = mutation {
            if record_id == new_id {
                findings.push(format!(
                    "rename mutation #{idx}: source and destination are identical (`{}`)",
                    new_id.as_str(),
                ));
            }
            if let Some(prev) = seen_new_ids.insert(new_id.as_str().to_owned(), idx) {
                findings.push(format!(
                    "rename target collision (intra-plan): mutations #{prev} and #{idx} both \
                     rename to `{}`",
                    new_id.as_str(),
                ));
            }
            renames.push((record_id.clone(), new_id.clone()));
        }
    }

    let db_path = vault_root.join(".cairn").join("cairn.db");
    if db_path.exists() && !renames.is_empty() {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let msg = format!("tokio init error: {e}");
                if json {
                    emit_json(&serde_json::json!({
                        "code": "Internal",
                        "message": msg,
                        "operation_id": op.0,
                    }));
                } else {
                    human_error("lint", "Internal", &msg, &op);
                }
                return ExitCode::FAILURE;
            }
        };
        // Round 10 review fix: use the same historical-collision rule
        // `flush apply rename` enforces, not just the live check —
        // otherwise lint can bless a plan that apply will reject for
        // hitting a retired target lineage.
        let renames_owned: Vec<(cairn_core::domain::TargetId, cairn_core::domain::TargetId)> =
            renames
                .iter()
                .map(|(s, d)| ((*s).clone(), (*d).clone()))
                .collect();
        let live_check: Result<Vec<String>, anyhow::Error> = rt.block_on(async {
            let store = cairn_store_sqlite::open(&db_path).await?;
            let renames = renames_owned;
            store
                .with_tx(move |tx| {
                    let mut hits = Vec::new();
                    for (src, dst) in &renames {
                        if tx.target_id_ever_used(dst)? {
                            let active = tx.get_active_by_target(dst)?;
                            let kind = if active.is_some() { "live" } else { "retired" };
                            hits.push(format!(
                                "rename target collision ({kind}): `{}` has been used as a \
                                 target lineage — renaming `{}` would conflict",
                                dst.as_str(),
                                src.as_str(),
                            ));
                        }
                    }
                    Ok(hits)
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        });
        match live_check {
            Ok(hits) => findings.extend(hits),
            Err(e) => {
                let msg = format!("open store for live rename check: {e}");
                if json {
                    emit_json(&serde_json::json!({
                        "code": "Internal",
                        "message": msg,
                        "operation_id": op.0,
                    }));
                } else {
                    human_error("lint", "Internal", &msg, &op);
                }
                return ExitCode::FAILURE;
            }
        }
    }

    let has_findings = !findings.is_empty();
    if json {
        emit_json(&serde_json::json!({
            "operation_id": op.0,
            "plan_id": plan_id,
            "findings": findings,
        }));
    } else if has_findings {
        eprintln!("cairn lint --plan {plan_id}: {} finding(s)", findings.len());
        for f in &findings {
            eprintln!("  - {f}");
        }
    } else {
        println!("cairn lint --plan {plan_id}: clean");
    }

    if has_findings {
        ExitCode::from(65) // EX_DATAERR
    } else {
        ExitCode::SUCCESS
    }
}

fn run_edge_lint(
    fix: bool,
    vault_root: Option<&Path>,
    operation_id: &Ulid,
) -> Result<EdgeLintReport, StoreError> {
    let root = vault_root.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let db_path = root.join(".cairn").join("cairn.db");

    if fix {
        let mut conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        lint_edges(&conn)?;
        resolve_edge_contradictions(
            &mut conn,
            chrono::Utc::now().timestamp_millis(),
            &operation_id.0,
        )
    } else {
        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        lint_edges(&conn)
    }
}

fn committed_response(operation_id: Ulid, data: LintData) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Lint(data)),
        error: None,
        operation_id,
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Lint,
    }
}

fn aborted_response(operation_id: Ulid, message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id,
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb: ResponseVerb::Lint,
    }
}

fn lint_data(report: EdgeLintReport) -> LintData {
    let total = usize_to_u64(report.findings.len());
    let summary = edge_summary(&report.findings, total, report.auto_resolved);
    LintData {
        findings: report.findings,
        report_path: None,
        summary,
    }
}

fn edge_summary(findings: &[Finding], total: u64, auto_resolved: u64) -> LintDataSummary {
    let mut by_severity = LintDataSummaryBySeverity {
        error: 0,
        warning: 0,
        info: 0,
    };
    let mut by_kind = serde_json::Map::new();

    for finding in findings {
        match finding.severity {
            Severity::Error => by_severity.error += 1,
            Severity::Info => by_severity.info += 1,
            _ => by_severity.warning += 1,
        }
        let key = kind_key(finding.kind);
        let entry = by_kind
            .entry(key)
            .or_insert_with(|| serde_json::Value::Number(0.into()));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::Number((n + 1).into());
        }
    }

    LintDataSummary {
        auto_resolved: Some(auto_resolved),
        by_kind: serde_json::Value::Object(by_kind),
        by_severity,
        total,
    }
}

fn emit_aborted(json: bool, operation_id: Ulid, message: &str) {
    let response = aborted_response(operation_id, message);
    if json {
        emit_json(&response);
    } else {
        human_error("lint", "Internal", message, &response.operation_id);
    }
}

fn has_warning_or_error(finding: &Finding) -> bool {
    matches!(finding.severity, Severity::Warning | Severity::Error)
}

fn emit_human(data: &LintData, operation_id: &Ulid) {
    println!("cairn lint: committed (operation_id: {})", operation_id.0);
    println!(
        "summary: total={} contradictions={} ambiguous_edges={} purge_pending={} auto_resolved={}",
        data.summary.total,
        summary_count(data, "contradictory_edge"),
        summary_count(data, "ambiguous_edge"),
        data.findings
            .iter()
            .filter(|finding| {
                finding.kind == Kind::DeferredCheck && finding.message.starts_with("purge_pending:")
            })
            .count(),
        data.summary.auto_resolved.unwrap_or(0),
    );

    for finding in &data.findings {
        println!("{}: {}", severity_key(finding.severity), finding.message);
    }
}

fn kind_key(kind: Kind) -> String {
    match kind {
        Kind::ContradictoryEdge => "contradictory_edge",
        Kind::AmbiguousEdge => "ambiguous_edge",
        Kind::Contradiction => "contradiction",
        Kind::Orphan => "orphan",
        Kind::Stale => "stale",
        Kind::MissingConcept => "missing_concept",
        Kind::DataGap => "data_gap",
        Kind::MalformedRecord => "malformed_record",
        Kind::BrokenActorChain => "broken_actor_chain",
        Kind::MissingProvenance => "missing_provenance",
        Kind::StaleSchema => "stale_schema",
        Kind::HotMemoryOverBudget => "hot_memory_over_budget",
        Kind::IndexDrift => "index_drift",
        Kind::DeferredCheck => "deferred_check",
        Kind::ProjectionDrift => "projection_drift",
        Kind::ProjectionMissing => "projection_missing",
        _ => "unknown",
    }
    .to_owned()
}

fn severity_key(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        _ => "unknown",
    }
}

fn summary_count(data: &LintData, kind: &str) -> u64 {
    data.summary
        .by_kind
        .as_object()
        .and_then(|by_kind| by_kind.get(kind))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Dispatch `--fix-markdown`: open the store, acquire the lint-repair lock,
/// run the handler, emit the result or error.
///
/// `vault_root` is the registry-resolved vault root from main.rs —
/// see [`run`] for why we accept the resolved `Path` instead of the
/// raw `--vault` selector string.
// Sequential dispatch: vault resolution, store open, lock acquire,
// outcome match. Splitting forces every error variant through extra
// helper signatures without making the flow easier to follow.
#[allow(clippy::too_many_lines)]
fn run_fix_markdown(json: bool, vault_root: Option<&Path>) -> ExitCode {
    // Mutating modes (`--fix-markdown`) MUST run against a
    // registry-resolved vault root, never the cwd fallback. Round 5
    // pointed out that `main.rs` tolerates non-`NotFound`
    // resolution failures (so `vault_root == None` can mean
    // "registry parse failed", not just "no vault selected"); a
    // cwd fallback would silently mutate the wrong tree. Fail
    // closed with EX_CONFIG so automation cannot misinterpret an
    // unresolved selector as a successful repair.
    let Some(vault_root) = vault_root else {
        let op = new_operation_id();
        let msg = "lint --fix-markdown requires a resolved vault root: \
                   pass --vault NAME_OR_PATH or set CAIRN_VAULT";
        if json {
            emit_json(&serde_json::json!({
                "code": "CapabilityUnavailable",
                "message": msg,
                "operation_id": op.0,
            }));
        } else {
            human_error("lint", "CapabilityUnavailable", msg, &op);
        }
        return ExitCode::from(78); // EX_CONFIG
    };
    let vault_root = vault_root.to_path_buf();
    let db_path = vault_root.join(".cairn/cairn.db");

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let op = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "code": "Internal",
                    "message": format!("tokio init error: {e}"),
                    "operation_id": op.0,
                }));
            } else {
                human_error("lint", "Internal", &format!("tokio init error: {e}"), &op);
            }
            return ExitCode::FAILURE;
        }
    };

    // Fail closed if the resolved vault has no SQLite store. `open()` is
    // open-or-create, so a mis-resolved cwd would silently create
    // `.cairn/cairn.db` and run --fix-markdown against an empty store.
    if let Some(exit) = require_existing_vault(json, &vault_root, &db_path) {
        return exit;
    }

    let outcome: Result<FixMarkdownResult, LintFixError> = rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path)
            .await
            .map_err(|e| LintFixError::Handler(anyhow::anyhow!("open store: {e}")))?;
        fix_markdown_with_lock(&store, &vault_root, Duration::from_mins(5)).await
    });

    match outcome {
        Ok(r) => {
            // Any blocked record means the run did NOT fully repair
            // the vault. Report it loudly and exit non-zero so
            // automation can distinguish "everything fixed" from
            // "some files require manual triage".
            let has_blocked = !r.blocked.is_empty();
            if json {
                emit_json(&r);
            } else if has_blocked {
                println!(
                    "cairn lint --fix-markdown: wrote {written}, {current} already current, \
                     {skipped} BLOCKED (manual triage required)",
                    written = r.written.len(),
                    current = r.already_current,
                    skipped = r.blocked.len(),
                );
                for b in &r.blocked {
                    println!(
                        "  - {path} [{reason}]: {detail}",
                        path = b.path.display(),
                        reason = b.reason,
                        detail = b.detail,
                    );
                }
            } else {
                println!(
                    "cairn lint --fix-markdown: wrote {written}, {current} already current",
                    written = r.written.len(),
                    current = r.already_current,
                );
            }
            if has_blocked {
                // EX_DATAERR (65): input data is malformed. The
                // unsafe-path / read-error cases are exactly that:
                // the vault contains files that block automated
                // repair. Distinct from EX_UNAVAILABLE so callers
                // can tell "another run is in progress" apart from
                // "manual cleanup needed".
                ExitCode::from(65)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(LintFixError::FixInProgress) => {
            let op = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "code": "CapabilityUnavailable",
                    "message": "lint.fix_in_progress: another --fix-markdown run holds the repair lock",
                    "operation_id": op.0,
                }));
            } else {
                human_error(
                    "lint",
                    "CapabilityUnavailable",
                    "lint.fix_in_progress: another --fix-markdown run holds the repair lock",
                    &op,
                );
            }
            ExitCode::from(69) // EX_UNAVAILABLE
        }
        Err(LintFixError::LockLost) => {
            let op = new_operation_id();
            let msg = "lint.lock_lost: this run's repair lease was reclaimed mid-run; \
                 re-run --fix-markdown to converge";
            if json {
                emit_json(&serde_json::json!({
                    "code": "CapabilityUnavailable",
                    "message": msg,
                    "operation_id": op.0,
                }));
            } else {
                human_error("lint", "CapabilityUnavailable", msg, &op);
            }
            ExitCode::from(69)
        }
        Err(e) => {
            let op = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "code": "Internal",
                    "message": format!("{e}"),
                    "operation_id": op.0,
                }));
            } else {
                human_error("lint", "Internal", &format!("{e}"), &op);
            }
            ExitCode::FAILURE
        }
    }
}

/// Verify `.cairn/cairn.db` exists at the resolved vault root. Returns
/// `Some(ExitCode)` to short-circuit `run_fix_markdown` with `EX_CONFIG`
/// when the file is absent; `None` when the vault looks live.
fn require_existing_vault(json: bool, vault_root: &Path, db_path: &Path) -> Option<ExitCode> {
    if db_path.is_file() {
        return None;
    }
    let op = new_operation_id();
    let msg = format!(
        "no Cairn vault at {}: .cairn/cairn.db is missing. \
         Set CAIRN_VAULT or run from the vault root.",
        vault_root.display()
    );
    if json {
        emit_json(&serde_json::json!({
            "code": "Internal",
            "message": msg,
            "operation_id": op.0,
        }));
    } else {
        human_error("lint", "Internal", &msg, &op);
    }
    Some(ExitCode::from(78)) // EX_CONFIG
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
            blocked: vec![],
        };
        assert_eq!(result.written.len(), 2);
        assert_eq!(result.already_current, 3);
        assert!(result.blocked.is_empty());
    }

    #[test]
    fn fix_markdown_result_empty() {
        let result = FixMarkdownResult {
            written: vec![],
            already_current: 0,
            blocked: vec![],
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
        // Bootstrap the projection layout the same way `cairn init` does;
        // the handler refuses to `create_dir_all` on an unchecked path.
        std::fs::create_dir_all(vault_root.path().join("raw")).unwrap();
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
        // (`MissingFromRegistry`). This test scopes to report-rendering;
        // the §6.2 Error path is exercised in detail by the integration
        // tests.
        let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
        let cfg = CairnConfig::default();
        let vault = tempfile::tempdir().expect("tempdir");
        let result = lint_handler(&store, &registry, None, &cfg, true, vault.path())
            .await
            .expect("handler");

        // Post-rebase posture: §6.2 actor_chain is live. The empty
        // registry means the sample record's author resolves to
        // MissingFromRegistry → BrokenActorChain Error, tripping
        // has_error. §6.5 consent runs against FixtureStore
        // (cap=true, every record LegacyEvent) but FixtureStore does
        // not implement ConsentLookup, so consent.rs returns no
        // findings. §6.3 (#257) now runs real source-link checks, and
        // FixtureStore records still carry empty `source_refs`, so
        // lint emits a `SourceLinkMissing` Warning. Info findings:
        // the §6.2 signature-verification-deferred advisory, the
        // consent-journal-unavailable DeferredCheck, and the two
        // hot_memory deferred advisories from the loader-backed path.
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
        // Empirical post-merge count varies as more checks come
        // online (#83, #257, #258 etc). Pin only that the aggregator
        // emits SOME Info findings rather than the exact count.
        assert!(
            info_count >= 1,
            "expected at least one Info finding; got {info_count}"
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
                auto_resolved: None,
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

    /// E2E corner cases for the §6.6 hot-memory walker (#83; closes #259)
    /// at the CLI handler boundary. Each test seeds a `FixtureStore`,
    /// builds a `CairnConfig` with a custom `vault.hot_memory.recipe`, runs
    /// the full lint pipeline, and inspects only §6.6-specific findings.
    ///
    /// The old deferred-step canary (#259) is gone. The real walker is
    /// gated on `hot_body_loader` being wired by the CLI (Task 25). Until
    /// Task 25 lands, `lint_handler` passes `hot_body_loader: None`, so
    /// the walker emits zero hot-memory findings at the CLI boundary.
    /// Author-registry errors from the empty registry are ignored here —
    /// the §6.2 path is exercised elsewhere.
    mod hot_memory_canary_e2e {
        use super::*;
        use cairn_core::config::{CairnConfig, HotMemoryRecipePreset, HotMemoryRecipeStep};

        #[allow(dead_code)]
        fn set_default_recipe(
            cfg: &mut CairnConfig,
            steps: Vec<HotMemoryRecipeStep>,
            max_bytes: u32,
        ) {
            // Lint resolves the active recipe through
            // `HotMemoryConfig::resolve_recipe`, which reads the named-recipe
            // table. Mutate `recipes[default_recipe]` so each test fixture's
            // overrides actually take effect.
            cfg.vault.hot_memory.recipes.insert(
                cfg.vault.hot_memory.default_recipe.clone(),
                HotMemoryRecipePreset { steps, max_bytes },
            );
        }
        use cairn_core::domain::taxonomy::MemoryKind;
        use cairn_core::generated::verbs::lint::{Kind, Severity};
        use cairn_store_sqlite::SqliteIdentityRegistry;
        use cairn_test_fixtures::sample_record;
        use cairn_test_fixtures::store::FixtureStore;

        async fn run_handler(store: &FixtureStore, cfg: &CairnConfig) -> LintHandlerResult {
            let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");
            let vault = tempfile::tempdir().expect("tempdir");
            lint_handler(store, &registry, None, cfg, false, vault.path())
                .await
                .expect("handler")
        }

        fn count_kind_severity(
            result: &LintHandlerResult,
            kind: Kind,
            severity: Severity,
        ) -> usize {
            result
                .data
                .findings
                .iter()
                .filter(|f| f.kind == kind && f.severity == severity)
                .count()
        }

        /// Counts old-canary (#259) `DeferredCheck` Warnings. After the
        /// rewrite (#83) this should always be zero — the canary is gone.
        fn count_259_deferred(result: &LintHandlerResult) -> usize {
            result
                .data
                .findings
                .iter()
                .filter(|f| {
                    f.kind == Kind::DeferredCheck
                        && f.tracking_issue == Some(259)
                        && f.severity == Severity::Warning
                })
                .count()
        }

        async fn upsert_with(
            store: &FixtureStore,
            seed: u64,
            kind: MemoryKind,
            body: String,
            salience: f32,
        ) {
            use cairn_core::contract::memory_store::MemoryStore;
            let mut r = sample_record(seed);
            r.kind = kind;
            r.body = body;
            r.salience = salience;
            store.upsert(&r).await.expect("upsert");
        }

        /// With no `hot_body_loader` wired (Task 25 is pending), the walker
        /// emits no findings for the default recipe and empty vault.
        /// The old deferred-step canary Warning (#259) is gone.
        #[tokio::test]
        async fn default_recipe_empty_vault_emits_no_hot_memory_findings() {
            let store = FixtureStore::default();
            let cfg = CairnConfig::default();
            let result = run_handler(&store, &cfg).await;
            // No loader → no assembler call → no over-budget finding.
            assert_eq!(count_259_deferred(&result), 0);
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Error),
                0,
            );
        }

        /// Without a loader, even a vault that would be over-budget produces
        /// no hot-memory findings. The real detection requires Task 25.
        #[tokio::test]
        async fn records_only_recipe_over_budget_no_findings_without_loader() {
            let store = FixtureStore::default();
            upsert_with(&store, 1, MemoryKind::Project, "x".repeat(2048), 0.9).await;
            let mut cfg = CairnConfig::default();
            set_default_recipe(&mut cfg, vec![HotMemoryRecipeStep::TopSalienceProject], 256);

            let result = run_handler(&store, &cfg).await;
            // No loader → no over-budget detection.
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Error),
                0,
            );
            // Canary deferred Warning is gone.
            assert_eq!(count_259_deferred(&result), 0);
        }

        /// No loader → no findings even for mixed recipes with deferred steps.
        /// The old canary's `DeferredCheck` Warning for filesystem-backed steps
        /// is removed (#83 closes #259).
        #[tokio::test]
        async fn mixed_recipe_no_findings_without_loader() {
            let store = FixtureStore::default();
            upsert_with(&store, 2, MemoryKind::Project, "tiny".to_owned(), 0.5).await;
            let mut cfg = CairnConfig::default();
            let default_max = cfg.vault.hot_memory.max_bytes;
            set_default_recipe(
                &mut cfg,
                vec![
                    HotMemoryRecipeStep::Index,
                    HotMemoryRecipeStep::TopSalienceProject,
                ],
                default_max,
            );
            let result = run_handler(&store, &cfg).await;
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            // The old #259 canary deferred Warning is gone.
            assert_eq!(count_259_deferred(&result), 0);
        }

        /// Records of kinds not in the recipe recipe do not emit findings.
        #[tokio::test]
        async fn excluded_kinds_do_not_emit_findings() {
            let store = FixtureStore::default();
            upsert_with(&store, 3, MemoryKind::UserSignal, "s".repeat(8192), 0.9).await;
            upsert_with(&store, 4, MemoryKind::Playbook, "p".repeat(8192), 0.9).await;
            let mut cfg = CairnConfig::default();
            set_default_recipe(&mut cfg, vec![HotMemoryRecipeStep::TopSalienceProject], 16);

            let result = run_handler(&store, &cfg).await;
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            assert_eq!(count_259_deferred(&result), 0);
        }

        /// No loader → no overflow detection even with tied-salience records.
        /// The real tied-salience tie-break behavior is tested in
        /// `cairn-core::verbs::lint::checks::hot_memory::tests`.
        #[tokio::test]
        async fn tied_salience_no_findings_without_loader() {
            let store = FixtureStore::default();
            for seed in 10..14 {
                upsert_with(&store, seed, MemoryKind::Project, "x".to_owned(), 0.5).await;
            }
            for seed in 20..26 {
                upsert_with(&store, seed, MemoryKind::Project, "L".repeat(800), 0.5).await;
            }
            let mut cfg = CairnConfig::default();
            set_default_recipe(
                &mut cfg,
                vec![HotMemoryRecipeStep::TopSalienceProject],
                2_000,
            );

            let result = run_handler(&store, &cfg).await;
            // No loader → no findings regardless of salience/budget.
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
                "no loader wired: walker must not emit over-budget finding",
            );
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Error),
                0,
            );
        }

        /// Degenerate `max_bytes=1` must not panic, and without a loader must
        /// produce no hot-memory findings.
        #[tokio::test]
        async fn degenerate_max_bytes_one_does_not_panic() {
            let store = FixtureStore::default();
            upsert_with(&store, 30, MemoryKind::Project, "x".to_owned(), 0.5).await;
            let mut cfg = CairnConfig::default();
            set_default_recipe(&mut cfg, vec![HotMemoryRecipeStep::TopSalienceProject], 1);

            let result = run_handler(&store, &cfg).await;
            // No panic. No findings without a loader.
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Error),
                0,
            );
        }

        /// Empty recipe produces no hot-memory findings with or without a loader.
        #[tokio::test]
        async fn empty_recipe_emits_no_hot_memory_findings() {
            let store = FixtureStore::default();
            upsert_with(&store, 40, MemoryKind::Project, "x".repeat(4096), 0.9).await;
            let mut cfg = CairnConfig::default();
            set_default_recipe(&mut cfg, vec![], 16);

            let result = run_handler(&store, &cfg).await;
            assert_eq!(
                count_kind_severity(&result, Kind::HotMemoryOverBudget, Severity::Warning),
                0,
            );
            assert_eq!(count_259_deferred(&result), 0);
        }
    }
}
