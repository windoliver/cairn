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

**Additive, not replacing.** Existing `source_hash: String` stays. New
field `source_refs: Vec<SourceRef>` is added alongside it with a
deterministic ordering invariant so the scalar remains stable:

```rust
pub struct SourceRef {
    pub id: String,    // logical identifier, opaque to lint
    pub hash: String,  // "<algo>:<hex>", algo ∈ {sha256, sha512, blake3}
}

pub struct Provenance {
    // ... existing fields preserved verbatim ...
    pub source_hash: String,  // PRESERVED — primary source's hash (back-compat)

    #[serde(default)]
    pub source_refs: Vec<SourceRef>,  // NEW
}
```

Rationale & compatibility:
- `source_hash` stays on the wire so existing vaults, snapshots, and
  generated IDL keep parsing. Per CLAUDE.md §6.10: "adding a required
  field is a breaking change; use `#[serde(default)]` + optional fields
  for forward compat."
- `source_refs` is `#[serde(default)]` → records written before this
  change deserialize with an empty vec. The first re-`ingest` populates
  it.
- **Deterministic primary-source semantics.** `Provenance::validate()`
  enforces, when `source_refs` is non-empty:
  1. Entries are sorted ascending by `id` (lex order, byte-wise).
  2. `id`s are unique within the vector.
  3. `source_refs[0].hash == source_hash` (the "primary" is the
     lex-smallest id; this is deterministic and does not depend on
     ingest order).
  Reordering the vector is therefore not legal — old readers that still
  rely on the scalar see a fixed primary source identity rather than an
  arbitrary one.
- Empty `source_refs` is permitted at the type level — the lint rule
  (`source_link_present`) flags it. This lets the brief evolve toward
  `Vec<SourceRef>`-only later without a second wire break: when the
  brief is amended, remove the scalar in a follow-up versioned
  migration.

`SourceRef::validate()`:
- `id` non-empty, no leading `/`, no `..` segments, no embedded NUL.
  `id` is a **logical key**, not a filesystem path. Mapping `id → path`
  is the resolver's job (see Component 3) and depends on configured
  vault layout, not on this struct.
- `hash` matches `<algo>:<hex>` with correct hex length (reuse existing
  `validate_source_hash` logic from `provenance.rs`).

### Wire compatibility checklist
- IDL change is additive: `source_refs` is optional in the schema.
- Codegen-regen committed; existing serialized records (pre-change)
  deserialize cleanly under tests.
- `cargo run -p cairn-cli --bin cairn-docgen -- --write` rerun if
  user-facing docs reference `Provenance`.

## Components

### 1. Domain (`cairn-core`)
- `domain/source_ref.rs` — new module, `SourceRef` + `validate`.
- `domain/provenance.rs` — keep `source_hash`, ADD `source_refs:
  Vec<SourceRef>` with `#[serde(default)]`. Cross-field invariant in
  `validate()`: when non-empty, `source_refs[0].hash == source_hash`.
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
    /// Diagnostic path for finding messages. Implementation-defined —
    /// callers MUST NOT parse this. Lint uses it only as a hint for the
    /// operator (e.g. `expected_path` in `source_link_dangling`).
    fn locator(&self, id: &str) -> String;
}
```

**No hard-coded layout.** `SourceRef.id` is a logical identifier. The
resolver maps `id → bytes` using configured vault layout (per brief §3:
folder names like `sources/` are configurable; some vaults may use
`inbox/` or per-sensor subtrees). Lint never assumes a path scheme.

Filesystem impl lives in the vault adapter crate (likely `cairn-cli` or
a new `cairn-vault-fs`); it consults `VaultLayout` from config to
resolve `id`. Trait is read-only — there is no `write` method by
construction, and the CI grep gate (Component 12) enforces no write
syscalls inside `verbs/lint/`.

### 4. Consent journal extension

Per brief §5.6, forget severs the source-to-memory link and future
re-ingestion must skip previously-forgotten **targets by content-hash**.
Two distinct scopes are possible:

- **Source-scope forget** — operator forgets a source file outright;
  every record derived from those source bytes is dead. Replay-block
  key is the **source-bytes hash** (`SourceRef.hash` space).
- **Target-scope forget** — operator forgets one specific derived
  record while other records from the same source stay live. Replay-
  block key is the **target body hash** — a content-derived digest
  over the canonicalized record body. **Not** `RecordId`: today the
  store mints ULID-based ids on upsert, so the same content re-derived
  later would get a new `RecordId` and slip past the journal lookup.

Replay key: a new dedicated digest, distinct from the existing
`BodyHash` used for upsert idempotency.

**Framing.** The digest input is the canonical-JSON serialization of a
single tagged, versioned struct — never raw concatenation. This rules
out boundary-shift collisions:

```rust
#[derive(Serialize)]
struct ReplayHashInput<'a> {
    v: u32,                          // schema version (initially 1)
    domain: &'static str,            // const "cairn.replay_hash.v1"
    body: &'a CanonicalBody,
    source_refs: &'a [SourceRef],    // sorted, unique (per Provenance invariant)
    originating_agent_id: &'a Identity,
    source_sensor: &'a Identity,
}

// replay_hash = sha256(canonical_json(ReplayHashInput { ... }))
```

Canonical-JSON rules (locked in `pipeline::canonical`):
- Object keys lex-sorted.
- No insignificant whitespace.
- Numbers in shortest-round-trip form.
- UTF-8 NFC for strings.

A property test in `pipeline::canonical::replay_hash` exercises:
field permutation → canonical sort → identical hash; tampered byte →
different hash; round-trip stability across `serde_json` versions.

**Domain location:** `pipeline::canonical::replay_hash`. The body-only
`BodyHash` (already used for upsert idempotency) is intentionally NOT
reused — that hash collapses two distinct records with the same body
text but different provenance, agent, or sensor.

**Persistence:** `MemoryRecord` does NOT gain a persisted `replay_hash`
field. Computing it is cheap, and persisting it would create a sync-
drift hazard. Both `forget` and `lint` compute it on demand from the
in-hand record.

**Legacy records — target-scope forget gating.** The reviewer-flagged
hazard: a legacy record forgotten with empty `source_refs` produces
one replay-hash; the same record re-ingested later with populated
`source_refs` produces a different replay-hash; forget no longer
matches → privacy regression.

Resolution: **target-scope forget is gated on non-empty
`source_refs`.** `forget` rejects a target-scope op against a record
with empty `source_refs` and returns
`ForgetError::SourceRefsRequiredForTargetScope` with remediation
pointing the operator to re-ingest the source first. Source-scope
forget (no `target_replay_hash`) still works against any record —
it's keyed by source-bytes hash which is stable.

After re-ingest populates `source_refs`, target-scope forget on that
record becomes available. The lint rule `source_link_missing` already
flags empty `source_refs` as `error`, so existing vaults will be
nudged toward re-ingest as part of normal lint hygiene before any
target-scope forget op is needed.

`forget` flow (issue #58 scope, called out here for the contract):
  1. Loads the target record by `RecordId`.
  2. If target-scope and `source_refs.is_empty()`, returns the gated
     error above.
  3. Computes `replay_hash` from the (now-populated) record in hand.
  4. Writes the journal row with `target_replay_hash = Some(<hash>)`.

Consent-journal variant:

```rust
pub enum ConsentKind {
    // ... existing variants ...
    SourceForget {
        source_id: String,                  // logical id, operator diagnostics
        source_bytes_hash: String,          // "<algo>:<hex>" — same hash space as SourceRef.hash
        target_replay_hash: Option<String>, // Some → target-scope; None → source-scope
        op_id: String,                      // forget operation that produced this row
    },
}
```

`source_bytes_hash` and `SourceRef.hash` share a hash space — both
digest raw source bytes. `target_replay_hash` is in the
`pipeline::canonical::replay_hash` space (defined above). The two
spaces never mix.

Journal query helpers (all O(n) over journal rows; callers cache):
```rust
fn forgotten_source_bytes_hashes(&self) -> HashSet<&str>; // source-scope set
fn forgotten_target_replay_hashes(&self) -> HashSet<&str>; // target-scope set
fn forget_op_for_source(&self, hash: &str) -> Option<&str>;
fn forget_op_for_target_replay(&self, hash: &str) -> Option<&str>;
```

Lint rule `source_not_forgotten` runs both checks:
1. For each `record.provenance.source_refs[i].hash`, look up in
   `forgotten_source_bytes_hashes()`. Hit ⇒ emit `source_after_forget`
   with scope=source.
2. Compute `replay_hash` for the record (via
   `pipeline::canonical::replay_hash`), look up in
   `forgotten_target_replay_hashes()`. Hit ⇒ emit `source_after_forget`
   with scope=target.

This survives source rename/copy (same bytes, same hash), survives
`RecordId` regeneration (target keyed by replay-hash), works for
legacy records without backfill (forget recomputes replay-hash from
record in hand), and never over-blocks (replay-hash includes
provenance disambiguators, so distinct records with the same body but
different provenance/agent/sensor get different keys).

`source_id` stays on the row for operator-facing finding messages
(`forgotten source <id>`) but does not participate in dedup logic.

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

### 7. LintInputs — fail-closed wiring

`lint` in `cairn-core` takes the resolver and journal as trait
objects. **`cairn-core` does not construct adapter-backed impls** —
that would violate the workspace dep rule (CLAUDE.md §3: core has zero
deps on other workspace crates). Callers inject the impls:

```rust
// cairn-core public entrypoint — trait-object inputs, no Option.
pub fn lint(
    cfg: &CairnConfig,
    records: &[MemoryRecord],
    source_resolver: &dyn SourceResolver,
    consent_journal: &dyn ConsentJournalReader,
    // ... existing inputs ...
) -> LintReport;
```

Per-surface wiring (each surface constructs the impls it owns, then
passes them through):

| Surface | Resolver impl | Journal impl |
|---|---|---|
| `cairn-cli` (ground truth) | `VaultFsResolver` (from `cairn-cli` or new `cairn-vault-fs` adapter) reading the configured vault layout | `SqliteConsentJournalReader` (from `cairn-store-sqlite`) |
| `cairn-mcp` | Re-uses the CLI's wiring — MCP runs inside the CLI process (brief §6.12). MCP itself stays protocol-only. | Same. |
| `cairn-sdk` | **Caller-injected.** The SDK adds `lint(cfg, records, resolver, journal)` as a function on its typed surface, parameterized over the trait objects. The SDK does NOT construct filesystem or sqlite-backed impls — that would break the thin-wrapper invariant. The SDK ships a convenience helper that returns a config-driven resolver+journal when the caller passes a vault path, *implemented as an optional feature `vault-fs` that pulls in the adapter crates*. The default SDK build remains core-only. |

This keeps "CLI is ground truth" (CLAUDE.md §3) intact. SDK callers
who don't enable the `vault-fs` feature must inject their own
implementations — there is no silent fall-through to empty stubs in
production code.

For unit tests inside `cairn-core`, a `LintInputs` builder exposes
in-memory stubs (`InMemorySourceResolver`, `InMemoryConsentJournal`)
from `cairn-test-fixtures` (dev-dep only).

If an operator deploys without a `sources/` directory at all,
`SourceResolver::exists` returns `false` for every id; the rules then
emit `error`-severity `source_link_dangling` findings rather than
silently passing. Empty consent journal is fine — the rules iterate
zero rows and produce zero forget-related findings, which is correct
(nothing has been forgotten).

### 8. Rules (`crates/cairn-core/src/verbs/lint/checks/source_links.rs`)
One module, five public fns:

- `pub fn run_source_link_present(inputs) -> Vec<Finding>`
- `pub fn run_source_link_resolves(inputs) -> Vec<Finding>` — needs resolver
- `pub fn run_source_hash_match(inputs) -> Vec<Finding>` — needs resolver
- `pub fn run_source_not_forgotten(inputs) -> Vec<Finding>` — needs journal
- `pub fn run_source_redact_on_forget_honored(inputs) -> Vec<Finding>` — needs resolver + journal

Rule fns take `&dyn SourceResolver` / `&dyn ConsentJournalReader`
directly (not `Option`) per Component 7 fail-closed wiring. There is
no `DeferredCheck` info-finding path in production code.

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

1. IDL: add optional `source_refs` to `Provenance`. Codegen regen.
2. `SourceRef` domain module + `Provenance` additive field +
   cross-field invariant + tests (existing tests keep passing — that's
   the back-compat acceptance bar). No fixture rewrites required.
3. `SourceResolver` trait + `VaultLayout`-driven filesystem impl.
4. `ConsentKind::SourceForget { source_id, content_hash, target_id,
   op_id }` + journal query helpers.
5. `SourceConfig` + config-defaults regen.
6. `Kind` enum extension via IDL regen.
7. `source_links.rs` rules + dispatcher rewire in `checks/provenance.rs`.
8. Fixtures + snapshot tests + integration test.
9. CI gate script + workflow wiring.

Step 2 is the contract-touching step; it must remain back-compat by
construction (existing serialized records deserialize identically).
Steps 3–4 carry their own brief-touching contract changes (the
resolver trait is new; the consent kind is new). All other steps are
mechanical.

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

- IDL regen produces drift across crates. Mitigation: CI already gates
  on `cairn-codegen --check`.
- Cross-field invariant (`source_refs[0].hash == source_hash`) could
  surprise authors of synthetic records. Mitigation: documented in the
  domain module's rustdoc; the invariant only activates when
  `source_refs` is non-empty.
- Brief amendment outstanding: the brief still defines provenance as
  `{..., source_hash, ...}` (scalar). This spec adds `source_refs`
  additively without removing the scalar — once #257 lands, a follow-up
  PR updates `docs/design/design-brief.md` §6.5 to acknowledge the
  vector. A second future PR removes `source_hash` after a deprecation
  window, gated by a wire-version bump.
- Resolver locator strings are diagnostic-only. Tests must assert
  finding shape (presence of `source_id`, presence of human-readable
  locator) rather than exact path bytes, so the filesystem layout can
  vary across operator configurations without breaking snapshots.
