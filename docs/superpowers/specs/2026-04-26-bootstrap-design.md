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

---

## Amendment 2026-04-27 — vault id + identity-aware re-bootstrap guard (issue #50)

Issue #50 (`Provision local human, agent, and sensor identities`) extends
this design with the minimum surface bootstrap needs in order to support
keychain-namespaced identity provisioning. The amendment is additive — it
does not change any of the dirs/files listed in the original tables — and
is the **only** load-bearing change to the bootstrap contract for #50.
The full identity design lives in
`2026-04-27-issue-50-identity-provisioning-design.md`; this section
records the bootstrap-side delta so the two specs cannot drift.

### New artefacts

| File | Content | Owned by |
|---|---|---|
| `.cairn/vault.id` | A single line containing a freshly minted ULID. | bootstrap (mints once; preserved on every subsequent run) |

`vault.id` joins `.cairn/config.yaml` in the placeholder-files table.
First run mints; subsequent runs preserve. `--force` does **not** rewrite
`vault.id` — see "Re-bootstrap guard" below.

### `BootstrapReceipt` change

`BootstrapReceipt` gains one field:

```rust
pub struct BootstrapReceipt {
    // ... existing fields ...
    pub vault_id: VaultId,        // NEW — always populated, even on idempotent runs
}
```

The human-readable output gains one line under the `config` block:

```
  vault id  01HV6N5K7Q9R3F8E2W7B4M5Z9X
```

### Re-bootstrap guard (DB-aware, fail-closed)

`vault.id` is non-regenerable once the vault has bound itself to the OS
keychain (every keychain entry is namespaced under `cairn:<vault_id>`,
so reminting a new id would orphan every key). Bootstrap therefore
performs a **read-only** SQLite probe before considering reminting and
**never repairs `.cairn/vault.id` itself** — repair is the job of the
dedicated `cairn identity vault-id-recover` command (identity spec
§3.7), which is the single authoritative recovery surface.

Bootstrap's only job here is to refuse to mint a fresh id when binding
already exists. Decision tree, in order:

1. **`.cairn/vault.id` present.** Parse it, then cross-check against
   **vault-local** evidence only (no keystore reads — bootstrap stays
   filesystem + read-only-SQLite):
   - If `.cairn/vault.binding.pending` exists, fail closed
     immediately with `BootstrapError::FirstBindInProgress` (mapped
     to `EX_TEMPFAIL = 75`). First-bind is mid-sequence; bootstrap
     must not race `finalise-binding`.
   - If `.cairn/cairn.db` exists and `vault_meta` has a row,
     compare the file's id against `vault_meta.vault_id`. On
     mismatch, fail closed with `BootstrapError::VaultIdConflict {
     file_id, db_id }` (mapped to `EX_DATAERR = 65`); recovery
     routes through `cairn identity vault-id-recover`.
   The DB row is the durable authority once first-bind has
   committed; the filesystem copy is a hint that the DB can
   override. Bootstrap intentionally does **not** read the OS
   keychain, does **not** enumerate namespaces, and does **not**
   compare hashes against the keychain witness — those checks live
   in `IdentityService::open()` (which all issuer-dependent verbs
   pass through) and in the explicit `vault-id-recover` /
   `finalise-binding` recovery commands. This keeps bootstrap a
   filesystem-only path that runs on locked / unavailable / headless
   keychain environments without availability regression.
   `IdentityService::open()` performs the keychain witness
   cross-check the next time an identity-using verb runs, so a
   silently-forked binding is still caught — just at the issuer
   boundary, not at bootstrap. If `vault_meta` is absent or the DB
   is absent, the file is taken as-is. Continue.
2. **`.cairn/vault.id` missing AND `.cairn/vault.binding` (or
   `.cairn/vault.binding.pending`) present.** Fail closed with
   `BootstrapError::VaultIdLost` (mapped to `EX_DATAERR = 65`). The
   error hint instructs the operator to run
   `cairn identity vault-id-recover` (which can also re-write the
   binding sentinel from `vault_meta.witness_sha256` — see identity
   spec §3.10). Bootstrap does **not** silently restore either file.
3. **`.cairn/vault.id` missing AND no binding sentinel.** Open
   `.cairn/cairn.db` **read-only** (`?mode=ro&immutable=0`) and
   `SELECT vault_id FROM vault_meta LIMIT 1`. `vault_meta` is created
   by identity migration `0002_identity.sql`;
   `IdentityRegistry::reserve_first_identity` is the only writer.
   - **Row present:** the SQLite half of first-bind committed but the
     filesystem artefacts are gone. Fail closed with
     `BootstrapError::VaultIdLost`; recovery again routes through
     `cairn identity vault-id-recover`. Bootstrap does **not** write
     `.cairn/vault.id`.
   - **Row absent (table exists, no rows):** no first-bind has run yet;
     mint a fresh ULID and write `.cairn/vault.id`.
   - **Table missing or DB file missing:** identity migrations have
     not run yet; mint a fresh ULID and write `.cairn/vault.id`.
   - **Any other error** (DB present, schema readable, but the SELECT
     fails — corruption, locked, permission denied, etc.): fail closed
     with `BootstrapError::VaultStateUnreadable` mapped to
     `EX_DATAERR = 65`. Do **not** mint a new id under ambiguity.

This split keeps bootstrap minimal (mint-or-refuse) and concentrates
all keychain-binding repair behind `vault-id-recover`, which is
audited, ack-gated, and aware of the full keychain/DB/sentinel
trifecta. Bootstrap can never silently mutate trust state.

### `--force` semantics

`--force` continues to overwrite `purpose.md`, `index.md`, and `log.md`.
It does **not** overwrite `.cairn/vault.id` or `.cairn/config.yaml`
under any circumstance once `.cairn/vault.id` exists or any binding
artefact (`.cairn/vault.binding`, `vault_meta` row) exists. There is
no bootstrap path that rebinds a vault to a fresh keychain namespace.
To abandon a half-completed first-bind (the only legitimate path that
detaches a vault from a partially established namespace), the operator
runs `cairn identity finalise-binding --abandon --vault-id <id>` per
identity spec §3.7. That flow has its own audit gating and is the
single authoritative abandon surface — bootstrap delegates rather than
duplicating.

### New tests

Added to `crates/cairn-cli/tests/bootstrap.rs`:

| Test | Asserts |
|---|---|
| `bootstrap_mints_vault_id_first_run` | `.cairn/vault.id` exists, valid ULID, receipt populates `vault_id` |
| `bootstrap_preserves_vault_id` | Second run leaves `.cairn/vault.id` byte-identical |
| `bootstrap_fails_closed_when_vault_id_lost_with_db_row` | Delete `.cairn/vault.id` while `vault_meta` row + `.cairn/vault.binding` exist; bootstrap exits `65` with `VaultIdLost`; no files written |
| `bootstrap_fails_closed_when_vault_id_lost_with_sentinel_only` | Delete `.cairn/vault.id` while `.cairn/vault.binding` exists (no DB yet); bootstrap exits `65`; no files written |
| `bootstrap_mints_when_no_binding_artefacts` | Delete `.cairn/vault.id`, no `vault_meta` row, no binding sentinel; bootstrap mints a fresh ULID |
| `bootstrap_fails_closed_on_unreadable_db` | Corrupt `.cairn/cairn.db` (truncate header), run bootstrap; exits `65`; `.cairn/vault.id` not touched |
| `bootstrap_force_does_not_rewrite_vault_id` | `--force` after first-bind leaves `vault.id` intact |
