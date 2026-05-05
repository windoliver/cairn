//! `cairn` binary entry point.
//!
//! Verb subcommands come from the IDL-generated clap builders (`mod generated`),
//! each wrapped with a `--json` flag via `cairn_cli::verbs::with_json()`. Actual
//! verb logic lives in `cairn_cli::verbs::*`; `main.rs` only owns parsing and
//! dispatch.

use std::io::Write;
use std::process::ExitCode;

use cairn_cli::{command, identity, plugins, verbs};
use cairn_core::contract::registry::PluginError;
use clap::ArgMatches;
fn registry_store() -> anyhow::Result<cairn_cli::vault::VaultRegistryStore> {
    let path = if let Ok(p) = std::env::var("CAIRN_REGISTRY") {
        std::path::PathBuf::from(p)
    } else {
        cairn_cli::vault::VaultRegistryStore::default_path()?
    };
    Ok(cairn_cli::vault::VaultRegistryStore::new(path))
}

/// Origin of the vault path returned by [`resolve_vault_or_cwd`].
///
/// Only [`VaultResolutionSource::CwdFallback`] should bypass the
/// vault-binding gate — every other source resolved a real vault path
/// and must therefore agree with `status`/`search`/`admin` on what
/// counts as a Cairn vault. Promoting the bare CWD fallback to bound
/// status would make `cairn status` advertise capabilities for any
/// directory; gating the other three sources keeps a stale registry
/// default or a half-bootstrapped CWD walk from advertising backed
/// capabilities (round-7 review #2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VaultResolutionSource {
    /// `--vault NAME_OR_PATH` flag or `CAIRN_VAULT` env var.
    Explicit,
    /// Walk-up from CWD found a `.cairn/` directory.
    CwdWalk,
    /// Registry `default` entry.
    RegistryDefault,
    /// No source resolved; degraded to bare CWD as a status-only
    /// safety net so an implicit-no-vault `cairn status` still emits a
    /// well-formed empty envelope.
    CwdFallback,
}

/// Resolve the active vault path through `vault::resolve_vault` (the
/// same resolver the top-level vault guard uses).
///
/// Returns:
/// - `Ok((path, source))` on success, where `source` records which
///   precedence rung produced `path` so callers can decide whether the
///   binding gate should fire.
/// - `Ok((cwd, CwdFallback))` only when the resolver reports
///   `NoneResolved` *and* the caller did not supply an explicit
///   selector — `cairn status` then advertises an empty capability
///   list rather than failing closed.
/// - `Err(...)` on any other resolver error: a missing named vault
///   (`NotFound`), a malformed registry, or a registry I/O failure.
///   Hiding these as a CWD fallback would let `cairn --vault NAME`
///   silently report capabilities for the current directory instead of
///   the named vault, breaking the advertised precedence (round-4
///   review #3).
///
/// `explicit` mirrors the merged `--vault` flag + `CAIRN_VAULT` env
/// stored in `main`'s `explicit_vault`.
fn resolve_vault_or_cwd(
    explicit: Option<&str>,
) -> anyhow::Result<(std::path::PathBuf, VaultResolutionSource)> {
    let cwd = std::env::current_dir().ok();
    let store = registry_store()?;
    let opts = cairn_cli::vault::ResolveOpts {
        explicit: explicit.map(str::to_owned),
        cwd: cwd.clone(),
        store: &store,
    };
    match cairn_cli::vault::resolve_vault(opts) {
        Ok(p) => {
            let source = if explicit.is_some() {
                VaultResolutionSource::Explicit
            } else if cwd
                .as_deref()
                .and_then(cairn_cli::vault::walk_up_to_vault)
                .as_deref()
                == Some(p.as_path())
            {
                VaultResolutionSource::CwdWalk
            } else {
                // Implicit request, no walk-up match — only the
                // registry default branch in `resolve_vault` could have
                // produced this path (`NoneResolved` is the alternative
                // and fell into the `Err` arm below).
                VaultResolutionSource::RegistryDefault
            };
            Ok((p, source))
        }
        Err(e) => {
            // Only `NoneResolved` for an *implicit* request degrades to
            // CWD. Any other error, or `NoneResolved` after an explicit
            // selector was given, surfaces upstream so the caller can
            // exit `EX_CONFIG` rather than report capabilities for the
            // wrong directory.
            let is_none_resolved = e
                .downcast_ref::<cairn_cli::vault::VaultError>()
                .is_some_and(|ve| matches!(ve, cairn_cli::vault::VaultError::NoneResolved));
            if is_none_resolved && explicit.is_none() {
                Ok((
                    cwd.unwrap_or_else(|| std::path::PathBuf::from(".")),
                    VaultResolutionSource::CwdFallback,
                ))
            } else {
                Err(e)
            }
        }
    }
}
fn main() -> ExitCode {
    let matches = match command::build_command().try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    ExitCode::SUCCESS
                }
                // EX_USAGE (64) for every clap-detected usage error.
                _ => ExitCode::from(64),
            };
        }
    };

    // Resolve --vault flag or CAIRN_VAULT env (§3.3 precedence 1 + 2).
    // Skip for `vault` and `bootstrap` management subcommands — they operate on the
    // registry/filesystem itself, not on a single vault's data.
    let explicit_vault: Option<String> = matches
        .get_one::<String>("vault")
        .cloned()
        .or_else(|| std::env::var("CAIRN_VAULT").ok());

    let active_subcommand = matches.subcommand_name().unwrap_or("");
    // admin verbs resolve their own vault path from CAIRN_VAULT / CWD; the
    // registry guard here would reject them when no vault is registered.
    // `identity` manages vault-path internally for each subcommand; exclude
    // from the top-level vault registry guard (which requires a named vault).
    let needs_vault_guard = !matches!(
        active_subcommand,
        "vault" | "bootstrap" | "plugins" | "mcp" | "admin" | "llm" | "identity"
    );

    if needs_vault_guard {
        let store = match registry_store() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn: registry path error — {e:#}");
                return ExitCode::from(78);
            }
        };
        let resolve_result = cairn_cli::vault::resolve_vault(cairn_cli::vault::ResolveOpts {
            explicit: explicit_vault.clone(),
            cwd: std::env::current_dir().ok(),
            store: &store,
        });
        match resolve_result {
            Ok(_vault_path) => {
                // vault_path resolved; will be passed to store context in #9
            }
            Err(e) => {
                // Hard-fail only for NotFound (explicit name that isn't registered).
                // NoneResolved is tolerated — all verbs return Internal anyway until #9.
                // NOTE: downcast_ref works only when no .context() wraps resolve_vault's error.
                // If #9 adds .context(...) at this call site, NotFound will silently become
                // tolerated. Update this guard when wiring the store.
                let is_not_found = e
                    .downcast_ref::<cairn_cli::vault::VaultError>()
                    .is_some_and(|ve| matches!(ve, cairn_cli::vault::VaultError::NotFound { .. }));
                if is_not_found {
                    eprintln!("cairn: {e:#}");
                    return ExitCode::from(78); // EX_CONFIG
                }
                // NoneResolved and other errors are tolerated until the store is wired (#9).
                let _e = e;
            }
        }
    }

    match matches.subcommand() {
        Some(("ingest", sub)) => verbs::ingest::run(sub),
        Some(("search", sub)) => match resolve_vault_or_cwd(explicit_vault.as_deref()) {
            // search has its own internal vault-binding gate, so the
            // resolution source doesn't change behaviour here — it only
            // needs the resolved path.
            Ok((vault_root, _source)) => verbs::search::run(sub, vault_root),
            Err(e) => {
                eprintln!("cairn search: vault resolution error — {e:#}");
                ExitCode::from(78) // EX_CONFIG
            }
        },
        Some(("retrieve", sub)) => verbs::retrieve::run(sub),
        Some(("summarize", sub)) => verbs::summarize::run(sub),
        Some(("assemble_hot", sub)) => verbs::assemble_hot::run(sub),
        Some(("capture_trace", sub)) => verbs::capture_trace::run(sub),
        Some(("lint", sub)) => verbs::lint::run(sub),
        Some(("forget", sub)) => verbs::forget::run(sub),
        Some(("status", sub)) => run_status(sub, explicit_vault.as_deref()),
        Some(("handshake", sub)) => run_handshake(sub, explicit_vault.as_deref()),
        Some(("plugins", sub)) => run_plugins(sub),
        Some(("bootstrap", sub)) => run_bootstrap(sub),
        Some(("mcp", _sub)) => cairn_cli::mcp::run(),
        Some(("vault", sub)) => run_vault(sub),
        Some(("skill", sub)) => run_skill(sub),
        Some(("admin", sub)) => run_admin(sub, explicit_vault.as_deref()),
        Some(("llm", sub)) => run_llm(sub),
        Some(("identity", sub)) => identity::cli::run_identity(sub, explicit_vault.clone()),
        None => unreachable!("subcommand_required(true) ensures a subcommand is always present"),
        Some((verb, _)) => {
            // Defensive: clap's subcommand_required(true) prevents this in practice.
            eprintln!("cairn: unknown subcommand '{verb}'");
            ExitCode::from(64)
        }
    }
}

/// `cairn handshake` dispatch (issue #52, brief §8.0.a).
///
/// Resolves the vault root the same way `status` does so the minted
/// challenge persists into `<vault>/.cairn/cairn.db`'s
/// `outstanding_challenges` table when the operator supplied
/// `--issuer`. Without `--issuer` the verb falls back to the pre-#52
/// ephemeral mint and prints a one-line warning explaining the
/// returned nonce is not redeemable.
fn run_handshake(sub: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let json = sub.get_flag("json");
    let issuer = sub.get_one::<String>("issuer").map(String::as_str);

    // `--issuer` absent ⇒ keep the ephemeral fallback path. Resolving a
    // vault here would only emit a confusing extra error message.
    if issuer.is_none() {
        return verbs::handshake::run_with_context(json, None, None);
    }

    let (vault_root, _source) = match resolve_vault_or_cwd(explicit_vault) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cairn handshake: vault resolution error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };
    verbs::handshake::run_with_context(json, Some(&vault_root), issuer)
}

/// `cairn status` dispatch.
///
/// Resolve vault + config so the search capabilities (keyword,
/// semantic, hybrid) and `policy_trace` are advertised when honored
/// end-to-end. Vault precedence (`CLAUDE.md` §6.5):
/// `--vault NAME_OR_PATH` flag → `CAIRN_VAULT` env → CWD walk →
/// registry default — handled centrally in `vault::resolve_vault`.
///
/// When the operator named a vault explicitly (`explicit_vault` is
/// `Some`) but the resolved path is not a valid bound vault, fail
/// closed with `EX_CONFIG` instead of silently reporting an empty
/// capability list — operators read `capabilities: []` as "vault has no
/// capabilities" rather than "this isn't a vault" (round-5 review #2).
///
/// Config load returns defaults when no config files exist (handled
/// inside `config::load`); a propagated error here means genuine
/// breakage — malformed YAML, unresolved env placeholders, or failed
/// validation. Fail closed with `EX_CONFIG` so clients do not
/// negotiate capabilities derived from a config the runtime would
/// also reject.
fn run_status(sub: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let json = sub.get_flag("json");
    let (vault_root, source) = match resolve_vault_or_cwd(explicit_vault) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cairn status: vault resolution error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };
    // Fire the vault-binding gate whenever the path came from a real
    // resolution source. Only the bare `CwdFallback` (implicit request
    // with no walk-up hit and no registry default) bypasses the gate so
    // an unconfigured CWD reports an empty capability list rather than
    // EX_CONFIG. A stale registry default or a half-bootstrapped CWD
    // walk must fail closed — they would otherwise advertise zero
    // capabilities while `cairn search`/`cairn admin` reject the same
    // path, and operators read `capabilities: []` as "vault has no
    // capabilities" rather than "this isn't a vault" (round-7 review #2).
    if source != VaultResolutionSource::CwdFallback {
        match verbs::status::probe_vault_binding(&vault_root) {
            verbs::status::VaultBinding::Bound => {}
            verbs::status::VaultBinding::Unbound => {
                let origin = match source {
                    VaultResolutionSource::Explicit => "--vault target",
                    VaultResolutionSource::CwdWalk => "vault discovered via cwd",
                    VaultResolutionSource::RegistryDefault => "registry default vault",
                    VaultResolutionSource::CwdFallback => unreachable!(),
                };
                eprintln!(
                    "cairn status: {origin} {} is not a Cairn vault \
                     (no .cairn/vault.id) — run `cairn bootstrap` first",
                    vault_root.display()
                );
                return ExitCode::from(78); // EX_CONFIG
            }
            verbs::status::VaultBinding::Invalid(reason) => {
                eprintln!("cairn status: vault binding error — {reason}");
                return ExitCode::from(78); // EX_CONFIG
            }
        }
    }
    let config =
        match cairn_cli::config::load(&vault_root, &cairn_cli::config::CliOverrides::default()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cairn status: config error — {e:#}");
                return ExitCode::from(78); // EX_CONFIG
            }
        };
    // require_bound mirrors the binding gate above: every real
    // resolution source already proved the vault is bound, so a
    // non-`Bound` recheck inside `run_with_context` means the binding
    // disappeared (TOCTOU, race with `cairn forget --vault`) and must
    // fail closed instead of silently downgrading to an empty
    // capability list (round-8 review #3).
    let require_bound = source != VaultResolutionSource::CwdFallback;
    verbs::status::run_with_context(json, Some(&vault_root), Some(&config), require_bound)
}

fn run_admin(matches: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    // Admin verbs mutate the vault store (e.g. `admin reindex` rebuilds
    // FTS / vector indexes). The selected vault must therefore honor
    // the same `--vault NAME_OR_PATH > CAIRN_VAULT > CWD` precedence
    // status uses; otherwise `CAIRN_VAULT=dev cairn --vault prod admin
    // reindex` would mutate `dev` despite the operator naming `prod`
    // (round-4 review #1).
    // admin gates every subcommand below via `enforce_vault_binding`,
    // so the resolution source isn't load-bearing here — discard it.
    let (vault_root, _source) = match resolve_vault_or_cwd(explicit_vault) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cairn admin: vault resolution error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    // Vault-binding gate: every admin subcommand mutates files under
    // `<vault_root>/.cairn` (`.cairn/cairn.db` for reindex,
    // `.cairn/models/...` for model fetch). Probe the sentinel BEFORE
    // loading config so an unbound or explicit non-vault path classifies
    // identically to status / search ("not a vault") instead of
    // bubbling a malformed `.cairn/config.yaml` as the primary
    // diagnostic — a non-vault config has no business shaping the
    // operator-facing error here (round-8 review #2).
    if let Some(rc) = enforce_vault_binding("admin", &vault_root) {
        return rc;
    }

    let config =
        match cairn_cli::config::load(&vault_root, &cairn_cli::config::CliOverrides::default()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cairn admin: config error — {e:#}");
                return ExitCode::from(78); // EX_CONFIG
            }
        };

    match matches.subcommand() {
        Some(("model", sub)) => match sub.subcommand() {
            Some(("fetch", fetch_sub)) => {
                verbs::admin_model_fetch::run(fetch_sub, &vault_root, &config)
            }
            _ => unreachable!(
                "clap subcommand_required(true) on admin model ensures a subcommand is always present"
            ),
        },
        Some(("reindex", sub)) => verbs::admin_reindex::run(sub, &vault_root, &config),
        _ => unreachable!(
            "clap subcommand_required(true) on admin ensures a subcommand is always present"
        ),
    }
}

/// Apply the file-only vault-binding gate (`probe_vault_binding`) and
/// translate a non-`Bound` outcome into an `EX_CONFIG` exit with a
/// human-readable diagnostic on stderr. Returns `Some(exit_code)` on
/// failure, `None` when the vault is bound.
///
/// Use for surfaces (admin / search) that mutate or open the store —
/// they must agree with `status` on what counts as a vault, otherwise
/// `cairn status` and the verbs would diverge on the same directory.
fn enforce_vault_binding(verb: &str, vault_root: &std::path::Path) -> Option<ExitCode> {
    match verbs::status::probe_vault_binding(vault_root) {
        verbs::status::VaultBinding::Bound => None,
        verbs::status::VaultBinding::Unbound => {
            eprintln!(
                "cairn {verb}: no Cairn vault at {} — run `cairn bootstrap` first",
                vault_root.display()
            );
            Some(ExitCode::from(78)) // EX_CONFIG
        }
        verbs::status::VaultBinding::Invalid(reason) => {
            eprintln!("cairn {verb}: vault binding error — {reason}");
            Some(ExitCode::from(78)) // EX_CONFIG
        }
    }
}

fn run_bootstrap(matches: &ArgMatches) -> ExitCode {
    let vault_path = std::path::PathBuf::from(
        matches
            .get_one::<String>("vault-path")
            .expect("invariant: vault-path has a default value"),
    );
    let json = matches.get_flag("json");
    let force = matches.get_flag("force");

    let opts = cairn_cli::vault::BootstrapOpts {
        vault_path: vault_path.clone(),
        force,
    };

    let receipt = match cairn_cli::vault::bootstrap(&opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn bootstrap: {e:#}");
            // EX_DATAERR (65) — vault.id is lost; DB or binding sentinel
            // proves the vault was already bound. The user must recover.
            return if format!("{e:#}").contains("vault.id lost") {
                ExitCode::from(65)
            } else {
                ExitCode::from(74) // EX_IOERR
            };
        }
    };

    // After vault layout creation, load config and optionally fetch the
    // embedding model (search.local_embeddings: true and model not present).
    let config = cairn_cli::config::load(&vault_path, &cairn_cli::config::CliOverrides::default())
        .unwrap_or_default();

    if config.search.local_embeddings {
        let models_root = vault_path.join(".cairn").join("models");
        let kind = config.search.embedding_model;
        let cache = cairn_embeddings_local::ModelCache::new(&models_root);
        if !cache.is_present(kind) {
            eprintln!(
                "cairn bootstrap: fetching embedding model '{}' (~25 MB)…",
                kind.as_str()
            );
            match cache.fetch(kind) {
                Ok(report) => {
                    eprintln!(
                        "cairn bootstrap: model '{}' fetched ({} bytes, integrity: {})",
                        kind.as_str(),
                        report.bytes_downloaded,
                        &report.integrity[..report.integrity.len().min(12)]
                    );
                }
                Err(e) => {
                    eprintln!(
                        "cairn bootstrap: failed to fetch embedding model '{}': {e:#}\n\
                         Check network access (or set HF_ENDPOINT for a mirror), \
                         or disable with `search.local_embeddings: false` in .cairn/config.yaml",
                        kind.as_str()
                    );
                    return ExitCode::from(69); // EX_UNAVAILABLE
                }
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt)
                .expect("invariant: BootstrapReceipt is always serializable")
        );
    } else {
        println!("{}", cairn_cli::vault::render_human(&receipt));
    }
    ExitCode::SUCCESS
}

fn run_skill(matches: &ArgMatches) -> ExitCode {
    match matches.subcommand() {
        Some(("install", sub)) => run_skill_install(sub),
        _ => unreachable!(
            "clap subcommand_required(true) on skill ensures a subcommand is always present"
        ),
    }
}

fn run_skill_install(matches: &ArgMatches) -> ExitCode {
    let harness = matches
        .get_one::<cairn_cli::skill::Harness>("harness")
        .expect("invariant: --harness is required by clap")
        .clone();

    let target_dir = if let Some(path) = matches.get_one::<String>("target-dir") {
        std::path::PathBuf::from(path)
    } else {
        match cairn_cli::skill::default_target_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("cairn skill install: {e:#}");
                return ExitCode::from(69); // EX_UNAVAILABLE
            }
        }
    };

    let force = matches.get_flag("force");
    let json = matches.get_flag("json");

    let opts = cairn_cli::skill::InstallOpts {
        target_dir,
        harness,
        force,
    };

    match cairn_cli::skill::install(&opts) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: InstallReceipt is always serializable")
                );
            } else {
                println!("{}", cairn_cli::skill::render_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skill install: {e:#}");
            ExitCode::from(74) // EX_IOERR
        }
    }
}

fn run_plugins(matches: &ArgMatches) -> ExitCode {
    let registry = match plugins::host::register_all() {
        Ok(r) => r,
        // EX_CONFIG (78) — bundled plugin.toml failed to parse.
        Err(PluginError::InvalidManifest(msg)) => {
            eprintln!("cairn plugins: bundled plugin manifest invalid — {msg}");
            return ExitCode::from(78);
        }
        // EX_UNAVAILABLE (69) — registry rejected a plugin.
        Err(e) => {
            eprintln!("cairn plugins: startup failed — {e}");
            return ExitCode::from(69);
        }
    };

    match matches.subcommand() {
        Some(("list", sub)) => {
            let json = sub.get_flag("json");
            let mut stdout = std::io::stdout().lock();
            let text = if json {
                plugins::list::render_json(&registry)
            } else {
                plugins::list::render_human(&registry)
            };
            let _ = writeln!(stdout, "{}", text.trim_end_matches('\n'));
            ExitCode::SUCCESS
        }
        Some(("verify", sub)) => {
            let strict = sub.get_flag("strict");
            let json = sub.get_flag("json");
            let report = plugins::verify::run(&registry);
            let text = if json {
                plugins::verify::render_json(&report)
            } else {
                plugins::verify::render_human(&report)
            };
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{}", text.trim_end_matches('\n'));
            ExitCode::from(plugins::verify::exit_code(&report, strict))
        }
        _ => unreachable!("clap subcommand_required(true) on plugins ensures a subcommand is set"),
    }
}

// Four subcommand branches (add/list/switch/remove) exceed the lint limit; split would add indirection for no gain.
#[allow(clippy::too_many_lines)]
fn run_vault(matches: &ArgMatches) -> ExitCode {
    let store = match registry_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cairn vault: registry path error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    match matches.subcommand() {
        Some(("add", sub)) => {
            let path = std::path::PathBuf::from(
                sub.get_one::<String>("path")
                    .expect("invariant: path is required"),
            );
            let name = sub
                .get_one::<String>("name")
                .expect("invariant: --name is required")
                .clone();
            let label = sub.get_one::<String>("label").cloned();
            let json = sub.get_flag("json");

            match cairn_cli::vault::add_vault(&store, path, name, label) {
                Ok(entry) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&entry)
                                .expect("invariant: VaultEntry always serializes")
                        );
                    } else {
                        println!(
                            "cairn vault add: registered '{}' → {}",
                            entry.name, entry.path
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cairn vault add: {e:#}");
                    ExitCode::from(78) // EX_CONFIG
                }
            }
        }
        Some(("list", sub)) => {
            let json = sub.get_flag("json");
            let reg = match store.load() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("cairn vault list: {e:#}");
                    return ExitCode::from(78);
                }
            };
            if json {
                let arr: Vec<serde_json::Value> = reg
                    .vaults
                    .iter()
                    .map(|v| {
                        let mut obj = serde_json::to_value(v)
                            .expect("invariant: VaultEntry always serializes to JSON");
                        obj["is_default"] =
                            serde_json::Value::Bool(reg.default.as_deref() == Some(&v.name));
                        obj
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&arr)
                        .expect("invariant: JSON array always serializes")
                );
            } else if reg.vaults.is_empty() {
                println!("cairn vault list: no vaults registered");
                println!("  add one with: cairn vault add <path> --name <name>");
            } else {
                for v in &reg.vaults {
                    let marker = if reg.default.as_deref() == Some(&v.name) {
                        "* "
                    } else {
                        "  "
                    };
                    let label = v
                        .label
                        .as_deref()
                        .map(|l| format!("  — {l}"))
                        .unwrap_or_default();
                    println!("{marker}{:<20} {}{}", v.name, v.path, label);
                }
            }
            ExitCode::SUCCESS
        }
        Some(("switch", sub)) => {
            let name = sub
                .get_one::<String>("name")
                .expect("invariant: name is required")
                .clone();
            let json = sub.get_flag("json");

            let mut reg = match store.load() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("cairn vault switch: {e:#}");
                    return ExitCode::from(78);
                }
            };
            if !reg.contains(&name) {
                eprintln!("cairn vault switch: vault '{name}' not found — run `cairn vault list`");
                return ExitCode::from(78);
            }
            reg.default = Some(name.clone());
            if let Err(e) = store.save(&reg) {
                eprintln!("cairn vault switch: {e:#}");
                return ExitCode::from(74); // EX_IOERR
            }
            if json {
                println!("{}", serde_json::json!({ "default": name }));
            } else {
                println!("cairn vault switch: default vault is now '{name}'");
            }
            ExitCode::SUCCESS
        }
        Some(("remove", sub)) => {
            let name = sub
                .get_one::<String>("name")
                .expect("invariant: name is required")
                .clone();
            let json = sub.get_flag("json");

            let mut reg = match store.load() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("cairn vault remove: {e:#}");
                    return ExitCode::from(78);
                }
            };
            if !reg.contains(&name) {
                eprintln!("cairn vault remove: vault '{name}' not found — run `cairn vault list`");
                return ExitCode::from(78);
            }
            if reg.default.as_deref() == Some(&name) {
                reg.default = None;
            }
            reg.vaults.retain(|v| v.name != name);
            if let Err(e) = store.save(&reg) {
                eprintln!("cairn vault remove: {e:#}");
                return ExitCode::from(74);
            }
            if json {
                println!("{}", serde_json::json!({ "removed": name }));
            } else {
                println!(
                    "cairn vault remove: removed '{name}' from registry (vault files untouched)"
                );
            }
            ExitCode::SUCCESS
        }
        _ => unreachable!("clap subcommand_required(true) on vault"),
    }
}

fn run_llm(matches: &ArgMatches) -> ExitCode {
    match matches.subcommand() {
        Some(("probe", sub)) => run_llm_probe(sub),
        _ => unreachable!("clap subcommand_required(true) on llm"),
    }
}

fn run_llm_probe(matches: &ArgMatches) -> ExitCode {
    let json = matches.get_flag("json");
    let prompt = matches.get_one::<String>("prompt").cloned();
    let schema_file = matches.get_one::<String>("schema-file").cloned();

    // Load config from cwd. The probe is a vault-agnostic diagnostic;
    // it reads `.cairn/config.yaml` from the current directory if present,
    // otherwise uses defaults (which produces NotConfigured).
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let config = match cairn_cli::config::load(&cwd, &cairn_cli::config::CliOverrides::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cairn llm probe: config error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    // Load schema file if provided. Compile it now (before any network
    // call) so an invalid schema fails fast as a config error and never
    // reaches the provider.
    let schema: Option<serde_json::Value> = match schema_file {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(v) => {
                    if let Err(e) = jsonschema::validator_for(&v) {
                        eprintln!("cairn llm probe: invalid JSON schema in {path}: {e}");
                        return ExitCode::from(78); // EX_CONFIG
                    }
                    Some(v)
                }
                Err(e) => {
                    eprintln!("cairn llm probe: schema parse error in {path}: {e}");
                    return ExitCode::from(78); // EX_CONFIG
                }
            },
            Err(e) => {
                eprintln!("cairn llm probe: cannot read schema file {path}: {e}");
                return ExitCode::from(78);
            }
        },
        None => None,
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn llm probe: tokio init error — {e}");
            return ExitCode::from(70); // EX_SOFTWARE
        }
    };

    runtime.block_on(cairn_cli::llm::run_probe(
        &config,
        json,
        prompt.as_deref(),
        schema,
    ))
}
