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

/// Returns the harness-specific registration hint printed after install.
#[must_use]
pub fn registration_hint(harness: &Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Codex => "# Add to your AGENTS.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Gemini => "# Add to your GEMINI.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Opencode => {
            "# Add to your opencode config skills path:\n~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Cursor => "# Add to your .cursorrules:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Custom => "# Skill bundle written. Register it with your harness manually.",
    }
}

// Embedded at compile time from the committed generated artifacts.
// The CI `--check` gate catches drift between these and what cairn-codegen emits.
const SKILL_MD: &str = include_str!("../../../skills/cairn/SKILL.md");
const CONVENTIONS_MD: &str = include_str!("../../../skills/cairn/conventions.md");
const VERSION_FILE: &str = include_str!("../../../skills/cairn/.version");

// Static example stubs — written once on install; user may edit after.
const EXAMPLE_01: &str = include_str!("../../../skills/cairn/examples/01-remember-preference.md");
const EXAMPLE_02: &str = include_str!("../../../skills/cairn/examples/02-forget-something.md");
const EXAMPLE_03: &str = include_str!("../../../skills/cairn/examples/03-search-prior-decision.md");
const EXAMPLE_04: &str = include_str!("../../../skills/cairn/examples/04-skillify-this.md");

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

/// Parses the `cairn-idl: X.Y.Z` line from a `.version` file.
fn parse_idl_version(version_file: &str) -> Option<String> {
    for line in version_file.lines() {
        if let Some(ver) = line.strip_prefix("cairn-idl: ") {
            return Some(ver.trim().to_owned());
        }
    }
    None
}

/// Compares two `X.Y.Z` version strings. Returns `Less` if `a < b`.
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

/// Installs the Cairn skill bundle at `opts.target_dir`.
///
/// Idempotent and version-aware. See the spec (§8.0.a-bis, §18.d) for the
/// full decision tree.
///
/// # Errors
/// Returns an error if a directory cannot be created or a file cannot be
/// written. Symlinked paths are rejected.
pub fn install(opts: &InstallOpts) -> Result<InstallReceipt> {
    let target = opts.target_dir.components().collect::<PathBuf>();
    let target = &target;
    // All workspace crates share a single version (see [workspace.package] in Cargo.toml),
    // so cairn-cli's CARGO_PKG_VERSION matches the cairn-idl version embedded in .version.
    let current_idl_version = env!("CARGO_PKG_VERSION");

    // Reject symlinked target root.
    if let Ok(meta) = std::fs::symlink_metadata(target)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!(
            "{} is a symlink — cairn will not write through it",
            target.display()
        );
    }

    // Create target dir and examples/ subdir.
    std::fs::create_dir_all(target.join("examples"))
        .with_context(|| format!("creating {}", target.join("examples").display()))?;

    // Version check.
    let installed_version = std::fs::read_to_string(target.join(".version"))
        .ok()
        .and_then(|s| parse_idl_version(&s));

    let skip_generated = match &installed_version {
        Some(installed) if installed == current_idl_version && !opts.force => true,
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
    crate::vault::write_once(
        &target.join("SKILL.md"),
        SKILL_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::write_once(
        &target.join("conventions.md"),
        CONVENTIONS_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::write_once(
        &target.join(".version"),
        VERSION_FILE,
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
    ] {
        crate::vault::write_once(
            &examples_dir.join(name),
            content,
            false, // never force-overwrite examples
            &mut files_created,
            &mut files_skipped,
        )?;
    }

    let hint = registration_hint(&opts.harness).to_owned();

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

        // All 7 files must be created (SKILL.md, conventions.md, .version, 4 examples).
        assert_eq!(
            receipt.files_created.len(),
            7,
            "expected 7 files on fresh install"
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
        let cases = [
            (Harness::ClaudeCode, "CLAUDE.md"),
            (Harness::Codex, "AGENTS.md"),
            (Harness::Gemini, "GEMINI.md"),
            (Harness::Opencode, "opencode"),
            (Harness::Cursor, ".cursorrules"),
            (Harness::Custom, "manually"),
        ];
        for (harness, expected_fragment) in &cases {
            let hint = registration_hint(harness);
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
    fn parse_idl_version_extracts_version() {
        let version_file = "contract: cairn.mcp.v1\ncairn-idl: 1.2.3\n";
        assert_eq!(parse_idl_version(version_file), Some("1.2.3".to_owned()));
    }

    #[test]
    fn compare_versions_ordering() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.0.1", "0.0.2"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.0.1", "0.0.1"), Ordering::Equal);
    }

    // Task 9: symlink rejection test

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
