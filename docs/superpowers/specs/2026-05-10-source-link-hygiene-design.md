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

// replay_hash = "sha256:" + hex(sha256(canonical_json(ReplayHashInput { ... })))
```

**Encoder is hand-rolled, not `serde_json`.** The spec rules out
delegating canonicalization to a third-party serializer whose output
may shift across versions. `pipeline::canonical::encode` is a small
deterministic encoder owned by `cairn-core`, fully specified by:

- **Domain tag**: every digest input includes `domain: "cairn.replay_hash.v1"`.
  Bumping `v` mints a new hash space — old journal entries do not
  silently match new computations.
- **Object key sort**: UTF-8 byte-wise ascending, stable, no Unicode
  collation.
- **String escapes**: `\"`, `\\`, `\n`, `\r`, `\t`, `\b`, `\f`, and
  `\u00XX` for `0x00..=0x1F` and `0x7F`. All other characters emitted
  literally as UTF-8 (no `\u` for non-ASCII).
- **Strings**: normalized to Unicode NFC before encoding.
- **Numbers**: integers as base-10 digits, no leading zeros, no signs
  except `-` for negatives. No floats (the input struct contains
  none).
- **Booleans**: `true` / `false`.
- **Null**: `null`.
- **Arrays**: `[v0,v1,…]`, no spaces.
- **Objects**: `{"k0":v0,"k1":v1,…}`, no spaces.
- No insignificant whitespace anywhere.

**Stability guarantees**:
- Golden test vectors checked into `crates/cairn-core/tests/golden/replay_hash/`.
  Each vector is `(input json, expected sha256:hex)`. Cover empty
  arrays, NFC normalization edges, escape boundary, sort ordering,
  large unicode strings, nested objects.
- The canonical encoder lives in `cairn-core` (no transitive
  dependency on `serde_json` for *output*). Internal Rust types can
  still use `serde_json` for I/O elsewhere; the canonical encoder is
  a separate code path.

**Versioning and cross-version forget continuity**:

The full replay-hash contract is versioned — not just the byte
encoder. Each version `vN` ships **three frozen artifacts**:

1. **Frozen input struct** `CanonicalReplayInputVN` — fixed field
   set, no future evolution. Lives in
   `pipeline::canonical::replay_hash::vN::Input`.
2. **Frozen projection** `project_vN(&MemoryRecord) ->
   CanonicalReplayInputVN` — reads only the `MemoryRecord` fields
   that existed when `vN` was minted. Future `MemoryRecord`
   additions are ignored by older projections.
3. **Frozen encoder** `encode_vN(&CanonicalReplayInputVN) -> Vec<u8>`
   — domain tag `cairn.replay_hash.vN`, canonical-JSON rules as
   specified above.

`replay_hash::compute(record, version)` chains
`project_vN ∘ encode_vN ∘ sha256`. All three artifacts are
immutable once shipped. Changes to `MemoryRecord` cannot retroactively
shift `compute(_, v1)` output — `project_v1` reads a frozen subset.

The set of supported versions is
`pub const SUPPORTED_REPLAY_HASH_VERSIONS: &[u32] = &[1, ...]`.
Removing a version is a deprecation cycle requiring an offline
journal-rewrite op (out of scope for #257).

Each `SourceForget` row persists the version used:

```rust
SourceForget {
    source_id: String,
    source_bytes_hash: String,
    target: Option<TargetReplayKey>,    // Some → target-scope; None → source-scope
    op_id: String,
}

pub struct TargetReplayKey {
    pub hash: String,                   // "<algo>:<hex>"
    pub version: u32,                   // must be in SUPPORTED_REPLAY_HASH_VERSIONS
}
```

`TargetReplayKey` keeps `hash` and `version` paired structurally —
no sentinel version values, no chance of a source-scope row leaking
into target-scope lookups.

Lint matching algorithm:
1. Group target-scope rows (`target.is_some()`) by `version`.
2. For each version `v` present in the journal, compute the candidate
   record's `replay_hash` at `v` via
   `replay_hash::compute(record, v)`.
3. Hit if the computed hash matches any row in that version's set.

Validation: persisting a `TargetReplayKey` with a `version` not in
`SUPPORTED_REPLAY_HASH_VERSIONS` is an integrity error rejected at
the journal-write boundary. Lint similarly rejects unknown versions
encountered at read time with an `error`-severity finding
(`source_after_forget_unknown_version`), so an out-of-band journal
corruption surfaces immediately rather than silently passing.

A target forgotten under `v1` stays blocked forever. No journal-row
migration is required — `project_v1` and `encode_v1` remain
available.

**Domain location:** `pipeline::canonical::replay_hash`. The body-only
`BodyHash` (already used for upsert idempotency) is intentionally NOT
reused — that hash collapses two distinct records with the same body
text but different provenance, agent, or sensor.

**Persistence:** `MemoryRecord` does NOT gain a persisted `replay_hash`
field. Computing it is cheap, and persisting it would create a sync-
drift hazard. Both `forget` and `lint` compute it on demand from the
in-hand record.

**Legacy records — invariants enforced in `forget`, not by lint
convention.** A pre-change record with empty `source_refs` and the
re-ingested record with populated `source_refs` hash differently. If
a legacy duplicate remains in the vault after re-ingest, target-scope
forget on the new record leaves the legacy copy live → privacy
regression.

`forget` is the privacy boundary, so it enforces the invariants
directly inside the WAL transaction. Lint reports the same conditions
for operator visibility, but is NOT load-bearing for safety.

Transactional pre-checks inside `forget` (target-scope path):

1. **Reject empty `source_refs`.** Returns
   `ForgetError::SourceRefsRequiredForTargetScope`. Remediation:
   re-ingest the source.

2. **Reject legacy duplicates.** Inside the same transaction that
   reads the target record, `forget` queries the store for any
   *other* active record `legacy` where:
   - `legacy.provenance.source_hash` equals any
     `target.provenance.source_refs[i].hash`, AND
   - `legacy.provenance.source_refs.is_empty()`.
   If found, `forget` returns
   `ForgetError::LegacyDuplicateExists { legacy_id, source_hash }`.
   Remediation: **operator runs the dedicated `cairn ingest --resync
   <source_id>` (or equivalent) to tombstone the legacy copy in a
   transaction**, then retries forget. Source-scope forget is NOT
   suggested as remediation — it would over-broaden the privacy
   action and remove unrelated records sharing the same source.

3. **Locked-snapshot semantics.** Both checks run against the same
   read snapshot that produces `target_replay_hash`. A concurrent
   re-ingest cannot race past these guards because the WAL state
   machine serializes the read+check+write.

Re-ingest (issue #61 scope, dependency of #257 privacy soundness):
re-ingest of a source MUST locate any pre-existing record produced
from the same `source_bytes_hash` with empty `source_refs` and
tombstone it via WAL Phase A in the same transaction that creates
the new record. The dedicated `--resync` mode also handles standalone
legacy cleanup without ingest of new bytes.

Lint rules (for operator visibility, not the privacy primitive):
- `source_link_missing` (`severity: error`) flags any record with
  empty `source_refs`.
- `source_link_legacy_duplicate` (`severity: error`) flags any pair
  of active records `(legacy, modern)` matching the criterion above.
  Finding includes both `RecordId`s and the shared source hash.

The combination — `forget` enforces invariants transactionally, lint
surfaces them ahead of time — converges legacy and modern copies on
the same forget key. Skipping lint cannot defeat the privacy
guarantee; it only delays operator awareness of the duplicate state.

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
        source_id: String,                // logical id, operator diagnostics
        source_bytes_hash: String,        // "<algo>:<hex>" — same hash space as SourceRef.hash
        target: Option<TargetReplayKey>,  // Some → target-scope; None → source-scope
        op_id: String,                    // forget operation that produced this row
    },
}

// hash + version paired structurally — no sentinel values.
pub struct TargetReplayKey {
    pub hash: String,    // "<algo>:<hex>" in replay_hash space
    pub version: u32,    // must be in SUPPORTED_REPLAY_HASH_VERSIONS
}
```

`source_bytes_hash` and `SourceRef.hash` share a hash space — both
digest raw source bytes. `target_replay_hash` is in the
`pipeline::canonical::replay_hash` space (defined above). The two
spaces never mix.

Journal query helpers (all O(n) over journal rows; callers cache):
```rust
fn forgotten_source_bytes_hashes(&self) -> HashSet<&str>;
// Returns versions present among rows whose `target` is `Some`.
// Source-scope rows are excluded — `0` is never a return value.
fn forgotten_target_replay_versions(&self) -> HashSet<u32>;
fn forgotten_target_replay_hashes_for_version(&self, v: u32) -> HashSet<&str>;
fn forget_op_for_source(&self, hash: &str) -> Option<&str>;
fn forget_op_for_target_replay(&self, key: &TargetReplayKey) -> Option<&str>;
```

Lint rule `source_not_forgotten` runs both checks:
1. For each `record.provenance.source_refs[i].hash`, look up in
   `forgotten_source_bytes_hashes()`. Hit ⇒ emit `source_after_forget`
   with scope=source.
2. For every encoder version `v` represented in the journal,
   compute the candidate record's `replay_hash` at version `v`
   (via `pipeline::canonical::replay_hash::compute(record, v)`) and
   look up in `forgotten_target_replay_hashes_for_version(v)`. Hit ⇒
   emit `source_after_forget` with scope=target. This guarantees a
   target forgotten under any historical version remains blocked.

This survives source rename/copy (same bytes, same hash), survives
`RecordId` regeneration (target keyed by replay-hash), and never
over-blocks (replay-hash includes provenance disambiguators, so
distinct records with the same body but different provenance/agent/
sensor get different keys). Legacy-record handling is covered by the
migration contract in Component 4 (target-scope gating + re-ingest
dedup + `source_link_legacy_duplicate` lint rule).

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
- `SourceAfterForgetUnknownVersion`
- `SourceRedactSkipped`
- `SourceLinkLegacyDuplicate`

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
4. `ConsentKind::SourceForget { source_id, source_bytes_hash,
   target_replay_hash, op_id }` + journal query helpers
   (`forgotten_source_bytes_hashes`, `forgotten_target_replay_hashes`,
   `forget_op_for_source`, `forget_op_for_target_replay`). Wire the
   target-scope gating error path in `forget` (#58 follow-up; #257
   only adds the journal schema and helpers).
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
