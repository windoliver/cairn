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
    reg.vaults.push(VaultEntry::new("work", "/tmp/work", Some("day job".into()), None));
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
    reg.vaults.push(VaultEntry::new(
        "myvault",
        vault_dir.path().to_str().unwrap(),
        None,
        None,
    ));
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
    reg.vaults.push(VaultEntry::new(
        "home",
        vault_dir.path().to_str().unwrap(),
        None,
        None,
    ));
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
    reg.vaults.push(VaultEntry::new(
        "alpha",
        vault_a.path().to_str().unwrap(),
        None,
        None,
    ));
    reg.vaults.push(VaultEntry::new(
        "beta",
        vault_b.path().to_str().unwrap(),
        None,
        None,
    ));
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
