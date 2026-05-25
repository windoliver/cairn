//! `cairn skillpack` handler — pack, install, inspect.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::ArgMatches;

/// Dispatch `cairn skillpack <subcommand>`.
///
/// Called from `main.rs` after clap has matched the top-level `skillpack`
/// subcommand. All three sub-subcommands (`pack`, `install`, `inspect`) are
/// synchronous I/O-only operations; no tokio runtime is needed.
#[must_use]
pub fn run(sub: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    match sub.subcommand() {
        Some(("pack", args)) => run_pack(args, explicit_vault),
        Some(("install", args)) => run_install(args, explicit_vault),
        Some(("inspect", args)) => run_inspect(args),
        _ => unreachable!(
            "clap subcommand_required(true) on skillpack ensures a subcommand is always present"
        ),
    }
}

// ---------------------------------------------------------------------------
// pack
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "linear argument extraction + builder call; splitting adds no clarity"
)]
fn run_pack(args: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let name = args
        .get_one::<String>("name")
        .expect("invariant: --name is required");
    let version = args
        .get_one::<String>("version")
        .expect("invariant: --version is required");
    let cairn_compat = args
        .get_one::<String>("cairn-compat")
        .expect("invariant: --cairn-compat is required");
    let description = args
        .get_one::<String>("description")
        .expect("invariant: --description is required");
    let candidates_raw = args
        .get_one::<String>("candidates")
        .expect("invariant: --candidates is required");
    let output_dir = args.get_one::<PathBuf>("output");

    let candidate_ids: Vec<&str> = candidates_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if candidate_ids.is_empty() {
        eprintln!("cairn skillpack pack: --candidates must list at least one candidate id");
        return ExitCode::from(64); // EX_USAGE
    }

    // Resolve vault root: explicit --vault flag wins, then CAIRN_VAULT, then CWD.
    let vault_root = resolve_vault(explicit_vault);

    // If an --output dir was given, resolve it; otherwise use the vault root.
    let build_dir = output_dir.cloned().unwrap_or_else(|| vault_root.clone());

    let mut builder = cairn_workflows::skillify::packer::SkillPackBuilder::new(
        name,
        version,
        cairn_compat,
        description,
    );
    for cid in &candidate_ids {
        builder = builder.add_candidate(cid);
    }

    // The packer writes the archive relative to the vault root regardless of
    // `--output`; if the caller wants a different dir we move it afterwards.
    match builder.build(&vault_root) {
        Ok(result) => {
            let archive_path = if output_dir.is_some() {
                let target = build_dir.join(
                    result
                        .archive_path
                        .file_name()
                        .unwrap_or(result.archive_path.as_os_str()),
                );
                if let Err(e) = std::fs::rename(&result.archive_path, &target) {
                    eprintln!("cairn skillpack pack: move archive to output dir: {e}");
                    return ExitCode::from(73); // EX_CANTCREAT
                }
                target
            } else {
                result.archive_path.clone()
            };

            let m = &result.manifest;
            println!("pack_id:      {}", m.pack_id);
            println!("name:         {}", m.name);
            println!("version:      {}", m.version);
            println!("cairn_compat: {}", m.cairn_compat);
            println!("description:  {}", m.description);
            println!("skills:       {}", m.skills.len());
            println!("provides:     {}", m.provides.join(", "));
            println!("sha256:       {}", m.content_sha256);
            println!("archive:      {}", archive_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skillpack pack: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines, reason = "linear install flow: resolve, stage, re-gate per candidate, commit/rollback; splitting fragments would obscure the transactional sequence")]
fn run_install(args: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let archive_path = args
        .get_one::<PathBuf>("path")
        .expect("invariant: <path> is required");

    let vault_root = resolve_vault(explicit_vault);
    let cairn_version = env!("CARGO_PKG_VERSION");

    match cairn_workflows::skillify::packer::unpack_archive_staged(
        archive_path,
        &vault_root,
        cairn_version,
    ) {
        Ok((manifest, transaction)) => {
            println!("installed pack:  {} v{}", manifest.name, manifest.version);
            println!("pack_id:         {}", manifest.pack_id);
            println!("skills installed:");
            for entry in &manifest.skills {
                println!(
                    "  - {} (lane: {}, slug: {})",
                    entry.candidate_id, entry.lane, entry.slug
                );
            }
            println!("vault:           {}", vault_root.display());

            // Round-14: re-gate every installed candidate synchronously
            // using the HealthCheckRunner. This converts the all-Blocked
            // gate-report the unpacker wrote (round-3 integrity hardening)
            // into a real gate report against the installed bytes — so
            // operators can actually use installed packs without first
            // running a separate re-gate workflow.
            //
            // No LLM is passed: the LlmEvalRunner will return Blocked,
            // which is the right behavior for an unauthenticated CLI
            // install. Operators who want LLM eval should run a separate
            // workflow with LLM credentials.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let runtime = match runtime {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("cairn skillpack install: tokio runtime: {e}");
                    return ExitCode::from(1);
                }
            };
            let health =
                cairn_workflows::skillify::health::HealthCheckRunner::new(vault_root.clone(), None);

            println!();
            println!("status:          re-gating installed candidates...");
            let mut any_unhealthy = false;
            for entry in &manifest.skills {
                let res = runtime.block_on(health.check(&entry.candidate_id));
                match res {
                    Ok(report) => {
                        // Round-17: with the LlmEvalRunner now returning
                        // Skipped (not Blocked) when no LLM is configured,
                        // and HealthCheckRunner excluding Skipped from
                        // regressions, the `regressions` list directly
                        // represents actionable failures. Check the
                        // persisted gate-report for an LLM Skipped entry
                        // to show a helpful suffix.
                        let llm_skipped = report.gate_report.gates.iter().any(|g| {
                            g.name == "llm_evals"
                                && matches!(
                                    g.status,
                                    cairn_core::pipeline::skillify::SkillifyGateStatus::Skipped
                                )
                        });
                        if report.regressions.is_empty() {
                            let suffix = if llm_skipped {
                                " (LLM eval skipped — no provider)"
                            } else {
                                ""
                            };
                            println!("  - {} : promotable{suffix}", entry.candidate_id);
                        } else {
                            any_unhealthy = true;
                            println!(
                                "  - {} : NOT promotable ({} gate(s) not passing: {})",
                                entry.candidate_id,
                                report.regressions.len(),
                                report.regressions.join(", "),
                            );
                        }
                    }
                    Err(e) => {
                        any_unhealthy = true;
                        println!("  - {} : re-gate failed: {e}", entry.candidate_id);
                    }
                }
            }
            if any_unhealthy {
                // Round-17 hardening: roll back the swap so the vault
                // returns to its pre-install state. Previously the
                // backup of any pre-existing candidate was deleted
                // immediately after the swap, so a re-gate failure
                // would leave the operator with neither the old nor
                // the new candidate. With InstallTransaction the
                // backups are preserved until commit/rollback.
                println!();
                println!(
                    "warning: at least one installed candidate is NOT promotable.\n\
                     Rolling back the install — vault returned to its pre-install\n\
                     state. Inspect each candidate's gate-report.json for details\n\
                     (the gate-report from the failed install is gone after\n\
                     rollback; rebuild the pack with fixes if needed)."
                );
                if let Err(rollback_err) = transaction.rollback() {
                    eprintln!(
                        "warning: rollback after failed re-gate did not fully \
                         restore prior state: {rollback_err}\n\
                         Manual cleanup of `.bak-*` directories under \
                         .cairn/evolution/skillify/ may be required.",
                    );
                    return ExitCode::from(2);
                }
                return ExitCode::from(1);
            }
            // All candidates promotable: commit the transaction so the
            // backups (if any) are removed.
            if let Err(commit_err) = transaction.commit() {
                eprintln!(
                    "warning: install committed but backup cleanup \
                     incomplete: {commit_err}\n\
                     Vault is in a correct state; leftover `.bak-*` \
                     directories may be removed manually.",
                );
                // The install succeeded; cleanup is an operator-visible
                // warning, not a hard failure. Exit success.
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skillpack install: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

fn run_inspect(args: &ArgMatches) -> ExitCode {
    use std::io::Read as _;

    let archive_path = args
        .get_one::<PathBuf>("path")
        .expect("invariant: <path> is required");

    let file = match std::fs::File::open(archive_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "cairn skillpack inspect: open {}: {e}",
                archive_path.display()
            );
            return ExitCode::from(66); // EX_NOINPUT
        }
    };

    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cairn skillpack inspect: read archive entries: {e}");
            return ExitCode::from(66);
        }
    };

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut candidate_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("cairn skillpack inspect: read entry: {e}");
                return ExitCode::from(66);
            }
        };
        let path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let path_str = path.to_string_lossy();
        if path_str == "manifest.json" {
            let mut buf = Vec::new();
            if let Err(e) = entry.read_to_end(&mut buf) {
                eprintln!("cairn skillpack inspect: read manifest.json: {e}");
                return ExitCode::from(66);
            }
            manifest_bytes = Some(buf);
        } else if let Some(rest) = path_str.strip_prefix("skills/") {
            // Record the unique candidate-id directory (first path segment).
            if let Some((cand, _)) = rest.split_once('/')
                && !cand.is_empty()
            {
                candidate_dirs.insert(cand.to_owned());
            }
        }
    }
    let skill_count = candidate_dirs.len();

    let Some(bytes) = manifest_bytes else {
        eprintln!("cairn skillpack inspect: manifest.json not found in archive");
        return ExitCode::from(65); // EX_DATAERR
    };

    let manifest: cairn_core::pipeline::skillify::SkillPackManifest =
        match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("cairn skillpack inspect: parse manifest.json: {e}");
                return ExitCode::from(65);
            }
        };

    println!("name:         {}", manifest.name);
    println!("version:      {}", manifest.version);
    println!("cairn_compat: {}", manifest.cairn_compat);
    println!("description:  {}", manifest.description);
    println!("pack_id:      {}", manifest.pack_id);
    println!(
        "skill count:  {} (manifest: {})",
        skill_count,
        manifest.skills.len()
    );
    if !manifest.requires.is_empty() {
        println!("requires:     {}", manifest.requires.join(", "));
    }
    if !manifest.provides.is_empty() {
        println!("provides:     {}", manifest.provides.join(", "));
    }
    if !manifest.content_sha256.is_empty() {
        println!("sha256:       {}", manifest.content_sha256);
    }
    println!("skills:");
    for entry in &manifest.skills {
        println!(
            "  - {} lane={} slug={}",
            entry.candidate_id, entry.lane, entry.slug
        );
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the vault root using the SAME precedence other vault-mutating
/// verbs use: `--vault` flag → `CAIRN_VAULT` env → walk up from CWD for
/// `.cairn/` → registry default. Round-16 hardening: the previous
/// shortcut just fell back to CWD, which silently wrote into a nested
/// `.cairn/evolution/skillify` tree when the operator ran `cairn
/// skillpack install` from a vault subdirectory.
fn resolve_vault(explicit_vault: Option<&str>) -> PathBuf {
    // Merge --vault flag and CAIRN_VAULT env (env-var fallback matches
    // main.rs's `explicit_vault` merging).
    let merged_explicit = explicit_vault
        .map(str::to_owned)
        .or_else(|| std::env::var("CAIRN_VAULT").ok().filter(|s| !s.is_empty()));

    // Use the canonical resolver so we walk up to find `.cairn/` and
    // honor the registry's default entry.
    let registry_path = std::env::var("CAIRN_REGISTRY")
        .ok()
        .map(PathBuf::from)
        .or_else(|| crate::vault::VaultRegistryStore::default_path().ok())
        .unwrap_or_else(|| PathBuf::from(".cairn-registry"));
    let store = crate::vault::VaultRegistryStore::new(registry_path);
    let opts = crate::vault::ResolveOpts {
        explicit: merged_explicit,
        cwd: std::env::current_dir().ok(),
        store: &store,
    };
    match crate::vault::resolve_vault(opts) {
        Ok(p) => p,
        // Last-resort fallback: cwd. The canonical resolver returns an
        // error only when nothing matches, which is operator error;
        // this keeps install/pack runnable in greenfield setups and is
        // consistent with the previous behavior.
        Err(_) => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}
