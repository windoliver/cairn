//! Cairn CLI entry point. Subcommand tree is generated from the IDL by
//! `cairn-codegen`; verb dispatch lands in #59 / #9. Until then, every verb
//! exits 2 with a not-implemented message so callers cannot mistake a
//! scaffold for a real memory operation.

use std::process::ExitCode;

mod generated;

fn main() -> ExitCode {
    let matches = generated::command().get_matches();
    match matches.subcommand() {
        Some((verb, _sub)) => {
            eprintln!(
                "cairn {verb}: not yet implemented in this P0 scaffold. \
                 Verb dispatch lands in #59 / #9; no memory operation was performed."
            );
            ExitCode::from(2)
        }
        None => {
            // No subcommand: print help and exit 0.
            let _ = generated::command().print_help();
            println!();
            ExitCode::SUCCESS
        }
    }
}
