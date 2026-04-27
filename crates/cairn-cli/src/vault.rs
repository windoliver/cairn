//! Vault initialization for `cairn bootstrap` (brief §3, §3.1).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use cairn_core::config::CairnConfig;

/// Options for [`bootstrap`].
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
        if dir.exists() {
            receipt.dirs_existing.push(dir.clone());
        } else {
            receipt.dirs_created.push(dir.clone());
        }
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }

    // --- placeholder files ---
    let config_yaml = serde_yaml::to_string(&CairnConfig::default())
        .context("serializing default config to YAML")?;
    write_once(&config_path, &config_yaml, opts.force, &mut receipt)?;
    write_once(&vault.join("purpose.md"), PURPOSE_MD, opts.force, &mut receipt)?;
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
    if path.exists() && !force {
        receipt.files_skipped.push(path.to_owned());
        return Ok(());
    }
    std::fs::write(path, content)
        .with_context(|| format!("writing {}", path.display()))?;
    receipt.files_created.push(path.to_owned());
    Ok(())
}
