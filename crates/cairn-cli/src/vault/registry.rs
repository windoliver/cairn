//! Vault registry I/O and resolution (brief §3.3).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use cairn_core::config::{VaultRegistry};

/// Errors that can occur during vault resolution or registry operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultError {
    /// A named vault was requested but is not in the registry.
    #[error("vault '{name}' not found in registry — run `cairn vault list` to see known vaults")]
    NotFound {
        /// The vault name that was not found.
        name: String,
    },
    /// No vault could be resolved from any source.
    #[error(
        "no active vault: set --vault <name|path>, CAIRN_VAULT=<name|path>, \
         run from inside a vault directory, or set a default with `cairn vault switch <name>`"
    )]
    NoneResolved,
    /// A vault with this name already exists in the registry.
    #[error("vault '{name}' already exists in registry — use a different name or remove it first")]
    DuplicateName {
        /// The duplicate vault name.
        name: String,
    },
    /// The target path is not a cairn vault (no `.cairn/` directory).
    #[error("'{path}' is not a cairn vault (no .cairn/ directory found) — run `cairn bootstrap` first")]
    NotAVault {
        /// The path that was checked.
        path: PathBuf,
    },
}

/// I/O wrapper around a `vaults.toml` file.
pub struct VaultRegistryStore {
    path: PathBuf,
}

impl VaultRegistryStore {
    /// Create a store pointing at the given `vaults.toml` path.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Return the canonical registry path for the current platform.
    ///
    /// - Linux/macOS: `$XDG_CONFIG_HOME/cairn/vaults.toml` or `$HOME/.config/cairn/vaults.toml`
    /// - Windows: `%APPDATA%\cairn\vaults.toml`
    ///
    /// # Errors
    /// Returns an error if the required env var (`HOME` / `APPDATA`) is unset.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            let appdata =
                std::env::var("APPDATA").context("APPDATA env var not set")?;
            Ok(PathBuf::from(appdata).join("cairn").join("vaults.toml"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            let config_dir = std::env::var("XDG_CONFIG_HOME").ok().map_or_else(
                || {
                    let home = std::env::var("HOME").unwrap_or_default();
                    PathBuf::from(home).join(".config")
                },
                PathBuf::from,
            );
            Ok(config_dir.join("cairn").join("vaults.toml"))
        }
    }

    /// Load the registry. Returns an empty registry if the file does not exist.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load(&self) -> anyhow::Result<VaultRegistry> {
        if !self.path.exists() {
            return Ok(VaultRegistry::default());
        }
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading registry at {}", self.path.display()))?;
        VaultRegistry::from_toml(&text)
            .with_context(|| format!("parsing registry at {}", self.path.display()))
    }

    /// Save the registry atomically (write to a temp file then rename).
    ///
    /// Creates parent directories if they do not exist.
    ///
    /// # Errors
    /// Returns an error if the directory cannot be created, the temp file cannot
    /// be written, or the atomic rename fails.
    pub fn save(&self, reg: &VaultRegistry) -> anyhow::Result<()> {
        use std::io::Write as _;

        let parent = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating registry parent dir {}", parent.display()))?;

        let toml = reg
            .to_toml()
            .context("serializing vault registry to TOML")?;

        let mut tmp = tempfile::Builder::new()
            .prefix(".vaults")
            .tempfile_in(parent)
            .with_context(|| format!("creating temp file in {}", parent.display()))?;
        tmp.write_all(toml.as_bytes())
            .context("writing registry temp file")?;
        tmp.as_file().sync_all().context("syncing registry temp file")?;
        tmp.persist(&self.path)
            .map_err(|e| e.error)
            .with_context(|| format!("persisting registry to {}", self.path.display()))?;
        Ok(())
    }

    /// Convenience: return the path this store manages.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Options for [`resolve_vault`].
pub struct ResolveOpts<'a> {
    /// Value from `--vault` flag or `CAIRN_VAULT` env var. `None` means
    /// neither was set.
    pub explicit: Option<String>,
    /// Starting directory for walk-up discovery. Defaults to `$PWD` when
    /// `None`.
    pub cwd: Option<PathBuf>,
    /// Registry store to use for name lookup and default fallback.
    pub store: &'a VaultRegistryStore,
}

/// Resolve the active vault path using the four-level precedence (§3.3):
///
/// 1. `opts.explicit` (from `--vault` or `CAIRN_VAULT`) — path or name
/// 2. Walk up from `opts.cwd` looking for `.cairn/`
/// 3. Registry `default` entry
///
/// Returns the vault root path.
///
/// # Errors
/// - [`VaultError::NotFound`] if an explicit name is given but not in the registry.
/// - [`VaultError::NoneResolved`] if no vault can be determined from any source.
pub fn resolve_vault(opts: ResolveOpts<'_>) -> anyhow::Result<PathBuf> {
    // 1. Explicit
    if let Some(ref s) = opts.explicit {
        return resolve_explicit(s, opts.store);
    }

    // 2. Walk up from cwd
    let cwd = opts
        .cwd
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    if let Some(found) = walk_up_to_vault(&cwd) {
        return Ok(found);
    }

    // 3. Registry default
    let reg = opts.store.load()?;
    if let Some(ref name) = reg.default
        && let Some(entry) = reg.get(name)
    {
        return Ok(expand_tilde(&entry.path));
    }

    Err(VaultError::NoneResolved.into())
}

fn resolve_explicit(s: &str, store: &VaultRegistryStore) -> anyhow::Result<PathBuf> {
    // Treat as a filesystem path when it starts with `/`, `~`, `./`, `../`,
    // or contains a path separator.
    if s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.contains(std::path::MAIN_SEPARATOR)
    {
        return Ok(expand_tilde(s));
    }
    // Otherwise look up by name in the registry.
    let reg = store.load()?;
    reg.get(s)
        .map(|e| expand_tilde(&e.path))
        .ok_or_else(|| VaultError::NotFound { name: s.to_owned() }.into())
}

/// Walk up the directory tree from `start` looking for a `.cairn/` directory.
/// Returns the first ancestor that has one.
#[must_use]
pub fn walk_up_to_vault(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".cairn").is_dir() {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

/// Expand a leading `~` to `$HOME` (or `$USERPROFILE` on Windows).
#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = home_dir().unwrap_or_default();
        home.join(rest)
    } else if path == "~" {
        home_dir().unwrap_or_default()
    } else {
        PathBuf::from(path)
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}
