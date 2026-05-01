//! Vault initialization for `cairn bootstrap` (brief §3, §3.1).
//!
//! # Vault-id lifecycle (bootstrap amendment 2026-04-27)
//!
//! * **First run** — `.cairn/vault.id` absent and no `vault_meta` DB row and
//!   no binding sentinel → mint a fresh [`VaultId`], write it to disk after
//!   the directory tree is created.
//! * **Subsequent runs** — `.cairn/vault.id` present → read it, leave it
//!   unchanged (`--force` does **not** overwrite `vault.id`).
//! * **Lost vault.id** — file absent *but* a `vault_meta` DB row *or* a
//!   binding sentinel exists → the vault has already been bound; continuing
//!   would mint a mismatched id. Return `EX_DATAERR` (65) and ask the user
//!   to run `cairn identity vault-id-recover` first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use cairn_core::config::CairnConfig;
use cairn_core::domain::identity::keys::VaultId;

/// Options for [`bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapOpts {
    /// The root directory for the vault.
    pub vault_path: PathBuf,
    /// If `true`, overwrite existing placeholder files.
    ///
    /// Note: `--force` does **not** rewrite `.cairn/vault.id`; that file is
    /// always preserved when it already exists (amendment 2026-04-27).
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
    /// The vault's stable identity token (ULID), minted on first bootstrap and
    /// preserved on every subsequent run (bootstrap amendment 2026-04-27).
    pub vault_id: VaultId,
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
/// On the first run a fresh [`VaultId`] is minted and written to
/// `.cairn/vault.id` after the directory tree has been created. On subsequent
/// runs the existing file is read and left intact. If the file is missing but
/// the vault has already been bound (DB row or binding sentinel present) the
/// function returns an error containing `"vault.id lost"` (mapped to
/// `EX_DATAERR=65` by the CLI).
///
/// # Errors
/// Returns an error if any directory cannot be created, any file cannot be
/// written, or if the vault.id consistency check fails (`EX_DATAERR`).
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
    let vault_id_path = vault.join(".cairn/vault.id");

    // ── vault.id pre-flight check (no side-effects) ────────────────────────
    // If the vault has been bound but vault.id is missing, fail now before
    // touching the filesystem.  Returns `None` when minting is needed (deferred
    // until after the directory tree is created).
    let maybe_vault_id = preflight_vault_id(&vault_id_path, &db_path, vault)?;
    // Track whether we need to write the file after dirs exist.
    let needs_vault_id_write = maybe_vault_id.is_none();
    let minted_vault_id = maybe_vault_id.unwrap_or_else(VaultId::mint);

    let mut receipt = BootstrapReceipt {
        vault_path: vault.clone(),
        config_path: config_path.clone(),
        db_path,
        dirs_created: Vec::new(),
        dirs_existing: Vec::new(),
        files_created: Vec::new(),
        files_skipped: Vec::new(),
        vault_id: minted_vault_id,
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

    // ── vault.id write (first run only) ───────────────────────────────────
    // `needs_vault_id_write` is `true` only on the very first run. Now that
    // `.cairn` exists we can atomically persist the id.  On re-runs the file
    // is already present and we leave it alone.
    if needs_vault_id_write {
        write_vault_id_atomic(&vault_id_path, &receipt.vault_id)?;
    }

    // --- placeholder files ---
    let config_yaml = serde_yml::to_string(&CairnConfig::default())
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

/// Check the vault.id state *without side-effects*.
///
/// Returns:
/// - `Ok(Some(id))` — `.cairn/vault.id` already exists; caller should use it.
/// - `Ok(None)` — file absent, vault fresh; caller should mint+write after dirs.
/// - `Err(…)` — file absent but vault was already bound (fail-closed).
fn preflight_vault_id(
    vault_id_path: &Path,
    db_path: &Path,
    vault: &Path,
) -> Result<Option<VaultId>> {
    if let Ok(raw) = std::fs::read_to_string(vault_id_path) {
        // File exists — validate and return; never rewrite.
        let id = VaultId::parse(raw.trim())
            .with_context(|| format!("{} contains an invalid vault id", vault_id_path.display()))?;
        return Ok(Some(id));
    }

    // vault.id is absent — check whether the vault has already been bound.
    if binding_sentinel_exists(vault) {
        anyhow::bail!(
            "vault.id lost — run `cairn identity vault-id-recover` to restore it \
             (binding sentinel exists but .cairn/vault.id is missing)"
        );
    }
    match probe_vault_meta(db_path) {
        VaultMetaProbe::Present => anyhow::bail!(
            "vault.id lost — run `cairn identity vault-id-recover` to restore it \
             (vault_meta row exists but .cairn/vault.id is missing)"
        ),
        VaultMetaProbe::Indeterminate(reason) => anyhow::bail!(
            "vault.id missing and {reason} — refusing to mint a new vault.id over a \
             possibly-bound vault. Run `cairn identity vault-id-recover` or \
             investigate the database."
        ),
        VaultMetaProbe::Absent => {}
    }

    // Fresh vault; caller will mint + write after dirs exist.
    Ok(None)
}

/// Atomically write `id` to `vault_id_path` using a temp file.
///
/// Uses `persist_noclobber` so a concurrent bootstrap that wins the race
/// is detected; in that case we read back and validate the winner's value.
fn write_vault_id_atomic(vault_id_path: &Path, id: &VaultId) -> Result<()> {
    use std::io::Write as _;

    let dir = vault_id_path.parent().unwrap_or(std::path::Path::new("."));

    let mut tmp = tempfile::Builder::new()
        .prefix(".vault-id")
        .tempfile_in(dir)
        .with_context(|| format!("creating temp file for {}", vault_id_path.display()))?;
    tmp.write_all(id.as_str().as_bytes()).with_context(|| {
        format!(
            "writing vault id to temp file for {}",
            vault_id_path.display()
        )
    })?;
    tmp.as_file()
        .sync_all()
        .with_context(|| format!("syncing vault id temp file for {}", vault_id_path.display()))?;

    // `persist_noclobber` fails if the file already exists, which would mean a
    // concurrent bootstrap wrote vault.id between our read and write above.
    // In that case, read back what was written and use it (idempotent).
    match tmp.persist_noclobber(vault_id_path) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another concurrent bootstrap won the race; read its result.
            // If it's also invalid that's a hard error.
            let raw = std::fs::read_to_string(vault_id_path)
                .with_context(|| format!("reading {} after race", vault_id_path.display()))?;
            VaultId::parse(raw.trim())
                .context("invalid vault id written by concurrent bootstrap")?;
            Ok(())
        }
        Err(e) => Err(anyhow::Error::from(e.error))
            .with_context(|| format!("persisting vault id to {}", vault_id_path.display())),
    }
}

/// Return `true` if a binding sentinel (`.cairn/vault.binding` or
/// `.cairn/vault.binding.pending`) is present. These files are written by the
/// identity first-bind workflow and indicate that this vault has been bound to
/// at least one identity — making a vault.id re-mint unsafe.
fn binding_sentinel_exists(vault: &Path) -> bool {
    vault.join(".cairn/vault.binding").exists()
        || vault.join(".cairn/vault.binding.pending").exists()
}

/// Tri-state result of probing the `vault_meta` row.
///
/// `Indeterminate` carries the underlying failure reason and forces the
/// caller to fail closed rather than treat an opaque DB error as an
/// "unbound" signal — minting a fresh `vault.id` over a locked or corrupt
/// database would silently strand committed identities under the prior
/// keystore namespace.
enum VaultMetaProbe {
    /// `vault_meta` row exists — vault was bound.
    Present,
    /// DB file missing OR `vault_meta` table missing OR no row — fresh vault.
    Absent,
    /// DB exists but could not be inspected. Bootstrap must refuse.
    Indeterminate(String),
}

/// Probe `vault_meta` in `db_path`. See [`VaultMetaProbe`].
fn probe_vault_meta(db_path: &Path) -> VaultMetaProbe {
    use rusqlite::{ErrorCode, OpenFlags};
    if !db_path.exists() {
        return VaultMetaProbe::Absent;
    }
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(c) => c,
        Err(e) => {
            return VaultMetaProbe::Indeterminate(format!(
                "could not open {}: {e}",
                db_path.display()
            ));
        }
    };
    match conn.query_row("SELECT 1 FROM vault_meta WHERE rowid = 1", [], |_| Ok(())) {
        Ok(()) => VaultMetaProbe::Present,
        Err(rusqlite::Error::QueryReturnedNoRows) => VaultMetaProbe::Absent,
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::Unknown => {
            // SQLITE_ERROR for missing table — the schema hasn't been initialised.
            VaultMetaProbe::Absent
        }
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => {
            VaultMetaProbe::Absent
        }
        Err(e) => VaultMetaProbe::Indeterminate(format!(
            "vault_meta probe failed for {}: {e}",
            db_path.display()
        )),
    }
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
        "{header}\n  vault.id  {vault_id}\n  config    {}  [{config_status}]\n  db        {}  (created on first ingest)\n  dirs      {} created, {} existing\n  files     {} created, {} skipped",
        receipt.config_path.display(),
        receipt.db_path.display(),
        receipt.dirs_created.len(),
        receipt.dirs_existing.len(),
        receipt.files_created.len(),
        receipt.files_skipped.len(),
        vault_id = receipt.vault_id,
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
