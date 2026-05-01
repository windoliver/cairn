# Bootstrap Vault Initialization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `cairn bootstrap` so it creates the full §3 vault directory tree, writes placeholder files idempotently, and emits a machine-readable `BootstrapReceipt`.

**Architecture:** New `cairn_cli::vault` module holds `BootstrapOpts`, `BootstrapReceipt`, `bootstrap()`, and `render_human()`. The existing `config::write_default` is removed; its callers and tests migrate to the new module. `main.rs` gains `--json` and `--force` flags on the `bootstrap` subcommand.

**Tech Stack:** Rust 1.95, `anyhow` (errors), `serde` + `serde_json` + `serde_yaml` (serialization), `insta` (snapshots), `tempfile` (test dirs), `cairn_core::config::CairnConfig` (default config).

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/cairn-cli/src/vault.rs` | **Create** | `BootstrapOpts`, `BootstrapReceipt`, `bootstrap()`, `render_human()` |
| `crates/cairn-cli/src/lib.rs` | **Modify** | add `pub mod vault;` |
| `crates/cairn-cli/src/config.rs` | **Modify** | remove `write_default`; keep `load` + `interpolate_env` |
| `crates/cairn-cli/src/main.rs` | **Modify** | add `--json` + `--force` to `bootstrap_subcommand`; update `run_bootstrap` |
| `crates/cairn-cli/tests/bootstrap.rs` | **Create** | all bootstrap integration tests |
| `crates/cairn-cli/tests/config.rs` | **Modify** | remove `write_default`-dependent tests (they move to `bootstrap.rs`) |
| `crates/cairn-cli/tests/snapshots/bootstrap_human_output.snap` | **Created by insta** | snapshot of human-readable receipt |

---

## Task 1: Create `vault.rs` with types and stub `bootstrap()`

**Files:**
- Create: `crates/cairn-cli/src/vault.rs`
- Modify: `crates/cairn-cli/src/lib.rs`

- [ ] **Step 1: Add `pub mod vault;` to `lib.rs`**

Open `crates/cairn-cli/src/lib.rs` and add the module declaration:

```rust
pub mod config;
pub mod plugins;
pub mod vault;
```

- [ ] **Step 2: Create `vault.rs` with types + stub**

Create `crates/cairn-cli/src/vault.rs`:

```rust
//! Vault initialization for `cairn bootstrap` (brief §3, §3.1).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;

use cairn_core::config::CairnConfig;

/// Options for [`bootstrap`].
pub struct BootstrapOpts {
    pub vault_path: PathBuf,
    pub force: bool,
}

/// Result of a bootstrap run, emitted as JSON with `--json` or formatted by
/// [`render_human`].
#[derive(Debug, Serialize)]
pub struct BootstrapReceipt {
    pub vault_path: PathBuf,
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub dirs_created: Vec<PathBuf>,
    pub dirs_existing: Vec<PathBuf>,
    pub files_created: Vec<PathBuf>,
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
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p cairn-cli --locked
```

Expected: no errors. If `serde_yaml` is missing from deps, it is already in `cairn-cli/Cargo.toml` — verify with `grep serde_yaml crates/cairn-cli/Cargo.toml`.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/vault.rs crates/cairn-cli/src/lib.rs
git commit -m "feat(bootstrap): add vault module with BootstrapOpts/Receipt types and bootstrap() (§3.1)"
```

---

## Task 2: TDD — directory creation

**Files:**
- Create: `crates/cairn-cli/tests/bootstrap.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-cli/tests/bootstrap.rs`:

```rust
//! Integration tests for vault bootstrap (brief §3.1, issue #41).

use std::path::Path;

use cairn_cli::vault::{BootstrapOpts, bootstrap};

fn opts(dir: &Path) -> BootstrapOpts {
    BootstrapOpts { vault_path: dir.to_path_buf(), force: false }
}

fn forced(dir: &Path) -> BootstrapOpts {
    BootstrapOpts { vault_path: dir.to_path_buf(), force: true }
}

#[test]
fn bootstrap_creates_full_tree() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    let expected_dirs = [
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
    for rel in &expected_dirs {
        assert!(
            dir.path().join(rel).is_dir(),
            "expected dir missing: {rel}"
        );
    }
}

#[test]
fn bootstrap_receipt_counts_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(receipt.dirs_created.len(), 19, "first run: all 19 dirs should be created");
    assert_eq!(receipt.dirs_existing.len(), 0);
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked 2>&1 | head -30
```

Expected: compilation error — `tests/bootstrap.rs` doesn't exist yet (this step creates it) or type errors.

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked
```

Expected: both tests PASS (the implementation was written in Task 1).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/tests/bootstrap.rs
git commit -m "test(bootstrap): directory creation + receipt dir counts"
```

---

## Task 3: TDD — placeholder files, idempotency, force

**Files:**
- Modify: `crates/cairn-cli/tests/bootstrap.rs`

- [ ] **Step 1: Add tests**

Append to `crates/cairn-cli/tests/bootstrap.rs`:

```rust
#[test]
fn bootstrap_creates_placeholder_files() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    assert!(dir.path().join(".cairn/config.yaml").is_file(), "config.yaml missing");
    assert!(dir.path().join("purpose.md").is_file(), "purpose.md missing");
    assert!(dir.path().join("index.md").is_file(), "index.md missing");
    assert!(dir.path().join("log.md").is_file(), "log.md missing");
}

#[test]
fn bootstrap_receipt_counts_files() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(receipt.files_created.len(), 4);
    assert_eq!(receipt.files_skipped.len(), 0);
}

#[test]
fn bootstrap_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // second run
    let receipt = bootstrap(&opts(dir.path())).unwrap();

    // dirs: all existing, none created
    assert_eq!(receipt.dirs_created.len(), 0);
    assert_eq!(receipt.dirs_existing.len(), 19);

    // files: all skipped, none created
    assert_eq!(receipt.files_created.len(), 0);
    assert_eq!(receipt.files_skipped.len(), 4);

    // vault is still intact
    assert!(dir.path().join(".cairn/config.yaml").is_file());
    assert!(dir.path().join("purpose.md").is_file());
}

#[test]
fn bootstrap_skips_user_edited_purpose() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // user edits purpose.md
    let purpose = dir.path().join("purpose.md");
    std::fs::write(&purpose, "# My vault\n\nPersonal knowledge base.\n").unwrap();

    // second run without --force
    bootstrap(&opts(dir.path())).unwrap();

    // user's content must survive
    let content = std::fs::read_to_string(&purpose).unwrap();
    assert_eq!(content, "# My vault\n\nPersonal knowledge base.\n");
}

#[test]
fn bootstrap_force_overwrites_files() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    // user edits purpose.md
    let purpose = dir.path().join("purpose.md");
    std::fs::write(&purpose, "# My vault\n\nPersonal knowledge base.\n").unwrap();

    // --force run
    let receipt = bootstrap(&forced(dir.path())).unwrap();

    // purpose.md is overwritten with the template
    let content = std::fs::read_to_string(&purpose).unwrap();
    assert_eq!(content, "# Purpose\n\n<!-- Why does this vault exist? -->\n");

    // receipt shows all 4 files created
    assert_eq!(receipt.files_created.len(), 4);
    assert_eq!(receipt.files_skipped.len(), 0);
}
```

- [ ] **Step 2: Run to verify they pass**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked
```

Expected: all tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/bootstrap.rs
git commit -m "test(bootstrap): placeholder files, idempotency, --force behavior"
```

---

## Task 4: TDD — receipt fields + JSON output

**Files:**
- Modify: `crates/cairn-cli/tests/bootstrap.rs`

- [ ] **Step 1: Add tests**

Append to `crates/cairn-cli/tests/bootstrap.rs`:

```rust
#[test]
fn bootstrap_reports_db_path() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(receipt.db_path, dir.path().join(".cairn/cairn.db"));
    // db is NOT created by bootstrap — only its path is reported
    assert!(!dir.path().join(".cairn/cairn.db").exists());
}

#[test]
fn bootstrap_receipt_serializes_to_json() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    let json = serde_json::to_string(&receipt).expect("receipt must serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("vault_path").is_some());
    assert!(parsed.get("config_path").is_some());
    assert!(parsed.get("db_path").is_some());
    assert!(parsed.get("dirs_created").is_some());
    assert!(parsed.get("dirs_existing").is_some());
    assert!(parsed.get("files_created").is_some());
    assert!(parsed.get("files_skipped").is_some());
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked
```

Expected: PASS. If `serde_json` isn't in test scope, add it to `[dev-dependencies]` in `crates/cairn-cli/Cargo.toml` — it is already present there.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/bootstrap.rs
git commit -m "test(bootstrap): db_path field + JSON serialization of receipt"
```

---

## Task 5: TDD — `render_human` snapshot

**Files:**
- Modify: `crates/cairn-cli/tests/bootstrap.rs`

- [ ] **Step 1: Add snapshot test**

Append to `crates/cairn-cli/tests/bootstrap.rs`:

```rust
#[test]
fn bootstrap_human_output_first_run() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let output = cairn_cli::vault::render_human(&receipt);
    // normalize absolute path so the snapshot is stable across machines
    let normalized = output.replace(dir.path().to_str().unwrap(), "<vault>");
    insta::assert_snapshot!(normalized);
}

#[test]
fn bootstrap_human_output_second_run() {
    let dir = tempfile::tempdir().unwrap();
    cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let receipt = cairn_cli::vault::bootstrap(&opts(dir.path())).unwrap();
    let output = cairn_cli::vault::render_human(&receipt);
    let normalized = output.replace(dir.path().to_str().unwrap(), "<vault>");
    insta::assert_snapshot!(normalized);
}
```

Also add `insta` to the imports at the top of the test file (it is already a dev-dep in `Cargo.toml`).

- [ ] **Step 2: Run to generate snapshots**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked 2>&1 | tail -20
```

Expected: tests fail with "snapshot not found" — that is correct for a first insta run.

- [ ] **Step 3: Review and accept snapshots**

```bash
cargo insta review
```

Review the two snapshots. First-run snapshot should look like:

```
cairn bootstrap: vault initialized at <vault>
  config    <vault>/.cairn/config.yaml  [created]
  db        <vault>/.cairn/cairn.db  (created on first ingest)
  dirs      19 created, 0 existing
  files     4 created, 0 skipped
```

Second-run snapshot should look like:

```
cairn bootstrap: vault already initialized at <vault>
  config    <vault>/.cairn/config.yaml  [existing]
  db        <vault>/.cairn/cairn.db  (created on first ingest)
  dirs      0 created, 19 existing
  files     0 created, 4 skipped
```

Accept both with `a`.

- [ ] **Step 4: Run again to confirm snapshots pass**

```bash
cargo nextest run -p cairn-cli --test bootstrap --locked
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/tests/bootstrap.rs crates/cairn-cli/tests/snapshots/
git commit -m "test(bootstrap): snapshot tests for render_human first + second run"
```

---

## Task 6: Update `main.rs` — `--json` + `--force` flags

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Write E2E test in `tests/cli.rs`**

Open `crates/cairn-cli/tests/cli.rs` and append:

```rust
#[test]
fn bootstrap_emits_json_with_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .args(["bootstrap", "--vault-path", dir.path().to_str().unwrap(), "--json"])
        .output()
        .expect("cairn bootstrap --json");
    assert!(out.status.success(), "exit: {:?}\nstderr: {}", out.status, String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect("--json must emit valid JSON");
    assert!(parsed.get("vault_path").is_some(), "JSON missing vault_path");
    assert!(parsed.get("dirs_created").is_some(), "JSON missing dirs_created");
}

#[test]
fn bootstrap_force_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    // first run
    cli()
        .args(["bootstrap", "--vault-path", dir.path().to_str().unwrap()])
        .output().unwrap();
    // second run with --force must succeed
    let out = cli()
        .args(["bootstrap", "--vault-path", dir.path().to_str().unwrap(), "--force"])
        .output()
        .expect("cairn bootstrap --force");
    assert!(out.status.success(), "exit: {:?}", out.status);
}

#[test]
fn bootstrap_io_error_exits_74() {
    // Point at a path we cannot write to — use a file as the vault path so
    // create_dir_all fails.
    let file = tempfile::NamedTempFile::new().unwrap();
    let out = cli()
        .args(["bootstrap", "--vault-path", file.path().to_str().unwrap()])
        .output()
        .expect("cairn bootstrap <file-as-vault>");
    assert_eq!(out.status.code(), Some(74), "expected EX_IOERR(74)");
}
```

Also add `use tempfile;` / ensure `tempfile` is in scope (it is a dev-dep in `Cargo.toml`).

- [ ] **Step 2: Run to confirm tests fail**

```bash
cargo nextest run -p cairn-cli --test cli bootstrap --locked 2>&1 | tail -20
```

Expected: tests fail — `--json` and `--force` flags are unknown (clap rejects them).

- [ ] **Step 3: Update `bootstrap_subcommand` in `main.rs`**

Replace the existing `bootstrap_subcommand` function:

```rust
fn bootstrap_subcommand() -> clap::Command {
    clap::Command::new("bootstrap")
        .about("Initialize a vault directory tree with the §3 layout")
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .help("Vault root directory (default: current directory)"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON receipt instead of human-readable output"),
        )
        .arg(
            clap::Arg::new("force")
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("Overwrite existing placeholder files"),
        )
}
```

- [ ] **Step 4: Update `run_bootstrap` in `main.rs`**

Replace the existing `run_bootstrap` function:

```rust
fn run_bootstrap(matches: &ArgMatches) -> ExitCode {
    let vault_path = std::path::PathBuf::from(
        matches
            .get_one::<String>("vault-path")
            .expect("invariant: vault-path has a default value"),
    );
    let json = matches.get_flag("json");
    let force = matches.get_flag("force");

    let opts = cairn_cli::vault::BootstrapOpts { vault_path, force };

    match cairn_cli::vault::bootstrap(&opts) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: BootstrapReceipt is always serializable")
                );
            } else {
                println!("{}", cairn_cli::vault::render_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn bootstrap: {e:#}");
            ExitCode::from(74) // EX_IOERR
        }
    }
}
```

- [ ] **Step 5: Run to confirm tests pass**

```bash
cargo nextest run -p cairn-cli --test cli --locked
```

Expected: all CLI tests PASS including the three new ones.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/main.rs crates/cairn-cli/tests/cli.rs
git commit -m "feat(bootstrap): --json + --force flags; EX_IOERR(74) on I/O failure"
```

---

## Task 7: Remove `write_default`, migrate stale tests

**Files:**
- Modify: `crates/cairn-cli/src/config.rs`
- Modify: `crates/cairn-cli/tests/config.rs`

- [ ] **Step 1: Remove `write_default` from `config.rs`**

Delete the `write_default` function (lines 98–126 in the current file) and its doc comment. Keep `load`, `interpolate_env`, `CliOverrides`, and all their tests.

- [ ] **Step 2: Remove bootstrap tests from `tests/config.rs`**

Delete the three tests under `// ── Bootstrap ───`:
- `bootstrap_writes_config_file`
- `bootstrap_round_trips_to_default`
- `bootstrap_fails_if_file_already_exists`

These behaviors are now covered by `tests/bootstrap.rs`.

Add one replacement test in `tests/config.rs` that verifies `load()` round-trips the config written by `vault::bootstrap`:

```rust
#[test]
fn bootstrap_config_round_trips() {
    use cairn_cli::vault::{BootstrapOpts, bootstrap};
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&BootstrapOpts { vault_path: dir.path().to_path_buf(), force: false }).unwrap();
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config, cairn_core::config::CairnConfig::default());
}
```

- [ ] **Step 3: Remove `write_default` from the machete ignore list if present**

Check `crates/cairn-cli/Cargo.toml` — it currently ignores `"anyhow"` via `cargo-machete`. If `write_default` was the last user of `anyhow` in `config.rs`, verify `anyhow` is still used elsewhere in `lib.rs`/`vault.rs` (it is — `vault.rs` uses `anyhow::Context`). No change needed.

- [ ] **Step 4: Run full test suite**

```bash
cargo nextest run -p cairn-cli --locked
cargo test --doc -p cairn-cli --locked
```

Expected: all tests PASS; no reference to `write_default` remains.

- [ ] **Step 5: Run verification checklist**

```bash
cargo fmt --all --check
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
./scripts/check-core-boundary.sh
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/config.rs crates/cairn-cli/tests/config.rs
git commit -m "refactor(config): remove write_default; bootstrap tests live in bootstrap.rs"
```

---

## Task 8: Full workspace verification

- [ ] **Step 1: Run the full CI checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all pass with no warnings promoted to errors.

- [ ] **Step 2: Smoke-test the binary manually**

```bash
cd /tmp && mkdir test-vault && cargo run -p cairn-cli -- bootstrap --vault-path test-vault
cargo run -p cairn-cli -- bootstrap --vault-path test-vault          # second run — must succeed
cargo run -p cairn-cli -- bootstrap --vault-path test-vault --json   # JSON output
cargo run -p cairn-cli -- bootstrap --vault-path test-vault --force  # force overwrite
find test-vault -type d | sort
```

Expected: 19 dirs visible; second run prints "vault already initialized"; JSON is valid.

- [ ] **Step 3: Commit if anything needed fixing**

If any clippy or fmt fix was needed:
```bash
git add -p
git commit -m "fix(bootstrap): clippy/fmt cleanup"
```
