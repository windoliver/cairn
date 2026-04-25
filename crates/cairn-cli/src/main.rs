//! Cairn CLI entry point. Subcommand tree is generated from the IDL by
//! `cairn-codegen`; verb dispatch lands in #59 / #9. Until then, every verb
//! exits 2 with a not-implemented message so callers cannot mistake a
//! scaffold for a real memory operation.

use std::process::ExitCode;

mod generated;

fn build_command() -> clap::Command {
    generated::command().version(env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    let matches = build_command().get_matches();
    if let Some((verb, _sub)) = matches.subcommand() {
        eprintln!(
            "cairn {verb}: not yet implemented in this P0 scaffold. \
             Verb dispatch lands in #59 / #9; no memory operation was performed."
        );
        ExitCode::from(2)
    } else {
        let _ = build_command().print_help();
        println!();
        ExitCode::SUCCESS
    }
}
