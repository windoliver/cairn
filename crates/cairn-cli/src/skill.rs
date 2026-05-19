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

/// Agents with first-slice generated skill-pack integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Claude Code project integration.
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex/OpenCode project instructions.
    Codex,
    /// Kiro always-included steering file.
    Kiro,
    /// Cursor always-applied rule file.
    Cursor,
}

impl Agent {
    /// First-slice agents in deterministic receipt order.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::Kiro, Self::Cursor];

    /// Compatibility mapping from the older issue #68 harness flag.
    #[must_use]
    pub const fn from_harness(harness: &Harness) -> Option<Self> {
        match harness {
            Harness::ClaudeCode => Some(Self::ClaudeCode),
            Harness::Codex => Some(Self::Codex),
            Harness::Cursor => Some(Self::Cursor),
            Harness::Gemini | Harness::Opencode | Harness::Custom => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex / OpenCode",
            Self::Kiro => "Kiro",
            Self::Cursor => "Cursor",
        }
    }
}

const AGENT_BLOCK_BEGIN: &str = "<!-- BEGIN CAIRN AGENT SKILL -->";
const AGENT_BLOCK_END: &str = "<!-- END CAIRN AGENT SKILL -->";
const SESSION_START_HOOK_COMMAND: &str =
    "cairn ingest --folder . --mode keyword >/tmp/cairn-session-start.log 2>&1 &";

fn render_agent_markdown_block(agent: Agent) -> String {
    format!(
        "{AGENT_BLOCK_BEGIN}\n\
         ## Cairn Memory Layer ({label})\n\n\
         Cairn is the persistent memory and knowledge-graph layer for this project.\n\n\
         - Use Cairn when persistent memory, recall from previous sessions, or graph-aware connections would help.\n\
         - Do not use Cairn tools for ordinary file reads or code execution.\n\
         - Prefer exact `cairn search` / `cairn retrieve` paths when the user names a known record or concept; use graph exploration only for non-obvious connections.\n\
         - At session start, run `cairn ingest --folder . --mode keyword` in the project root.\n\
         - Use `cairn assemble_hot` when you need a hot-memory prefix for the current session.\n\
         - `/remember` maps to `cairn ingest`; `/forget` maps to `cairn forget` after explicit user confirmation.\n\
         {AGENT_BLOCK_END}\n",
        label = agent.label()
    )
}

fn upsert_guarded_markdown(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(AGENT_BLOCK_BEGIN)
        && let Some(end_rel) = existing[start..].find(AGENT_BLOCK_END)
    {
        let end = start + end_rel + AGENT_BLOCK_END.len();
        let mut out = String::new();
        out.push_str(&existing[..start]);
        out.push_str(block.trim_end());
        out.push('\n');
        out.push_str(&existing[end..]);
        return ensure_trailing_newline(out);
    }

    let mut out = ensure_trailing_newline(existing.to_owned());
    if !out.trim().is_empty() {
        out.push('\n');
    }
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
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

/// Options for installing the skill bundle plus generated agent integrations.
#[derive(Debug, Clone)]
pub struct AgentInstallOpts {
    /// Target directory for the shared Cairn skill bundle.
    pub target_dir: PathBuf,
    /// Project directory where harness integration files are written.
    pub project_dir: PathBuf,
    /// Agents to write integrations for.
    pub agents: Vec<Agent>,
    /// Compatibility harness used for the shared bundle registration hint.
    pub harness: Harness,
    /// If `true`, overwrite generated files even if the version matches.
    pub force: bool,
}

/// Result of a skill bundle plus agent integration install.
#[derive(Debug, serde::Serialize)]
pub struct AgentInstallReceipt {
    /// Receipt from the shared skill bundle install.
    pub bundle: InstallReceipt,
    /// Per-agent generated integration writes.
    pub integrations: Vec<AgentIntegrationReceipt>,
}

/// Files written for one generated agent integration.
#[derive(Debug, serde::Serialize)]
pub struct AgentIntegrationReceipt {
    /// Agent integration that was installed.
    pub agent: Agent,
    /// New files created during the integration write.
    pub files_created: Vec<PathBuf>,
    /// Existing files updated during the integration write.
    pub files_updated: Vec<PathBuf>,
    /// Files left unchanged because generated content already matched.
    pub files_skipped: Vec<PathBuf>,
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
        target,
        &target.join("SKILL.md"),
        SKILL_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::bootstrap::write_once(
        target,
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
            target,
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
        target,
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

/// Install the Cairn skill bundle and generated agent integration files.
///
/// # Errors
/// Returns an error if the shared bundle install fails, an integration file
/// cannot be read or written, or a JSON integration file is malformed.
pub fn install_agent_pack(opts: &AgentInstallOpts) -> Result<AgentInstallReceipt> {
    let bundle = install(&InstallOpts {
        target_dir: opts.target_dir.clone(),
        harness: opts.harness.clone(),
        force: opts.force,
    })?;
    let project_dir = if opts.project_dir.is_absolute() {
        opts.project_dir.components().collect::<PathBuf>()
    } else {
        std::env::current_dir()
            .context("resolving current directory for project path")?
            .join(&opts.project_dir)
            .components()
            .collect::<PathBuf>()
    };

    let mut integrations = Vec::new();
    for agent in &opts.agents {
        integrations.push(match agent {
            Agent::ClaudeCode => install_claude_code_integration(&project_dir, opts.force)?,
            Agent::Codex => install_codex_integration(&project_dir)?,
            Agent::Kiro => install_kiro_integration(&project_dir, opts.force)?,
            Agent::Cursor => install_cursor_integration(&project_dir, opts.force)?,
        });
    }

    Ok(AgentInstallReceipt {
        bundle,
        integrations,
    })
}

fn install_claude_code_integration(
    project_dir: &std::path::Path,
    force: bool,
) -> Result<AgentIntegrationReceipt> {
    let mut receipt = AgentIntegrationReceipt::new(Agent::ClaudeCode);
    let settings_path = project_dir.join(".claude/settings.json");
    let existing = read_json_or_empty(&settings_path)?;
    let merged = merge_claude_settings(existing)?;
    let settings = format!(
        "{}\n",
        serde_json::to_string_pretty(&merged).context("serializing Claude Code settings JSON")?
    );
    write_if_changed(&settings_path, &settings, force, &mut receipt)?;

    let project_mcp_path = project_dir.join(".mcp.json");
    let existing_mcp = read_json_or_empty(&project_mcp_path)?;
    let merged_mcp = merge_project_mcp_json(existing_mcp)?;
    let project_mcp = format!(
        "{}\n",
        serde_json::to_string_pretty(&merged_mcp).context("serializing project MCP JSON")?
    );
    write_if_changed(&project_mcp_path, &project_mcp, force, &mut receipt)?;

    write_guarded_markdown(
        &project_dir.join("CLAUDE.md"),
        &render_agent_markdown_block(Agent::ClaudeCode),
        &mut receipt,
    )?;
    for (name, content) in claude_slash_commands() {
        write_generated_guarded_file(
            &project_dir.join(".claude/commands").join(name),
            content,
            force,
            &mut receipt,
        )?;
    }
    Ok(receipt)
}

fn install_codex_integration(project_dir: &std::path::Path) -> Result<AgentIntegrationReceipt> {
    let mut receipt = AgentIntegrationReceipt::new(Agent::Codex);
    write_guarded_markdown(
        &project_dir.join("AGENTS.md"),
        &render_agent_markdown_block(Agent::Codex),
        &mut receipt,
    )?;
    Ok(receipt)
}

fn install_kiro_integration(
    project_dir: &std::path::Path,
    force: bool,
) -> Result<AgentIntegrationReceipt> {
    let mut receipt = AgentIntegrationReceipt::new(Agent::Kiro);
    let content = format!(
        "---\ninclusion: always\n---\n\n{}",
        render_agent_markdown_block(Agent::Kiro)
    );
    write_generated_guarded_file(
        &project_dir.join(".kiro/steering/cairn.md"),
        &content,
        force,
        &mut receipt,
    )?;
    Ok(receipt)
}

fn install_cursor_integration(
    project_dir: &std::path::Path,
    force: bool,
) -> Result<AgentIntegrationReceipt> {
    let mut receipt = AgentIntegrationReceipt::new(Agent::Cursor);
    let content = format!(
        "---\nalwaysApply: true\n---\n\n{}",
        render_agent_markdown_block(Agent::Cursor)
    );
    write_generated_guarded_file(
        &project_dir.join(".cursor/rules/cairn.mdc"),
        &content,
        force,
        &mut receipt,
    )?;
    write_guarded_markdown(
        &project_dir.join(".cursorrules"),
        &render_agent_markdown_block(Agent::Cursor),
        &mut receipt,
    )?;
    Ok(receipt)
}

fn claude_slash_commands() -> [(&'static str, &'static str); 4] {
    [
        ("remember.md", CLAUDE_REMEMBER_COMMAND),
        ("forget.md", CLAUDE_FORGET_COMMAND),
        ("recall.md", CLAUDE_RECALL_COMMAND),
        ("graph.md", CLAUDE_GRAPH_COMMAND),
    ]
}

const CLAUDE_REMEMBER_COMMAND: &str = r#"---
description: Remember durable project context in Cairn
argument-hint: <memory>
---

<!-- BEGIN CAIRN AGENT SKILL -->
Store the user's provided memory in Cairn.

Run:
`cairn ingest --kind user --body "$ARGUMENTS"`
<!-- END CAIRN AGENT SKILL -->
"#;

const CLAUDE_FORGET_COMMAND: &str = r#"---
description: Forget a Cairn record after explicit confirmation
argument-hint: <record-id>
---

<!-- BEGIN CAIRN AGENT SKILL -->
Confirm the user wants to forget the named Cairn record, then run:

`cairn forget --record "$ARGUMENTS"`
<!-- END CAIRN AGENT SKILL -->
"#;

const CLAUDE_RECALL_COMMAND: &str = r#"---
description: Search and retrieve relevant Cairn memory
argument-hint: <query>
---

<!-- BEGIN CAIRN AGENT SKILL -->
Search Cairn for relevant memory, then retrieve exact records when needed.

Start with:
`cairn search --mode keyword "$ARGUMENTS"`

Then run:
`cairn retrieve <record-id>`
<!-- END CAIRN AGENT SKILL -->
"#;

const CLAUDE_GRAPH_COMMAND: &str = r#"---
description: Explore non-obvious Cairn graph connections
argument-hint: <entity-ids>
---

<!-- BEGIN CAIRN AGENT SKILL -->
Use the MCP graph tools for non-obvious connections between known entity ids.

Prefer `graph.surprising_connections` when comparing multiple entities.
Do not use graph tools for ordinary file reads or code execution.
<!-- END CAIRN AGENT SKILL -->
"#;

impl AgentIntegrationReceipt {
    fn new(agent: Agent) -> Self {
        Self {
            agent,
            files_created: Vec::new(),
            files_updated: Vec::new(),
            files_skipped: Vec::new(),
        }
    }
}

fn read_json_or_empty(path: &std::path::Path) -> Result<serde_json::Value> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("parsing JSON from {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    }
}

fn merge_claude_settings(mut value: serde_json::Value) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude Code settings root must be a JSON object"))?;
    insert_cairn_mcp_server(root)?;

    let hooks = root.remove("hooks");
    root.insert("hooks".to_owned(), merged_claude_hooks(hooks));
    Ok(value)
}

fn merge_project_mcp_json(mut value: serde_json::Value) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("project .mcp.json root must be a JSON object"))?;
    insert_cairn_mcp_server(root)?;
    Ok(value)
}

fn insert_cairn_mcp_server(root: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mcpServers must be a JSON object"))?;
    servers.insert(
        "cairn".to_owned(),
        serde_json::json!({"command": "cairn", "args": ["mcp"]}),
    );
    Ok(())
}

fn merged_claude_hooks(existing: Option<serde_json::Value>) -> serde_json::Value {
    let mut hooks = existing
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    hooks.insert(
        "SessionStart".to_owned(),
        serde_json::json!([
            {
                "command": SESSION_START_HOOK_COMMAND
            }
        ]),
    );
    serde_json::Value::Object(hooks)
}

fn write_guarded_markdown(
    path: &std::path::Path,
    block: &str,
    receipt: &mut AgentIntegrationReceipt,
) -> Result<()> {
    let existing = read_optional_string(path)?;
    let updated = upsert_guarded_markdown(existing.as_deref().unwrap_or(""), block);
    write_if_changed(path, &updated, false, receipt)
}

fn write_generated_guarded_file(
    path: &std::path::Path,
    content: &str,
    force: bool,
    receipt: &mut AgentIntegrationReceipt,
) -> Result<()> {
    if !force
        && let Some(existing) = read_optional_string(path)?
        && !existing.contains(AGENT_BLOCK_BEGIN)
        && existing.trim() != content.trim()
    {
        anyhow::bail!(
            "{} exists without a Cairn guard; pass --force to overwrite it",
            path.display()
        );
    }
    write_if_changed(path, content, force, receipt)
}

fn write_if_changed(
    path: &std::path::Path,
    content: &str,
    force: bool,
    receipt: &mut AgentIntegrationReceipt,
) -> Result<()> {
    let existing = read_optional_string(path)?;
    if !force && existing.as_deref() == Some(content) {
        receipt.files_skipped.push(path.to_path_buf());
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    if existing.is_some() {
        receipt.files_updated.push(path.to_path_buf());
    } else {
        receipt.files_created.push(path.to_path_buf());
    }
    Ok(())
}

fn read_optional_string(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    }
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

/// Renders a human-readable summary of an agent-pack install receipt.
#[must_use]
pub fn render_agent_human(receipt: &AgentInstallReceipt) -> String {
    let mut lines = vec![render_human(&receipt.bundle)];
    for integration in &receipt.integrations {
        lines.push(String::new());
        lines.push(format!("{} integration:", integration.agent.label()));
        for path in &integration.files_created {
            lines.push(format!("  {}  [created]", path.display()));
        }
        for path in &integration.files_updated {
            lines.push(format!("  {}  [updated]", path.display()));
        }
        for path in &integration.files_skipped {
            lines.push(format!("  {}  [skipped]", path.display()));
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

    #[test]
    fn agent_values_render_expected_fragments() {
        assert_eq!(
            Agent::ClaudeCode
                .to_possible_value()
                .expect("possible value")
                .get_name(),
            "claude-code"
        );
        assert_eq!(
            Agent::Codex
                .to_possible_value()
                .expect("possible value")
                .get_name(),
            "codex"
        );
        assert_eq!(
            Agent::Kiro
                .to_possible_value()
                .expect("possible value")
                .get_name(),
            "kiro"
        );
        assert_eq!(
            Agent::Cursor
                .to_possible_value()
                .expect("possible value")
                .get_name(),
            "cursor"
        );

        let block = render_agent_markdown_block(Agent::Codex);
        assert!(block.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
        assert!(block.contains("cairn ingest --folder . --mode keyword"));
        assert!(
            block.contains("Do not use Cairn tools for ordinary file reads or code execution.")
        );
        assert!(block.contains("/remember"));
        assert!(block.contains("/forget"));
    }

    #[test]
    fn harness_maps_to_first_slice_agent_when_supported() {
        assert_eq!(
            Agent::from_harness(&Harness::ClaudeCode),
            Some(Agent::ClaudeCode)
        );
        assert_eq!(Agent::from_harness(&Harness::Codex), Some(Agent::Codex));
        assert_eq!(Agent::from_harness(&Harness::Cursor), Some(Agent::Cursor));
        assert_eq!(Agent::from_harness(&Harness::Gemini), None);
        assert_eq!(Agent::from_harness(&Harness::Opencode), None);
        assert_eq!(Agent::from_harness(&Harness::Custom), None);
    }

    #[test]
    fn guarded_markdown_appends_to_user_content() {
        let original = "# Existing\n\nKeep me.\n";
        let block = render_agent_markdown_block(Agent::Codex);
        let updated = upsert_guarded_markdown(original, &block);

        assert!(updated.starts_with(original));
        assert!(updated.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
        assert!(updated.ends_with('\n'));
    }

    #[test]
    fn guarded_markdown_replaces_existing_block_once() {
        let old = "# Existing\n\n<!-- BEGIN CAIRN AGENT SKILL -->\nold\n<!-- END CAIRN AGENT SKILL -->\n\nTail\n";
        let block = render_agent_markdown_block(Agent::Codex);
        let updated = upsert_guarded_markdown(old, &block);

        assert!(updated.contains("# Existing"));
        assert!(updated.contains("Tail"));
        assert!(!updated.contains("\nold\n"));
        assert_eq!(
            updated.matches("<!-- BEGIN CAIRN AGENT SKILL -->").count(),
            1
        );
    }

    #[test]
    fn install_agent_pack_codex_writes_agents_md_and_bundle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::write(project.join("AGENTS.md"), "# Project\n").expect("seed agents");
        let target = tmp.path().join("skills/cairn");

        let receipt = install_agent_pack(&AgentInstallOpts {
            target_dir: target.clone(),
            project_dir: project.clone(),
            agents: vec![Agent::Codex],
            harness: Harness::Codex,
            force: false,
        })
        .expect("install codex agent pack");

        assert!(target.join("SKILL.md").exists());
        let agents = std::fs::read_to_string(project.join("AGENTS.md")).expect("read agents");
        assert!(agents.contains("# Project"));
        assert!(agents.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
        assert_eq!(receipt.integrations.len(), 1);
        assert_eq!(receipt.integrations[0].agent, Agent::Codex);
    }

    #[test]
    fn install_agent_pack_all_writes_first_slice_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let target = tmp.path().join("skills/cairn");

        let receipt = install_agent_pack(&AgentInstallOpts {
            target_dir: target,
            project_dir: project.clone(),
            agents: Agent::ALL.to_vec(),
            harness: Harness::ClaudeCode,
            force: false,
        })
        .expect("install all agent packs");

        assert!(project.join(".claude/settings.json").exists());
        assert!(project.join("CLAUDE.md").exists());
        assert!(project.join("AGENTS.md").exists());
        assert!(project.join(".kiro/steering/cairn.md").exists());
        assert!(project.join(".cursor/rules/cairn.mdc").exists());
        assert_eq!(receipt.integrations.len(), 4);
    }

    #[test]
    fn install_agent_pack_claude_writes_slash_commands_and_background_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let target = tmp.path().join("skills/cairn");

        install_agent_pack(&AgentInstallOpts {
            target_dir: target,
            project_dir: project.clone(),
            agents: vec![Agent::ClaudeCode],
            harness: Harness::ClaudeCode,
            force: false,
        })
        .expect("install Claude Code agent pack");

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude/settings.json")).expect("settings"),
        )
        .expect("settings json");
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).expect("mcp"))
                .expect("mcp json");
        assert_eq!(mcp["mcpServers"]["cairn"]["command"], "cairn");
        assert_eq!(
            mcp["mcpServers"]["cairn"]["args"],
            serde_json::json!(["mcp"])
        );

        let command = settings["hooks"]["SessionStart"][0]["command"]
            .as_str()
            .expect("SessionStart command");
        assert!(command.contains("cairn ingest --folder . --mode keyword"));
        assert!(
            command.trim_end().ends_with('&'),
            "session-start hook should run in the background: {command}"
        );

        let commands = project.join(".claude/commands");
        let remember =
            std::fs::read_to_string(commands.join("remember.md")).expect("remember command");
        let forget = std::fs::read_to_string(commands.join("forget.md")).expect("forget command");
        let recall = std::fs::read_to_string(commands.join("recall.md")).expect("recall command");
        let graph = std::fs::read_to_string(commands.join("graph.md")).expect("graph command");
        assert!(remember.contains("cairn ingest"));
        assert!(forget.contains("cairn forget"));
        assert!(recall.contains("cairn retrieve"));
        assert!(graph.contains("graph.surprising_connections"));
    }

    #[test]
    fn install_agent_pack_cursor_writes_modern_and_legacy_rules() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        let target = tmp.path().join("skills/cairn");

        install_agent_pack(&AgentInstallOpts {
            target_dir: target,
            project_dir: project.clone(),
            agents: vec![Agent::Cursor],
            harness: Harness::Cursor,
            force: false,
        })
        .expect("install Cursor agent pack");

        assert!(project.join(".cursor/rules/cairn.mdc").exists());
        let cursorrules =
            std::fs::read_to_string(project.join(".cursorrules")).expect(".cursorrules");
        assert!(cursorrules.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
        assert!(cursorrules.contains("cairn ingest --folder . --mode keyword"));
    }

    #[test]
    fn claude_settings_merge_preserves_unrelated_keys() {
        let existing = serde_json::json!({
            "theme": "dark",
            "mcpServers": {
                "other": {"command": "other", "args": []}
            }
        });
        let merged = merge_claude_settings(existing).expect("merge settings");

        assert_eq!(merged["theme"], "dark");
        assert_eq!(merged["mcpServers"]["other"]["command"], "other");
        assert_eq!(merged["mcpServers"]["cairn"]["command"], "cairn");
        assert_eq!(
            merged["mcpServers"]["cairn"]["args"],
            serde_json::json!(["mcp"])
        );
        let rendered = serde_json::to_string(&merged).expect("json");
        assert!(rendered.contains("cairn ingest --folder . --mode keyword"));
    }

    #[test]
    fn agent_markdown_block_snapshot() {
        insta::assert_snapshot!(render_agent_markdown_block(Agent::Codex));
    }

    #[test]
    fn claude_settings_snapshot() {
        let merged = merge_claude_settings(serde_json::json!({})).expect("merge");
        insta::assert_json_snapshot!(merged);
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
