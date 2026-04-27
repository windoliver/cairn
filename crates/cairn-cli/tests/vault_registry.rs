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

mod cli_vault_list {
    use std::process::Command;
    use cairn_cli::vault::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    fn cairn() -> Command {
        Command::new(env!("CARGO_BIN_EXE_cairn"))
    }

    fn reg_with_two_vaults() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        let a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".cairn")).unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
        let mut reg = VaultRegistry::default();
        reg.default = Some("alpha".into());
        reg.vaults.push(VaultEntry::new("alpha", a.path().to_str().unwrap(), Some("first vault".into()), None));
        reg.vaults.push(VaultEntry::new("beta", b.path().to_str().unwrap(), None, None));
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
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // alpha is the default in reg_with_two_vaults(); beta is not
        assert_eq!(arr[0]["name"], "alpha");
        assert!(
            arr[0]["is_default"].as_bool().unwrap_or(false),
            "alpha should be is_default=true"
        );
        assert_eq!(arr[1]["name"], "beta");
        assert!(
            !arr[1]["is_default"].as_bool().unwrap_or(true),
            "beta should be is_default=false"
        );
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

    #[test]
    fn list_human_output_snapshot() {
        let a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join(".cairn")).unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join(".cairn")).unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let store = VaultRegistryStore::new(reg_dir.path().join("vaults.toml"));
        let mut reg = VaultRegistry::default();
        reg.default = Some("work".into());
        reg.vaults.push(cairn_core::config::VaultEntry::new(
            "work",
            "/home/alice/vaults/work",
            Some("day job".into()),
            None,
        ));
        reg.vaults.push(cairn_core::config::VaultEntry::new(
            "personal",
            "/home/alice/vaults/personal",
            None,
            None,
        ));
        store.save(&reg).unwrap();

        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
            .env("CAIRN_REGISTRY", reg_dir.path().join("vaults.toml").to_str().unwrap())
            .args(["vault", "list"])
            .output()
            .expect("cairn vault list");
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8(out.stdout).unwrap();
        insta::assert_snapshot!(stdout);
    }
}
