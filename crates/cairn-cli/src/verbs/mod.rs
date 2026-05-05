//! Verb handler dispatch — one submodule per verb.

pub mod admin_model_fetch;
pub mod admin_reindex;
pub mod assemble_hot;
pub mod capture_trace;
pub mod envelope;
pub mod flush;
pub mod forget;
pub mod handshake;
pub mod ingest;
pub mod lint;
pub mod retrieve;
pub mod search;
pub mod status;
pub mod summarize;

/// Exported only for the smoke test; not part of the public API.
#[doc(hidden)]
pub fn smoke_fn() {}

/// Add `--json` flag to any generated subcommand without modifying generated files.
#[must_use]
pub fn with_json(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("json")
            .long("json")
            .action(clap::ArgAction::SetTrue)
            .help("Emit machine-readable JSON response envelope to stdout"),
    )
}

/// Add `--fix-markdown` flag to the `lint` subcommand.
///
/// Augments the generated subcommand builder without touching generated files,
/// using the same pattern as `with_json`.
#[must_use]
pub fn with_fix_markdown(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("fix-markdown")
            .long("fix-markdown")
            .action(clap::ArgAction::SetTrue)
            .help("Regenerate missing or stale markdown projections for all active records"),
    )
}

/// Add `--fix-folders` flag to the `lint` subcommand.
///
/// Augments the generated subcommand builder without touching generated
/// files, using the same pattern as [`with_fix_markdown`].
#[must_use]
pub fn with_fix_folders(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("fix-folders")
            .long("fix-folders")
            .action(clap::ArgAction::SetTrue)
            .help(
                "Regenerate folder _index.md sidecars and backlinks for every \
                 non-empty folder (brief §3.4, #44)",
            ),
    )
}

/// Augments the `ingest` subcommand with the `--resync <path>` flag.
///
/// Uses the same pattern as [`with_json`] and [`with_fix_markdown`]: the
/// generated subcommand builder is wrapped rather than modified.
#[must_use]
pub fn with_resync(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("resync")
            .long("resync")
            .value_name("PATH")
            .help("Re-ingest an out-of-band edited markdown projection (brief §3.0, #43)")
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(std::path::PathBuf)),
    )
}

/// Add `--dry-run`, `--human-review`, `--no-diff` to a generated subcommand.
/// `--dry-run` and `--human-review` are mutually exclusive. Together they map
/// onto the [`FlushMode`](cairn_core::domain::flush_plan::FlushMode) enum.
#[must_use]
pub fn with_flush_modes(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("dry-run")
            .long("dry-run")
            .action(clap::ArgAction::SetTrue)
            .conflicts_with("human-review")
            .help("Produce a FlushPlan and emit it; write nothing to the vault (brief §5.5)"),
    )
    .arg(
        clap::Arg::new("human-review")
            .long("human-review")
            .action(clap::ArgAction::SetTrue)
            .conflicts_with("dry-run")
            .help("Persist a FlushPlan under .cairn/flush/pending/ for explicit apply"),
    )
    .arg(
        clap::Arg::new("no-diff")
            .long("no-diff")
            .action(clap::ArgAction::SetTrue)
            .help("Skip the markdown diff sidecar in human-review mode"),
    )
}

/// Stub planner used until the full ingest / forget pipelines land (#9).
/// Builds a minimal placeholder `FlushPlan` from the CLI args and either
/// prints it (`dry_run`) or persists it under `.cairn/flush/pending/`
/// (`human_review`).
///
/// When #9 ships, `ingest::run` and `forget::run` will build real plans
/// from a capture + extract + classify run and call into the same
/// persistence helper here.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "stub planner — each branch (dry_run, human_review, fallback) is linear and best read top-to-bottom"
)]
pub fn ingest_plan_stub(
    sub: &clap::ArgMatches,
    mode: cairn_core::domain::flush_plan::FlushMode,
    no_diff: bool,
    json: bool,
) -> std::process::ExitCode {
    use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, diff_path, plan_path};
    use cairn_core::domain::flush_plan::{
        FlushMode, FlushPlan, PersistedPlan, PlanReason, PlannedMutation, diff,
    };
    use cairn_core::domain::{Identity, ScopeTuple, TargetId};
    use cairn_core::generated::common::Ulid;

    #[allow(
        clippy::expect_used,
        reason = "stub planner — fixture issuer is hard-coded valid"
    )]
    let issuer = Identity::parse("agt:cairn-cli:planner:v0").expect("hard-coded valid identity");

    // Build a mutation that reflects the caller's actual args so a
    // persisted human-review plan is at least *honest* about what the
    // operator asked for, even though the WAL apply hasn't run yet
    // (#9). For `cairn forget --record <ULID>` we honor the supplied
    // target; otherwise we fall back to a hard-coded fixture target so
    // the plan is well-formed but the `placeholder` flag (set below)
    // tells `apply` to warn that this is not a real planner output.
    #[allow(
        clippy::expect_used,
        reason = "stub planner — fixture target id is hard-coded valid Crockford ULID"
    )]
    let fallback_target =
        TargetId::parse("01HQZX9F5N0000000000000000").expect("hard-coded valid Crockford ULID");
    let record_arg = sub
        .try_get_one::<String>("record_id")
        .ok()
        .flatten()
        .or_else(|| sub.try_get_one::<String>("record").ok().flatten())
        .cloned();
    let mutation = match record_arg.as_deref().and_then(|s| TargetId::parse(s).ok()) {
        Some(t) => PlannedMutation::ForgetRecord { target: t },
        None => PlannedMutation::ForgetRecord {
            target: fallback_target,
        },
    };
    let plan = FlushPlan {
        operation_id: Ulid(synth_ulid()),
        issued_at: synth_now(),
        issuer,
        principal: None,
        scope: ScopeTuple::default(),
        mode,
        mutations: vec![mutation],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: std::collections::BTreeMap::new(),
        dependencies: vec![],
        expires_at: synth_expires(),
        // Marks every plan produced by this stub so `cairn flush apply`
        // can warn the operator that the plan does NOT reflect a real
        // ingest/forget pipeline run. Cleared once #9 wires the real
        // planner.
        placeholder: true,
    };
    // Touch ingest-specific args if present (suppresses unused-var warnings
    // until #9 starts using them). Use try_get_one to avoid panicking when
    // called from forget::run, which has no "kind" or "body" args.
    let _ = sub.try_get_one::<String>("kind").ok();
    let _ = sub.try_get_one::<String>("body").ok();

    match mode {
        FlushMode::DryRun => {
            if json {
                let envelope = serde_json::json!({
                    "operation_id": plan.operation_id.0,
                    "mode": "dry_run",
                    "plan": plan,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&envelope).unwrap_or_default()
                );
            } else {
                println!("dry-run: plan {}", plan.operation_id.0);
                println!("{}", diff::render(&plan));
            }
            std::process::ExitCode::SUCCESS
        }
        FlushMode::HumanReview => {
            let Some(vault) = std::env::var_os("CAIRN_VAULT").map(std::path::PathBuf::from) else {
                eprintln!("cairn: CAIRN_VAULT must be set for --human-review");
                return std::process::ExitCode::from(78);
            };
            let pending_dir = bucket_dir(&vault, Bucket::Pending);
            if let Err(e) = std::fs::create_dir_all(&pending_dir) {
                eprintln!("cairn: mkdir {}: {e}", pending_dir.display());
                return std::process::ExitCode::from(73);
            }
            // Use `create_new(true)` so two concurrent stub-planner
            // invocations (or two within the same `synth_ulid` clock tick)
            // never silently overwrite each other's pending plan.
            // On EEXIST, mint a fresh id and retry up to a small bound
            // before failing closed. Collision is checked across ALL
            // lifecycle locations — not just `pending/<id>.plan.json` —
            // so a `synth_ulid` collision against an existing
            // `applied/<id>` or `rejected/<id>` (or an in-flight claim)
            // does not produce a pending plan that the apply path will
            // immediately treat as an idempotent no-op.
            let mut plan = plan;
            let mut path = plan_path(&vault, Bucket::Pending, &plan.operation_id);
            let mut retries = 0_u8;
            loop {
                if id_in_use_anywhere(&vault, &plan.operation_id) {
                    retries += 1;
                    if retries > 8 {
                        eprintln!(
                            "cairn: gave up minting unique plan id after 8 retries (last id {})",
                            plan.operation_id.0,
                        );
                        return std::process::ExitCode::from(70);
                    }
                    plan.operation_id = Ulid(synth_ulid());
                    path = plan_path(&vault, Bucket::Pending, &plan.operation_id);
                    continue;
                }
                let persisted = PersistedPlan::pending(plan.clone());
                let bytes = match serde_json::to_vec_pretty(&persisted) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("cairn: serialize plan: {e}");
                        return std::process::ExitCode::from(70);
                    }
                };
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut f) => {
                        use std::io::Write as _;
                        if let Err(e) = f.write_all(&bytes) {
                            eprintln!("cairn: write {}: {e}", path.display());
                            return std::process::ExitCode::from(73);
                        }
                        if let Err(e) = f.sync_all() {
                            eprintln!("cairn: fsync {}: {e}", path.display());
                            return std::process::ExitCode::from(73);
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // A peer process raced us between the
                        // `id_in_use_anywhere` probe and `create_new` —
                        // mint a fresh id and retry.
                        retries += 1;
                        if retries > 8 {
                            eprintln!(
                                "cairn: gave up minting unique plan id after 8 retries (last id {})",
                                plan.operation_id.0,
                            );
                            return std::process::ExitCode::from(70);
                        }
                        plan.operation_id = Ulid(synth_ulid());
                        path = plan_path(&vault, Bucket::Pending, &plan.operation_id);
                    }
                    Err(e) => {
                        eprintln!("cairn: write {}: {e}", path.display());
                        return std::process::ExitCode::from(73);
                    }
                }
            }
            if !no_diff {
                let dpath = diff_path(&vault, &plan.operation_id);
                let _ = std::fs::write(&dpath, diff::render(&plan));
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "operation_id": plan.operation_id.0,
                        "mode": "human_review",
                        "plan_ref": path.display().to_string(),
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("human-review: plan written to {}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        FlushMode::Autonomous => {
            unreachable!("planner stub only handles dry_run / human_review")
        }
        // catch-all guard: if FlushMode ever gains new variants before #9
        // ships the real planner, fall through to a clear error exit rather
        // than silently mishandling the mode.
        #[allow(
            unreachable_patterns,
            reason = "FlushMode is not #[non_exhaustive] today but may grow; guard is intentional"
        )]
        _ => std::process::ExitCode::from(70),
    }
}

/// Returns `true` if the given operation id is already represented by a
/// file in any `FlushPlan` lifecycle bucket — `pending/`, `applied/`,
/// `rejected/`, or either `<bucket>/<id>.plan.json.in-flight` claim
/// path. Used by the stub planner to refuse minting a pending plan that
/// would immediately collide with an existing terminal or claimed plan.
fn id_in_use_anywhere(vault: &std::path::Path, ulid: &cairn_core::generated::common::Ulid) -> bool {
    use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
    let pending = plan_path(vault, Bucket::Pending, ulid);
    let applied = plan_path(vault, Bucket::Applied, ulid);
    let rejected = plan_path(vault, Bucket::Rejected, ulid);
    let applied_in_flight = applied.with_extension("json.in-flight");
    let rejected_in_flight = rejected.with_extension("json.in-flight");
    pending.exists()
        || applied.exists()
        || rejected.exists()
        || applied_in_flight.exists()
        || rejected_in_flight.exists()
}

/// Synthesize a 26-char Crockford-base32 ULID-shaped string. Not a real
/// ULID generator (#9 swaps in the proper one). Mixes the current
/// nanosecond clock with a per-process counter so two calls within the
/// same clock tick still produce distinct ids; the call site additionally
/// guards against collision via `create_new(true)` + retry.
fn synth_ulid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let bump = u128::from(COUNTER.fetch_add(1, Ordering::Relaxed));
    let mut n = nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(bump);
    let mut buf = [b'0'; 26];
    // Fill from right; leftmost char stays '0' to satisfy first-char rule.
    for slot in buf.iter_mut().skip(1).rev() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "explicit 5-bit mask, value always < ALPHABET.len()"
        )]
        let idx = (n & 0x1F) as usize;
        *slot = ALPHABET[idx];
        n >>= 5;
    }
    // Safety: every byte is ASCII from `ALPHABET`.
    String::from_utf8(buf.to_vec()).unwrap_or_else(|_| "01000000000000000000000000".into())
}

fn synth_now() -> String {
    "2026-05-04T00:00:00Z".to_string()
}

fn synth_expires() -> String {
    "2026-05-04T00:05:00Z".to_string()
}
