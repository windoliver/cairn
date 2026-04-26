//! `cairn` binary entry point.
//!
//! Verb subcommands (`ingest`, `search`, …) come from the IDL-generated
//! clap `Command` tree (`mod generated`); the `plugins` subcommand is
//! augmented at runtime. Until the verb layer lands (#59 / #9), every
//! verb exits 2 with a not-implemented message so callers cannot mistake
//! a scaffold for a real memory operation.

use std::io::Write;
use std::process::ExitCode;

use cairn_cli::plugins;
use clap::ArgMatches;

mod generated;

fn build_command() -> clap::Command {
    generated::command()
        .version(env!("CARGO_PKG_VERSION"))
        .about("Cairn — agent memory framework")
        .subcommand(plugins_subcommand())
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
                _ => ExitCode::from(2),
            };
        }
    };

    match matches.subcommand() {
        Some(("plugins", sub)) => run_plugins(sub),
        Some((verb, _)) => {
            eprintln!(
                "cairn {verb}: not yet implemented in this P0 scaffold. \
                 Verb dispatch lands in #59 / #9; no memory operation was \
                 performed."
            );
            ExitCode::from(2)
        }
        None => {
            let _ = build_command().print_help();
            println!();
            ExitCode::SUCCESS
        }
    }
}

fn run_plugins(matches: &ArgMatches) -> ExitCode {
    let registry = match plugins::host::register_all() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn plugins: startup failed — {e}");
            // EX_UNAVAILABLE
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
            // Newline at end ensures the human table flushes cleanly.
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
