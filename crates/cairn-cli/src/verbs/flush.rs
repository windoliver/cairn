//! `cairn flush list / apply / reject` — admin-style subcommands for the
//! human-review flow (brief §5.5). Not in IDL; CLI-only.
//!
//! Vault root resolved from `CAIRN_VAULT` env var. The plan files live
//! under `<vault>/.cairn/flush/{pending,applied,rejected}/`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{ApplyKind, PersistedPlan, PlanStatus};

/// Validate that `s` is a canonical 26-char Crockford-base32 ULID
/// (uppercase, no `I L O U`, leading char `0..=7`). Mirrors the
/// `Ulid::deserialize` validator in `cairn-core::generated::common` so
/// the CLI cannot turn an unvalidated CLI argument into a filesystem
/// path component (`/`, `..`, absolute path) before the embedded
/// `operation_id` gate has a chance to run.
fn is_valid_ulid_str(s: &str) -> bool {
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

/// Dispatch the `flush` group.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let vault = match resolve_vault() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush: {e}");
            return ExitCode::from(78); // EX_CONFIG
        }
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

    // Phase 1 — drift check. Without a wired MemoryStore in this PR, the
    // check is a no-op pass-through. When #9 lands (the WAL state machine),
    // replace this with a real `MemoryStore::get_active_by_target` +
    // body-hash comparison against `persisted.plan.target_hash(&target)`.

    // Phase 2 — apply. Same story: no MemoryStore wired here. The mutation
    // walk is a no-op for now; this is the shape the WAL apply will take.

    // Honest no-op: warn the operator that this binary moves the plan but
    // does NOT execute mutations against MemoryStore. Persisted status
    // captures `apply_kind = MetadataOnly` so the audit trail is truthful.
    eprintln!(
        "cairn flush apply: WARNING — MemoryStore mutations are not yet wired (#9). \
         Plan {id} will be marked applied for audit only; no records were written."
    );
    if persisted.plan.placeholder {
        eprintln!(
            "cairn flush apply: NOTE — plan {id} was produced by the CLI stub planner \
             (`cairn-cli::ingest_plan_stub`) and does NOT reflect a real ingest/forget \
             pipeline run. Treat as a placeholder for testing the lifecycle only."
        );
    }
    persisted.status = PlanStatus::Applied {
        at: now_rfc3339(),
        apply_kind: ApplyKind::MetadataOnly,
    };
    if let Err(e) = publish_terminal(vault, &claim, &applied, &persisted, &ulid) {
        eprintln!("cairn flush apply: publish failed: {e}");
        return ExitCode::from(70); // EX_SOFTWARE
    }
    emit_apply_ok(json, id, "applied");
    ExitCode::SUCCESS
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
    // Crashed-owner recovery: a previous process renamed the canonical
    // in-flight file to `.in-flight.<pid>` and crashed before publish.
    // Pending and the canonical claim are both gone. Scan the bucket dir
    // for any orphan `.in-flight.<pid>` matching this id and re-claim
    // it — but ONLY when the orphan is provably stale (mtime older than
    // `ORPHAN_STALE_THRESHOLD_SECS`). Younger orphans are assumed to
    // belong to a still-running peer process and we fail closed with a
    // recovery-in-progress conflict so we don't steal a live owner's
    // claim. Operators can wait the threshold or manually rename a
    // verified-dead orphan back to the canonical path.
    if !pending.exists()
        && let Some(orphan) = find_orphan_owned_claim(&claim, ulid)
    {
        let owned = process_owned_claim(&claim);
        if orphan == owned {
            return ClaimOutcome::Claimed(owned);
        }
        if !is_orphan_stale(&orphan) {
            return ClaimOutcome::Err(std::io::Error::other(format!(
                "recovery for {0} held by recent owner ({1}); waited <{2}s — \
                 retry once that process exits or rename {1} back to {0}.plan.json.in-flight \
                 manually if you have verified the owner is dead",
                ulid.0,
                orphan
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<unknown>"),
                ORPHAN_STALE_THRESHOLD_SECS,
            )));
        }
        match std::fs::rename(&orphan, &owned) {
            Ok(()) => return ClaimOutcome::Claimed(owned),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Another concurrent recoverer beat us — fall through to
                // the pending-rename attempt, which will return `NotFound`
                // and the apply/reject caller will retry on the terminal
                // (which the winner is in the process of publishing).
            }
            Err(e) => return ClaimOutcome::Err(e),
        }
    }
    match std::fs::rename(&pending, &claim) {
        Ok(()) => ClaimOutcome::Claimed(claim),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClaimOutcome::NotFound,
        Err(e) => ClaimOutcome::Err(e),
    }
}

/// Mtime threshold above which an orphan `.in-flight.<pid>` is treated
/// as stale and recoverable. Matches brief §5.6 `expires_at` 5-minute
/// receipt TTL — a process that has held a claim longer than this is
/// almost certainly dead. Operators can rename a verified-dead orphan
/// back to the canonical path manually if they need to recover sooner.
const ORPHAN_STALE_THRESHOLD_SECS: u64 = 300;

/// Returns `true` when `path` exists and its mtime is older than
/// [`ORPHAN_STALE_THRESHOLD_SECS`]. Treats unreadable metadata as
/// non-stale (fail closed) so a transient stat error never causes us
/// to steal a live owner's claim.
fn is_orphan_stale(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = md.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(mtime) else {
        return false;
    };
    age.as_secs() > ORPHAN_STALE_THRESHOLD_SECS
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
/// lexicographic string compare. Works because both ends use the same
/// fixed-width `YYYY-MM-DDTHH:MM:SSZ` form. Returns `false` (i.e. NOT
/// expired) on any parse / clock error so we never block on a faulty
/// clock — the caller's gate is conservative either way.
fn expires_at_in_past(expires_at: &str) -> bool {
    let now = now_rfc3339();
    expires_at < now.as_str()
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

fn resolve_vault() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CAIRN_VAULT") {
        return Ok(PathBuf::from(p));
    }
    Err("vault root not set: pass CAIRN_VAULT".into())
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
    for b in buckets {
        let dir = bucket_dir(vault, b);
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
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
