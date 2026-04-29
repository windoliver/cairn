//! Vault initialization for `cairn bootstrap` (brief §3, §3.1).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use cairn_core::config::CairnConfig;

/// Options for [`bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapOpts {
    /// The root directory for the vault.
    pub vault_path: PathBuf,
    /// If `true`, overwrite existing placeholder files.
    pub force: bool,
}

/// Result of a bootstrap run, emitted as JSON with `--json` or formatted by
/// [`render_human`].
#[derive(Debug, Serialize)]
pub struct BootstrapReceipt {
    /// The root vault directory.
    pub vault_path: PathBuf,
    /// Path to the generated config file.
    pub config_path: PathBuf,
    /// Path where the `SQLite` database will be created on first ingest.
    pub db_path: PathBuf,
    /// Directories that were newly created.
    pub dirs_created: Vec<PathBuf>,
    /// Directories that already existed.
    pub dirs_existing: Vec<PathBuf>,
    /// Placeholder files that were newly written.
    pub files_created: Vec<PathBuf>,
    /// Placeholder files that were skipped because they already existed.
    pub files_skipped: Vec<PathBuf>,
}

/// All directories the vault layout requires (brief §3.1). Listed in creation
/// order so parents always precede children.
const VAULT_DIRS: &[&str] = &[
    "sources",
    "sources/articles",
    "sources/papers",
    "sources/transcripts",
    "sources/documents",
    "sources/chat",
    "sources/assets",
    "raw",
    "wiki",
    "wiki/entities",
    "wiki/concepts",
    "wiki/summaries",
    "wiki/synthesis",
    "wiki/prompts",
    "skills",
    ".cairn",
    ".cairn/evolution",
    ".cairn/cache",
    ".cairn/models",
];

const PURPOSE_MD: &str = "# Purpose\n\n<!-- Why does this vault exist? -->\n";

/// Initialize the vault directory tree and placeholder files at `opts.vault_path`.
///
/// Idempotent: existing directories are left unchanged; existing files are
/// added to [`BootstrapReceipt::files_skipped`] unless `opts.force` is set.
///
/// # Errors
/// Returns an error if any directory cannot be created or any file cannot be
/// written.
pub fn bootstrap(opts: &BootstrapOpts) -> Result<BootstrapReceipt> {
    let vault = &opts.vault_path;

    // Reject a symlinked vault root — all subsequent paths are derived from it,
    // so a symlink here can route every write outside the intended directory.
    // Canonicalize the component count to strip trailing separators and `.`
    // segments; `symlink_metadata("link/")` follows the link on POSIX, so
    // we must operate on the clean form.
    let vault = vault.components().collect::<PathBuf>();
    let vault = &vault;
    if let Ok(meta) = std::fs::symlink_metadata(vault)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!(
            "{} is a symlink — pass the real vault path to bootstrap",
            vault.display()
        );
    }

    let config_path = vault.join(".cairn/config.yaml");
    let db_path = vault.join(".cairn/cairn.db");

    let mut receipt = BootstrapReceipt {
        vault_path: vault.clone(),
        config_path: config_path.clone(),
        db_path,
        dirs_created: Vec::new(),
        dirs_existing: Vec::new(),
        files_created: Vec::new(),
        files_skipped: Vec::new(),
    };

    // --- directory tree ---
    for rel in VAULT_DIRS {
        let dir = vault.join(rel);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                anyhow::bail!(
                    "{} is a symlink — bootstrap will not traverse it",
                    dir.display()
                );
            }
            Ok(_) => receipt.dirs_existing.push(dir.clone()),
            Err(_) => receipt.dirs_created.push(dir.clone()),
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // Post-creation check: a symlink swap between the lstat above and
        // create_dir_all would make the created path land in the link target.
        // Re-lstat immediately after creation to catch persistent symlinks.
        let post = std::fs::symlink_metadata(&dir)
            .with_context(|| format!("verifying {} after creation", dir.display()))?;
        if post.file_type().is_symlink() {
            anyhow::bail!(
                "{} became a symlink during creation — possible race attack",
                dir.display()
            );
        }
    }

    // --- placeholder files ---
    let config_yaml = serde_yaml::to_string(&CairnConfig::default())
        .context("serializing default config to YAML")?;
    write_once(
        vault,
        &config_path,
        &config_yaml,
        opts.force,
        &mut receipt.files_created,
        &mut receipt.files_skipped,
    )?;
    write_once(
        vault,
        &vault.join("purpose.md"),
        PURPOSE_MD,
        opts.force,
        &mut receipt.files_created,
        &mut receipt.files_skipped,
    )?;
    write_once(
        vault,
        &vault.join("index.md"),
        "",
        opts.force,
        &mut receipt.files_created,
        &mut receipt.files_skipped,
    )?;
    write_once(
        vault,
        &vault.join("log.md"),
        "",
        opts.force,
        &mut receipt.files_created,
        &mut receipt.files_skipped,
    )?;

    Ok(receipt)
}

/// Render a human-readable summary of a bootstrap receipt.
#[must_use]
pub fn render_human(receipt: &BootstrapReceipt) -> String {
    let header = if receipt.dirs_created.is_empty() && receipt.files_created.is_empty() {
        format!(
            "cairn bootstrap: vault already initialized at {}",
            receipt.vault_path.display()
        )
    } else {
        format!(
            "cairn bootstrap: vault initialized at {}",
            receipt.vault_path.display()
        )
    };
    let config_status = if receipt.files_skipped.contains(&receipt.config_path) {
        "existing"
    } else {
        "created"
    };
    format!(
        "{header}\n  config    {}  [{config_status}]\n  db        {}  (created on first ingest)\n  dirs      {} created, {} existing\n  files     {} created, {} skipped",
        receipt.config_path.display(),
        receipt.db_path.display(),
        receipt.dirs_created.len(),
        receipt.dirs_existing.len(),
        receipt.files_created.len(),
        receipt.files_skipped.len(),
    )
}

/// No-follow validation for a write target. Rejects symlinked parents,
/// symlinked / non-regular destinations, AND any symlinked ancestor between
/// `vault_root` (exclusive) and the destination's parent (inclusive).
///
/// Why every ancestor: `symlink_metadata` only refuses to follow the
/// **final** path component; intermediate components are still resolved
/// through symlinks. So `lstat("vault/raw/a/_index.md".parent())` returns
/// metadata for the leaf `a` even if `raw` itself is a symlink that
/// silently redirects every nested write outside the vault. Walking each
/// segment from the vault root closes that gap.
///
/// Callers that want to read the file before deciding whether to write
/// (e.g. `lint --fix-folders` skipping unchanged indexes) MUST run this
/// first; otherwise a symlinked `_index.md` would be read through to its
/// target before the no-follow guards in [`write_once`] get a chance to
/// fire.
pub(crate) fn check_write_safe(vault_root: &std::path::Path, path: &std::path::Path) -> Result<()> {
    // Walk every existing ancestor between vault_root (exclusive) and the
    // destination's parent (inclusive).  `vault_root` itself is the user's
    // working directory and is intentionally not validated — outside our
    // trust boundary — but everything beneath it must be a real directory.
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    if let Ok(rel) = parent.strip_prefix(vault_root) {
        let mut cur = vault_root.to_path_buf();
        for comp in rel.components() {
            cur.push(comp);
            if let Ok(meta) = std::fs::symlink_metadata(&cur)
                && meta.file_type().is_symlink()
            {
                anyhow::bail!(
                    "ancestor {} is a symlink — cairn will not write through it",
                    cur.display()
                );
            }
        }
    } else {
        // `path` is not under `vault_root` — fall back to the immediate
        // parent check.  This preserves prior behavior for callers that
        // pass a working-directory-relative path.
        if let Ok(meta) = std::fs::symlink_metadata(parent)
            && meta.file_type().is_symlink()
        {
            anyhow::bail!(
                "parent directory {} is a symlink — cairn will not write through it",
                parent.display()
            );
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let ft = meta.file_type();
        if ft.is_symlink() {
            anyhow::bail!(
                "{} is a symlink — cairn will not write through it",
                path.display()
            );
        }
        if !ft.is_file() {
            anyhow::bail!(
                "{} exists but is not a regular file — cairn cannot overwrite it",
                path.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn write_once(
    vault_root: &std::path::Path,
    path: &std::path::Path,
    content: &str,
    force: bool,
    created: &mut Vec<PathBuf>,
    skipped: &mut Vec<PathBuf>,
) -> Result<()> {
    use std::io::Write as _;

    // Validate every ancestor + target with no-follow lstat.  This rejects
    // a symlink-swapped parent (e.g. `.cairn`) AND a symlink anywhere
    // between `vault_root` and the target, before any read or write
    // touches them.
    check_write_safe(vault_root, path)?;

    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        // Path exists and `check_write_safe` confirmed it is a regular file.
        let _ = meta;
        if !force {
            skipped.push(path.to_owned());
            return Ok(());
        }
        // force=true, regular file — fall through to atomic overwrite
    }

    // Write to a randomly-named temp file in the same directory.
    // A random name eliminates the predictable-temp-path symlink attack.
    // Both the force and non-force paths use this temp file; only the final
    // publish step differs.
    let mut tmp = tempfile::Builder::new()
        .prefix(".bootstrap")
        .tempfile_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    tmp.write_all(content.as_bytes())
        .with_context(|| format!("writing temp file for {}", path.display()))?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("syncing temp file for {}", path.display()))?;

    if force {
        // Atomic overwrite: persist renames the temp file over the target.
        // rename(2) is atomic on the same filesystem — no partial-write window.
        tmp.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("persisting to {}", path.display()))?;
    } else {
        // Atomic exclusive create: persist_noclobber fails if the target
        // appeared between the symlink_metadata check above and now, so a
        // partial write can never be left at the final path.
        match tmp.persist_noclobber(path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Re-check: a race may have placed a symlink or non-regular
                // file at the path between our initial check and now.
                // Only skip if symlink_metadata confirms a regular file;
                // propagate all other outcomes (errors, NotFound, non-regular).
                match std::fs::symlink_metadata(path) {
                    Ok(m) if m.file_type().is_symlink() => anyhow::bail!(
                        "{} is a symlink — cairn will not write through it",
                        path.display()
                    ),
                    Ok(m) if !m.file_type().is_file() => anyhow::bail!(
                        "{} exists but is not a regular file — cairn cannot overwrite it",
                        path.display()
                    ),
                    Ok(_) => {
                        // is_file() is the only remaining case after the arms above.
                        skipped.push(path.to_owned());
                        return Ok(());
                    }
                    Err(re) => {
                        return Err(anyhow::Error::from(re)).with_context(|| {
                            format!("revalidating {} after AlreadyExists race", path.display())
                        });
                    }
                }
            }
            Err(e) => {
                return Err(anyhow::Error::from(e.error))
                    .with_context(|| format!("persisting to {}", path.display()));
            }
        }
    }

    created.push(path.to_owned());
    Ok(())
}
