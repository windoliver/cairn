//! Cairn CLI entry point (P0 scaffold).
//!
//! Real command dispatch lands when the verb layer does. Until then the
//! binary fails closed on every advertised verb and on unknown arguments:
//! any caller relying on exit status cannot mistake a scaffold for a real
//! memory operation. `--help`/`--version`/no-args continue to succeed.

use std::process::ExitCode;

const VERBS: &[&str] = &[
    "ingest",
    "search",
    "retrieve",
    "summarize",
    "assemble_hot",
    "capture_trace",
    "lint",
    "forget",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("cairn {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(arg) if VERBS.contains(&arg) => {
            eprintln!(
                "cairn {arg}: not yet implemented in this P0 scaffold. \
                 The verb layer lands in follow-up issues; no memory \
                 operation was performed."
            );
            ExitCode::from(2)
        }
        Some(arg) => {
            eprintln!(
                "cairn: unknown argument {arg:?}. Run `cairn --help` for the \
                 list of verbs this scaffold advertises (all currently \
                 return a not-implemented error)."
            );
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("cairn {} — P0 scaffold", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Verbs (not yet implemented — every verb exits 2):");
    for v in VERBS {
        println!("  cairn {v}");
    }
}
