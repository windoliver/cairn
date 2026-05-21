//! Memory gate subcommand.

pub mod manifest;
pub mod sizer;

use std::path::PathBuf;
use std::process::Command;

use clap::Args;

use crate::gates::report::GateOutcome;

/// Arguments for the `memory` subcommand.
#[derive(Args, Debug)]
pub struct MemoryArgs {
    /// Path to the manifest TOML.
    #[arg(long, default_value = "crates/cairn-bench/manifests/memory.toml")]
    pub manifest: PathBuf,

    /// Profile name to run.
    #[arg(long, default_value = "default")]
    pub profile: String,

    /// Skip the `cargo build --release` step (use the existing binary).
    #[arg(long)]
    pub no_build: bool,

    /// Skip the `cairn bootstrap` step (use the existing tempdir's models).
    #[arg(long)]
    pub no_bootstrap: bool,

    /// Output dir.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,
}

impl MemoryArgs {
    /// Construct args with CI-appropriate defaults.
    #[must_use]
    pub fn default_for_ci() -> Self {
        Self {
            manifest: "crates/cairn-bench/manifests/memory.toml".into(),
            profile: "default".into(),
            no_build: false,
            no_bootstrap: false,
            out_dir: "target/cairn-bench".into(),
        }
    }
}

/// Run the memory gate.
///
/// # Errors
/// Returns an error if the manifest is unreadable, the cargo build fails, or
/// asset walks hit I/O errors.
pub fn run(args: &MemoryArgs) -> anyhow::Result<GateOutcome> {
    let raw = manifest::load(&args.manifest)?;
    let resolved = manifest::resolve(&raw, &args.profile)?;

    if !args.no_build {
        let mut cmd = Command::new("cargo");
        cmd.args(["build", "--release", "-p", "cairn-cli", "--locked"]);
        for f in &resolved.features {
            cmd.args(["--features", f]);
        }
        let status = cmd.status()?;
        anyhow::ensure!(status.success(), "cargo build failed");
    }

    let vault_dir = tempfile::tempdir()?;
    if !args.no_bootstrap {
        // Bootstrap a vault so the embedding model is downloaded into `.cairn/models/`.
        let bin = std::path::Path::new(&resolved.binary);
        if bin.exists() {
            let status = Command::new(bin)
                .args(["bootstrap", "--vault-path"])
                .arg(vault_dir.path())
                .status()?;
            anyhow::ensure!(status.success(), "cairn bootstrap failed");
        } else {
            eprintln!(
                "skipping bootstrap: binary not built at {} (use --no-build only if it exists)",
                resolved.binary
            );
        }
    }

    let report = sizer::measure(&resolved, vault_dir.path())?;

    std::fs::create_dir_all(&args.out_dir)?;
    let out = args.out_dir.join("memory.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
    print_human_summary(&report);
    Ok(sizer::outcome(&report))
}

fn print_human_summary(r: &sizer::MemoryReport) {
    println!(
        "memory gate ({}): {}",
        r.profile,
        if r.ok { "PASS" } else { "FAIL" }
    );
    for a in &r.assets {
        let mark = if a.ok { "OK" } else { "FAIL" };
        let suffix = a
            .note
            .as_deref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        println!(
            "  [{mark}] {name}: {sz:.1} MB (expected {exp:.1} ±{tol:.0}%){suffix}",
            mark = mark,
            name = a.name,
            sz = a.size_mb,
            exp = a.expected_mb,
            tol = a.tolerance_pct
        );
    }
    println!(
        "  total: {:.1} MB / budget {:.1} MB",
        r.total_mb, r.budget_mb
    );
    if !r.failures.is_empty() {
        println!("failures:");
        for f in &r.failures {
            println!("  - {f}");
        }
    }
}
