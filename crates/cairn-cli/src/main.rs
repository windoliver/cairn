//! Cairn CLI entry point (P0 scaffold).
//!
//! Real command dispatch lands when the verb layer does. Until then the
//! binary fails closed on every advertised verb, unknown argument, and any
//! malformed argv — including trailing junk after `--help` or `--version`.
//! Any caller relying on exit status cannot mistake a scaffold for a real
//! memory operation.

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
    // Skip argv[0] (the program name). Everything after that must match one
    // of the expected shapes exactly.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print_help();
            ExitCode::SUCCESS
        }
        [flag] if flag == "--help" || flag == "-h" => {
            print_help();
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("cairn {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [verb, rest @ ..] if VERBS.contains(&verb.as_str()) => {
            eprintln!(
                "cairn {verb}: not yet implemented in this P0 scaffold. \
                 The verb layer lands in follow-up issues; no memory \
                 operation was performed."
            );
            if !rest.is_empty() {
                eprintln!(
                    "cairn: ignored {n} trailing argument(s) — argv parsing \
                     arrives with the verb layer.",
                    n = rest.len()
                );
            }
            ExitCode::from(2)
        }
        _ => {
            eprintln!(
                "cairn: unrecognised argv {args:?}. Run `cairn --help` for \
                 the list of verbs this scaffold advertises (all currently \
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
