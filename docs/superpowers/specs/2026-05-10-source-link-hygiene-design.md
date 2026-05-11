# Source-link hygiene for provenance lint (#257)

Date: 2026-05-10
Issue: [#257](https://github.com/windoliver/cairn/issues/257)
Brief refs: §3 (vault), §5.6 (forget), §6.5 (provenance)

## Goal

Turn brief §6.5's "provenance is mandatory" into five runtime-checkable
lint rules over a real source-link data model. Replace the current
placeholder check that just emits a `DeferredCheck` pointing at this
issue.

## Data-model decision

Replace `Provenance.source_hash: String` (single) with:

```rust
pub struct SourceRef {
    pub id: String,    // path-relative key under <vault>/sources/
    pub hash: String,  // "<algo>:<hex>", algo ∈ {sha256, sha512, blake3}
}

pub struct Provenance {
    // ... existing fields ...
    pub source_refs: Vec<SourceRef>,  // was: source_hash: String
}
```

Rationale: the `source_hash_match` rule needs a per-source hash; parallel
`source_ids` / `source_hashes` arrays are a known antipattern; the brief
treats provenance as a set without committing to scalar.

`SourceRef::validate()`:
- `id` non-empty, no leading `/`, no `..` segments.
- `hash` matches `<algo>:<hex>` with correct hex length (reuse existing
  `validate_source_hash` logic from `provenance.rs`).

`Provenance::validate()` accepts empty `source_refs` at the type level —
the lint rule (not the domain validator) enforces non-empty, because
historical fixtures and synthetic records may legitimately have zero
sources.

## Components

### 1. Domain (`cairn-core`)
- `domain/source_ref.rs` — new module, `SourceRef` + `validate`.
- `domain/provenance.rs` — drop `source_hash`, add `source_refs: Vec<SourceRef>`.
- `domain/error.rs` — no new variants; reuse `MissingProvenance { field }`
  and `InvalidIdentity`.

### 2. IDL + codegen
- Update `cairn-idl` schema for `Provenance`.
- Run `cargo run -p cairn-idl --bin cairn-codegen` and commit regenerated
  `crates/cairn-core/src/generated/`.

### 3. Source resolver (`cairn-core::contract::source_resolver`)
```rust
pub trait SourceResolver {
    fn exists(&self, id: &str) -> bool;
    fn read(&self, id: &str) -> Result<Vec<u8>, SourceResolverError>;
    fn vault_path(&self, id: &str) -> PathBuf;  // for error messages
}
```
Filesystem impl in `cairn-cli` (or `cairn-test-fixtures` for tests):
reads `<vault>/sources/<id>`. Trait is read-only — there is no `write`.

### 4. Consent journal extension
- `domain/consent.rs`: add `ConsentKind::SourceForget` variant carrying
  `{ source_id: String, op_id: String }`.
- `contract::consent_journal` (or wherever the journal query lives):
  add `fn forgotten_source_ids(&self) -> HashSet<String>` default impl
  iterating over rows.

### 5. Config
- `config/mod.rs`: add `SourceConfig { redact_on_forget: bool }` under
  `CairnConfig.source`. Default `false`. `serde(default)`.

### 6. Lint Kind enum
Extend generated `Kind`:
- `SourceLinkMissing`
- `SourceLinkDangling`
- `SourceHashMismatch`
- `SourceAfterForget`
- `SourceRedactSkipped`

Removing `DeferredCheck`-emitting path from `checks/provenance.rs` is in
scope (no longer needed). Keep `DeferredCheck` variant itself for future
use.

### 7. LintInputs
Extend `LintInputs<'a>`:
```rust
pub source_resolver: Option<&'a dyn SourceResolver>,
pub consent_journal: Option<&'a dyn ConsentJournalReader>,
```
Optional so unit tests can run without filesystem.

### 8. Rules (`crates/cairn-core/src/verbs/lint/checks/source_links.rs`)
One module, five public fns:

- `pub fn run_source_link_present(inputs) -> Vec<Finding>`
- `pub fn run_source_link_resolves(inputs) -> Vec<Finding>` — needs resolver
- `pub fn run_source_hash_match(inputs) -> Vec<Finding>` — needs resolver
- `pub fn run_source_not_forgotten(inputs) -> Vec<Finding>` — needs journal
- `pub fn run_source_redact_on_forget_honored(inputs) -> Vec<Finding>` — needs resolver + journal

Resolver/journal-dependent rules emit one info `DeferredCheck` finding
when their dependency is absent, so lint stays a no-op when called
without filesystem.

`run` in `checks/provenance.rs` becomes a thin dispatcher calling the
five new fns (preserves the existing entrypoint name in `lint/mod.rs`).

### 9. Hash recomputation
Helper in `cairn-core::pipeline::hash`:
```rust
pub fn recompute(bytes: &[u8], algo: &str) -> Option<String>;
```
Supports `sha256`, `sha512`, `blake3` (deps already in tree via
`cairn-core`'s existing `source_hash` validation neighbours — verify
during impl).

Binary-safe (no line-ending normalization). Per-issue note: line-ending
tolerance for text sources is out of scope for v0.1 — sources are
immutable per §3, so any whitespace edit is a real mismatch.

### 10. Fixtures
Under `crates/cairn-test-fixtures/src/source_links/`:
- `clean/` — one record + matching source, zero findings.
- `empty_source_refs/` — record with `source_refs: []`.
- `dangling/` — record references missing file.
- `hash_mismatch/` — source mutated post-ingest.
- `forgotten_still_referenced/` — `consent_journal` row + active record.
- `redact_skipped/` — `redact_on_forget: true` config, full bytes remain.

### 11. Tests
- Unit: per-rule, in-memory resolver, table-driven via `rstest`.
- Integration: `tempfile::tempdir()`, seed via `ingest`, mutate
  `sources/`, run `lint`, assert exact finding set.
- Snapshot: `insta` for `Vec<Finding>` JSON per fixture.
- Property: hash recomputation round-trip (any bytes → recompute →
  matches `Provenance.source_refs[i].hash` after `ingest`).

### 12. CI gate
`scripts/check-lint-readonly-sources.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
hits=$(grep -rEn 'fs::write|OpenOptions::new\(\).*\.write\(true\)|File::create' \
  crates/cairn-core/src/verbs/lint/ || true)
if [ -n "$hits" ]; then
  echo "lint must never open source files for write:"
  echo "$hits"
  exit 1
fi
```
Wired into `ci.yml` alongside `check-core-boundary.sh`.

## Migration steps (commit-by-commit)

1. IDL change + codegen regen.
2. `SourceRef` domain + `Provenance` field swap. Fix every compile error
   (fixtures, ingest test bodies, snapshot outputs). One commit, but
   expect ~15 files touched.
3. `SourceResolver` trait + filesystem impl in `cairn-cli`.
4. `ConsentKind::SourceForget` + journal query helper.
5. `SourceConfig` + config-defaults regen.
6. `Kind` enum extension via IDL regen.
7. `source_links.rs` rules + dispatcher rewire in `checks/provenance.rs`.
8. Fixtures + snapshot tests + integration test.
9. CI gate script + workflow wiring.

## Out of scope

- Auto-repair / re-ingest. Belongs to future `cairn ingest --resync`.
- Source-file *signatures* (P1+, when `source_signature` becomes
  mandatory).
- Cross-vault dedup (P2 federation).
- Text-mode line-ending tolerance.

## Acceptance mapping

| Issue acceptance | Fixture |
|---|---|
| Empty `source_ids` caught | `empty_source_refs/` |
| Deleted source caught (dangling) | `dangling/` |
| Edited source caught (hash mismatch) | `hash_mismatch/` |
| Active record after forget caught | `forgotten_still_referenced/` |
| Redact-on-forget violation caught | `redact_skipped/` |
| Finding has `source_id` + `expected_path` | Finding struct fields |
| Clean vault → zero findings | `clean/` |

## Risks

- Big mechanical diff on `Provenance` field swap. Mitigation: do step 2
  in one commit, no other logic changes mixed in.
- IDL regen produces drift across crates. Mitigation: CI already gates
  on `cairn-codegen --check`.
- Existing `Provenance::validate` tests assume scalar `source_hash` —
  rewrite, don't extend (test names will change).
