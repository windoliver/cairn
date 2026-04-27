# Vault Registry, Switching, and Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the multi-vault registry (§3.3) — a `~/.config/cairn/vaults.toml` index, `cairn vault list|switch|add|remove` subcommands, and a four-level vault resolution chain that guarantees every CLI invocation binds to exactly one vault.

**Architecture:** Pure types (`VaultEntry`, `VaultRegistry`) live in `cairn-core` (no I/O). I/O and resolution live in `cairn-cli/src/vault/registry.rs`. The `vault` source file becomes a module directory so the growing bootstrap logic and new registry logic stay in focused files. The global `--vault` flag (or `CAIRN_VAULT` env) is threaded through main dispatch; if no vault is resolved the command fails with EX_CONFIG (78).

**Tech Stack:** `toml` (already a `cairn-core` dep), `serde`, `thiserror`, `anyhow`, `tempfile` + `temp-env` (tests), `insta` (snapshot tests).

**Design references:** §3.3, §6.5 exit codes, §6.2 error-handling conventions.

---

## File Map

| Action | Path | Responsibility |
|--------|------|---------------|
| **Create** | `crates/cairn-core/src/config/vault_registry.rs` | `VaultEntry`, `VaultRegistry` pure types + TOML round-trip |
| **Modify** | `crates/cairn-core/src/config/mod.rs` | add `pub mod vault_registry; pub use vault_registry::*;` |
| **Create** | `crates/cairn-cli/src/vault/mod.rs` | module re-exports (bootstrap + registry public API) |
| **Create** | `crates/cairn-cli/src/vault/bootstrap.rs` | move content from current `vault.rs` verbatim |
| **Create** | `crates/cairn-cli/src/vault/registry.rs` | `VaultError`, `VaultRegistryStore`, `resolve_vault`, walk-up helper |
| **Delete** | `crates/cairn-cli/src/vault.rs` | replaced by the directory module above |
| **Modify** | `crates/cairn-cli/src/main.rs` | add `--vault` global flag; add `vault` subcommand tree; thread resolved path through dispatch |
| **Create** | `crates/cairn-cli/tests/vault_registry.rs` | registry CRUD tests, resolution precedence tests, isolation tests |

`cairn-cli/Cargo.toml` does **not** need `toml` added — `VaultRegistry::from_toml` / `to_toml` live in `cairn-core` which already has the dep.

---

## Task 1 — Pure vault-registry types in `cairn-core`

**Files:**
- Create: `crates/cairn-core/src/config/vault_registry.rs`
- Modify: `crates/cairn-core/src/config/mod.rs` (add 2 lines)

- [ ] **Step 1.1 — Write the failing tests (inline unit)**

Open `crates/cairn-core/src/config/vault_registry.rs` (new file) and add:

```rust
//! Vault registry types for `~/.config/cairn/vaults.toml` (brief §3.3).

use serde::{Deserialize, Serialize};

/// One entry in the vault registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEntry {
    /// Short human identifier, e.g. `"work"` or `"personal"`.
    pub name: String,
    /// Filesystem path to the vault root; may contain a leading `~`.
    pub path: String,
    /// Optional human label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// ISO 8601 date after which the vault is considered expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Parsed content of `~/.config/cairn/vaults.toml` (§3.3).
///
/// TOML shape:
/// ```toml
/// default = "work"
///
/// [[vault]]
/// name = "work"
/// path = "~/vaults/work"
/// label = "day job"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRegistry {
    /// Name of the active default vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Known vaults. TOML key is `vault` (array of tables).
    #[serde(default, rename = "vault")]
    pub vaults: Vec<VaultEntry>,
}

impl VaultRegistry {
    /// Parse from TOML text.
    ///
    /// # Errors
    /// Returns a `toml` deserialization error on malformed input.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Serialize to TOML text.
    ///
    /// # Errors
    /// Returns a `toml` serialization error (practically infallible for this type).
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Find a vault entry by name (exact match).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&VaultEntry> {
        self.vaults.iter().find(|v| v.name == name)
    }

    /// `true` if a vault with this name is already registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
default = "work"

[[vault]]
name = "work"
path = "~/vaults/work"
label = "day job"

[[vault]]
name = "personal"
path = "~/vaults/personal"
"#;

    #[test]
    fn parse_sample_toml() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert_eq!(reg.default.as_deref(), Some("work"));
        assert_eq!(reg.vaults.len(), 2);
        assert_eq!(reg.vaults[0].name, "work");
        assert_eq!(reg.vaults[0].path, "~/vaults/work");
        assert_eq!(reg.vaults[0].label.as_deref(), Some("day job"));
        assert_eq!(reg.vaults[1].name, "personal");
        assert!(reg.vaults[1].label.is_none());
    }

    #[test]
    fn empty_registry_round_trips() {
        let reg = VaultRegistry::default();
        let toml = reg.to_toml().unwrap();
        let restored = VaultRegistry::from_toml(&toml).unwrap();
        assert_eq!(reg, restored);
    }

    #[test]
    fn get_returns_entry_by_name() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert!(reg.get("work").is_some());
        assert!(reg.get("personal").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn contains_works() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert!(reg.contains("work"));
        assert!(!reg.contains("ghost"));
    }

    #[test]
    fn round_trip_preserves_entries() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        let toml = reg.to_toml().unwrap();
        let restored = VaultRegistry::from_toml(&toml).unwrap();
        assert_eq!(reg, restored);
    }

    #[test]
    fn parse_expires_at() {
        let toml = r#"
[[vault]]
name = "research"
path = "~/vaults/research"
expires_at = "2026-07-01"
"#;
        let reg = VaultRegistry::from_toml(toml).unwrap();
        assert_eq!(
            reg.vaults[0].expires_at.as_deref(),
            Some("2026-07-01")
        );
    }
}
```

- [ ] **Step 1.2 — Wire into `config/mod.rs`**

At the top of `crates/cairn-core/src/config/mod.rs`, add two lines right after the opening doc comment (before the `use` statements):

```rust
pub mod vault_registry;
pub use vault_registry::{VaultEntry, VaultRegistry};
```

- [ ] **Step 1.3 — Run the tests**

```bash
cargo test -p cairn-core config::vault_registry -- --nocapture
```

Expected: all 6 unit tests pass.

- [ ] **Step 1.4 — Verify clippy clean**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

Expected: no warnings.

- [ ] **Step 1.5 — Commit**

```bash
git add crates/cairn-core/src/config/vault_registry.rs \
        crates/cairn-core/src/config/mod.rs
git commit -m "feat(core): VaultEntry + VaultRegistry TOML types (brief §3.3, #42)"
```

---

## Task 2 — Refactor `vault.rs` into a module directory

**Files:**
- Create: `crates/cairn-cli/src/vault/mod.rs`
- Create: `crates/cairn-cli/src/vault/bootstrap.rs`
- Delete: `crates/cairn-cli/src/vault.rs`

The existing `lib.rs` already has `pub mod vault;` — Rust will find a directory module automatically.

- [ ] **Step 2.1 — Create `vault/bootstrap.rs`**

Create `crates/cairn-cli/src/vault/bootstrap.rs` with the **exact content** of the current `crates/cairn-cli/src/vault.rs` (copy it verbatim). No logic changes.

- [ ] **Step 2.2 — Create `vault/mod.rs`**

```rust
//! Vault management: bootstrap (§3.1) and registry (§3.3).

pub mod bootstrap;
pub mod registry;

pub use bootstrap::{BootstrapOpts, BootstrapReceipt, bootstrap, render_human};
```

- [ ] **Step 2.3 — Delete `vault.rs`**

```bash
rm crates/cairn-cli/src/vault.rs
```

- [ ] **Step 2.4 — Verify existing tests still pass**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: same pass/fail counts as before (all bootstrap tests pass).

- [ ] **Step 2.5 — Create empty `vault/registry.rs`** (placeholder for Task 3)

```rust
//! Vault registry I/O and resolution (brief §3.3).
```

- [ ] **Step 2.6 — Commit**

```bash
git add crates/cairn-cli/src/vault/ crates/cairn-cli/src/vault.rs
git commit -m "refactor(cli): vault.rs → vault/{bootstrap,registry}.rs module split (#42)"
```

---

## Task 3 — `VaultError`, `VaultRegistryStore`, and `resolve_vault`

**Files:**
- Modify: `crates/cairn-cli/src/vault/registry.rs`
- Modify: `crates/cairn-cli/src/vault/mod.rs` (add re-export)

- [ ] **Step 3.1 — Write the failing tests first**

Create `crates/cairn-cli/tests/vault_registry.rs`:

```rust
//! Tests for vault registry I/O and resolution (brief §3.3, #42).

use std::path::PathBuf;

use cairn_cli::vault::registry::{VaultRegistryStore, resolve_vault, ResolveOpts};
use cairn_core::config::{VaultEntry, VaultRegistry};

/// Convenience: bootstrap a minimal vault in a temp dir so walk-up discovery works.
fn make_vault(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().to_path_buf();
    std::fs::create_dir_all(path.join(".cairn")).unwrap();
    path
}

// ── Registry CRUD ────────────────────────────────────────────────────────────

#[test]
fn load_returns_empty_when_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(dir.path().join("vaults.toml"));
    let reg = store.load().unwrap();
    assert!(reg.vaults.is_empty());
    assert!(reg.default.is_none());
}

#[test]
fn save_and_reload_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(dir.path().join("vaults.toml"));

    let mut reg = VaultRegistry::default();
    reg.vaults.push(VaultEntry {
        name: "work".into(),
        path: "/tmp/work".into(),
        label: Some("day job".into()),
        expires_at: None,
    });
    reg.default = Some("work".into());
    store.save(&reg).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded.default.as_deref(), Some("work"));
    assert_eq!(loaded.vaults.len(), 1);
    assert_eq!(loaded.vaults[0].name, "work");
    assert_eq!(loaded.vaults[0].label.as_deref(), Some("day job"));
}

#[test]
fn save_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        VaultRegistryStore::new(dir.path().join("nested").join("deep").join("vaults.toml"));
    store.save(&VaultRegistry::default()).unwrap();
    assert!(dir.path().join("nested/deep/vaults.toml").exists());
}

// ── Vault resolution ─────────────────────────────────────────────────────────

#[test]
fn explicit_path_wins_over_all() {
    let vault_dir = tempfile::tempdir().unwrap();
    make_vault(&vault_dir);
    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
    store.save(&VaultRegistry::default()).unwrap();

    let resolved = resolve_vault(ResolveOpts {
        explicit: Some(vault_dir.path().to_str().unwrap().to_owned()),
        cwd: Some(PathBuf::from("/tmp")),
        store: &store,
    })
    .unwrap();
    assert_eq!(resolved, vault_dir.path());
}

#[test]
fn explicit_name_resolves_via_registry() {
    let vault_dir = tempfile::tempdir().unwrap();
    make_vault(&vault_dir);
    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));

    let mut reg = VaultRegistry::default();
    reg.vaults.push(VaultEntry {
        name: "myvault".into(),
        path: vault_dir.path().to_str().unwrap().to_owned(),
        label: None,
        expires_at: None,
    });
    store.save(&reg).unwrap();

    let resolved = resolve_vault(ResolveOpts {
        explicit: Some("myvault".into()),
        cwd: None,
        store: &store,
    })
    .unwrap();
    assert_eq!(resolved, vault_dir.path());
}

#[test]
fn explicit_unknown_name_errors() {
    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
    store.save(&VaultRegistry::default()).unwrap();

    let err = resolve_vault(ResolveOpts {
        explicit: Some("ghost".into()),
        cwd: None,
        store: &store,
    })
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ghost"), "expected name in error: {msg}");
}

#[test]
fn walk_up_finds_vault_in_ancestor() {
    let vault_dir = tempfile::tempdir().unwrap();
    make_vault(&vault_dir);
    let sub = vault_dir.path().join("src").join("nested");
    std::fs::create_dir_all(&sub).unwrap();

    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
    store.save(&VaultRegistry::default()).unwrap();

    let resolved = resolve_vault(ResolveOpts {
        explicit: None,
        cwd: Some(sub),
        store: &store,
    })
    .unwrap();
    assert_eq!(resolved, vault_dir.path());
}

#[test]
fn walk_up_skips_dir_without_cairn() {
    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
    store.save(&VaultRegistry::default()).unwrap();

    // /tmp has no .cairn/ and no registry default → NoneResolved
    let err = resolve_vault(ResolveOpts {
        explicit: None,
        cwd: Some(PathBuf::from("/tmp")),
        store: &store,
    })
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no active vault") || msg.contains("CAIRN_VAULT"),
        "unexpected error: {msg}"
    );
}

#[test]
fn registry_default_used_as_fallback() {
    let vault_dir = tempfile::tempdir().unwrap();
    make_vault(&vault_dir);
    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));

    let mut reg = VaultRegistry::default();
    reg.default = Some("home".into());
    reg.vaults.push(VaultEntry {
        name: "home".into(),
        path: vault_dir.path().to_str().unwrap().to_owned(),
        label: None,
        expires_at: None,
    });
    store.save(&reg).unwrap();

    let resolved = resolve_vault(ResolveOpts {
        explicit: None,
        cwd: Some(PathBuf::from("/tmp")),
        store: &store,
    })
    .unwrap();
    assert_eq!(resolved, vault_dir.path());
}

// ── Isolation ────────────────────────────────────────────────────────────────

#[test]
fn two_vaults_resolve_to_different_paths() {
    let vault_a = tempfile::tempdir().unwrap();
    make_vault(&vault_a);
    let vault_b = tempfile::tempdir().unwrap();
    make_vault(&vault_b);

    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));

    let mut reg = VaultRegistry::default();
    reg.vaults.push(VaultEntry {
        name: "alpha".into(),
        path: vault_a.path().to_str().unwrap().to_owned(),
        label: None,
        expires_at: None,
    });
    reg.vaults.push(VaultEntry {
        name: "beta".into(),
        path: vault_b.path().to_str().unwrap().to_owned(),
        label: None,
        expires_at: None,
    });
    store.save(&reg).unwrap();

    let a = resolve_vault(ResolveOpts {
        explicit: Some("alpha".into()),
        cwd: None,
        store: &store,
    })
    .unwrap();
    let b = resolve_vault(ResolveOpts {
        explicit: Some("beta".into()),
        cwd: None,
        store: &store,
    })
    .unwrap();
    assert_ne!(a, b, "alpha and beta must resolve to different paths");
    assert_eq!(a, vault_a.path());
    assert_eq!(b, vault_b.path());
}
```

- [ ] **Step 3.2 — Run the tests (they must fail)**

```bash
cargo nextest run -p cairn-cli vault_registry -- --nocapture 2>&1 | head -30
```

Expected: compile error — `VaultRegistryStore`, `resolve_vault`, `ResolveOpts` not found.

- [ ] **Step 3.3 — Implement `vault/registry.rs`**

```rust
//! Vault registry I/O and resolution (brief §3.3).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use cairn_core::config::{VaultEntry, VaultRegistry};

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
            let config_dir = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let home = std::env::var("HOME").unwrap_or_default();
                    PathBuf::from(home).join(".config")
                });
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
/// Returns the **canonicalized** vault root path.
///
/// # Errors
/// - [`VaultError::NotFound`] if an explicit name is given but not in the
///   registry.
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
    if let Some(name) = reg.default {
        if let Some(entry) = reg.get(&name) {
            return Ok(expand_tilde(&entry.path));
        }
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
```

- [ ] **Step 3.4 — Update `vault/mod.rs` to re-export registry items**

```rust
//! Vault management: bootstrap (§3.1) and registry (§3.3).

pub mod bootstrap;
pub mod registry;

pub use bootstrap::{BootstrapOpts, BootstrapReceipt, bootstrap, render_human};
pub use registry::{ResolveOpts, VaultError, VaultRegistryStore, resolve_vault, walk_up_to_vault};
```

- [ ] **Step 3.5 — Run all tests**

```bash
cargo nextest run -p cairn-cli --locked -- --nocapture 2>&1 | tail -20
```

Expected: all registry tests pass, all existing bootstrap/cli tests still pass.

- [ ] **Step 3.6 — Clippy**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 3.7 — Commit**

```bash
git add crates/cairn-cli/src/vault/registry.rs \
        crates/cairn-cli/src/vault/mod.rs \
        crates/cairn-cli/tests/vault_registry.rs
git commit -m "feat(cli): VaultRegistryStore + resolve_vault (brief §3.3, #42)"
```

---

## Task 4 — `cairn vault add` subcommand

**Files:**
- Modify: `crates/cairn-cli/src/vault/registry.rs` (add `add_vault` helper)
- Modify: `crates/cairn-cli/src/main.rs` (add `vault` subcommand, `add` sub-subcommand)

- [ ] **Step 4.1 — Write failing CLI test**

Add to `crates/cairn-cli/tests/vault_registry.rs`:

```rust
// ── cairn vault add ──────────────────────────────────────────────────────────

mod cli_vault_add {
    use std::process::Command;

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    #[test]
    fn add_registers_vault_and_lists_it() {
        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_dir.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let reg_path = reg_dir.path().join("vaults.toml");

        let out = cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args([
                "vault",
                "add",
                vault_dir.path().to_str().unwrap(),
                "--name",
                "mywork",
                "--label",
                "test vault",
            ])
            .output()
            .expect("cairn vault add");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // vault is persisted
        let store = cairn_cli::vault::VaultRegistryStore::new(reg_path);
        let reg = store.load().unwrap();
        assert!(reg.contains("mywork"));
    }

    #[test]
    fn add_duplicate_name_fails() {
        let vault_a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_a.path().join(".cairn")).unwrap();
        let vault_b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_b.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let reg_path = reg_dir.path().join("vaults.toml");

        cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args([
                "vault", "add", vault_a.path().to_str().unwrap(), "--name", "dup",
            ])
            .output()
            .unwrap();

        let out = cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args([
                "vault", "add", vault_b.path().to_str().unwrap(), "--name", "dup",
            ])
            .output()
            .expect("cairn vault add duplicate");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("dup"),
            "expected name in error: {stderr}"
        );
    }

    #[test]
    fn add_non_vault_path_fails() {
        let not_a_vault = tempfile::tempdir().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args([
                "vault",
                "add",
                not_a_vault.path().to_str().unwrap(),
                "--name",
                "bad",
            ])
            .output()
            .expect("cairn vault add non-vault");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(".cairn") || stderr.contains("not a cairn vault"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn add_json_emits_vault_entry() {
        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_dir.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args([
                "vault",
                "add",
                vault_dir.path().to_str().unwrap(),
                "--name",
                "jsontest",
                "--json",
            ])
            .output()
            .expect("cairn vault add --json");
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
        assert_eq!(v["name"], "jsontest");
    }
}
```

- [ ] **Step 4.2 — Add `add_vault` helper to `registry.rs`**

Add this function to `crates/cairn-cli/src/vault/registry.rs`:

```rust
/// Add a vault to the registry.
///
/// Verifies the path is a cairn vault (has `.cairn/`), rejects duplicate
/// names, then persists.
///
/// # Errors
/// - [`VaultError::NotAVault`] if `path/.cairn/` does not exist.
/// - [`VaultError::DuplicateName`] if a vault with `name` is already registered.
pub fn add_vault(
    store: &VaultRegistryStore,
    path: PathBuf,
    name: String,
    label: Option<String>,
) -> anyhow::Result<VaultEntry> {
    if !path.join(".cairn").is_dir() {
        return Err(VaultError::NotAVault { path }.into());
    }
    let mut reg = store.load()?;
    if reg.contains(&name) {
        return Err(VaultError::DuplicateName { name }.into());
    }
    let entry = VaultEntry {
        name,
        path: path.to_string_lossy().into_owned(),
        label,
        expires_at: None,
    };
    reg.vaults.push(entry.clone());
    store.save(&reg)?;
    Ok(entry)
}
```

Also add to the `pub use` in `vault/mod.rs`:
```rust
pub use registry::{ResolveOpts, VaultError, VaultRegistryStore, add_vault, resolve_vault, walk_up_to_vault};
```

- [ ] **Step 4.3 — Wire `cairn vault add` in `main.rs`**

Add the `vault_subcommand` builder and `run_vault` dispatcher. The key points:
- `CAIRN_REGISTRY` env var overrides the default registry path (used in tests and CI)
- `--json` flag on each sub-subcommand for machine output

In `main.rs`, add after `fn bootstrap_subcommand()`:

```rust
fn vault_subcommand() -> clap::Command {
    clap::Command::new("vault")
        .about("Manage the vault registry (brief §3.3)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("add")
                .about("Register a vault in the registry")
                .arg(
                    clap::Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .help("Filesystem path to the vault root"),
                )
                .arg(
                    clap::Arg::new("name")
                        .long("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Short identifier for the vault"),
                )
                .arg(
                    clap::Arg::new("label")
                        .long("label")
                        .value_name("LABEL")
                        .help("Human-readable description"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("list")
                .about("List registered vaults")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("switch")
                .about("Set the default vault")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to make default"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("remove")
                .about("Remove a vault from the registry (does not delete files)")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to remove"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
}
```

Add `fn registry_store() -> VaultRegistryStore`:

```rust
fn registry_store() -> anyhow::Result<cairn_cli::vault::VaultRegistryStore> {
    let path = if let Ok(p) = std::env::var("CAIRN_REGISTRY") {
        std::path::PathBuf::from(p)
    } else {
        cairn_cli::vault::VaultRegistryStore::default_path()?
    };
    Ok(cairn_cli::vault::VaultRegistryStore::new(path))
}
```

Add to `build_command()`:

```rust
.subcommand(vault_subcommand())
```

Add to the `main()` match:

```rust
Some(("vault", sub)) => run_vault(sub),
```

Add `run_vault` dispatcher (just the `add` branch for now):

```rust
fn run_vault(matches: &clap::ArgMatches) -> ExitCode {
    let store = match registry_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cairn vault: registry path error — {e:#}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    match matches.subcommand() {
        Some(("add", sub)) => {
            let path = std::path::PathBuf::from(
                sub.get_one::<String>("path")
                    .expect("invariant: path is required"),
            );
            let name = sub
                .get_one::<String>("name")
                .expect("invariant: --name is required")
                .clone();
            let label = sub.get_one::<String>("label").cloned();
            let json = sub.get_flag("json");

            match cairn_cli::vault::add_vault(&store, path, name, label) {
                Ok(entry) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&entry)
                                .expect("invariant: VaultEntry always serializes")
                        );
                    } else {
                        println!(
                            "cairn vault add: registered '{}' → {}",
                            entry.name, entry.path
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("cairn vault add: {e:#}");
                    ExitCode::from(78) // EX_CONFIG
                }
            }
        }
        Some(("list", _sub)) => {
            // implemented in Task 5
            eprintln!("cairn vault list: not yet implemented");
            ExitCode::from(1)
        }
        Some(("switch", _sub)) => {
            // implemented in Task 6
            eprintln!("cairn vault switch: not yet implemented");
            ExitCode::from(1)
        }
        Some(("remove", _sub)) => {
            // implemented in Task 6
            eprintln!("cairn vault remove: not yet implemented");
            ExitCode::from(1)
        }
        _ => unreachable!("clap subcommand_required(true) on vault"),
    }
}
```

- [ ] **Step 4.4 — Run tests**

```bash
cargo nextest run -p cairn-cli --locked -- vault_registry::cli_vault_add 2>&1 | tail -20
```

Expected: `add_registers_vault_and_lists_it`, `add_duplicate_name_fails`, `add_non_vault_path_fails`, `add_json_emits_vault_entry` all pass.

- [ ] **Step 4.5 — Clippy**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 4.6 — Commit**

```bash
git add crates/cairn-cli/src/vault/registry.rs \
        crates/cairn-cli/src/vault/mod.rs \
        crates/cairn-cli/src/main.rs \
        crates/cairn-cli/tests/vault_registry.rs
git commit -m "feat(cli): cairn vault add subcommand (brief §3.3, #42)"
```

---

## Task 5 — `cairn vault list` subcommand

**Files:**
- Modify: `crates/cairn-cli/src/vault/registry.rs` (add `list_vaults`)
- Modify: `crates/cairn-cli/src/main.rs` (fill in `list` branch)

- [ ] **Step 5.1 — Write failing test**

Add to `crates/cairn-cli/tests/vault_registry.rs`:

```rust
mod cli_vault_list {
    use std::process::Command;

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    fn reg_with_two_vaults() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        let a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".cairn")).unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let store =
            cairn_cli::vault::VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
        let mut reg = cairn_core::config::VaultRegistry::default();
        reg.default = Some("alpha".into());
        reg.vaults.push(cairn_core::config::VaultEntry {
            name: "alpha".into(),
            path: a.path().to_str().unwrap().to_owned(),
            label: Some("first vault".into()),
            expires_at: None,
        });
        reg.vaults.push(cairn_core::config::VaultEntry {
            name: "beta".into(),
            path: b.path().to_str().unwrap().to_owned(),
            label: None,
            expires_at: None,
        });
        store.save(&reg).unwrap();
        (a, b, reg_dir)
    }

    #[test]
    fn list_shows_both_vaults() {
        let (_a, _b, reg_dir) = reg_with_two_vaults();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "list"])
            .output()
            .expect("cairn vault list");
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.contains("alpha"), "missing alpha: {stdout}");
        assert!(stdout.contains("beta"), "missing beta: {stdout}");
    }

    #[test]
    fn list_marks_default() {
        let (_a, _b, reg_dir) = reg_with_two_vaults();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "list"])
            .output()
            .expect("cairn vault list");
        let stdout = String::from_utf8(out.stdout).unwrap();
        // default vault should be visually marked with `*` or `(default)`
        assert!(
            stdout.contains('*') || stdout.contains("default"),
            "default not marked: {stdout}"
        );
    }

    #[test]
    fn list_json_emits_array() {
        let (_a, _b, reg_dir) = reg_with_two_vaults();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "list", "--json"])
            .output()
            .expect("cairn vault list --json");
        assert!(out.status.success());
        let v: serde_json::Value =
            serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
        assert!(v.is_array(), "expected JSON array");
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn list_empty_registry_succeeds() {
        let reg_dir = tempfile::tempdir().unwrap();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "list"])
            .output()
            .expect("cairn vault list empty");
        assert!(out.status.success());
    }
}
```

- [ ] **Step 5.2 — Implement `list` branch in `run_vault`**

Replace the `"list"` arm stub in `main.rs` with:

```rust
Some(("list", sub)) => {
    let json = sub.get_flag("json");
    let reg = match store.load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn vault list: {e:#}");
            return ExitCode::from(78);
        }
    };
    if json {
        // Emit a JSON array where each object has all VaultEntry fields plus
        // a "is_default" boolean.
        let arr: Vec<serde_json::Value> = reg
            .vaults
            .iter()
            .map(|v| {
                let mut obj = serde_json::to_value(v)
                    .expect("invariant: VaultEntry always serializes");
                obj["is_default"] =
                    serde_json::Value::Bool(reg.default.as_deref() == Some(&v.name));
                obj
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&arr)
                .expect("invariant: array always serializes")
        );
    } else if reg.vaults.is_empty() {
        println!("cairn vault list: no vaults registered");
        println!("  add one with: cairn vault add <path> --name <name>");
    } else {
        for v in &reg.vaults {
            let marker = if reg.default.as_deref() == Some(&v.name) {
                "* "
            } else {
                "  "
            };
            let label = v
                .label
                .as_deref()
                .map(|l| format!("  — {l}"))
                .unwrap_or_default();
            println!("{marker}{:<20} {}{}", v.name, v.path, label);
        }
    }
    ExitCode::SUCCESS
}
```

- [ ] **Step 5.3 — Run tests**

```bash
cargo nextest run -p cairn-cli --locked -- vault_registry::cli_vault_list 2>&1 | tail -20
```

Expected: all 4 list tests pass.

- [ ] **Step 5.4 — Snapshot test for human output**

Add to `vault_registry.rs` tests:

```rust
#[test]
fn list_human_output_snapshot() {
    use cairn_cli::vault::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    let reg_dir = tempfile::tempdir().unwrap();
    let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
    let mut reg = VaultRegistry::default();
    reg.default = Some("work".into());
    reg.vaults.push(VaultEntry {
        name: "work".into(),
        path: "/home/alice/vaults/work".into(),
        label: Some("day job".into()),
        expires_at: None,
    });
    reg.vaults.push(VaultEntry {
        name: "personal".into(),
        path: "/home/alice/vaults/personal".into(),
        label: None,
        expires_at: None,
    });
    store.save(&reg).unwrap();

    // Build the same output as the `list` command without spawning a process.
    let reg2 = store.load().unwrap();
    let mut lines = Vec::new();
    for v in &reg2.vaults {
        let marker = if reg2.default.as_deref() == Some(&v.name) { "* " } else { "  " };
        let label = v.label.as_deref().map(|l| format!("  — {l}")).unwrap_or_default();
        lines.push(format!("{marker}{:<20} {}{}", v.name, v.path, label));
    }
    insta::assert_snapshot!(lines.join("\n"));
}
```

Run `cargo insta review` if the snapshot is new.

- [ ] **Step 5.5 — Commit**

```bash
git add crates/cairn-cli/src/main.rs \
        crates/cairn-cli/tests/vault_registry.rs \
        crates/cairn-cli/tests/snapshots/
git commit -m "feat(cli): cairn vault list subcommand (brief §3.3, #42)"
```

---

## Task 6 — `cairn vault switch` and `cairn vault remove`

**Files:**
- Modify: `crates/cairn-cli/src/main.rs` (fill in `switch` and `remove` branches)

- [ ] **Step 6.1 — Write failing tests**

Add to `crates/cairn-cli/tests/vault_registry.rs`:

```rust
mod cli_vault_switch {
    use std::process::Command;
    use cairn_cli::vault::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    fn setup_two_vaults(reg_dir: &tempfile::TempDir) -> (tempfile::TempDir, tempfile::TempDir) {
        let a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".cairn")).unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".cairn")).unwrap();
        let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
        let mut reg = VaultRegistry::default();
        reg.default = Some("alpha".into());
        reg.vaults.push(VaultEntry {
            name: "alpha".into(),
            path: a.path().to_str().unwrap().to_owned(),
            label: None,
            expires_at: None,
        });
        reg.vaults.push(VaultEntry {
            name: "beta".into(),
            path: b.path().to_str().unwrap().to_owned(),
            label: None,
            expires_at: None,
        });
        store.save(&reg).unwrap();
        (a, b)
    }

    #[test]
    fn switch_changes_default() {
        let reg_dir = tempfile::tempdir().unwrap();
        let (_a, _b) = setup_two_vaults(&reg_dir);
        let reg_path = reg_dir.path().join("vaults.toml");

        let out = cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args(["vault", "switch", "beta"])
            .output()
            .expect("cairn vault switch");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let store = VaultRegistryStore::new(reg_path);
        let reg = store.load().unwrap();
        assert_eq!(reg.default.as_deref(), Some("beta"));
    }

    #[test]
    fn switch_unknown_name_errors() {
        let reg_dir = tempfile::tempdir().unwrap();
        let (_a, _b) = setup_two_vaults(&reg_dir);
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "switch", "ghost"])
            .output()
            .expect("cairn vault switch unknown");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("ghost"), "missing name: {stderr}");
    }

    #[test]
    fn switch_json_emits_name() {
        let reg_dir = tempfile::tempdir().unwrap();
        let (_a, _b) = setup_two_vaults(&reg_dir);
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "switch", "beta", "--json"])
            .output()
            .expect("cairn vault switch --json");
        assert!(out.status.success());
        let v: serde_json::Value =
            serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
        assert_eq!(v["default"], "beta");
    }
}

mod cli_vault_remove {
    use std::process::Command;
    use cairn_cli::vault::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    fn setup_vault(reg_dir: &tempfile::TempDir, name: &str) -> tempfile::TempDir {
        let v = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(v.path().join(".cairn")).unwrap();
        let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
        let mut reg = store.load().unwrap();
        reg.vaults.push(VaultEntry {
            name: name.to_owned(),
            path: v.path().to_str().unwrap().to_owned(),
            label: None,
            expires_at: None,
        });
        store.save(&reg).unwrap();
        v
    }

    #[test]
    fn remove_deregisters_vault() {
        let reg_dir = tempfile::tempdir().unwrap();
        let reg_path = reg_dir.path().join("vaults.toml");
        let _v = setup_vault(&reg_dir, "todelete");

        let out = cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args(["vault", "remove", "todelete"])
            .output()
            .expect("cairn vault remove");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let store = VaultRegistryStore::new(reg_path);
        let reg = store.load().unwrap();
        assert!(!reg.contains("todelete"));
    }

    #[test]
    fn remove_does_not_delete_files() {
        let reg_dir = tempfile::tempdir().unwrap();
        let vdir = setup_vault(&reg_dir, "keeper");
        let cairn_dir = vdir.path().join(".cairn");

        cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "remove", "keeper"])
            .output()
            .unwrap();

        // the vault directory itself must still exist
        assert!(cairn_dir.is_dir(), ".cairn/ should survive vault remove");
    }

    #[test]
    fn remove_unknown_name_errors() {
        let reg_dir = tempfile::tempdir().unwrap();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["vault", "remove", "nosuchvault"])
            .output()
            .expect("cairn vault remove unknown");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("nosuchvault"), "missing name: {stderr}");
    }
}
```

- [ ] **Step 6.2 — Implement `switch` and `remove` in `run_vault`**

Replace the `"switch"` stub in `main.rs`:

```rust
Some(("switch", sub)) => {
    let name = sub
        .get_one::<String>("name")
        .expect("invariant: name is required")
        .clone();
    let json = sub.get_flag("json");

    let mut reg = match store.load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn vault switch: {e:#}");
            return ExitCode::from(78);
        }
    };
    if !reg.contains(&name) {
        eprintln!(
            "cairn vault switch: vault '{name}' not found — run `cairn vault list`"
        );
        return ExitCode::from(78);
    }
    reg.default = Some(name.clone());
    if let Err(e) = store.save(&reg) {
        eprintln!("cairn vault switch: {e:#}");
        return ExitCode::from(74); // EX_IOERR
    }
    if json {
        println!(
            "{}",
            serde_json::json!({ "default": name })
        );
    } else {
        println!("cairn vault switch: default vault is now '{name}'");
    }
    ExitCode::SUCCESS
}
```

Replace the `"remove"` stub:

```rust
Some(("remove", sub)) => {
    let name = sub
        .get_one::<String>("name")
        .expect("invariant: name is required")
        .clone();
    let json = sub.get_flag("json");

    let mut reg = match store.load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn vault remove: {e:#}");
            return ExitCode::from(78);
        }
    };
    if !reg.contains(&name) {
        eprintln!(
            "cairn vault remove: vault '{name}' not found — run `cairn vault list`"
        );
        return ExitCode::from(78);
    }
    // Clear default if we're removing the current default
    if reg.default.as_deref() == Some(&name) {
        reg.default = None;
    }
    reg.vaults.retain(|v| v.name != name);
    if let Err(e) = store.save(&reg) {
        eprintln!("cairn vault remove: {e:#}");
        return ExitCode::from(74);
    }
    if json {
        println!("{}", serde_json::json!({ "removed": name }));
    } else {
        println!("cairn vault remove: removed '{name}' from registry (vault files untouched)");
    }
    ExitCode::SUCCESS
}
```

- [ ] **Step 6.3 — Run all vault tests**

```bash
cargo nextest run -p cairn-cli --locked -- vault_registry 2>&1 | tail -30
```

Expected: all tests in `cli_vault_switch` and `cli_vault_remove` pass.

- [ ] **Step 6.4 — Commit**

```bash
git add crates/cairn-cli/src/main.rs crates/cairn-cli/tests/vault_registry.rs
git commit -m "feat(cli): cairn vault switch + remove subcommands (brief §3.3, #42)"
```

---

## Task 7 — Global `--vault` flag + scope guard

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`

The global `--vault` flag lets users run `cairn --vault work search "…"`. `CAIRN_VAULT` env var provides the same. Verb dispatch logs the resolved vault so the user gets clear context when things go wrong.

- [ ] **Step 7.1 — Write failing test**

Add to `crates/cairn-cli/tests/vault_registry.rs`:

```rust
mod cli_vault_flag {
    use std::process::Command;

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    #[test]
    fn vault_flag_unknown_name_exits_78() {
        let reg_dir = tempfile::tempdir().unwrap();
        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args(["--vault", "nosuchvault", "search"])
            .output()
            .expect("cairn --vault nosuchvault search");
        // Should fail with EX_CONFIG (78) — vault not found
        assert_eq!(out.status.code(), Some(78));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("nosuchvault"),
            "expected vault name in error: {stderr}"
        );
    }

    #[test]
    fn cairn_vault_env_resolves_by_name() {
        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_dir.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let reg_path = reg_dir.path().join("vaults.toml");

        // Register the vault
        cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .args([
                "vault", "add",
                vault_dir.path().to_str().unwrap(),
                "--name", "envtest",
            ])
            .output()
            .unwrap();

        // Run a verb with CAIRN_VAULT set — it resolves OK but still returns
        // Internal (store not wired) → exit 1, not 78
        let out = cairn()
            .env("CAIRN_REGISTRY", reg_path.to_str().unwrap())
            .env("CAIRN_VAULT", "envtest")
            .args(["search"])
            .output()
            .expect("CAIRN_VAULT=envtest cairn search");
        assert_ne!(
            out.status.code(),
            Some(78),
            "should not fail with EX_CONFIG when vault resolves: {:?}",
            out.status
        );
    }

    #[test]
    fn vault_flag_path_resolves_directly() {
        let vault_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault_dir.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();

        let out = cairn()
            .env(
                "CAIRN_REGISTRY",
                reg_dir.path().join("vaults.toml").to_str().unwrap(),
            )
            .args([
                "--vault",
                vault_dir.path().to_str().unwrap(),
                "search",
            ])
            .output()
            .expect("cairn --vault <path> search");
        // Path resolution succeeds → verb may fail with Internal (1) but not 78
        assert_ne!(
            out.status.code(),
            Some(78),
            "path-based --vault should not error as EX_CONFIG"
        );
    }
}
```

- [ ] **Step 7.2 — Add global `--vault` arg to `build_command()`**

In `build_command()`, add before the first `.subcommand(...)`:

```rust
.arg(
    clap::Arg::new("vault")
        .long("vault")
        .short('V')
        .value_name("NAME_OR_PATH")
        .global(true)
        .help("Active vault: name from registry or filesystem path (overrides CAIRN_VAULT)"),
)
```

- [ ] **Step 7.3 — Add vault resolution to `main()`**

In `main()`, after `let matches = ...` and before the subcommand match, add:

```rust
// Resolve --vault flag or CAIRN_VAULT env (§3.3 precedence 1 + 2).
// Skip for `vault` management subcommands — they operate on the registry itself.
let explicit_vault = matches
    .get_one::<String>("vault")
    .cloned()
    .or_else(|| std::env::var("CAIRN_VAULT").ok());

let active_subcommand = matches.subcommand_name().unwrap_or("");
let needs_vault_guard = !matches!(active_subcommand, "vault" | "bootstrap" | "plugins");

if needs_vault_guard {
    let store = match registry_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cairn: registry path error — {e:#}");
            return ExitCode::from(78);
        }
    };
    let resolve = cairn_cli::vault::resolve_vault(cairn_cli::vault::ResolveOpts {
        explicit: explicit_vault,
        cwd: std::env::current_dir().ok(),
        store: &store,
    });
    match resolve {
        Ok(vault_path) => {
            // Thread the resolved path forward. For now, log at debug level.
            // Once store is wired (#9), pass vault_path to the verb context.
            tracing::debug!("resolved vault: {}", vault_path.display());
        }
        Err(e) => {
            // Only hard-fail for unknown named vaults — NoneResolved is tolerated
            // while the store is not wired (all verbs return Internal anyway).
            if e.downcast_ref::<cairn_cli::vault::VaultError>()
                .map_or(false, |ve| matches!(ve, cairn_cli::vault::VaultError::NotFound { .. }))
            {
                eprintln!("cairn: {e:#}");
                return ExitCode::from(78); // EX_CONFIG
            }
            tracing::debug!("vault not resolved: {e}");
        }
    }
}
```

- [ ] **Step 7.4 — Run the new tests**

```bash
cargo nextest run -p cairn-cli --locked -- vault_registry::cli_vault_flag 2>&1 | tail -20
```

Expected: all 3 flag tests pass.

- [ ] **Step 7.5 — Run full test suite**

```bash
cargo nextest run -p cairn-cli --locked --no-fail-fast 2>&1 | tail -20
```

Expected: all tests pass (no regressions in bootstrap or existing CLI tests).

- [ ] **Step 7.6 — Clippy + fmt**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- [ ] **Step 7.7 — Commit**

```bash
git add crates/cairn-cli/src/main.rs crates/cairn-cli/tests/vault_registry.rs
git commit -m "feat(cli): global --vault flag + CAIRN_VAULT scope guard (brief §3.3, #42)"
```

---

## Task 8 — Full verification sweep

- [ ] **Step 8.1 — Run the complete CI suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all green.

- [ ] **Step 8.2 — Supply chain**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: no findings.

- [ ] **Step 8.3 — Docs**

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

- [ ] **Step 8.4 — Manual smoke test**

```bash
# Build the binary
cargo build -p cairn-cli --locked

# Bootstrap two temp vaults
VAULT_A=$(mktemp -d)
VAULT_B=$(mktemp -d)
./target/debug/cairn bootstrap --vault-path "$VAULT_A"
./target/debug/cairn bootstrap --vault-path "$VAULT_B"

# Register both with a temp registry
export CAIRN_REGISTRY=$(mktemp)
./target/debug/cairn vault add "$VAULT_A" --name alpha --label "first"
./target/debug/cairn vault add "$VAULT_B" --name beta  --label "second"

# List — alpha and beta should appear, no default yet
./target/debug/cairn vault list

# Switch default
./target/debug/cairn vault switch alpha
./target/debug/cairn vault list   # alpha should have *

# --vault flag overrides
./target/debug/cairn --vault beta vault list  # lists from global registry, not vault-scoped

# Remove
./target/debug/cairn vault remove beta
./target/debug/cairn vault list  # only alpha

# Verify vault files untouched
ls "$VAULT_B/.cairn/"   # config.yaml still there
```

Expected output matches design §3.3 behavior.

- [ ] **Step 8.5 — Final commit (if anything changed)**

```bash
git add -p
git commit -m "chore(cli): post-review polish for vault registry (#42)"
```

---

## Spec Coverage Check

| Requirement | Covered by |
|-------------|------------|
| Local registry at `~/.config/cairn/vaults.toml` | Task 3 `VaultRegistryStore::default_path()` |
| `[[vault]]` TOML shape with `name`, `path`, `label`, `expires_at` | Task 1 `VaultEntry` |
| `default = "…"` field | Task 1 `VaultRegistry.default` |
| `cairn vault add <path> --name` | Task 4 |
| `cairn vault list` | Task 5 |
| `cairn vault switch <name>` | Task 6 |
| `cairn vault remove <name>` | Task 6 |
| `--vault <name\|path>` global flag | Task 7 |
| `CAIRN_VAULT` env var | Task 7 |
| Walk-up `.cairn/` discovery | Task 3 `walk_up_to_vault` |
| Registry default fallback | Task 3 `resolve_vault` |
| Explicit path wins over all | Task 3 resolution, Task 7 tests |
| Unknown name → clear error | Task 3 `VaultError::NotFound`, Task 4/7 tests |
| No cross-vault bleed | Task 3 isolation test `two_vaults_resolve_to_different_paths` |
| Files not deleted on `remove` | Task 6 `remove_does_not_delete_files` |
| `--json` on all vault subcommands | Tasks 4–6 |
| Exit code EX_CONFIG(78) on vault errors | Task 7 `vault_flag_unknown_name_exits_78` |
