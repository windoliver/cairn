//! `cairn skill install` — writes the Cairn skill bundle to the harness skill
//! directory (§8.0.a-bis, §18.d).

use anyhow::{Context as _, Result};
use clap::ValueEnum;
use std::path::PathBuf;

/// Supported harnesses for `cairn skill install`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    /// Claude Code harness — registers the skill via CLAUDE.md.
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex harness — registers the skill via AGENTS.md.
    Codex,
    /// Gemini CLI harness — registers the skill via GEMINI.md.
    Gemini,
    /// `OpenCode` harness — registers the skill via the opencode config skills path.
    Opencode,
    /// Cursor harness — registers the skill via .cursorrules.
    Cursor,
    /// Custom or unknown harness — prints a generic registration hint.
    Custom,
}

/// Returns the harness-specific registration hint for the actual install path.
///
/// The hint uses `target_dir` so that `--target-dir` installs produce a hint
/// that points at the real installed location, not the default path.
#[must_use]
pub fn registration_hint(harness: &Harness, target_dir: &std::path::Path) -> String {
    let skill_path = target_dir.join("SKILL.md").display().to_string();
    match harness {
        Harness::ClaudeCode => format!("# Add to your CLAUDE.md:\n@{skill_path}"),
        Harness::Codex => format!("# Add to your AGENTS.md:\n@{skill_path}"),
        Harness::Gemini => format!("# Add to your GEMINI.md:\n@{skill_path}"),
        Harness::Opencode => format!("# Add to your opencode config skills path:\n{skill_path}"),
        Harness::Cursor => format!("# Add to your .cursorrules:\n@{skill_path}"),
        Harness::Custom => format!(
            "# Skill bundle written to {skill_path}. Register it with your harness manually."
        ),
    }
}

// Embedded at compile time from the committed generated artifacts.
// The CI `--check` gate catches drift between these and what cairn-codegen emits.
const SKILL_MD: &str = include_str!("../../../skills/cairn/SKILL.md");
const CONVENTIONS_MD: &str = include_str!("../../../skills/cairn/conventions.md");
const VERSION_FILE: &str = include_str!("../../../skills/cairn/.version");

// Example stubs — codegen owns the source-of-truth copy in skills/cairn/examples/;
// installed copies are written once and user-editable afterwards.
const EXAMPLE_01: &str = include_str!("../../../skills/cairn/examples/01-remember-preference.md");
const EXAMPLE_02: &str = include_str!("../../../skills/cairn/examples/02-forget-something.md");
const EXAMPLE_03: &str = include_str!("../../../skills/cairn/examples/03-search-prior-decision.md");
const EXAMPLE_04: &str = include_str!("../../../skills/cairn/examples/04-skillify-this.md");
const EXAMPLE_05: &str = include_str!("../../../skills/cairn/examples/05-retrieve-context.md");
const EXAMPLE_06: &str = include_str!("../../../skills/cairn/examples/06-lint-memory.md");

/// Options for [`install`].
#[derive(Debug, Clone)]
pub struct InstallOpts {
    /// Target directory. Default: `~/.cairn/skills/cairn/`.
    pub target_dir: PathBuf,
    /// Which harness to generate the registration hint for.
    pub harness: Harness,
    /// If `true`, overwrite generated files even if the version matches.
    pub force: bool,
}

/// Result of a skill install run.
#[derive(Debug, serde::Serialize)]
pub struct InstallReceipt {
    /// The directory where the skill was installed.
    pub target_dir: PathBuf,
    /// Version string from the contract (cairn-idl crate version).
    pub contract_version: String,
    /// Version string from the IDL / SKILL.md.
    pub idl_version: String,
    /// Paths to files created during the install.
    pub files_created: Vec<PathBuf>,
    /// Paths to files skipped (already present, version match).
    pub files_skipped: Vec<PathBuf>,
    /// Harness-specific registration hint for the user.
    pub registration_hint: String,
}

/// Resolves the default install directory (`~/.cairn/skills/cairn/`).
///
/// # Errors
/// Returns an error if `HOME` is not set in the environment.
pub fn default_target_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home).join(".cairn/skills/cairn"))
}

/// Compares two `X.Y.Z` version strings. Returns `Less` if `a < b`.
///
/// Inputs are expected to be valid `X.Y.Z`; unparseable values map to
/// `Equal` as a safe fallback (avoids panics on unexpected input).
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut it = v.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => std::cmp::Ordering::Equal,
    }
}

/// Rejects symlinks anywhere in the target path, including ancestors.
///
/// Relative paths are made absolute first so that relative symlink ancestors
/// (e.g. `./link/cairn`) are caught. The first two components after root are
/// skipped to accommodate OS-managed root symlinks (`/var → /private/var` on
/// macOS); all deeper components and the final target itself are always checked.
fn reject_symlink_ancestors(path: &std::path::Path) -> Result<()> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolving current directory for symlink check")?
            .join(path)
    };

    let mut check = PathBuf::new();
    let mut depth = 0usize;
    for component in abs.components() {
        check.push(component);
        depth += 1;
        // depth 1 = root `/`; depth 2 = first Normal (e.g. `var`, `Users`, `home`).
        // These are OS-managed on macOS and skipped to prevent false positives from
        // `/var → /private/var`; everything deeper is user-controlled.
        let is_final = check == abs;
        if (depth > 2 || is_final)
            && std::fs::symlink_metadata(&check)
                .ok()
                .is_some_and(|m| m.file_type().is_symlink())
        {
            anyhow::bail!(
                "{} is a symlink — cairn will not write through it",
                check.display()
            );
        }
    }
    Ok(())
}

/// Creates each directory component one at a time, checking immediately after
/// each `mkdir` that the created (or pre-existing) entry is not a symlink.
///
/// Narrows the TOCTOU window compared to `create_dir_all` + a single preflight,
/// while still respecting the macOS depth-2 skip (OS-managed root symlinks).
fn create_dir_checked(path: &std::path::Path) -> Result<()> {
    let mut check = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        check.push(component);
        depth += 1;
        match std::fs::create_dir(&check) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // AlreadyExists only succeeds if the entry is a real directory;
                // a regular file at this path would cause later writes to fail
                // mid-install, leaving a partial install without .version.
                if !std::fs::metadata(&check).is_ok_and(|m| m.is_dir()) {
                    anyhow::bail!("{} exists but is not a directory", check.display());
                }
            }
            Err(e) => {
                return Err(anyhow::Error::from(e)
                    .context(format!("creating directory {}", check.display())));
            }
        }
        // Same depth-2 skip as reject_symlink_ancestors for macOS /var.
        let is_final = check == path;
        if (depth > 2 || is_final)
            && std::fs::symlink_metadata(&check)
                .ok()
                .is_some_and(|m| m.file_type().is_symlink())
        {
            anyhow::bail!(
                "{} is a symlink — cairn will not create directories through it",
                check.display()
            );
        }
    }
    Ok(())
}

/// Reads the installed IDL version from `.version`.
///
/// Requires both `contract: cairn.mcp.v1` and `cairn-idl: X.Y.Z` to be present
/// and valid; any other content is treated as malformed.
///
/// - `NotFound` → `Ok(None)` (fresh install, no prior Cairn install).
/// - File exists but invalid schema + `!force` → `Err` (fail closed; corrupt metadata).
/// - File exists but invalid schema + `force` → `Ok(None)` (user explicitly overrides).
/// - Symlink → always `Err`.
fn read_installed_version(version_path: &std::path::Path, force: bool) -> Result<Option<String>> {
    if let Ok(meta) = std::fs::symlink_metadata(version_path)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!(
            "{} is a symlink — cairn will not read through it",
            version_path.display()
        );
    }
    let content = match std::fs::read_to_string(version_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::Error::from(e).context(format!(
                "reading installed version from {}",
                version_path.display()
            )));
        }
    };
    // Strict schema: exactly one 'contract: cairn.mcp.v1', exactly one valid
    // 'cairn-idl: X.Y.Z', no other non-empty lines. Extra/duplicate fields would
    // let a spoofed .version bypass the foreign-content preflight.
    let mut contract_count = 0u32;
    let mut idl_version: Option<String> = None;
    let mut unknown_count = 0u32;
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        if line == "contract: cairn.mcp.v1" {
            contract_count += 1;
        } else if let Some(v) = line.strip_prefix("cairn-idl: ") {
            let v = v.trim();
            let mut parts = v.splitn(4, '.');
            let valid = parts.next().and_then(|s| s.parse::<u64>().ok()).is_some()
                && parts.next().and_then(|s| s.parse::<u64>().ok()).is_some()
                && parts.next().and_then(|s| s.parse::<u64>().ok()).is_some()
                && parts.next().is_none();
            if valid {
                idl_version = Some(v.to_owned());
            } else {
                unknown_count += 1;
            }
        } else {
            unknown_count += 1;
        }
    }
    let valid = contract_count == 1 && idl_version.is_some() && unknown_count == 0;
    match (valid, idl_version) {
        (true, Some(v)) => Ok(Some(v)),
        _ if force => Ok(None),
        _ => anyhow::bail!(
            "{} is not a valid Cairn .version file (must contain exactly \
             'contract: cairn.mcp.v1' and 'cairn-idl: X.Y.Z', nothing else) — \
             pass --force to overwrite it",
            version_path.display()
        ),
    }
}

/// Checks that `dir` contains only Cairn-created entries (or is empty).
///
/// For generated files (`SKILL.md`, `conventions.md`), byte-compares the
/// existing content against the embedded Cairn artifacts. A matching file
/// is from a partial install and safe to retry; a differing file (e.g. a
/// user's own SKILL.md) is treated as foreign and triggers a bail. This
/// prevents silent overwrites while still allowing idempotent retry.
fn check_no_foreign_content(dir: &std::path::Path) -> Result<()> {
    const CAIRN_ENTRIES: &[&str] = &["SKILL.md", "conventions.md", ".version", "examples"];
    const GENERATED_FILES: &[(&str, &str)] =
        &[("SKILL.md", SKILL_MD), ("conventions.md", CONVENTIONS_MD)];
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("checking contents of {}", dir.display()))?
    {
        let name = entry.context("reading directory entry")?.file_name();
        if !CAIRN_ENTRIES.iter().any(|&n| name == n) {
            anyhow::bail!(
                "{} contains non-Cairn files but has no Cairn .version — \
                 pass --force to install into this directory",
                dir.display()
            );
        }
        // `examples` must be a real directory; validate its contents too so that
        // a pre-existing `examples/` with arbitrary files does not bypass the guard.
        if name == "examples" {
            const EXAMPLE_NAMES: &[&str] = &[
                "01-remember-preference.md",
                "02-forget-something.md",
                "03-search-prior-decision.md",
                "04-skillify-this.md",
                "05-retrieve-context.md",
                "06-lint-memory.md",
            ];
            let examples_path = dir.join("examples");
            if !std::fs::metadata(&examples_path).is_ok_and(|m| m.is_dir()) {
                anyhow::bail!(
                    "{} exists but is not a directory — pass --force to overwrite",
                    examples_path.display()
                );
            }
            for ex_entry in std::fs::read_dir(&examples_path)
                .with_context(|| format!("checking contents of {}", examples_path.display()))?
            {
                let ex_name = ex_entry
                    .context("reading examples directory entry")?
                    .file_name();
                if !EXAMPLE_NAMES.iter().any(|&n| ex_name == n) {
                    anyhow::bail!(
                        "{} contains unexpected files but has no Cairn .version — \
                         pass --force to install into this directory",
                        examples_path.display()
                    );
                }
            }
        }
        // Byte-compare generated files against the embedded artifacts.
        // A file with different content is not Cairn-produced and must not
        // be silently overwritten.
        if let Some((_, expected)) = GENERATED_FILES.iter().find(|(n, _)| name == *n) {
            let path = dir.join(&name);
            let actual = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            if actual.as_str() != *expected {
                anyhow::bail!(
                    "{} exists with content that does not match the Cairn artifact — \
                     pass --force to overwrite",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

/// Installs the Cairn skill bundle at `opts.target_dir`.
///
/// Idempotent and version-aware. See the spec (§8.0.a-bis, §18.d) for the
/// full decision tree.
///
/// # Errors
/// Returns an error if a directory cannot be created or a file cannot be
/// written. Symlinked paths are rejected.
pub fn install(opts: &InstallOpts) -> Result<InstallReceipt> {
    // Reject empty paths before any filesystem access.
    if opts.target_dir.as_os_str().is_empty() {
        anyhow::bail!("--target-dir must not be empty");
    }

    // Normalize to an absolute, lexically clean path so that:
    //  - relative paths produce absolute registration hints
    //  - the non-empty preflight sees the actual filesystem directory
    //  - registration hints paste correctly from any working directory
    let target = if opts.target_dir.is_absolute() {
        opts.target_dir.components().collect::<PathBuf>()
    } else {
        std::env::current_dir()
            .context("resolving current directory for target path")?
            .join(&opts.target_dir)
            .components()
            .collect::<PathBuf>()
    };
    let target = &target;

    // All workspace crates share a single version (see [workspace.package] in Cargo.toml),
    // so cairn-cli's CARGO_PKG_VERSION matches the cairn-idl version embedded in .version.
    let current_idl_version = env!("CARGO_PKG_VERSION");

    // Reject symlinks anywhere in the target path (catches symlinked ancestors).
    reject_symlink_ancestors(target)?;

    // Read .version before creating directories so a malformed/symlinked .version
    // fails closed without leaving filesystem side effects behind.
    let installed_version = read_installed_version(&target.join(".version"), opts.force)?;

    // Refuse to install into a non-empty directory that contains non-Cairn files
    // and has no Cairn .version. Foreign-content check allows retry after a partial
    // install (e.g. crash before .version is written) while blocking accidental
    // installs into user directories.
    if !opts.force && installed_version.is_none() && target.is_dir() {
        check_no_foreign_content(target)?;
    }

    // Create target dir and examples/ subdir component-by-component, validating
    // each entry for symlinks immediately after creation to narrow the TOCTOU window.
    create_dir_checked(&target.join("examples"))?;

    let skip_generated = match &installed_version {
        Some(installed) if installed == current_idl_version && !opts.force => {
            // Same version: still byte-check on-disk files against the embedded artifacts.
            // Content can drift in development (e.g. two builds at the same version) and
            // version equality alone is not sufficient proof of freshness.
            let skill_current =
                std::fs::read_to_string(target.join("SKILL.md")).is_ok_and(|s| s == SKILL_MD);
            let conventions_current = std::fs::read_to_string(target.join("conventions.md"))
                .is_ok_and(|s| s == CONVENTIONS_MD);
            skill_current && conventions_current
        }
        Some(installed)
            if compare_versions(installed, current_idl_version) == std::cmp::Ordering::Greater =>
        {
            eprintln!(
                "cairn skill install: warning — installed version ({installed}) is newer \
                 than this binary ({current_idl_version}); proceeding with downgrade"
            );
            false
        }
        _ => false,
    };

    let mut files_created: Vec<PathBuf> = Vec::new();
    let mut files_skipped: Vec<PathBuf> = Vec::new();

    // Generated files: overwrite unless skip_generated.
    let gen_force = opts.force || !skip_generated;
    crate::vault::bootstrap::write_once(
        &target.join("SKILL.md"),
        SKILL_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::bootstrap::write_once(
        &target.join("conventions.md"),
        CONVENTIONS_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;

    // Example stubs: write-once, never overwrite (user may have edited).
    let examples_dir = target.join("examples");
    for (name, content) in [
        ("01-remember-preference.md", EXAMPLE_01),
        ("02-forget-something.md", EXAMPLE_02),
        ("03-search-prior-decision.md", EXAMPLE_03),
        ("04-skillify-this.md", EXAMPLE_04),
        ("05-retrieve-context.md", EXAMPLE_05),
        ("06-lint-memory.md", EXAMPLE_06),
    ] {
        crate::vault::bootstrap::write_once(
            &examples_dir.join(name),
            content,
            false, // never force-overwrite examples
            &mut files_created,
            &mut files_skipped,
        )?;
    }

    // Write .version last so a partial failure (e.g. in examples/) never
    // leaves a version-stamped incomplete install.
    crate::vault::bootstrap::write_once(
        &target.join(".version"),
        VERSION_FILE,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;

    let hint = registration_hint(&opts.harness, target);

    // Parse contract version from embedded .version file for the receipt.
    let contract_version = VERSION_FILE
        .lines()
        .find_map(|l| l.strip_prefix("contract: ").map(str::to_owned))
        .unwrap_or_else(|| "cairn.mcp.v1".to_owned());

    Ok(InstallReceipt {
        target_dir: target.clone(),
        contract_version,
        idl_version: current_idl_version.to_owned(),
        files_created,
        files_skipped,
        registration_hint: hint,
    })
}

/// Renders a human-readable summary of an install receipt.
#[must_use]
pub fn render_human(receipt: &InstallReceipt) -> String {
    let header = if receipt.files_created.is_empty() && receipt.files_skipped.is_empty() {
        format!(
            "cairn skill install: nothing to do at {}",
            receipt.target_dir.display()
        )
    } else if receipt.files_created.is_empty() {
        format!(
            "cairn skill install: already up to date at {} (v{})\n  (pass --force to overwrite generated files)",
            receipt.target_dir.display(),
            receipt.idl_version,
        )
    } else {
        format!(
            "cairn skill install: skill bundle installed at {}",
            receipt.target_dir.display()
        )
    };

    let mut lines = vec![header];
    for path in &receipt.files_created {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        lines.push(format!("  {name}  [created]"));
    }
    for path in &receipt.files_skipped {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        lines.push(format!("  {name}  [skipped]"));
    }

    if !receipt.registration_hint.is_empty() {
        lines.push(String::new());
        lines.push("Registration hint:".to_owned());
        for hint_line in receipt.registration_hint.lines() {
            lines.push(format!("  {hint_line}"));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_fresh_creates_expected_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("skills/cairn");
        let opts = InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        };

        let receipt = install(&opts).expect("fresh install");

        // Generated files must exist.
        assert!(target.join("SKILL.md").exists(), "SKILL.md missing");
        assert!(
            target.join("conventions.md").exists(),
            "conventions.md missing"
        );
        assert!(target.join(".version").exists(), ".version missing");

        // Example stubs must exist.
        assert!(
            target.join("examples/01-remember-preference.md").exists(),
            "example 01 missing"
        );
        assert!(
            target.join("examples/04-skillify-this.md").exists(),
            "example 04 missing"
        );

        // Fresh install: nothing skipped.
        assert!(
            receipt.files_skipped.is_empty(),
            "expected no skips on fresh install"
        );
        assert!(
            !receipt.files_created.is_empty(),
            "expected files to be created"
        );

        // Receipt fields are populated.
        assert_eq!(receipt.contract_version, "cairn.mcp.v1");
        assert!(!receipt.idl_version.is_empty());
        assert!(!receipt.registration_hint.is_empty());

        // All 9 files must be created (SKILL.md, conventions.md, .version, 6 examples).
        assert_eq!(
            receipt.files_created.len(),
            9,
            "expected 9 files on fresh install"
        );

        // Hint for ClaudeCode must mention CLAUDE.md.
        assert!(
            receipt.registration_hint.contains("CLAUDE.md"),
            "hint for ClaudeCode must mention CLAUDE.md"
        );

        // All 4 examples must exist.
        assert!(
            target.join("examples/02-forget-something.md").exists(),
            "example 02 missing"
        );
        assert!(
            target.join("examples/03-search-prior-decision.md").exists(),
            "example 03 missing"
        );
    }

    #[test]
    fn registration_hint_covers_all_harnesses() {
        let dir = std::path::Path::new("/home/user/.cairn/skills/cairn");
        let cases = [
            (Harness::ClaudeCode, "CLAUDE.md"),
            (Harness::Codex, "AGENTS.md"),
            (Harness::Gemini, "GEMINI.md"),
            (Harness::Opencode, "opencode"),
            (Harness::Cursor, ".cursorrules"),
            (Harness::Custom, "manually"),
        ];
        for (harness, expected_fragment) in &cases {
            let hint = registration_hint(harness, dir);
            assert!(
                hint.contains(expected_fragment),
                "hint for {harness:?} should mention '{expected_fragment}' — got: {hint:?}"
            );
        }
    }

    // Task 7: idempotency and version-same skip tests

    #[test]
    fn install_idempotent_same_version_skips_generated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("skills/cairn");
        let opts = InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        };

        // First install.
        install(&opts).expect("first install");

        // Second install — same version, no --force.
        let receipt2 = install(&opts).expect("second install");

        // Generated files should be in files_skipped on the second run.
        let skipped_names: Vec<_> = receipt2
            .files_skipped
            .iter()
            .filter_map(|p| p.file_name())
            .collect();
        assert!(
            skipped_names.contains(&std::ffi::OsStr::new("SKILL.md")),
            "SKILL.md should be skipped on same-version reinstall"
        );
        assert!(
            skipped_names.contains(&std::ffi::OsStr::new("conventions.md")),
            "conventions.md should be skipped"
        );
        assert!(
            skipped_names.contains(&std::ffi::OsStr::new(".version")),
            ".version should be skipped"
        );
    }

    #[test]
    fn install_force_overwrites_generated_even_on_same_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("skills/cairn");

        // First install.
        install(&InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        })
        .expect("first install");

        // Second install with --force.
        let receipt2 = install(&InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: true,
        })
        .expect("second install with force");

        let created_names: Vec<_> = receipt2
            .files_created
            .iter()
            .filter_map(|p| p.file_name())
            .collect();

        assert!(
            created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
            "SKILL.md should be recreated with --force"
        );
        // Examples must still be skipped even with --force.
        let skipped_names: Vec<_> = receipt2
            .files_skipped
            .iter()
            .filter_map(|p| p.file_name())
            .collect();
        assert!(
            skipped_names.contains(&std::ffi::OsStr::new("01-remember-preference.md")),
            "example stubs must not be overwritten even with --force"
        );
    }

    // Task 8: version upgrade and downgrade tests

    #[test]
    fn install_upgrades_when_older_version_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("skills/cairn");
        std::fs::create_dir_all(target.join("examples")).expect("create dir");

        // Write a stale .version file with an older idl version.
        std::fs::write(
            target.join(".version"),
            "contract: cairn.mcp.v1\ncairn-idl: 0.0.0\n",
        )
        .expect("write stale version");

        let opts = InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        };
        let receipt = install(&opts).expect("upgrade install");

        // Generated files must be in created (not skipped) because version differs.
        let created_names: Vec<_> = receipt
            .files_created
            .iter()
            .filter_map(|p| p.file_name())
            .collect();
        assert!(
            created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
            "SKILL.md should be updated on version upgrade"
        );
    }

    #[test]
    fn install_downgrade_proceeds_with_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("skills/cairn");
        std::fs::create_dir_all(target.join("examples")).expect("create dir");

        // Write a .version file with a higher idl version (simulated downgrade).
        std::fs::write(
            target.join(".version"),
            "contract: cairn.mcp.v1\ncairn-idl: 999.0.0\n",
        )
        .expect("write future version");

        let opts = InstallOpts {
            target_dir: target.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        };

        // Downgrade should succeed (not error).
        let result = install(&opts);
        assert!(
            result.is_ok(),
            "downgrade should proceed without error, got: {result:?}"
        );

        // Generated files should be updated (overwritten).
        let receipt = result.unwrap();
        let created_names: Vec<_> = receipt
            .files_created
            .iter()
            .filter_map(|p| p.file_name())
            .collect();
        assert!(
            created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
            "SKILL.md should be overwritten on downgrade"
        );
    }

    #[test]
    fn read_installed_version_accepts_valid_schema() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let vpath = tmp.path().join(".version");
        std::fs::write(&vpath, "contract: cairn.mcp.v1\ncairn-idl: 1.2.3\n").expect("write");
        let v = read_installed_version(&vpath, false).expect("parse");
        assert_eq!(v, Some("1.2.3".to_owned()));
    }

    #[test]
    fn read_installed_version_rejects_malformed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let vpath = tmp.path().join(".version");
        // Missing contract line.
        std::fs::write(&vpath, "cairn-idl: 1.2.3\n").expect("write");
        assert!(read_installed_version(&vpath, false).is_err());
        // Malformed version string.
        std::fs::write(&vpath, "contract: cairn.mcp.v1\ncairn-idl: garbage\n").expect("write");
        assert!(read_installed_version(&vpath, false).is_err());
        // Extra unknown line.
        std::fs::write(
            &vpath,
            "contract: cairn.mcp.v1\ncairn-idl: 1.2.3\nextra: field\n",
        )
        .expect("write");
        assert!(read_installed_version(&vpath, false).is_err());
        // --force overrides malformed content.
        assert!(read_installed_version(&vpath, true).is_ok());
    }

    #[test]
    fn compare_versions_ordering() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.0.1", "0.0.2"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.0.1", "0.0.1"), Ordering::Equal);
    }

    // Task 9: symlink rejection test

    // Task 10: render_human snapshot tests

    #[test]
    fn render_human_snapshot_fresh_install() {
        let receipt = InstallReceipt {
            target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
            contract_version: "cairn.mcp.v1".to_owned(),
            idl_version: "0.0.1".to_owned(),
            files_created: vec![
                PathBuf::from("/home/user/.cairn/skills/cairn/SKILL.md"),
                PathBuf::from("/home/user/.cairn/skills/cairn/conventions.md"),
                PathBuf::from("/home/user/.cairn/skills/cairn/.version"),
                PathBuf::from("/home/user/.cairn/skills/cairn/examples/01-remember-preference.md"),
            ],
            files_skipped: vec![],
            registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
                .to_owned(),
        };
        insta::assert_snapshot!(render_human(&receipt));
    }

    #[test]
    fn render_human_snapshot_already_installed() {
        let receipt = InstallReceipt {
            target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
            contract_version: "cairn.mcp.v1".to_owned(),
            idl_version: "0.0.1".to_owned(),
            files_created: vec![],
            files_skipped: vec![
                PathBuf::from("/home/user/.cairn/skills/cairn/SKILL.md"),
                PathBuf::from("/home/user/.cairn/skills/cairn/conventions.md"),
                PathBuf::from("/home/user/.cairn/skills/cairn/.version"),
            ],
            registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
                .to_owned(),
        };
        insta::assert_snapshot!(render_human(&receipt));
    }

    #[test]
    fn receipt_json_snapshot() {
        let receipt = InstallReceipt {
            target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
            contract_version: "cairn.mcp.v1".to_owned(),
            idl_version: "0.0.1".to_owned(),
            files_created: vec![PathBuf::from("/home/user/.cairn/skills/cairn/SKILL.md")],
            files_skipped: vec![],
            registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
                .to_owned(),
        };
        insta::assert_json_snapshot!(receipt);
    }

    #[test]
    #[cfg(unix)]
    fn install_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let real_dir = tmp.path().join("real");
        std::fs::create_dir_all(&real_dir).expect("create real dir");

        let link = tmp.path().join("link");
        symlink(&real_dir, &link).expect("create symlink");

        let opts = InstallOpts {
            target_dir: link.clone(),
            harness: Harness::ClaudeCode,
            force: false,
        };
        let result = install(&opts);
        assert!(result.is_err(), "install into a symlinked dir must fail");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("symlink"),
            "error message should mention symlink — got: {msg}"
        );
    }
}
