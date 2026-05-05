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

    // Synthesize a stand-in target id so the plan has shape. Real pipeline
    // (#9) mints this through MemoryRecord construction.
    #[allow(
        clippy::expect_used,
        reason = "stub planner — fixture target id is hard-coded valid Crockford ULID"
    )]
    let target =
        TargetId::parse("01HQZX9F5N0000000000000000").expect("hard-coded valid Crockford ULID");
    #[allow(
        clippy::expect_used,
        reason = "stub planner — fixture issuer is hard-coded valid"
    )]
    let issuer = Identity::parse("agt:cairn-cli:planner:v0").expect("hard-coded valid identity");
    let plan = FlushPlan {
        operation_id: Ulid(synth_ulid()),
        issued_at: synth_now(),
        issuer,
        principal: None,
        scope: ScopeTuple::default(),
        mode,
        // ForgetRecord — lightweight placeholder. #9 swaps in Upsert
        // built from the captured body.
        mutations: vec![PlannedMutation::ForgetRecord { target }],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: std::collections::BTreeMap::new(),
        dependencies: vec![],
        expires_at: synth_expires(),
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
            let path = plan_path(&vault, Bucket::Pending, &plan.operation_id);
            let persisted = PersistedPlan::pending(plan.clone());
            let bytes = match serde_json::to_vec_pretty(&persisted) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("cairn: serialize plan: {e}");
                    return std::process::ExitCode::from(70);
                }
            };
            if let Err(e) = std::fs::write(&path, &bytes) {
                eprintln!("cairn: write {}: {e}", path.display());
                return std::process::ExitCode::from(73);
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

/// Synthesize a 26-char Crockford-base32 ULID-shaped string from the
/// current wall clock. Not a real ULID generator — #9 swaps in the
/// proper one. Output: 26 chars, leading char `0`, rest `[0-9A-HJKMNP-TV-Z]`.
fn synth_ulid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut buf = [b'0'; 26];
    let mut n = nanos;
    // Fill from right; leftmost char stays '0' to satisfy first-char rule.
    for slot in buf.iter_mut().skip(1).rev() {
        *slot = ALPHABET[(n & 0x1F) as usize];
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
