//! Cairn CLI entry point (P0 scaffold).
//!
//! Real command dispatch lands when the verb layer does. For now the binary
//! prints its version or a help listing so smoke tests have something to
//! exercise.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some("--version" | "-V") = args.get(1).map(String::as_str) {
        println!("cairn {}", env!("CARGO_PKG_VERSION"));
    } else {
        println!("cairn {} — P0 scaffold", env!("CARGO_PKG_VERSION"));
        println!();
        println!("Verbs (not yet implemented):");
        for v in [
            "ingest", "search", "retrieve", "summarize",
            "assemble_hot", "capture_trace", "lint", "forget",
        ] {
            println!("  cairn {v}");
        }
    }
}
