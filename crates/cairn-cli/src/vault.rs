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
    if let Ok(meta) = std::fs::symlink_metadata(vault) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "{} is a symlink — pass the real vault path to bootstrap",
                vault.display()
            );
        }
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
    write_once(&config_path, &config_yaml, opts.force, &mut receipt)?;
    write_once(
        &vault.join("purpose.md"),
        PURPOSE_MD,
        opts.force,
        &mut receipt,
    )?;
    write_once(&vault.join("index.md"), "", opts.force, &mut receipt)?;
    write_once(&vault.join("log.md"), "", opts.force, &mut receipt)?;

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

fn write_once(
    path: &std::path::Path,
    content: &str,
    force: bool,
    receipt: &mut BootstrapReceipt,
) -> Result<()> {
    use std::io::Write as _;

    // Inspect the final target without following symlinks.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let ft = meta.file_type();
        if ft.is_symlink() {
            anyhow::bail!(
                "{} is a symlink — bootstrap will not write through it",
                path.display()
            );
        }
        if !ft.is_file() {
            // A directory or special file at a placeholder path means the
            // vault is in an inconsistent state; bootstrap cannot repair it.
            anyhow::bail!(
                "{} exists but is not a regular file — bootstrap cannot overwrite it",
                path.display()
            );
        }
        if !force {
            receipt.files_skipped.push(path.to_owned());
            return Ok(());
        }
        // force=true, regular file — fall through to atomic overwrite
    }

    // Write to a randomly-named temp file in the same directory.
    // A random name eliminates the predictable-temp-path symlink attack.
    // Both the force and non-force paths use this temp file; only the final
    // publish step differs.
    //
    // Re-validate the parent directory immediately before opening the temp
    // file: a symlink swap of the parent after the directory-tree pass
    // would otherwise let the write escape to the symlink target.
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "parent directory {} is a symlink — bootstrap will not write through it",
                dir.display()
            );
        }
    }
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
                        "{} is a symlink — bootstrap will not write through it",
                        path.display()
                    ),
                    Ok(m) if !m.file_type().is_file() => anyhow::bail!(
                        "{} exists but is not a regular file — bootstrap cannot overwrite it",
                        path.display()
                    ),
                    Ok(_) => {
                        // is_file() is the only remaining case after the arms above.
                        receipt.files_skipped.push(path.to_owned());
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

    receipt.files_created.push(path.to_owned());
    Ok(())
}
