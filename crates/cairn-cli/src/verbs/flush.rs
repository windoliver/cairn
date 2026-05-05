//! `cairn flush list / apply / reject` — admin-style subcommands for the
//! human-review flow (brief §5.5). Not in IDL; CLI-only.
//!
//! Vault root resolved from `CAIRN_VAULT` env var. The plan files live
//! under `<vault>/.cairn/flush/{pending,applied,rejected}/`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{ApplyKind, PersistedPlan, PlanStatus};
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
    match std::fs::rename(&pending, &claim) {
        Ok(()) => ClaimOutcome::Claimed(claim),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClaimOutcome::NotFound,
        Err(e) => ClaimOutcome::Err(e),
    }
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
    let mut tmp = terminal.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        // Ensure bytes hit storage before we expose the terminal name.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, terminal)?;
    // Best-effort directory fsync so the rename survives power loss. This
    // is a no-op on platforms where opening a directory for sync is not
    // supported; we ignore failures here because the rename is already
    // committed at the inode level on the common-case Linux/macOS
    // filesystems Cairn targets.
    if let Some(parent) = terminal.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    // Claim file is no longer needed (terminal is the canonical record).
    let _ = std::fs::remove_file(claim);
    let _ = std::fs::remove_file(cairn_core::domain::flush_plan::store::diff_path(
        vault, ulid,
    ));
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

fn list(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let buckets: Vec<Bucket> = if m.get_flag("all") {
        Bucket::all().to_vec()
    } else {
        vec![Bucket::Pending]
    };
    let mut rows: Vec<PlanSummary> = Vec::new();
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
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else if rows.is_empty() {
        println!("(no plans)");
    } else {
        for r in &rows {
            println!(
                "{} {:<8} {:<14} mutations={} issued={} status={}",
                r.id, r.bucket, r.mode, r.mutations, r.issued_at, r.status
            );
        }
    }
    ExitCode::SUCCESS
}
