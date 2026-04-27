//! `cairn` binary entry point.
//!
//! Verb subcommands come from the IDL-generated clap builders (`mod generated`),
//! each wrapped with a `--json` flag via `cairn_cli::verbs::with_json()`. Actual
//! verb logic lives in `cairn_cli::verbs::*`; `main.rs` only owns parsing and
//! dispatch.

use std::io::Write;
use std::process::ExitCode;

use cairn_cli::{plugins, verbs};
use cairn_core::contract::registry::PluginError;
use clap::ArgMatches;

mod generated;

fn build_command() -> clap::Command {
    clap::Command::new("cairn")
        .about("Cairn — agent memory framework (cairn.mcp.v1)")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true)
        // Eight core verbs, each with --json added.
        .subcommand(verbs::with_json(generated::verbs::ingest_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::search_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::retrieve_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::summarize_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::assemble_hot_subcommand()))
        .subcommand(verbs::with_json(
            generated::verbs::capture_trace_subcommand(),
        ))
        .subcommand(verbs::with_json(generated::verbs::lint_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::forget_subcommand()))
        // Protocol preludes.
        .subcommand(verbs::with_json(generated::prelude::handshake_subcommand()))
        .subcommand(verbs::with_json(generated::prelude::status_subcommand()))
        // Management subcommand (plugins already has --json per sub-subcommand).
        .subcommand(plugins_subcommand())
        .subcommand(bootstrap_subcommand())
        .subcommand(vault_subcommand())
}

fn bootstrap_subcommand() -> clap::Command {
    clap::Command::new("bootstrap")
        .about("Initialize a vault directory tree with the §3 layout")
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .help("Vault root directory (default: current directory)"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON receipt instead of human-readable output"),
        )
        .arg(
            clap::Arg::new("force")
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("Overwrite existing placeholder files"),
        )
}

fn vault_subcommand() -> clap::Command {
    clap::Command::new("vault")
        .about("Manage the vault registry (brief §3.3)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("add")
                .about("Register a vault in the registry")
                .arg(
                    clap::Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .help("Filesystem path to the vault root"),
                )
                .arg(
                    clap::Arg::new("name")
                        .long("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Short identifier for the vault"),
                )
                .arg(
                    clap::Arg::new("label")
                        .long("label")
                        .value_name("LABEL")
                        .help("Human-readable description"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("list")
                .about("List registered vaults")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("switch")
                .about("Set the default vault")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to make default"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("remove")
                .about("Remove a vault from the registry (does not delete files)")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to remove"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
}

fn registry_store() -> anyhow::Result<cairn_cli::vault::VaultRegistryStore> {
    let path = if let Ok(p) = std::env::var("CAIRN_REGISTRY") {
        std::path::PathBuf::from(p)
    } else {
        cairn_cli::vault::VaultRegistryStore::default_path()?
    };
    Ok(cairn_cli::vault::VaultRegistryStore::new(path))
}

fn plugins_subcommand() -> clap::Command {
    clap::Command::new("plugins")
        .about("Manage and inspect bundled plugins")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("list").about("List loaded plugins").arg(
                clap::Arg::new("json")
                    .long("json")
                    .action(clap::ArgAction::SetTrue)
                    .help("Emit JSON instead of a human-readable table"),
            ),
        )
        .subcommand(
            clap::Command::new("verify")
                .about("Run the conformance suite against every loaded plugin")
                .arg(
                    clap::Arg::new("strict")
                        .long("strict")
                        .action(clap::ArgAction::SetTrue)
                        .help("Treat tier-2 `pending` cases as failures"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON instead of a human-readable report"),
                ),
        )
}

fn main() -> ExitCode {
    let matches = match build_command().try_get_matches() {
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

    match matches.subcommand() {
        Some(("ingest", sub)) => verbs::ingest::run(sub),
        Some(("search", sub)) => verbs::search::run(sub),
        Some(("retrieve", sub)) => verbs::retrieve::run(sub),
        Some(("summarize", sub)) => verbs::summarize::run(sub),
        Some(("assemble_hot", sub)) => verbs::assemble_hot::run(sub),
        Some(("capture_trace", sub)) => verbs::capture_trace::run(sub),
        Some(("lint", sub)) => verbs::lint::run(sub),
        Some(("forget", sub)) => verbs::forget::run(sub),
        Some(("status", sub)) => verbs::status::run(sub.get_flag("json")),
        Some(("handshake", sub)) => verbs::handshake::run(sub.get_flag("json")),
        Some(("plugins", sub)) => run_plugins(sub),
        Some(("bootstrap", sub)) => run_bootstrap(sub),
        Some(("vault", sub)) => run_vault(sub),
        None => unreachable!("subcommand_required(true) ensures a subcommand is always present"),
        Some((verb, _)) => {
            // Defensive: clap's subcommand_required(true) prevents this in practice.
            eprintln!("cairn: unknown subcommand '{verb}'");
            ExitCode::from(64)
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

    let opts = cairn_cli::vault::BootstrapOpts { vault_path, force };

    match cairn_cli::vault::bootstrap(&opts) {
        Ok(receipt) => {
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
        Err(e) => {
            eprintln!("cairn bootstrap: {e:#}");
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
        Some(("list", _sub)) => {
            // implemented in Task 5
            eprintln!("cairn vault list: not yet implemented");
            ExitCode::from(1)
        }
        Some(("switch", _sub)) => {
            // implemented in Task 6
            eprintln!("cairn vault switch: not yet implemented");
            ExitCode::from(1)
        }
        Some(("remove", _sub)) => {
            // implemented in Task 6
            eprintln!("cairn vault remove: not yet implemented");
            ExitCode::from(1)
        }
        _ => unreachable!("clap subcommand_required(true) on vault"),
    }
}
