# Design: `cairn bootstrap` vault initialization

**Date:** 2026-04-26  
**Issue:** [#41](https://github.com/windoliver/cairn/issues/41)  
**Brief sections:** §3 Vault Layout, §3.1 Layout template, §19.a v0.1 subset  
**Status:** Approved

---

## Problem

`cairn bootstrap` currently only writes `.cairn/config.yaml`. It does not create the vault directory tree, does not emit a machine-readable receipt, and fails on a second invocation. Issue #41 requires full vault initialization: complete directory scaffold, idempotent behavior, and a structured receipt for SDK/MCP callers.

---

## Approach

New `cairn_cli::vault` module in `cairn-cli`. `BootstrapReceipt` lives in `cairn-cli` (management command, not a core verb; MCP wrapping of bootstrap is P1+). `config::write_default` is removed and replaced by `vault::bootstrap`.

---

## Module structure

```
cairn-cli/src/
├── config.rs          unchanged except write_default removed
├── vault.rs           NEW — bootstrap logic + receipt type
├── lib.rs             adds `pub mod vault;`
└── main.rs            bootstrap_subcommand gains --json + --force; run_bootstrap calls vault::bootstrap
```

---

## Types

```rust
// cairn-cli/src/vault.rs

pub struct BootstrapOpts {
    pub vault_path: PathBuf,
    pub force: bool,
}

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

pub fn bootstrap(opts: &BootstrapOpts) -> Result<BootstrapReceipt>
```

---

## Directory tree

All dirs created via `create_dir_all` (idempotent; no `--force` needed for dirs):

```
sources/articles/
sources/papers/
sources/transcripts/
sources/documents/
sources/chat/
sources/assets/
raw/
wiki/entities/
wiki/concepts/
wiki/summaries/
wiki/synthesis/
wiki/prompts/
skills/
.cairn/evolution/
.cairn/cache/
.cairn/models/
```

---

## Placeholder files

Created on first run; skipped on subsequent runs unless `--force`:

| File | Content | Owned by |
|---|---|---|
| `.cairn/config.yaml` | `CairnConfig::default()` serialized as YAML | human (config) |
| `purpose.md` | `# Purpose\n\n<!-- Why does this vault exist? -->\n` | human |
| `index.md` | empty | LLM |
| `log.md` | empty | LLM |

`.cairn/cairn.db` is **not** created by bootstrap (out of scope per issue; the store adapter owns DB initialization). The receipt reports its expected path regardless.

---

## Idempotency and exit behavior

| Scenario | Behavior | Exit code |
|---|---|---|
| First run, clean dir | Create all dirs + files; receipt shows full `dirs_created` / `files_created` | 0 |
| Second run, no `--force` | Dirs silently ensured; existing files go to `files_skipped`; receipt emitted | 0 |
| Second run, `--force` | Overwrites all placeholder files; dirs unchanged | 0 |
| Any I/O error | `eprintln!` error message | 74 (`EX_IOERR`) |

---

## CLI surface

```
cairn bootstrap [--vault-path PATH] [--json] [--force]
```

Human output (no `--json`):

```
cairn bootstrap: vault initialized at /path/to/vault
  config    /path/to/vault/.cairn/config.yaml  [created]
  db        /path/to/vault/.cairn/cairn.db  (created on first ingest)
  dirs      19 created, 0 existing
  files     4 created, 0 skipped
```

Second run (no `--force`):

```
cairn bootstrap: vault already initialized at /path/to/vault
  config    /path/to/vault/.cairn/config.yaml  [existing]
  db        /path/to/vault/.cairn/cairn.db  (created on first ingest)
  dirs      0 created, 19 existing
  files     0 created, 4 skipped
```

`--json` emits `BootstrapReceipt` as JSON to stdout.

---

## Tests

New integration test file: `crates/cairn-cli/tests/bootstrap.rs`

| Test | Asserts |
|---|---|
| `bootstrap_creates_full_tree` | All 14 dirs + 4 files exist after first run |
| `bootstrap_idempotent` | Second run exits 0; `files_skipped == 4`; no dirs/files destroyed |
| `bootstrap_force_overwrites_files` | `--force` re-creates placeholder files even when they exist |
| `bootstrap_skips_user_edited_purpose` | User edits `purpose.md`; second run without `--force` leaves content intact |
| `bootstrap_receipt_json` | `--json` emits valid JSON matching `BootstrapReceipt` shape |
| `bootstrap_reports_db_path` | Receipt `db_path` == `.cairn/cairn.db` regardless of whether file exists |

Existing `config.rs` tests updated: `bootstrap_fails_if_file_already_exists` changes to verify idempotent behavior (files skipped, not error). Snapshot test for human-readable stdout via `insta`.

---

## Invariants touched

- Invariant 3 (CLI is ground truth): `bootstrap` is a management command, not a core verb; logic lives in `cairn-cli`
- Invariant 4 (seven contracts, pure functions otherwise): no new contract added; `vault::bootstrap` is a plain function with `Result` return
- No WAL involvement (bootstrap is not a memory mutation)
