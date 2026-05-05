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

fn apply(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    #[allow(clippy::expect_used, reason = "clap declared this required")]
    let id = m.get_one::<String>("id").expect("clap-required");
    let ulid = cairn_core::generated::common::Ulid(id.clone());

    let pending = plan_path(vault, Bucket::Pending, &ulid);
    let applied = plan_path(vault, Bucket::Applied, &ulid);
    let rejected = plan_path(vault, Bucket::Rejected, &ulid);

    // Idempotent re-apply on Applied → success no-op. We re-emit the same
    // human / JSON shape the original apply produced.
    if applied.exists() {
        emit_apply_ok(json, id, "applied (no-op)");
        return ExitCode::SUCCESS;
    }
    // Re-apply on Rejected → AlreadyTerminal.
    if rejected.exists() {
        eprintln!("cairn flush apply: {id} is already terminal: rejected");
        return ExitCode::from(65); // EX_DATAERR
    }
    // Read pending.
    let Ok(bytes) = std::fs::read(&pending) else {
        eprintln!("cairn flush apply: plan {id} not found in pending/");
        return ExitCode::from(66); // EX_NOINPUT
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

    // Schema-version compatibility gate. Future plan formats must round-trip
    // through their own CLI; refuse to act on a version this binary does
    // not understand rather than silently overwriting status fields.
    if persisted.schema_version != PersistedPlan::SCHEMA_VERSION {
        eprintln!(
            "cairn flush apply: plan {id} schema_version {} unsupported (this CLI handles {})",
            persisted.schema_version,
            PersistedPlan::SCHEMA_VERSION,
        );
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
    if let Err(e) = persist_and_move(vault, &pending, &applied, &persisted, &ulid) {
        eprintln!("cairn flush apply: write failed: {e}");
        return ExitCode::from(70); // EX_SOFTWARE
    }
    emit_apply_ok(json, id, "applied");
    ExitCode::SUCCESS
}

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

    let pending = plan_path(vault, Bucket::Pending, &ulid);
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
    let Ok(bytes) = std::fs::read(&pending) else {
        eprintln!("cairn flush reject: plan {id} not found in pending/");
        return ExitCode::from(66);
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
        return ExitCode::from(65);
    }
    persisted.status = PlanStatus::Rejected {
        at: now_rfc3339(),
        reason: reason.clone(),
    };
    if let Err(e) = persist_and_move(vault, &pending, &rejected, &persisted, &ulid) {
        eprintln!("cairn flush reject: write failed: {e}");
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

/// Persist `p` to `to` and remove `from` using a write-tmp → atomic-rename
/// pattern so the terminal file appears in one filesystem step rather than
/// being written in place. Sequence:
///
/// 1. Serialize `p` and write to `<to>.tmp` (replace any prior tmp).
/// 2. `rename(<to>.tmp, <to>)` — atomic on POSIX same-filesystem.
/// 3. `remove_file(<from>)` — pending file gone.
/// 4. Best-effort `remove_file(<diff sidecar>)`.
///
/// A crash between steps 2 and 3 leaves both `pending/` and the terminal
/// file present for the same `operation_id`. The terminal file is
/// authoritative — apply/reject pre-checks treat any existing
/// `applied/<id>.plan.json` or `rejected/<id>.plan.json` as the
/// canonical state and short-circuit accordingly.
fn persist_and_move(
    vault: &Path,
    from: &Path,
    to: &Path,
    p: &PersistedPlan,
    ulid: &cairn_core::generated::common::Ulid,
) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?;
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    // Write to tmp first; atomic rename publishes the terminal file in one
    // step. If `to` already exists, `rename` overwrites it on POSIX —
    // callers must guard with the pre-check on `applied`/`rejected`
    // existence to avoid clobbering a peer process's terminal file.
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, to)?;
    // Pending now redundant; remove. A crash here leaves pending+terminal
    // for the same id; terminal wins per the pre-check semantics above.
    std::fs::remove_file(from)?;
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
