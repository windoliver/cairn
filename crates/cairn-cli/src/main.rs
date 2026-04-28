//! `cairn` binary entry point.
//!
//! Verb subcommands come from the IDL-generated clap builders (`mod generated`),
//! each wrapped with a `--json` flag via `cairn_cli::verbs::with_json()`. Actual
//! verb logic lives in `cairn_cli::verbs::*`; `main.rs` only owns parsing and
//! dispatch.

use std::io::Write;
use std::process::ExitCode;

use cairn_cli::config as cli_config;
use cairn_cli::{command, plugins, verbs};
use cairn_core::contract::registry::PluginError;
use clap::ArgMatches;

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
            .expect("vault-path has a default value"),
    );

    match cli_config::write_default(&vault_path) {
        Ok(()) => {
            println!(
                "cairn bootstrap: wrote default config to {}",
                vault_path.join(".cairn/config.yaml").display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            // EX_CONFIG (78) — bad config or file already exists
            eprintln!("cairn bootstrap: {e:#}");
            ExitCode::from(78)
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
