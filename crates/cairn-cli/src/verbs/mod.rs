//! Verb handler dispatch — one submodule per verb.

use std::path::Path;

use cairn_core::config::{CairnConfig, EmbeddingProvider};

pub mod admin_archive_session;
pub mod admin_connector;
pub mod admin_grant;
pub mod admin_model_fetch;
pub mod admin_reindex;
pub mod admin_replay_wal;
pub mod admin_restore;
pub mod admin_snapshot;
/// Hidden dev/E2E surface for issue #92 — drives the scheduler against
/// the live vault DB so an operator can verify dead-letter, crash
/// recovery, and metrics emission end-to-end.
pub mod admin_workflow;
pub mod admin_zero_capture_report;
pub mod assemble_hot;
pub mod backup;
pub mod capture_trace;
pub(crate) mod cold_session;
pub mod envelope;
pub mod flush;
/// Real `flush apply` executor for non-placeholder plans (issue #289).
pub mod flush_apply;
pub mod forget;
pub mod handshake;
pub mod ingest;
pub mod ingest_jsonl;
pub mod lint;
pub mod reindex;
pub mod retrieve;
pub mod screen;
pub mod search;
pub mod sensor;
pub mod session;
pub mod share;
pub mod signed;
pub mod skillpack;
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

/// Adjust generated `ingest` CLI requirements that are enforced in the
/// handler because folder ingest does not need a taxonomy kind.
#[must_use]
pub fn with_ingest_runtime_overrides(cmd: clap::Command) -> clap::Command {
    cmd.mut_arg("kind", |arg| arg.required(false))
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

/// Add `--plan <ULID>` flag to the `lint` subcommand. Lints the named pending
/// `FlushPlan` instead of running the live-store edge linter — surfaces issues
/// (e.g. rename target collisions) before `flush apply` runs.
#[must_use]
pub fn with_lint_plan(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("plan")
            .long("plan")
            .value_name("ULID")
            .action(clap::ArgAction::Set)
            .help(
                "Lint the pending FlushPlan with this id (issue #289 — surfaces \
                 rename target collisions before apply)",
            ),
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

/// Add `--scope-tenant`, `--scope-workspace`, `--scope-user`, `--scope-agent`
/// flags to the `search` subcommand. Each populates the matching
/// [`cairn_core::domain::ScopeTuple`] dimension on `SearchRequest.auth_scope`,
/// which the verb dispatcher threads into every leg's SQL predicate.
/// Issue #191.
#[must_use]
pub fn with_search_scope(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("scope-tenant")
            .long("scope-tenant")
            .value_name("TENANT")
            .help("Narrow auth_scope.tenant on every search leg (issue #191)")
            .action(clap::ArgAction::Set),
    )
    .arg(
        clap::Arg::new("scope-workspace")
            .long("scope-workspace")
            .value_name("WORKSPACE")
            .help("Narrow auth_scope.workspace on every search leg")
            .action(clap::ArgAction::Set),
    )
    .arg(
        clap::Arg::new("scope-user")
            .long("scope-user")
            .value_name("HMN_USER_ID")
            .help("Narrow auth_scope.user (must be canonical `hmn:…` form)")
            .action(clap::ArgAction::Set),
    )
    .arg(
        clap::Arg::new("scope-agent")
            .long("scope-agent")
            .value_name("AGENT_ID")
            .help("Narrow auth_scope.agent on every search leg")
            .action(clap::ArgAction::Set),
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

/// Add `--pin <ULID>` to `forget` without changing the wire-level IDL.
///
/// Pinning is an operator convenience over record metadata: it prevents
/// salience-decay eviction, but it is not a protocol forget request.
#[must_use]
pub fn with_forget_pin(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("pin_record_id")
            .long("pin")
            .value_name("ULID")
            .action(clap::ArgAction::Set)
            .help("Pin a record so salience-decay eviction will not forget it"),
    )
    .mut_group("forget_target", |group| group.arg("pin_record_id"))
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
    use std::io::Write as _;

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
            // Vault precedence: top-level `--vault NAME_OR_PATH` (a
            // `global(true)` clap flag, so it's visible on the
            // subcommand matches) wins over `CAIRN_VAULT`. Prevents
            // `CAIRN_VAULT=dev cairn --vault prod ingest --human-review`
            // from silently writing the plan into `dev`.
            let vault = sub
                .try_get_one::<String>("vault")
                .ok()
                .flatten()
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("CAIRN_VAULT").map(std::path::PathBuf::from));
            let Some(vault) = vault else {
                eprintln!(
                    "cairn: --vault NAME_OR_PATH or CAIRN_VAULT must be set for --human-review"
                );
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
                // Write to a per-process temp path first, fsync, then
                // atomic-rename to the final pending path. This way a
                // crash/EIO between open and fsync never leaves a
                // truncated `pending/<id>.plan.json` at the canonical
                // path. The `.tmp.<pid>` suffix avoids collision between
                // concurrent stub planners (rename is the
                // single-writer race resolver).
                let mut tmp = path.as_os_str().to_owned();
                tmp.push(format!(".tmp.{}", std::process::id()));
                let tmp = std::path::PathBuf::from(tmp);
                {
                    let mut f = match std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&tmp)
                    {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("cairn: open tmp {}: {e}", tmp.display());
                            return std::process::ExitCode::from(73);
                        }
                    };
                    if let Err(e) = f.write_all(&bytes) {
                        eprintln!("cairn: write {}: {e}", tmp.display());
                        let _ = std::fs::remove_file(&tmp);
                        return std::process::ExitCode::from(73);
                    }
                    if let Err(e) = f.sync_all() {
                        eprintln!("cairn: fsync {}: {e}", tmp.display());
                        let _ = std::fs::remove_file(&tmp);
                        return std::process::ExitCode::from(73);
                    }
                }
                // Atomic publish: `link` then `unlink(tmp)` is the
                // no-clobber primitive on POSIX (`rename` would
                // overwrite an existing pending file, breaking the
                // single-writer contract). On EEXIST we lost the race
                // — drop the tmp and retry with a fresh id.
                match std::fs::hard_link(&tmp, &path) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&tmp);
                        // Post-create re-check: between our pre-check
                        // and `link(pending)`, a peer could have
                        // produced + published a colliding terminal.
                        // If so, our pending file is orphaned (apply
                        // would short-circuit on the terminal). Drop
                        // it and retry.
                        if id_collides_with_terminal_or_inflight(&vault, &plan.operation_id) {
                            let _ = std::fs::remove_file(&path);
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
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let _ = std::fs::remove_file(&tmp);
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
                        eprintln!("cairn: link {} → {}: {e}", tmp.display(), path.display());
                        let _ = std::fs::remove_file(&tmp);
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

/// Returns `true` if the given operation id is already represented in a
/// terminal bucket (`applied/`, `rejected/`) or in either bucket's
/// `<bucket>/<id>.plan.json.in-flight` claim path. Excludes the pending
/// bucket — the caller is the one writing pending in the post-create
/// re-check, so seeing pending here would always trip.
fn id_collides_with_terminal_or_inflight(
    vault: &std::path::Path,
    ulid: &cairn_core::generated::common::Ulid,
) -> bool {
    use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
    let applied = plan_path(vault, Bucket::Applied, ulid);
    let rejected = plan_path(vault, Bucket::Rejected, ulid);
    let applied_in_flight = applied.with_extension("json.in-flight");
    let rejected_in_flight = rejected.with_extension("json.in-flight");
    applied.exists()
        || rejected.exists()
        || applied_in_flight.exists()
        || rejected_in_flight.exists()
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

/// Determine whether the configured embedding *provider* is ready to produce
/// vectors end-to-end.
///
/// Shared by `status::compute_capabilities` and `search::run_async` so the
/// two call sites apply the same rule and cannot drift.
///
/// - [`EmbeddingProvider::Local`]: ready iff the model file is on disk
///   (`model_present` from `ModelCache::is_present`).
/// - [`EmbeddingProvider::OpenAi`]: ready iff the `openai` Cargo feature is
///   compiled in AND `OPENAI_API_KEY` is set in the environment. A local
///   model file on disk is irrelevant for a cloud provider.
/// - Any future provider variant: `false` until explicit support is added
///   here (fail-closed per CLAUDE.md §4.6).
///
/// `_vault_root` is unused for cloud providers (no filesystem stat needed); it
/// is accepted to mirror callers' available context without them needing an
/// adapter.
pub(crate) fn embedding_provider_ready(
    config: &CairnConfig,
    model_present: bool,
    _vault_root: Option<&Path>,
) -> bool {
    match config.search.default_provider {
        EmbeddingProvider::Local => model_present,
        EmbeddingProvider::OpenAi => {
            #[cfg(feature = "openai")]
            {
                // Delegate to the shared predicate in `cairn-embeddings-openai`
                // so this site and `resolve_openai_embedder` apply identical
                // validation:
                //   1. API key present and non-whitespace.
                //   2. `embedding_model` is an OpenAI native variant
                //      (not a local candle model like `bge-small-en-v1.5`).
                //   3. `openai` Cargo feature compiled in (this arm).
                let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                cairn_embeddings_openai::config_ready(config.search.embedding_model, key.trim())
            }
            #[cfg(not(feature = "openai"))]
            {
                false
            }
        }
        _ => false,
    }
}
