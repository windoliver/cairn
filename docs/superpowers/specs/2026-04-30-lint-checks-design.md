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
    pub records: &'a [StoredRecord],          // active records, list_active_stored
    pub consent_events: &'a [ConsentEvent],   // ordered, from consent journal
    pub config: &'a CairnConfig,              // resolved config snapshot
    pub index_stats: IndexStats,              // counts, see §6.7
    pub schema_version: SchemaVersion,        // current contract version
}
```

### 6.1 Malformed records (`malformed_record`)

Severity: **error**. Per record, validate:
- ID, target, kind, scope, schema_version are present and parseable.
- `frontmatter` round-trips through `serde_yaml`.
- `actor_chain` is non-empty and each entry has a parseable principal.

Test fixture: vault seeded with one record whose `frontmatter` has a
duplicate key, one whose `kind` is empty.

### 6.2 Broken actor chains (`broken_actor_chain`)

Severity: **error**. For each record's `actor_chain`, run
`cairn_core::verifier::verify_signed_intent` on the trailing signed intent. Any
signature failure, expired credential, or chain-rooting violation produces a
finding pointing at the offending record id.

Test fixture: tamper one record's signature byte after upsert; lint must flag
exactly that record.

### 6.3 Missing provenance (`missing_provenance`)

Severity: **warning**. A record is missing provenance when:
- It has no `source_refs` and its kind is not in the
  `provenance_optional` set (`raw/observation` only at v0.1).
- A `source_ref` points at a `sources/` path that no record under
  `records` claims as its origin (dangling forward link).

Suggested fix: "rerun ingest with `--source` set" or "remove the dangling
source_ref".

### 6.4 Stale schema (`stale_schema`)

Severity: **warning** when `record.schema_version < current` by one minor;
**error** when more than one minor behind. Reasoning: one minor lag is normal
between a contract bump and the back-fill workflow; further lag means the
back-fill is stuck.

Suggested fix: "run `cairn migrate --to <current>`" (today not implemented;
finding still useful as the canary).

### 6.5 Missing consent for sensors (`missing_consent`)

Severity: **error**. For each record whose `actor_chain` cites a sensor
principal (anything matching `sensor:*`):
- Look up the latest `consent_events` row for the sensor + scope tuple.
- If absent or `revoked`, emit a finding.

Test fixture: ingest a `sensor:clipboard` record without granting consent;
lint must flag.

### 6.6 Hot memory over budget (`hot_memory_over_budget`)

Severity: **warning**.

Static estimate, no `assemble_hot` call:
```
hot_records = records.filter(|r| r.hot_scope && active)
estimated_tokens = sum(token_estimate(r.body) for r in hot_records)
if estimated_tokens > config.hot_memory.budget_tokens: emit finding
```

`token_estimate` is the same `chars / 4` heuristic used elsewhere in the
codebase (search if a helper already exists; otherwise add one in
`cairn-core::domain::token`). One finding per overflow, target = vault path,
message includes the overflow ratio.

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

1. Build the runtime store (live in this PR's wiring path; today the file
   has a TODO blocked on #46. We are not unblocking #46 here — gated as
   below).
2. Load the resolved config and current schema version.
3. Pull `list_active_stored`, `consent_events`, `index_stats`.
4. Build `LintInputs`, call `cairn_core::verbs::lint::run_checks`.
5. If `--write-report`, project to markdown and write
   `.cairn/lint-report.md` via the same atomic-rename helper used by
   `fix_markdown_handler`.
6. Print human or JSON; exit 1 if any `error`-severity finding, else 0.

Because store wiring is still gated on #46, this PR ships:
- The full pure check engine in core, fully unit-tested against
  `FixtureStore` + hand-rolled `LintInputs`.
- The CLI dispatch path that calls into the engine, exercised via
  integration tests that build `LintInputs` directly (skipping the store
  bring-up).
- A documented TODO in `run` referencing #46 for the live-store wiring,
  matching the existing `fix-markdown` / `fix-folders` pattern in the same
  file.

This keeps the diff scoped to the verb logic and respects "Keep the diff
scoped" (CLAUDE.md §5.3). When #46 lands, the live wiring is a small
follow-up.

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
