//! `cairn skillpack` handler — pack, install, inspect.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::ArgMatches;

/// Dispatch `cairn skillpack <subcommand>`.
///
/// Called from `main.rs` after clap has matched the top-level `skillpack`
/// subcommand. All three sub-subcommands (`pack`, `install`, `inspect`) are
/// synchronous I/O-only operations; no tokio runtime is needed.
#[must_use]
pub fn run(sub: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    match sub.subcommand() {
        Some(("pack", args)) => run_pack(args, explicit_vault),
        Some(("install", args)) => run_install(args, explicit_vault),
        Some(("inspect", args)) => run_inspect(args),
        _ => unreachable!(
            "clap subcommand_required(true) on skillpack ensures a subcommand is always present"
        ),
    }
}

// ---------------------------------------------------------------------------
// pack
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "linear argument extraction + builder call; splitting adds no clarity"
)]
fn run_pack(args: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let name = args
        .get_one::<String>("name")
        .expect("invariant: --name is required");
    let version = args
        .get_one::<String>("version")
        .expect("invariant: --version is required");
    let cairn_compat = args
        .get_one::<String>("cairn-compat")
        .expect("invariant: --cairn-compat is required");
    let description = args
        .get_one::<String>("description")
        .expect("invariant: --description is required");
    let candidates_raw = args
        .get_one::<String>("candidates")
        .expect("invariant: --candidates is required");
    let output_dir = args.get_one::<PathBuf>("output");

    let candidate_ids: Vec<&str> = candidates_raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if candidate_ids.is_empty() {
        eprintln!("cairn skillpack pack: --candidates must list at least one candidate id");
        return ExitCode::from(64); // EX_USAGE
    }

    // Resolve vault root: explicit --vault flag wins, then CAIRN_VAULT, then CWD.
    let vault_root = resolve_vault(explicit_vault);

    // If an --output dir was given, resolve it; otherwise use the vault root.
    let build_dir = output_dir.cloned().unwrap_or_else(|| vault_root.clone());

    let mut builder = cairn_workflows::skillify::packer::SkillPackBuilder::new(
        name,
        version,
        cairn_compat,
        description,
    );
    for cid in &candidate_ids {
        builder = builder.add_candidate(cid);
    }

    // The packer writes the archive relative to the vault root regardless of
    // `--output`; if the caller wants a different dir we move it afterwards.
    match builder.build(&vault_root) {
        Ok(result) => {
            let archive_path = if output_dir.is_some() {
                let target = build_dir.join(
                    result
                        .archive_path
                        .file_name()
                        .unwrap_or(result.archive_path.as_os_str()),
                );
                if let Err(e) = std::fs::rename(&result.archive_path, &target) {
                    eprintln!("cairn skillpack pack: move archive to output dir: {e}");
                    return ExitCode::from(73); // EX_CANTCREAT
                }
                target
            } else {
                result.archive_path.clone()
            };

            let m = &result.manifest;
            println!("pack_id:      {}", m.pack_id);
            println!("name:         {}", m.name);
            println!("version:      {}", m.version);
            println!("cairn_compat: {}", m.cairn_compat);
            println!("description:  {}", m.description);
            println!("skills:       {}", m.skills.len());
            println!("provides:     {}", m.provides.join(", "));
            println!("sha256:       {}", m.content_sha256);
            println!("archive:      {}", archive_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skillpack pack: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

fn run_install(args: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let archive_path = args
        .get_one::<PathBuf>("path")
        .expect("invariant: <path> is required");

    let vault_root = resolve_vault(explicit_vault);
    let cairn_version = env!("CARGO_PKG_VERSION");

    match cairn_workflows::skillify::packer::unpack_archive(
        archive_path,
        &vault_root,
        cairn_version,
    ) {
        Ok(manifest) => {
            println!("installed pack:  {} v{}", manifest.name, manifest.version);
            println!("pack_id:         {}", manifest.pack_id);
            println!("skills installed:");
            for entry in &manifest.skills {
                println!(
                    "  - {} (lane: {}, slug: {})",
                    entry.candidate_id, entry.lane, entry.slug
                );
            }
            println!("vault:           {}", vault_root.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skillpack install: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

fn run_inspect(args: &ArgMatches) -> ExitCode {
    use std::io::Read as _;

    let archive_path = args
        .get_one::<PathBuf>("path")
        .expect("invariant: <path> is required");

    let file = match std::fs::File::open(archive_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "cairn skillpack inspect: open {}: {e}",
                archive_path.display()
            );
            return ExitCode::from(66); // EX_NOINPUT
        }
    };

    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cairn skillpack inspect: read archive entries: {e}");
            return ExitCode::from(66);
        }
    };

    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut candidate_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("cairn skillpack inspect: read entry: {e}");
                return ExitCode::from(66);
            }
        };
        let path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let path_str = path.to_string_lossy();
        if path_str == "manifest.json" {
            let mut buf = Vec::new();
            if let Err(e) = entry.read_to_end(&mut buf) {
                eprintln!("cairn skillpack inspect: read manifest.json: {e}");
                return ExitCode::from(66);
            }
            manifest_bytes = Some(buf);
        } else if let Some(rest) = path_str.strip_prefix("skills/") {
            // Record the unique candidate-id directory (first path segment).
            if let Some((cand, _)) = rest.split_once('/')
                && !cand.is_empty()
            {
                candidate_dirs.insert(cand.to_owned());
            }
        }
    }
    let skill_count = candidate_dirs.len();

    let Some(bytes) = manifest_bytes else {
        eprintln!("cairn skillpack inspect: manifest.json not found in archive");
        return ExitCode::from(65); // EX_DATAERR
    };

    let manifest: cairn_core::pipeline::skillify::SkillPackManifest =
        match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("cairn skillpack inspect: parse manifest.json: {e}");
                return ExitCode::from(65);
            }
        };

    println!("name:         {}", manifest.name);
    println!("version:      {}", manifest.version);
    println!("cairn_compat: {}", manifest.cairn_compat);
    println!("description:  {}", manifest.description);
    println!("pack_id:      {}", manifest.pack_id);
    println!(
        "skill count:  {} (manifest: {})",
        skill_count,
        manifest.skills.len()
    );
    if !manifest.requires.is_empty() {
        println!("requires:     {}", manifest.requires.join(", "));
    }
    if !manifest.provides.is_empty() {
        println!("provides:     {}", manifest.provides.join(", "));
    }
    if !manifest.content_sha256.is_empty() {
        println!("sha256:       {}", manifest.content_sha256);
    }
    println!("skills:");
    for entry in &manifest.skills {
        println!(
            "  - {} lane={} slug={}",
            entry.candidate_id, entry.lane, entry.slug
        );
    }

    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the vault root: `--vault` flag (via `explicit_vault`) > `CAIRN_VAULT`
/// env var > current working directory.
///
/// This is intentionally simpler than the full `vault::resolve_vault` used by
/// the core verbs — `skillpack` does not need registry lookup or
/// walk-up logic, just a directory to anchor file operations.
fn resolve_vault(explicit_vault: Option<&str>) -> PathBuf {
    if let Some(v) = explicit_vault {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var_os("CAIRN_VAULT") {
        return PathBuf::from(v);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
