//! `cairn flush list / apply / reject` — admin-style subcommands for the
//! human-review flow (brief §5.5). Not in IDL; CLI-only.
//!
//! Vault root resolved from `CAIRN_VAULT` env var. The plan files live
//! under `<vault>/.cairn/flush/{pending,applied,rejected}/`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{ApplyKind, FlushPlan, PersistedPlan, PlanStatus};

/// Validate that `s` is a canonical 26-char Crockford-base32 ULID
/// (uppercase, no `I L O U`, leading char `0..=7`). Mirrors the
/// `Ulid::deserialize` validator in `cairn-core::generated::common` so
/// the CLI cannot turn an unvalidated CLI argument into a filesystem
/// path component (`/`, `..`, absolute path) before the embedded
/// `operation_id` gate has a chance to run.
pub(crate) fn is_valid_ulid_str(s: &str) -> bool {
    if s.len() != 26 {
        return false;
    }
    let bytes = s.as_bytes();
    if !matches!(bytes[0], b'0'..=b'7') {
        return false;
    }
    bytes[1..].iter().all(|b| {
        matches!(b,
            b'0'..=b'9'
            | b'A'..=b'H'
            | b'J'
            | b'K'
            | b'M'
            | b'N'
            | b'P'..=b'T'
            | b'V'..=b'Z'
        )
    })
}
use clap::{Arg, ArgAction, ArgMatches, Command};

use super::flush_apply;

/// Build the `flush` subcommand group.
#[must_use]
pub fn command() -> Command {
    Command::new("flush")
        .about("Manage human-review FlushPlans (brief §5.5)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("list")
                .about("List FlushPlans under .cairn/flush/")
                .arg(
                    Arg::new("all")
                        .long("all")
                        .action(ArgAction::SetTrue)
                        .help("Include applied/ and rejected/ buckets"),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
        .subcommand(
            Command::new("apply")
                .about("Apply a pending plan to MemoryStore")
                .arg(Arg::new("id").required(true).help("Plan ULID to apply"))
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
        .subcommand(
            Command::new("reject")
                .about("Reject a pending plan; record a reason")
                .arg(Arg::new("id").required(true).help("Plan ULID to reject"))
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .required(true)
                        .help("Free-form reason recorded with the rejection"),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
}

/// Dispatch the `flush` group. `resolved_vault` is the vault path the
/// shared resolver produced for this invocation (honors `--vault` and
/// `CAIRN_VAULT` together with the vault registry / CWD walk-up). When
/// it's `None` we fall back to the bare `CAIRN_VAULT` env so operators
/// can still inspect a vault outside the normal precedence chain.
#[must_use]
pub fn run(sub: &ArgMatches, resolved_vault: Option<PathBuf>) -> ExitCode {
    let Some(vault) = resolved_vault.or_else(|| std::env::var_os("CAIRN_VAULT").map(PathBuf::from))
    else {
        eprintln!("cairn flush: vault root not set: pass --vault NAME_OR_PATH or CAIRN_VAULT");
        return ExitCode::from(78); // EX_CONFIG
    };
    match sub.subcommand() {
        Some(("list", m)) => list(&vault, m),
        Some(("apply", m)) => apply(&vault, m),
        Some(("reject", m)) => reject(&vault, m),
        _ => ExitCode::from(64),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "linear pre-check → claim → validate-and-rollback-on-failure → publish; \
              splitting hides the lifecycle that needs to be read top-to-bottom"
)]
fn apply(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    #[allow(clippy::expect_used, reason = "clap declared this required")]
    let id = m.get_one::<String>("id").expect("clap-required");
    // Validate ULID grammar BEFORE constructing any filesystem path so an
    // attacker / typo cannot smuggle `..` or `/` through `plan_path`.
    if !is_valid_ulid_str(id) {
        eprintln!("cairn flush apply: invalid ULID {id} (expected 26-char Crockford base32)");
        return ExitCode::from(64); // EX_USAGE
    }
    let ulid = cairn_core::generated::common::Ulid(id.clone());

    let applied = plan_path(vault, Bucket::Applied, &ulid);
    let rejected = plan_path(vault, Bucket::Rejected, &ulid);

    // Idempotent re-apply on Applied → success no-op.
    if applied.exists() {
        emit_apply_ok(json, id, "applied (no-op)");
        return ExitCode::SUCCESS;
    }
    // Re-apply on Rejected → AlreadyTerminal.
    if rejected.exists() {
        eprintln!("cairn flush apply: {id} is already terminal: rejected");
        return ExitCode::from(65); // EX_DATAERR
    }

    // Atomically claim the pending file. POSIX `rename` returns ENOENT if
    // the source vanished — that's how concurrent apply/reject callers
    // race-free settle to a single winner. The loser sees ENOENT and
    // returns NotFound (or AlreadyTerminal if the winner committed).
    // Stranded marker (issue #289 review round 2): a prior attempt
    // committed SQLite mutations but failed to write the committed
    // sidecar, leaving an unrecoverable state. Refuse to apply this id
    // again until an operator inspects and removes the marker — replaying
    // would double-mutate.
    let stranded = stranded_marker_path(vault, &ulid);
    if stranded.exists() {
        eprintln!(
            "cairn flush apply: plan {id} is stranded ({}). A prior attempt committed \
             SQLite mutations but could not persist its recovery sidecar; replaying would \
             double-mutate. Manually verify the database state, then remove the stranded \
             marker to proceed.",
            stranded.display(),
        );
        return ExitCode::from(70);
    }
    let claim = match claim_pending(vault, &ulid, "applied") {
        ClaimOutcome::Claimed(p) => p,
        ClaimOutcome::NotFound => {
            // Re-check terminal in case a peer committed between our
            // pre-check and our rename attempt. If a terminal now exists,
            // surface it as AlreadyTerminal; otherwise NotFound.
            if applied.exists() {
                emit_apply_ok(json, id, "applied (no-op)");
                return ExitCode::SUCCESS;
            }
            if rejected.exists() {
                eprintln!("cairn flush apply: {id} is already terminal: rejected");
                return ExitCode::from(65);
            }
            eprintln!("cairn flush apply: plan {id} not found in pending/");
            return ExitCode::from(66);
        }
        ClaimOutcome::Err(e) => {
            eprintln!("cairn flush apply: claim failed: {e}");
            return ExitCode::from(70);
        }
    };

    let bytes = match std::fs::read(&claim) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cairn flush apply: read claimed file failed: {e}");
            rollback_claim(vault, &claim, &ulid);
            return ExitCode::from(70);
        }
    };
    #[allow(
        clippy::single_match_else,
        reason = "needs error detail in user-facing message"
    )]
    let mut persisted: PersistedPlan = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush apply: malformed plan {id}: {e}");
            rollback_claim(vault, &claim, &ulid);
            return ExitCode::from(65);
        }
    };

    // Schema-version compatibility gate. Future plan formats must
    // round-trip through their own CLI; refuse to act on a version this
    // binary does not understand rather than silently overwriting status
    // fields. Rollback the claim — restore the pending file so a later
    // CLI / operator can still see and recover it.
    if persisted.schema_version != PersistedPlan::SCHEMA_VERSION {
        eprintln!(
            "cairn flush apply: plan {id} schema_version {} unsupported (this CLI handles {})",
            persisted.schema_version,
            PersistedPlan::SCHEMA_VERSION,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    // Identity gate: refuse a pending file whose embedded `operation_id`
    // does not match the path / requested id (tampering or stale-content
    // protection).
    if persisted.plan.operation_id.0 != *id {
        eprintln!(
            "cairn flush apply: plan {id} content operation_id `{}` mismatches filename id",
            persisted.plan.operation_id.0,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    // State gate: only `Pending` plans may be advanced.
    if !matches!(persisted.status, PlanStatus::Pending) {
        eprintln!(
            "cairn flush apply: plan {id} is not in Pending state ({:?})",
            persisted.status,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    // TTL gate (brief §5.6 `expires_at`). Real plans (`!placeholder`)
    // must be applied within their declared TTL — past expiry, refuse
    // and roll back so the plan stays pending for re-issue. Stub-planner
    // plans use synthetic timestamps and skip the check.
    if !persisted.plan.placeholder && expires_at_in_past(&persisted.plan.expires_at) {
        eprintln!(
            "cairn flush apply: plan {id} expired at {} (now past TTL)",
            persisted.plan.expires_at,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }

    if persisted.plan.placeholder {
        eprintln!(
            "cairn flush apply: WARNING — MemoryStore mutations are not yet wired (#9). \
             Plan {id} will be marked applied for audit only; no records were written."
        );
        eprintln!(
            "cairn flush apply: NOTE — plan {id} was produced by the CLI stub planner \
             (`cairn-cli::ingest_plan_stub`) and does NOT reflect a real ingest/forget \
             pipeline run. Treat as a placeholder for testing the lifecycle only."
        );
        persisted.status = PlanStatus::Applied {
            at: now_rfc3339(),
            apply_kind: ApplyKind::MetadataOnly,
        };
    } else {
        // Sidecar (issue #289 review round 1): if a prior apply attempt
        // crashed between the SQLite commit and `publish_terminal`, the
        // sidecar `<claim>.committed` is on disk. Replaying the mutations
        // against the already-mutated store would double-apply (Rename
        // would now hit `RenameTargetConflict`, Patch would fail drift,
        // etc). When the sidecar is present we skip apply and proceed
        // straight to publish — the DB side is durable.
        let committed_sidecar = committed_sidecar_path(&claim);
        if committed_sidecar.exists() {
            eprintln!(
                "cairn flush apply: detected committed-sidecar for {id}; \
                 DB mutations from a prior attempt are durable, resuming publish only."
            );
        } else if let Err(e) = apply_real_plan(vault, &persisted.plan) {
            eprintln!("cairn flush apply: apply failed: {e:#}");
            rollback_claim(vault, &claim, &ulid);
            return ExitCode::from(70);
        } else if let Err(e) = write_committed_sidecar(&committed_sidecar) {
            // DB is mutated but the recovery sidecar is missing. Plant the
            // stranded marker (issue #289 review round 2) so the apply
            // entry point refuses to re-execute mutations on retry. If
            // marker placement also fails, surface both errors — manual
            // intervention is required.
            let stranded = stranded_marker_path(vault, &ulid);
            if let Err(me) = write_stranded_marker(&stranded) {
                eprintln!(
                    "cairn flush apply: SQLite mutations committed but committed-sidecar \
                     write failed: {e}; ALSO failed to plant stranded marker at {}: {me}. \
                     DO NOT retry without manual recovery.",
                    stranded.display(),
                );
            } else {
                eprintln!(
                    "cairn flush apply: SQLite mutations committed but committed-sidecar \
                     write failed: {e}. Planted stranded marker at {}; future apply \
                     attempts will refuse this id until an operator inspects and removes \
                     the marker.",
                    stranded.display(),
                );
            }
            return ExitCode::from(70);
        }
        persisted.status = PlanStatus::Applied {
            at: now_rfc3339(),
            apply_kind: ApplyKind::Full,
        };
    }
    if let Err(e) = publish_terminal(vault, &claim, &applied, &persisted, &ulid) {
        eprintln!("cairn flush apply: publish failed: {e}");
        return ExitCode::from(70); // EX_SOFTWARE
    }
    emit_apply_ok(json, id, "applied");
    ExitCode::SUCCESS
}

/// Stranded marker path (issue #289 review round 2). Planted in the
/// `applied/` directory when `SQLite` mutations have committed but the
/// committed-sidecar could not be written; blocks future apply attempts
/// for the same id until an operator removes the marker.
fn stranded_marker_path(
    vault: &Path,
    ulid: &cairn_core::generated::common::Ulid,
) -> PathBuf {
    use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
    plan_path(vault, Bucket::Applied, ulid).with_extension("json.stranded")
}

/// Write the stranded marker durably so a crash mid-write cannot leave
/// an ambiguous half-written file. fsync the parent so the rename
/// survives.
fn write_stranded_marker(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(b"stranded: committed-sidecar write failed; manual recovery required\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Sidecar path used to mark "`SQLite` mutations committed, publish still
/// pending" — see issue #289 review round 1. Lives next to the in-flight
/// claim and is removed after `publish_terminal` succeeds.
fn committed_sidecar_path(claim: &Path) -> PathBuf {
    let mut p = claim.as_os_str().to_owned();
    p.push(".committed");
    PathBuf::from(p)
}

/// Atomically write the committed-sidecar file. fsyncs the file and its
/// parent directory so a crash here cannot leave an "almost-written"
/// sidecar that's later truncated.
fn write_committed_sidecar(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(b"committed\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn apply_real_plan(vault: &Path, plan: &FlushPlan) -> anyhow::Result<()> {
    let db_path = crate::mcp::store_db_path(vault);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let sqlite = Arc::new(cairn_store_sqlite::open(&db_path).await?);
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> = sqlite.clone();
        flush_apply::apply_real_plan(&store, &sqlite, plan).await
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "linear pre-check → claim → validate-and-rollback-on-failure → publish; \
              splitting hides the lifecycle that needs to be read top-to-bottom"
)]
fn reject(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    #[allow(clippy::expect_used, reason = "clap declared this required")]
    let id = m.get_one::<String>("id").expect("clap-required");
    #[allow(clippy::expect_used, reason = "clap declared this required")]
    let reason = m
        .get_one::<String>("reason")
        .expect("clap-required")
        .clone();
    if !is_valid_ulid_str(id) {
        eprintln!("cairn flush reject: invalid ULID {id} (expected 26-char Crockford base32)");
        return ExitCode::from(64);
    }
    let ulid = cairn_core::generated::common::Ulid(id.clone());

    let applied = plan_path(vault, Bucket::Applied, &ulid);
    let rejected = plan_path(vault, Bucket::Rejected, &ulid);

    if applied.exists() {
        eprintln!("cairn flush reject: {id} is already terminal: applied");
        return ExitCode::from(65);
    }
    if rejected.exists() {
        eprintln!("cairn flush reject: {id} is already terminal: rejected");
        return ExitCode::from(65);
    }

    let claim = match claim_pending(vault, &ulid, "rejected") {
        ClaimOutcome::Claimed(p) => p,
        ClaimOutcome::NotFound => {
            if applied.exists() {
                eprintln!("cairn flush reject: {id} is already terminal: applied");
                return ExitCode::from(65);
            }
            if rejected.exists() {
                eprintln!("cairn flush reject: {id} is already terminal: rejected");
                return ExitCode::from(65);
            }
            eprintln!("cairn flush reject: plan {id} not found in pending/");
            return ExitCode::from(66);
        }
        ClaimOutcome::Err(e) => {
            eprintln!("cairn flush reject: claim failed: {e}");
            return ExitCode::from(70);
        }
    };

    let bytes = match std::fs::read(&claim) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cairn flush reject: read claimed file failed: {e}");
            rollback_claim(vault, &claim, &ulid);
            return ExitCode::from(70);
        }
    };
    #[allow(
        clippy::single_match_else,
        reason = "needs error detail in user-facing message"
    )]
    let mut persisted: PersistedPlan = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush reject: malformed plan {id}: {e}");
            rollback_claim(vault, &claim, &ulid);
            return ExitCode::from(65);
        }
    };
    if persisted.schema_version != PersistedPlan::SCHEMA_VERSION {
        eprintln!(
            "cairn flush reject: plan {id} schema_version {} unsupported (this CLI handles {})",
            persisted.schema_version,
            PersistedPlan::SCHEMA_VERSION,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    if persisted.plan.operation_id.0 != *id {
        eprintln!(
            "cairn flush reject: plan {id} content operation_id `{}` mismatches filename id",
            persisted.plan.operation_id.0,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    if !matches!(persisted.status, PlanStatus::Pending) {
        eprintln!(
            "cairn flush reject: plan {id} is not in Pending state ({:?})",
            persisted.status,
        );
        rollback_claim(vault, &claim, &ulid);
        return ExitCode::from(65);
    }
    // Rejecting an expired non-placeholder plan is still safe (it's a
    // cleanup operation), but we surface the staleness so an operator
    // can audit. Don't block — let the operator complete the rejection.
    if !persisted.plan.placeholder && expires_at_in_past(&persisted.plan.expires_at) {
        eprintln!(
            "cairn flush reject: NOTE — plan {id} TTL ({}) is in the past; rejection still proceeds",
            persisted.plan.expires_at,
        );
    }
    persisted.status = PlanStatus::Rejected {
        at: now_rfc3339(),
        reason: reason.clone(),
    };
    if let Err(e) = publish_terminal(vault, &claim, &rejected, &persisted, &ulid) {
        eprintln!("cairn flush reject: publish failed: {e}");
        return ExitCode::from(70);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "operation_id": id,
                "status": "rejected",
                "reason": reason,
            }))
            .unwrap_or_default()
        );
    } else {
        println!("flush reject {id}: rejected ({reason})");
    }
    ExitCode::SUCCESS
}

/// Restore a claimed pending file to `pending/<id>.plan.json`. Best-effort:
/// callers invoke this after a validation gate trips so a future apply or
/// operator can still see the original plan. If the rename fails (e.g. the
/// pending dir has been removed mid-flight), surface the in-flight path
/// in the log and leave it for manual cleanup.
fn rollback_claim(vault: &Path, claim: &Path, ulid: &cairn_core::generated::common::Ulid) {
    let pending = plan_path(vault, Bucket::Pending, ulid);
    if let Some(parent) = pending.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "cairn flush: rollback could not recreate pending dir {}: {e}",
            parent.display(),
        );
        return;
    }
    if let Err(e) = std::fs::rename(claim, &pending) {
        eprintln!(
            "cairn flush: rollback could not restore pending file (claim left at {}): {e}",
            claim.display(),
        );
    }
}

/// Outcome of an atomic claim attempt on `pending/<id>.plan.json`.
enum ClaimOutcome {
    /// Successfully renamed pending → claim path; returned path is the
    /// claim file, ready for read + status mutation.
    Claimed(std::path::PathBuf),
    /// `pending/<id>.plan.json` did not exist (or was claimed by a
    /// concurrent peer in the same instant). Caller decides whether to
    /// surface `NotFound` or `AlreadyTerminal` based on what's now in
    /// `applied/` / `rejected/`.
    NotFound,
    /// Other I/O error.
    Err(std::io::Error),
}

/// Atomically claim `pending/<id>.plan.json` for one operator (apply or
/// reject) by renaming it under a `<bucket>/<id>.plan.json.in-flight`
/// path. POSIX `rename(2)` is atomic and returns `ENOENT` if the source
/// vanished, so two concurrent callers race race-free for the same
/// pending file: the loser sees `NotFound` and exits without writing
/// anything. Different `role` strings ("applied" / "rejected") give each
/// caller a distinct destination so they never overwrite each other's
/// in-flight state.
fn claim_pending(
    vault: &Path,
    ulid: &cairn_core::generated::common::Ulid,
    role: &str,
) -> ClaimOutcome {
    let pending = plan_path(vault, Bucket::Pending, ulid);
    let bucket = match role {
        "applied" => Bucket::Applied,
        "rejected" => Bucket::Rejected,
        _ => return ClaimOutcome::Err(std::io::Error::other(format!("unknown role {role}"))),
    };
    let claim = plan_path(vault, bucket, ulid).with_extension("json.in-flight");
    if let Some(parent) = claim.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ClaimOutcome::Err(e);
    }
    // Resume path: if a prior attempt for this same role already claimed
    // pending and crashed before publishing, the in-flight file is still
    // there. Use it instead of trying to rename pending again — that
    // would otherwise return `NotFound` and strand the claim.
    //
    // BUT — if `pending/<id>` ALSO exists alongside the in-flight file,
    // we have ambiguous state: rolling back the in-flight (e.g. on a
    // validation failure) would overwrite the valid pending file. That
    // is operator-resolvable, not auto-resolvable; fail closed with a
    // conflict error so the operator can inspect both files before any
    // CLI action mutates them.
    if claim.exists() {
        if pending.exists() {
            return ClaimOutcome::Err(std::io::Error::other(format!(
                "conflict: both pending/{0}.plan.json and {1}/{0}.plan.json.in-flight exist; \
                 inspect both files manually before any flush apply / reject",
                ulid.0,
                bucket.dir_name(),
            )));
        }
        // Take exclusive ownership of the resume by renaming the
        // in-flight file to a per-process path. POSIX rename is
        // atomic; concurrent resumers race race-free here — the
        // loser sees `ENOENT` and is told another process is
        // already recovering this claim. Without this step two
        // concurrent retries would share the same claim path and
        // race to publish the terminal.
        let owned = process_owned_claim(&claim);
        match std::fs::rename(&claim, &owned) {
            Ok(()) => return ClaimOutcome::Claimed(owned),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ClaimOutcome::Err(std::io::Error::other(format!(
                    "recovery for {0} already in flight (in-flight file claimed by another \
                     process); retry once that process exits",
                    ulid.0,
                )));
            }
            Err(e) => return ClaimOutcome::Err(e),
        }
    }
    // Crashed-owner recovery is INTENTIONALLY operator-driven, not
    // automatic. If a previous process renamed the canonical in-flight
    // file to `<id>.plan.json.in-flight.<pid>` and crashed before
    // publish, the orphan is visible in `flush list` (with a
    // `stranded` status) but `apply`/`reject` will not auto-claim it.
    // Reasons:
    //   - file mtime is mutable (`touch -m`) and PID can be reused, so
    //     no filesystem-only signal can reliably prove the owner is
    //     dead;
    //   - auto-recovery from a fresh orphan can steal a live owner's
    //     claim, leading to duplicate or partial publish;
    //   - the safe path is for the operator to verify the owner is
    //     dead (`ps -p <pid>`, etc.) and manually rename
    //     `<id>.plan.json.in-flight.<pid>` back to
    //     `<id>.plan.json.in-flight`, after which the next
    //     `flush apply` / `flush reject` resumes via the existing
    //     `claim.exists()` branch above.
    if !pending.exists()
        && let Some(orphan) = find_orphan_owned_claim(&claim, ulid)
    {
        return ClaimOutcome::Err(std::io::Error::other(format!(
            "stranded in-flight claim for {0} at {1}; auto-recovery is disabled. \
             Verify the owning process is dead, then manually rename it back to \
             the canonical claim path (`{0}.plan.json.in-flight`) and retry.",
            ulid.0,
            orphan
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<unknown>"),
        )));
    }
    match std::fs::rename(&pending, &claim) {
        Ok(()) => ClaimOutcome::Claimed(claim),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClaimOutcome::NotFound,
        Err(e) => ClaimOutcome::Err(e),
    }
}

/// Look for any `<id>.plan.json.in-flight.<pid>` orphan in the same
/// directory as `claim`. Returns the first match in sorted order so
/// the result is deterministic across runs. Bounded by
/// [`MAX_INFLIGHT_SCAN`] to defend against attacker-staged or
/// corrupted-vault directories with unbounded entries.
fn find_orphan_owned_claim(
    claim: &Path,
    ulid: &cairn_core::generated::common::Ulid,
) -> Option<std::path::PathBuf> {
    let dir = claim.parent()?;
    let prefix = format!("{}.plan.json.in-flight.", ulid.0);
    let read = std::fs::read_dir(dir).ok()?;
    let mut best: Option<std::path::PathBuf> = None;
    for entry in read.flatten().take(MAX_INFLIGHT_SCAN) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let path = entry.path();
        match &best {
            None => best = Some(path),
            Some(b) => {
                if path.file_name() < b.file_name() {
                    best = Some(path);
                }
            }
        }
    }
    best
}

/// Append a per-process suffix so two concurrent claimers cannot share
/// the same resume path. `<id>.plan.json.in-flight` →
/// `<id>.plan.json.in-flight.<pid>`.
fn process_owned_claim(claim: &Path) -> std::path::PathBuf {
    let mut owned = claim.as_os_str().to_owned();
    owned.push(format!(".{}", std::process::id()));
    std::path::PathBuf::from(owned)
}

/// Publish a terminal `applied/<id>.plan.json` or `rejected/<id>.plan.json`
/// from a previously-claimed in-flight file. Sequence:
///
/// 1. Serialize `p` and write to `<terminal>.tmp` via a `File` that we
///    `sync_all` before close — guarantees bytes are durable.
/// 2. `rename(<terminal>.tmp, <terminal>)` — atomic on POSIX
///    same-filesystem.
/// 3. `fsync` the terminal directory so the rename is durable across
///    a power-loss event (best-effort on platforms that allow it).
/// 4. Remove the claim file (`<terminal>.in-flight`).
/// 5. Best-effort: remove the markdown diff sidecar.
fn publish_terminal(
    vault: &Path,
    claim: &Path,
    terminal: &Path,
    p: &PersistedPlan,
    ulid: &cairn_core::generated::common::Ulid,
) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = terminal.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?;
    // Per-process tmp path so two concurrent publishers (e.g. resumers
    // of the same in-flight claim that somehow both started) cannot
    // truncate or interleave each other's tmp file content.
    let mut tmp = terminal.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        // Ensure bytes hit storage before we expose the terminal name.
        f.sync_all()?;
    }
    // No-clobber publish: `link(2)` fails with `EEXIST` if the terminal
    // file already exists (a peer published first). `rename(2)` would
    // overwrite; that loses the peer's terminal status/reason/timestamp.
    // On `EEXIST` we accept the peer's outcome as authoritative,
    // discard our tmp, and clean up our claim.
    if let Err(e) = std::fs::hard_link(&tmp, terminal) {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            // Peer beat us to publish; remove our claim and surface
            // success — the lifecycle reached a terminal state.
            let _ = std::fs::remove_file(claim);
            return Ok(());
        }
        return Err(e);
    }
    let _ = std::fs::remove_file(&tmp);
    // Directory fsync — required for the rename to survive a power-loss
    // event on POSIX (POSIX rename is atomic in the running kernel but
    // the directory entry change must be flushed). Propagate the error
    // so a publish that cannot be made durable is not reported as
    // success. On unsupported platforms the open will fail with
    // `ErrorKind::PermissionDenied` or similar — surface that to the
    // caller rather than silently swallowing it.
    if let Some(parent) = terminal.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    // Claim file is no longer needed (terminal is the canonical record).
    let _ = std::fs::remove_file(claim);
    // The committed-sidecar (issue #289 review round 1) only meant
    // "publish still pending"; once the terminal hard-link is durable the
    // sidecar must go so a future, separate apply on a fresh claim does
    // not mistake it for an already-applied operation.
    let _ = std::fs::remove_file(committed_sidecar_path(claim));
    let _ = std::fs::remove_file(cairn_core::domain::flush_plan::store::diff_path(
        vault, ulid,
    ));
    // Also fsync the in-flight (claim) directory to make the
    // `remove_file(claim)` above durable across a power-loss event.
    if let Some(parent) = claim.parent()
        && parent != terminal.parent().unwrap_or(parent)
        && let Ok(dir) = std::fs::File::open(parent)
    {
        // Best-effort here only — the canonical state is the terminal
        // file we already fsynced above. A surviving stranded claim is
        // recoverable via `flush list`'s in-flight scan (Finding 3).
        let _ = dir.sync_all();
    }
    Ok(())
}

fn emit_apply_ok(json: bool, id: &str, status: &str) {
    if json {
        let body = serde_json::json!({ "operation_id": id, "status": status });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    } else {
        println!("flush apply {id}: {status}");
    }
}

/// Minimal RFC-3339 wall-clock formatter for the audit timestamp. Avoids
/// pulling `chrono` for one string. When `chrono` arrives as a workspace
/// dep elsewhere, swap this for a real formatter.
/// Compare an `expires_at` RFC-3339 timestamp to wall-clock now via
/// chronological parse. Lexical compare is wrong because fractional
/// seconds (`...00.999Z` vs `...00Z`) and offset forms (`+02:00`)
/// don't sort the way ASCII does. Uses
/// [`cairn_core::domain::Rfc3339Timestamp::cmp_chronological`] so a
/// malformed `expires_at` returns `true` (treat as expired → fail
/// closed) rather than silently passing.
fn expires_at_in_past(expires_at: &str) -> bool {
    use cairn_core::domain::Rfc3339Timestamp;
    let Ok(expires) = Rfc3339Timestamp::parse(expires_at) else {
        // Malformed plan timestamp — fail closed by treating it as
        // expired. The caller's gate logs the value so an operator
        // can see what went wrong.
        return true;
    };
    let Ok(now) = Rfc3339Timestamp::parse(now_rfc3339()) else {
        // Our own `now_rfc3339` is always well-formed by construction;
        // if it isn't, fail closed.
        return true;
    };
    expires.cmp_chronological(&now) == std::cmp::Ordering::Less
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut t = secs;
    let mins = (t / 60) % 60;
    t /= 60;
    let hours = (t / 60) % 24;
    t /= 60;
    let days_total = t / 24;
    let (year, month, day) = epoch_days_to_ymd(days_total);
    format!(
        "{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{:02}Z",
        secs % 60
    )
}

#[allow(clippy::cast_possible_truncation, reason = "year/month/day fit in u32")]
fn epoch_days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970_u32;
    loop {
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let yd: u64 = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        year += 1;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while days >= months[m] {
        days -= months[m];
        m += 1;
    }
    (year, (m + 1) as u32, (days + 1) as u32)
}

/// Summary row emitted by `flush list`.
#[derive(serde::Serialize)]
struct PlanSummary {
    id: String,
    bucket: &'static str,
    mode: String,
    mutations: usize,
    issued_at: String,
    status: String,
}

/// Bucket-level scan-cap notice — surfaced in JSON output as a typed
/// envelope field, not as a fake plan row, so consumers cannot
/// accidentally feed a marker into automation.
#[derive(serde::Serialize)]
struct OmittedNotice {
    bucket: &'static str,
    reason: String,
}

/// JSON envelope for `flush list --json`. Splits real plan rows from
/// scan-cap notices so machine consumers can parse `plans` cleanly.
#[derive(serde::Serialize)]
struct ListEnvelope<'a> {
    plans: &'a [PlanSummary],
    omitted: &'a [OmittedNotice],
}

/// Maximum file size we are willing to read for a `.in-flight` recovery
/// row in `flush list`. Real plans are well under 64 KiB; anything
/// larger is listed as `oversize` without being read.
const MAX_PLAN_BYTES: u64 = 1024 * 1024;

/// Per-bucket cap on the *number of directory entries scanned* (not
/// rows emitted). A flooded `.in-flight` directory with mostly invalid
/// names cannot make `flush list` walk forever just because we filter
/// them out. When this limit is hit, an `omitted=N` marker row is
/// emitted so the operator sees that recovery state may be hidden.
const MAX_INFLIGHT_SCAN: usize = 1024;

/// Per-bucket cap on rows emitted from the recovery scan. Beyond this
/// the operator gets an explicit `omitted` marker row rather than a
/// silently truncated list.
const MAX_INFLIGHT_ROWS: usize = 256;

#[allow(
    clippy::too_many_lines,
    reason = "linear: bounded recovery scan + bounded normal scan + format/emit; \
              splitting hides the row-shape contract"
)]
fn list(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let buckets: Vec<Bucket> = if m.get_flag("all") {
        Bucket::all().to_vec()
    } else {
        vec![Bucket::Pending]
    };
    let mut rows: Vec<PlanSummary> = Vec::new();
    let mut omitted: Vec<OmittedNotice> = Vec::new();
    // Always scan applied/ + rejected/ for stranded `.in-flight` claim
    // files so an operator can see and recover plans whose owning process
    // crashed between claim and publish. Emit a row for every matching
    // filename even if the file fails to read or parse — the most
    // recovery-critical claims are exactly the malformed / partially
    // written ones. Defense-in-depth: validate filename stems as ULIDs,
    // refuse to read files larger than `MAX_PLAN_BYTES`, and cap
    // per-bucket row count.
    let inflight_buckets = [Bucket::Applied, Bucket::Rejected];
    for b in &inflight_buckets {
        let dir = bucket_dir(vault, *b);
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sort entries so the scan is deterministic across runs (avoids
        // filesystem-order silently hiding particular ids when the cap
        // trips) and emit an `omitted=N` marker when we hit either cap.
        // Bound the read to `MAX_INFLIGHT_SCAN + 1` BEFORE collecting so
        // a flooded directory cannot force unbounded memory allocation
        // before the cap is checked.
        let mut entries: Vec<_> = read.flatten().take(MAX_INFLIGHT_SCAN + 1).collect();
        let read_full = entries.len() > MAX_INFLIGHT_SCAN;
        if read_full {
            entries.truncate(MAX_INFLIGHT_SCAN);
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bucket_rows = 0_usize;
        // `hit_scan_cap` is true whenever the streaming `take` above
        // had to truncate — that's the canonical signal that some
        // entries went unscanned.
        let hit_scan_cap = read_full;
        let mut hit_row_cap = false;
        for entry in entries {
            if bucket_rows >= MAX_INFLIGHT_ROWS {
                hit_row_cap = true;
                break;
            }
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            // Accept either the canonical `<id>.plan.json.in-flight`
            // or the per-process owned `<id>.plan.json.in-flight.<pid>`
            // form so a recovering process's claim is also visible.
            let stem_opt = name
                .strip_suffix(".plan.json.in-flight")
                .or_else(|| name.rfind(".plan.json.in-flight.").map(|i| &name[..i]));
            let Some(stem) = stem_opt else {
                continue;
            };
            // Refuse non-ULID stems — defends against an attacker /
            // corrupted-vault staging arbitrary filenames in the
            // recovery scan path.
            if !is_valid_ulid_str(stem) {
                continue;
            }
            let bucket_label = match b {
                Bucket::Applied => "in-flight (apply)",
                Bucket::Rejected => "in-flight (reject)",
                Bucket::Pending => "in-flight",
            };
            let oversize = entry
                .metadata()
                .ok()
                .is_some_and(|md| md.len() > MAX_PLAN_BYTES);
            let (mode, mutations, issued_at, status) = if oversize {
                (
                    "?".to_owned(),
                    0,
                    "?".to_owned(),
                    "stranded (oversize)".to_owned(),
                )
            } else {
                match std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<PersistedPlan>(&bytes).ok())
                {
                    Some(p) => (
                        format!("{:?}", p.plan.mode),
                        p.plan.mutations.len(),
                        p.plan.issued_at.clone(),
                        "stranded".to_owned(),
                    ),
                    None => (
                        "?".to_owned(),
                        0,
                        "?".to_owned(),
                        "stranded (unreadable)".to_owned(),
                    ),
                }
            };
            rows.push(PlanSummary {
                id: stem.to_owned(),
                bucket: bucket_label,
                mode,
                mutations,
                issued_at,
                status,
            });
            bucket_rows += 1;
        }
        if hit_scan_cap || hit_row_cap {
            let bucket_label = match b {
                Bucket::Applied => "in-flight (apply)",
                Bucket::Rejected => "in-flight (reject)",
                Bucket::Pending => "in-flight",
            };
            let why = if hit_scan_cap {
                format!("omitted (scan cap {MAX_INFLIGHT_SCAN})")
            } else {
                format!("omitted (row cap {MAX_INFLIGHT_ROWS})")
            };
            omitted.push(OmittedNotice {
                bucket: bucket_label,
                reason: why,
            });
        }
    }
    // Apply the same defenses to the normal pending/applied/rejected
    // scan: stream-truncate at MAX_INFLIGHT_SCAN, refuse oversize files,
    // and emit an `omitted` notice when caps trip. A flooded bucket
    // cannot turn `flush list` into an unbounded read.
    for b in buckets {
        let dir = bucket_dir(vault, b);
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = read.flatten().take(MAX_INFLIGHT_SCAN + 1).collect();
        let read_full = entries.len() > MAX_INFLIGHT_SCAN;
        if read_full {
            entries.truncate(MAX_INFLIGHT_SCAN);
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bucket_rows = 0_usize;
        let mut hit_row_cap = false;
        for entry in entries {
            if bucket_rows >= MAX_INFLIGHT_ROWS {
                hit_row_cap = true;
                break;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            // Skip files larger than the defensive size cap. A
            // corrupted/attacker-staged huge `.plan.json` cannot make
            // the recovery list hang on read.
            let oversize = entry
                .metadata()
                .ok()
                .is_some_and(|md| md.len() > MAX_PLAN_BYTES);
            if oversize {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_owned();
                rows.push(PlanSummary {
                    id: stem,
                    bucket: b.dir_name(),
                    mode: "?".into(),
                    mutations: 0,
                    issued_at: "?".into(),
                    status: "stranded (oversize)".into(),
                });
                bucket_rows += 1;
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(p) = serde_json::from_slice::<PersistedPlan>(&bytes) else {
                continue;
            };
            rows.push(PlanSummary {
                id: p.plan.operation_id.0.clone(),
                bucket: b.dir_name(),
                mode: format!("{:?}", p.plan.mode),
                mutations: p.plan.mutations.len(),
                issued_at: p.plan.issued_at.clone(),
                status: match p.status {
                    PlanStatus::Pending => "pending".into(),
                    PlanStatus::Applied { .. } => "applied".into(),
                    PlanStatus::Rejected { .. } => "rejected".into(),
                    _ => "unknown".into(),
                },
            });
            bucket_rows += 1;
        }
        if read_full || hit_row_cap {
            let why = if read_full {
                format!("omitted (scan cap {MAX_INFLIGHT_SCAN})")
            } else {
                format!("omitted (row cap {MAX_INFLIGHT_ROWS})")
            };
            omitted.push(OmittedNotice {
                bucket: b.dir_name(),
                reason: why,
            });
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    if json {
        let env = ListEnvelope {
            plans: &rows,
            omitted: &omitted,
        };
        println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
    } else if rows.is_empty() && omitted.is_empty() {
        println!("(no plans)");
    } else {
        for r in &rows {
            println!(
                "{} {:<19} {:<14} mutations={} issued={} status={}",
                r.id, r.bucket, r.mode, r.mutations, r.issued_at, r.status
            );
        }
        for o in &omitted {
            eprintln!("note: {}: {}", o.bucket, o.reason);
        }
    }
    ExitCode::SUCCESS
}
