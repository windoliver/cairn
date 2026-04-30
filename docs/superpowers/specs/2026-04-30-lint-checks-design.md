# `lint` health checks — design

- Issue: [#96](https://github.com/windoliver/cairn/issues/96)
- Parent: [#17](https://github.com/windoliver/cairn/issues/17) — privacy, consent journal, redaction, lint surface
- Brief sources: §8.0 (verbs), §14 (privacy), §15 (evaluation)
- Status: design, not yet implemented
- Date: 2026-04-30

---

## 1. Goal

Wire `cairn lint` to perform seven read-only health checks, returning structured
findings (severity, target, explanation, suggested fix). Findings flow through
the canonical IDL so CLI / MCP / SDK / skill all see the same shape. Lint never
mutates the vault.

Acceptance (from #96):
- Lint finds seeded defects in fixture vaults.
- Findings stable and actionable across all four surfaces.
- Lint is read-only; auto-repair is out of scope.

## 2. Non-goals

- Auto-repair mode (#96 explicitly out of scope).
- Re-deriving embeddings or FTS5 content from scratch (counts-only, see §6.7).
- Running `assemble_hot` to size the hot prefix (static estimate, see §6.6).
- Any cross-vault / federation checks (P3+).

## 3. Surface

CLI (matches brief §8 row 7):

```
cairn lint [--write-report] [--json]
```

- Default: prints a structured report to stdout (human-readable). Exit 0 if no
  Error-severity findings; exit 1 otherwise. Warning/Info do not fail the run.
- `--json`: emits the canonical `LintData` envelope.
- `--write-report`: in addition to stdout/JSON, writes
  `.cairn/lint-report.md` (brief §3 vault layout, line 245).
- Existing `--fix-markdown` / `--fix-folders` paths are untouched. They are
  separate repair paths and remain mutating; they are gated by their own
  capability flags and pre-date this PR.

MCP / SDK / skill: identical fields, same `LintData` types — these are
generated from the IDL.

## 4. IDL changes

`crates/cairn-idl/idl/verbs/lint.cairn` (and the generated
`crates/cairn-mcp/src/generated/schemas/verbs/lint.json`) gain:

```
enum LintFindingSeverity { error, warning, info }

enum LintFindingKind {
  // Existing — unchanged for back-compat:
  contradiction
  orphan
  stale
  missing_concept
  data_gap
  // New, per #96:
  malformed_record
  broken_actor_chain
  missing_provenance
  stale_schema
  missing_consent
  hot_memory_over_budget
  index_drift
}

struct LintFindingTarget {
  // Discriminated union: at most one set.
  record_id?: Ulid
  operation_id?: Ulid
  path?: string         // for vault-scoped checks (e.g. derived-index drift)
}

struct LintFinding {
  kind: LintFindingKind
  severity: LintFindingSeverity
  message: string             // human-readable explanation
  suggested_fix?: string      // canonical, deterministic remediation hint
  target?: LintFindingTarget
}

struct LintSummary {
  total: u64
  by_severity: { error: u64, warning: u64, info: u64 }
  by_kind: map<LintFindingKind, u64>
}

struct LintData {
  findings: list<LintFinding>
  summary: LintSummary
  report_path?: string
}
```

Notes:
- `LintFindingKind` keeps the legacy five variants; nothing in the verb table
  hard-removes them. Today nothing populates them; this PR does not start
  emitting them — that is the existing follow-up workflow (orphans /
  contradictions belong to `EvaluationWorkflow`, brief §10).
- `severity` is `#[non_exhaustive]` per CLAUDE.md §6.10.
- `LintFindingTarget` is a struct of optional fields, not a tagged union, to
  avoid forcing wire-shape choices on consumers; the contract is "at most one
  field set." Validated by a property test on the core `LintFinding`
  constructor.

After IDL edits, run `cargo run -p cairn-idl --bin cairn-codegen` and commit
the regenerated files (CLAUDE.md §10).

## 5. Module layout

```
crates/cairn-core/src/verbs/lint/
├── mod.rs              ← LintInputs, run_checks, LintFinding ctors
├── checks/
│   ├── malformed.rs
│   ├── actor_chain.rs
│   ├── provenance.rs
│   ├── schema.rs
│   ├── consent.rs
│   ├── hot_memory.rs
│   └── index_drift.rs
└── report.rs           ← LintData → markdown projector for --write-report
```

This is the first verb under `cairn-core/src/verbs/`. The directory is created
in this PR; future verbs follow the same shape (one folder per verb, pure
functions over input snapshots).

`cairn-core` invariant 4 (no I/O) is satisfied: every check is a pure function
over the `LintInputs` snapshot. The CLI handler (§7) is the only place that
talks to adapters.

## 6. The seven checks

Each check returns `Vec<LintFinding>` from a sub-slice of `LintInputs`. The
inputs struct carries pre-fetched data so checks stay pure and deterministic:

```rust
pub struct LintInputs<'a> {
    pub records: &'a [LintRecord],            // active records + per-row consent_model
    pub projections: &'a [VaultProjection],   // markdown bytes per record (§6.1.b)
    pub projection_status: ProjectionStatus,  // fail-closed contract, §6 below
    pub consent_journal: &'a dyn ConsentLookup, // §6.5 lifecycle queries
    pub config: &'a CairnConfig,              // resolved config snapshot
    pub index_stats: IndexStats,              // counts, see §6.7
    pub schema_version: SchemaVersion,        // current contract version
}

pub struct LintRecord {
    pub stored: StoredRecord,
    /// Per-row gate from the `records.consent_model` column. Drives the
    /// per-record §6.3.a / §6.5 routing — never a vault-level lookup.
    pub consent_model: ConsentModel,
}
pub enum ConsentModel { LegacyEvent, ReceiptTimeline }
```

There is no `vault_meta.consent_model` lookup in the lint path —
gating is per-row, sourced from the `records.consent_model` column the
migration adds. Mixed vaults (legacy rows + receipt-timeline rows in
the same store) are first-class: each row routes independently.

`ConsentLookup` is a small, pure trait — `cairn-core` only, no I/O at the
trait level; the SQLite impl pre-loads the relevant rows into an in-memory
snapshot before constructing `LintInputs`. The trait is defined once,
below in the **Lifecycle model** subsection. There is no flat `resolve()`
+ `revoked_at()` API: a collapsed revoke timestamp cannot represent
grant→revoke→re-grant histories, and is unsupported.

**Lifecycle model — event timeline, not collapsed timestamps.** Consent is
a stream of events keyed by `(consent_ref, sequence)`, never a single
"revoked_at" cell. A consent_ref can be granted, revoked, granted again,
revoked again; the timeline preserves the full history so a record's
`created_at` can be evaluated against the **most recent grant interval that
contains it**.

```rust
pub enum ConsentTimelineEvent {
    Grant {
        consent_ref: ConsentRef,
        seq: u64,                       // monotonic per consent_ref
        source_sensor: Identity,
        scope: Scope,
        issued_at: Timestamp,
        expires_at: Option<Timestamp>,
        state_at_issue: ConsentState,
    },
    Revoke {
        consent_ref: ConsentRef,
        seq: u64,
        revoked_at: Timestamp,
    },
}

pub trait ConsentLookup {
    /// Full ordered timeline for this consent_ref (ascending seq).
    fn timeline(&self, r: &ConsentRef) -> &[ConsentTimelineEvent];
    /// Convenience: the grant interval (issue, optional revoke) that
    /// contains `at`, or None if the record is outside every grant
    /// interval. Implemented over `timeline` — pure derivation.
    fn covering_grant(
        &self,
        r: &ConsentRef,
        at: Timestamp,
    ) -> Option<CoveringGrant<'_>>;
}
```

`covering_grant` is the only entry point §6.5 needs: it returns the Grant
event whose interval contains the record's `created_at`, or None. The §6.5
checks then evaluate `source_sensor`, `scope`, `expires_at`, and
`state_at_issue` against that specific grant interval — not against the
journal head, and not against an aggregated `revoked_at`.

**Migration.** A new SQLite migration adds:
- `consent_timeline` table keyed by `(consent_ref, seq)`, columns
  mirroring `ConsentTimelineEvent`. Append-only; never updated. Re-grant
  and re-revoke each get a new row with a higher `seq`.

**No backfill of legacy `ConsentEvent` rows.** Legacy rows do not carry a
durable `snr:` binding for `source_sensor` — fabricating one would
mis-attribute consent. Instead, this PR ships **per-record** version
gating, not vault-wide gating, so legacy vaults cannot become a side
door for unvalidated new writes:

- The migration that introduces `consent_timeline` also adds a non-null
  column `consent_model` to the records table with two values:
  `legacy_event` and `receipt_timeline`. The migration sets every
  pre-existing row to `legacy_event` (preserving valid history).
- **Rollout safety — phased, not big-bang.** The migration ships with a
  writer-version guard so binary skew during rollout cannot poison the
  store:
  1. **Phase A (this PR).** Migration runs; `consent_model` column added;
     every existing row tagged `legacy_event`. The default for new
     inserts stays `legacy_event` and the `consent_timeline` table is
     populated only by writers that explicitly opt in. New writes from
     an older binary continue to land cleanly as `legacy_event` rows;
     lint surfaces them via the aggregated info finding rather than as
     errors. This phase is what this PR ships.
  2. **Phase B (separate PR after the binary is broadly deployed).**
     Flip the default for new inserts to `receipt_timeline`, add a
     check constraint that rejects ingest of a `receipt_timeline` row
     without a matching `consent_timeline` grant, and call-boundary-
     reject ingest from a writer that still emits the pre-migration
     record shape. Phase B is gated on a `cairn.writer.min_version`
     metadata key set by the operator to acknowledge "all my writers
     are upgraded".
  - In Phase A, lint already enforces full §6.5 on rows that the new
    binary writes with `consent_model = receipt_timeline` (opt-in,
    typically tests + new sensor flows). Older writers continue to
    function. There is no Phase A state where ingest is rejected because
    of consent_model.
  - In Phase B, mixed-version writes are impossible at the schema level,
    so the per-record gate stays the only enforcement boundary lint
    needs.
- §6.3.a / §6.5 run per record:
  - `consent_model = receipt_timeline` records get the full enforcement
    described above. New writes in old vaults still receive full
    validation — there is no vault-level bypass.
  - `consent_model = legacy_event` records are skipped by §6.5 and
    receive an `info`-severity finding aggregated once per lint run
    (one finding per N legacy records, not N findings) explaining the
    migration path.
- The `cairn migrate --consent-model` flow that lifts existing
  `legacy_event` records into the timeline model is **out of scope for
  this PR** (separate issue) but is explicitly named so operators know
  how to retire the legacy bucket.

This per-record gate means: legacy vault history stays valid;
post-migration writes in any vault — old or new — receive the full
consent boundary; and the migration path is non-noisy.

`VaultProjection { record_id, path, bytes }` is a small record per record
on disk; the CLI handler reads each projection once before building
`LintInputs`. To prevent the projection sub-check from silently disappearing
when projections fail to load, `LintInputs` carries a `projection_status`
field of type `ProjectionStatus`:

```rust
pub enum ProjectionStatus {
    /// Projections are intentionally not provided (e.g., a unit test that
    /// constructs LintInputs directly with no vault). Set ONLY by callers
    /// that explicitly assert "no vault root attached".
    NotAttached,
    /// Projections were loaded successfully for every active record.
    Loaded,
    /// One or more projections failed to load. The check engine emits a
    /// `malformed_record` finding (severity error, target = path) for each
    /// missing/failed entry — silent skipping is forbidden.
    PartiallyLoaded { errors: Vec<ProjectionLoadError> },
}
```

The CLI handler always sets `Loaded` or `PartiallyLoaded`; only a test that
explicitly opts out of vault attachment may use `NotAttached`. The check
engine refuses to run §6.1.b silently — a `PartiallyLoaded` status
guarantees one finding per failed projection, so operator confidence cannot
form on the back of a hidden skip.

### 6.1 Malformed records (`malformed_record`)

Severity: **error**. Two sub-checks, because malformed YAML is rejected at
ingestion and never lands in `StoredRecord` — the raw bytes are normalized
into typed fields plus `extra_frontmatter`. We therefore split the check into
"defects that survive parsing" (over `StoredRecord`) and "defects in the
on-disk projection" (over the markdown bytes the projector wrote):

(a) **In-record invariants** (over `StoredRecord`):
- Required typed fields are present and non-empty (`kind`, `scope`,
  `schema_version`, `actor_chain`).
- `actor_chain` is non-empty and each entry parses as a typed `Identity`.
- `extra_frontmatter` does not collide with reserved keys (a parser bug
  could land an unexpected key here; the check is the canary).
- Per-kind invariants already enforced by `MemoryRecord::validate` round-trip
  cleanly.

(b) **Projection round-trip** (over markdown bytes the projector wrote into
the vault, when `vault_root` is supplied — CLI handler reads each
`projected.path` once into the `LintInputs` snapshot, see §6):

Two distinct findings, two distinct severities — projector evolution
must not be reclassified as corruption:

- `malformed_record` (severity error): the on-disk file fails to parse
  as YAML frontmatter + body. Causes: duplicate frontmatter keys,
  control-character abuse, truncated headers, non-UTF-8 sequences in
  metadata, body that bypasses the frontmatter delimiter. These are
  vault corruption — they cannot be repaired by a re-projection.
- `index_drift` (severity warning, sub-classification "projection
  drift"): the file parses cleanly, but the parsed frontmatter +
  body's semantic content differs from `projector.project(stored)`.
  This includes byte-level differences caused by projector
  upgrades (field reorder, newline normalization, formatting fix)
  that `cairn lint --fix-markdown` is designed to resolve. The
  finding's `suggested_fix` is exactly `"run cairn lint
  --fix-markdown"`. Routine projector evolution lands here, not in
  the error bucket.

Implementation: parse on-disk bytes; if parse fails →
`malformed_record`. If parse succeeds, compare the structured re-parsed
projection (typed frontmatter struct + body string) against
`projector.project(stored)`. Differ → projection-drift `index_drift`
warning. Equal → no finding.

This split keeps `--fix-markdown` as the always-available remediation
for benign drift and reserves error severity for true corruption.

Test fixtures:
- (a) construct a `StoredRecord` with `actor_chain = []` directly in the
  test — must flag.
- (b1) write a vault file with a hand-rolled duplicate-key frontmatter
  → `malformed_record` (error).
- (b2) write a vault file whose frontmatter parses but whose body has
  been deleted or replaced with unrelated content → projection-drift
  `index_drift` (warning), suggested_fix points at `--fix-markdown`.
  This is benign-drift handling, not corruption.
- (b3) write a vault file with non-UTF-8 garbage in the metadata
  block → `malformed_record` (error).
- Negative: a clean vault where the projector was upgraded after files
  were last written (whitespace-only diff). After regenerating
  `projections` from the upgraded projector vs disk, the test asserts
  exactly one `index_drift` warning per drifted file and zero
  `malformed_record` findings.

### 6.2 Broken actor chains (`broken_actor_chain`)

Severity: **error**. For each record's `actor_chain`, run
`cairn_core::verifier::verify_signed_intent` on the trailing signed intent. Any
signature failure, expired credential, or chain-rooting violation produces a
finding pointing at the offending record id.

Test fixture: tamper one record's signature byte after upsert; lint must flag
exactly that record.

### 6.3 Missing provenance (`missing_provenance`)

Severity: **error** for the consent-boundary invariants below; **warning**
for the optional `source_refs` link-hygiene checks. Provenance is not a
single field — there are two distinct concerns and one severity per
concern:

(a) **Consent-boundary invariants — error, gated on `consent_model ==
receipt_timeline`.** Every record's `provenance.source_sensor` must be a
parseable `snr:` identity, and `provenance.consent_ref` must be set.
Absence corrupts the consent boundary and is a blocking finding. This
check runs only on receipt-timeline vaults; on `legacy_event` vaults it
is suppressed (legacy `ConsentEvent` rows do not bind `source_sensor`
durably, so flagging would mass-fail valid history). Legacy vaults
receive the single `info` finding from §6.5's gate, and §6.3.a re-engages
once the operator runs `cairn migrate --consent-model`.

This is the single source of truth for the "missing/malformed
provenance" defect under the receipt-timeline model — §6.5 routes here
rather than emitting its own provenance findings, so the same defect
cannot be both a warning and an error.

(b) **Source-link hygiene — warning.** Independent of consent:
- A record with no `source_refs` whose kind is not in
  `provenance_optional` (`raw/observation` only at v0.1) → warning.
- A `source_ref` pointing at a `sources/` path that no record under
  `records` claims → warning (dangling forward link).

Suggested fixes are sub-check specific: "rerun ingest with `--source`
set" / "remove the dangling source_ref" / "this record has no consent
receipt — re-ingest under a fresh handshake or forget".

### 6.4 Stale schema (`stale_schema`)

Severity: **warning** when `record.schema_version < current` by one minor;
**error** when more than one minor behind. Reasoning: one minor lag is normal
between a contract bump and the back-fill workflow; further lag means the
back-fill is stuck.

Suggested fix: "run `cairn migrate --to <current>`" (today not implemented;
finding still useful as the canary).

### 6.5 Missing consent for sensors (`missing_consent`)

Severity: **error**. Every record carries `provenance.source_sensor` (an
`snr:` identity) and `provenance.consent_ref` — a pointer to the specific
consent receipt that authorized this write. The provenance contract requires
both fields on every record; absence is a lint failure, not a skip. Validate
the receipt at the **record's creation time** — not the journal head — so a
later revoke does not retroactively invalidate records that were written
under a valid receipt.

For each record:

1. **Provenance shape.** If `provenance.source_sensor` is missing, blank,
   or not a parseable `snr:` identity, or if `provenance.consent_ref` is
   missing, route to §6.3.a (which owns this defect) and stop further
   §6.5 sub-checks for that record — do not double-report. Silent skipping
   is forbidden: §6.3.a guarantees one error per record with corrupt
   consent-boundary provenance.
2. **Covering grant.** Call `ConsentLookup::covering_grant(consent_ref,
   record.created_at)`. None → flag with severity error: the record's
   `created_at` falls outside every grant interval for this `consent_ref`.
   This single call subsumes the issuance lower-bound, the expiry upper-
   bound, and the post-revoke-before-regrant cases — they are all "the
   record sits outside any grant interval".
3. **Sensor binding.** Covering grant's `source_sensor` must equal the
   record's `provenance.source_sensor`. Mismatch → flag (cross-sensor
   replay).
4. **Scope binding.** Covering grant's authorized scope must cover the
   record's scope (per `Scope::is_subset`). Receipt for a narrower or
   sibling scope → flag (cross-scope replay).
5. **State at issue.** If the covering grant was `denied` at issue time,
   flag.

Each sub-check produces a distinct `suggested_fix` so operators can
distinguish "rerun ingest with a fresh handshake" from "this record was
written outside its consent envelope and must be forgotten".

Test fixtures (each seeded as a separate vault):
- Missing `consent_ref` → `missing_provenance` (covered by §6.3 with this
  cross-link).
- No grant in timeline at all → flag.
- Record `created_at` < earliest grant `issued_at` → flag (pre-consent).
- Record sits in a revoked window between two grants → flag (post-revoke
  before re-grant).
- Record sits after a final revoke with no later grant → flag.
- Record `created_at` > grant `expires_at` → flag (expired).
- Covering grant's `source_sensor` differs from record → flag.
- Covering grant's scope is narrower / sibling vs record's scope → flag.
- Covering grant was `denied` at issue → flag.
- Negative: grant→revoke→grant timeline, record sits in the second grant
  interval → must **not** flag.
- Negative: revoke that post-dates record `created_at` → must not flag.
- Negative: open-ended grant covering record's scope and time → must not
  flag.

### 6.6 Hot memory over budget (`hot_memory_over_budget`)

Severity: **warning**.

Static estimate, no `assemble_hot` call. Use the same unit the runtime
already enforces (`config.vault.hot_memory.max_bytes`) so the lint warning
and the assembler agree on accounting.

**Source of size — fail-closed.** Sizing reads from the in-memory projection
of each hot record (`MarkdownProjector::project(stored).content.len()`),
**not** from the on-disk `VaultProjection` bytes. The projector is pure and
deterministic, so it cannot be defeated by a partial vault read; this
guarantees the hot-memory check produces the same number whether projections
loaded successfully or not.

```
hot_records = records.filter(|r| r.hot_scope && active)
estimated_bytes = sum(MarkdownProjector::project(r).content.len()
                      for r in hot_records)
if estimated_bytes > config.vault.hot_memory.max_bytes: emit finding
```

The size accounting MUST be the same helper the assembler uses to size its
budget. If no shared helper exists today, extracting one in this PR is
in-scope; otherwise importing the existing helper directly is preferred to
keep the assembler and the lint check on a single source of truth.

The finding's `message` includes both `estimated_bytes` and `max_bytes`.
One finding per overflow, target = `path: ".cairn/config.yaml"`.

Token-based heuristics are deliberately not used here: the assembler caps in
bytes, so a token-based lint would diverge from runtime enforcement and
produce false positives/negatives.

### 6.7 Derived-index drift (`index_drift`)

Severity: **error**. The store adapter exposes a small `IndexStats` struct:

```rust
pub struct IndexStats {
    pub records_active: u64,
    pub fts5_rows: u64,
    pub vec_rows: u64,
}
```

If `fts5_rows != records_active` or `vec_rows != records_active`, emit a
finding. Counts-only — content-level diff is a separate workflow (out of
scope, see §2). Adds one method to `MemoryStore`:
`async fn index_stats(&self) -> Result<IndexStats, StoreError>`. The fixture
store returns hand-rolled values; the SQLite store runs three `SELECT
COUNT(*)` queries.

## 7. CLI wiring

`cairn-cli/src/verbs/lint.rs` `run` (default branch, no `--fix-*` flag):

**Snapshot consistency.** Lint must observe a coherent view across the
store and the on-disk vault, otherwise a concurrent `ingest` or
`--fix-markdown` rewrite can manufacture spurious `malformed_record` or
`index_drift` findings. The handler establishes one snapshot via:

1. Acquire the shared `.cairn/lint.lock` advisory lock (non-blocking
   `flock`) for the duration of the run. `--fix-markdown`,
   `--fix-folders`, and the WAL apply phase already cooperate on the
   same lock as part of the bootstrap protocol — extending lint to
   participate keeps lint and rewriters from racing. Lint takes the
   shared lock; rewriters take exclusive.
2. Open the SQLite store at a consistent read-only transaction
   (`BEGIN DEFERRED`); all subsequent store reads in this run see the
   same MVCC snapshot.
3. Read the consent timeline snapshot inside the same transaction.
4. Drop the SQLite snapshot once all store data is in memory; keep the
   advisory lock until the projection reads complete so on-disk files
   cannot be rewritten under us.
5. Read each active record's projection bytes from disk into a
   `Vec<VaultProjection>`. A read miss/`ENOENT` while the lock is held
   indicates real corruption (no rewriter could have moved the file)
   and is recorded in `projection_status = PartiallyLoaded { errors }`
   to fail closed. Without the lock, an `ENOENT` would be ambiguous;
   under the lock it is a true defect.
6. Release the lock; build `LintInputs` and call
   `cairn_core::verbs::lint::run_checks`.

Then:

7. Pull `index_stats` (three `SELECT COUNT(*)` queries on the store).
   Done outside the lock — counts-only is naturally tolerant of small
   transient skew, and a real drift will recur on the next run.
8. If `--write-report`, project the `LintData` to markdown and write
   `.cairn/lint-report.md` via the same atomic-rename helper used by
   `fix_markdown_handler`.
9. Print human or JSON; exit 1 if any `error`-severity finding, else 0.

The advisory-lock + read-only-tx pattern keeps lint deterministic and
re-runnable on a healthy live vault: concurrent rewriters block briefly
but never produce false errors.

Live store wiring lands in this PR. Reasoning: the new fail-closed
guarantees (projection load failures, consent timeline gaps) are exactly
the surfaces that an end-to-end path stresses. Shipping core-only would
leave projection IO, journal-snapshot construction, and the
`receipt_timeline` migration outside the only real `cairn lint` execution
path — a gap that defeats the verb's premise.

Concretely:
- `cairn-cli/src/verbs/lint.rs::run` opens the SQLite store via the same
  bootstrap path the other verbs use once #46 lands. If #46 is not yet
  merged when this PR is ready, this PR rebases on top of it; the lint
  work does not regress to a core-only ship.
- The handler loads the consent timeline snapshot
  (`cairn-store-sqlite::consent::load_timeline_snapshot`) and the
  projection bytes (one read per active record), populates `LintInputs`,
  and runs the engine.
- Integration tests at `crates/cairn-cli/tests/lint.rs` use a real
  on-disk SQLite store + a `tempfile::tempdir()` vault, exercise the full
  code path, and cover three end-to-end scenarios:
  1. Clean vault → zero findings, exit 0.
  2. Vault with one defect per check kind → exactly the expected findings,
     exit 1.
  3. Projection-loader failure (one file removed mid-run) → explicit
     `malformed_record` finding, exit 1 — the fail-closed contract.
- A second integration test covers the `legacy_event` consent gate: an
  existing-vault fixture migrated up to but not past the receipt-timeline
  migration produces the single aggregated `info` finding and zero
  false-positive consent errors.
- A third integration test — **required ship gate** — covers a **mixed
  vault**: the same store contains some `legacy_event` rows (from the
  migration's backfill) and at least one freshly-ingested
  `receipt_timeline` row. Assertions:
  1. Legacy rows produce the aggregated `info` finding only.
  2. The receipt-timeline row, seeded with a deliberately broken
     `consent_ref` (no covering grant), produces the expected §6.5
     error finding.
  3. A second receipt-timeline row, seeded with valid provenance and a
     valid covering grant, produces zero findings.
  This test is the regression guard against silently regressing to
  vault-wide gating: any implementation that suppresses §6.5 for the
  whole vault will fail assertion 2.

If #46 has not landed by the time this PR is ready to ship, this PR
**blocks on it** rather than degrading to core-only. The dependency is
declared explicitly in the PR description.

## 8. Testing strategy

- **Unit tests** (cairn-core): one `#[test]` table per check, table-driven
  via `rstest`. Each row asserts the exact `LintFinding` produced.
- **Property tests** (`proptest`): `LintFinding` constructor rejects targets
  with more than one field set; `run_checks` is order-independent over the
  records slice.
- **Integration tests** (cairn-cli): one fixture vault per check seeded with
  the defect, plus a "clean vault" fixture that produces zero findings.
- **Snapshot tests** (`insta`): `LintData` JSON, human report markdown, CLI
  human output. Reviewed via `cargo insta review` and committed.
- **Read-only assertion**: integration tests record a vault hash before and
  after lint runs and assert byte-equality.

Every check is testable in core without I/O — the `LintInputs` snapshot is
constructed directly in the test.

## 9. Documentation

- Regenerate the verb reference: `cargo run -p cairn-cli --bin cairn-docgen
  -- --write` (CLAUDE.md §8).
- Update `docs/design/traceability.md` to map §8.0 row 7 + §14 to this PR.

## 10. Verification

Per CLAUDE.md §8 — full checklist before pushing. Notable items:
- `./scripts/check-core-boundary.sh` (new core code must not pull in adapter
  crates).
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`.
- `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`.

## 11. Open follow-ups (separate issues, not this PR)

- Live-store wiring once #46 lands.
- `--deep` sampling mode for index drift if a real bug demands it.
- `EvaluationWorkflow`-driven contradiction / orphan / data-gap findings —
  the legacy IDL kinds stay in the schema but are emitted by the workflow,
  not by this verb.
