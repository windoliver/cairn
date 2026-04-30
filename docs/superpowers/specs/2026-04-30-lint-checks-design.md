# `lint` health checks — design (PR-1, scoped)

- Issue: [#96](https://github.com/windoliver/cairn/issues/96)
- Parent: [#17](https://github.com/windoliver/cairn/issues/17) — privacy, consent journal, redaction, lint surface
- Brief sources: §8.0 (verbs), §14 (privacy), §15 (evaluation)
- Status: design, ready for implementation plan
- Date: 2026-04-30

---

## 1. Goal

Wire `cairn lint` to perform read-only health checks against the data model
that exists today, returning structured findings (severity, target,
explanation, suggested fix). Findings flow through the canonical IDL so CLI
/ MCP / SDK / skill all see the same shape. Lint never mutates the vault.

This PR ships the **check engine + IDL surface + live CLI wiring**. Checks
that require new contract-level infrastructure (consent receipt timeline,
lint/ingest advisory lock, Phase-B consent enforcement) are deferred to
follow-up issues — see §11. Each deferred check emits a single
`info`-severity finding pointing at its tracking issue, so lint output
remains honest about its current coverage.

Acceptance (from #96):
- Lint finds seeded defects in fixture vaults.
- Findings stable and actionable across all four surfaces.
- Lint is read-only; auto-repair is out of scope.

## 2. Non-goals

- Auto-repair mode (#96 out of scope).
- Re-deriving embeddings or FTS5 content (counts-only, see §6.7).
- Running `assemble_hot` to size the hot prefix (static estimate, see §6.6).
- Cross-vault / federation checks (P3+).
- New consent storage model — that lives in follow-up #253 (§11).
- Concurrency-safe coordination with `--fix-*` and WAL apply — follow-up
  #254 (§11). PR-1 documents the race window and accepts that lint may
  produce transient false findings under concurrent rewrites; this is
  consistent with lint's role as a canary, not a transactional invariant.

## 3. Surface

CLI (matches brief §8 row 7):

```
cairn lint [--write-report] [--json]
```

- Default: structured human-readable report on stdout. Exit 0 if no
  Error-severity finding; exit 1 otherwise. Warning/Info do not fail.
- `--json`: emit canonical `LintData` envelope.
- `--write-report`: also write `.cairn/lint-report.md` (brief §3).
- Existing `--fix-markdown` / `--fix-folders` paths untouched.

MCP / SDK / skill: identical fields via the IDL.

## 4. IDL changes

`crates/cairn-idl/idl/verbs/lint.cairn` (and the regenerated
`crates/cairn-mcp/src/generated/schemas/verbs/lint.json`) gain:

```
enum LintFindingSeverity { error, warning, info }

enum LintFindingKind {
  // Existing — kept for back-compat, populated by EvaluationWorkflow not
  // by this verb (brief §10):
  contradiction
  orphan
  stale
  missing_concept
  data_gap
  // New, this PR:
  malformed_record
  broken_actor_chain
  missing_provenance
  stale_schema
  hot_memory_over_budget
  index_drift
  // Deferred-coverage placeholder, this PR:
  deferred_check
}

struct LintFindingTarget {
  record_id?: Ulid
  operation_id?: Ulid
  path?: string
}

struct LintFinding {
  kind: LintFindingKind
  severity: LintFindingSeverity
  message: string
  suggested_fix?: string
  target?: LintFindingTarget
  // For severity=info findings that name a tracking issue.
  tracking_issue?: u32
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

After IDL edits run `cargo run -p cairn-idl --bin cairn-codegen` and
commit the regenerated files (CLAUDE.md §10).

`#[non_exhaustive]` on `LintFindingKind`, `LintFindingSeverity`, and
`LintFindingTarget` per CLAUDE.md §6.10.

## 5. Module layout

```
crates/cairn-core/src/verbs/lint/
├── mod.rs              ← LintInputs, run_checks, LintFinding ctors
├── checks/
│   ├── malformed.rs
│   ├── actor_chain.rs
│   ├── provenance.rs
│   ├── schema.rs
│   ├── hot_memory.rs
│   └── index_drift.rs
└── report.rs           ← LintData → markdown for --write-report
```

First verb under `cairn-core/src/verbs/`. `cairn-core` invariant 4 (no
I/O) is satisfied — every check is a pure function over the `LintInputs`
snapshot. The CLI handler is the only place that talks to adapters.

```rust
pub struct LintInputs<'a> {
    pub records: &'a [StoredRecord],
    pub config: &'a CairnConfig,
    pub index_stats: IndexStats,
    pub schema_version: SchemaVersion,
}
```

No projection-bytes input, no consent-lookup trait — those land with the
follow-up infra. Keeping `LintInputs` small and additive lets the
follow-ups extend it without breaking PR-1's check signatures.

## 6. The six implemented checks + one deferred placeholder

### 6.1 Malformed records (`malformed_record`) — error

Pure invariants on `StoredRecord`:
- `kind`, `scope`, `schema_version` parse cleanly into typed values.
- `actor_chain` is non-empty and each entry's `Identity` parses.
- Per-kind invariants from `MemoryRecord::validate` round-trip.
- `extra_frontmatter` does not collide with reserved keys.

YAML-level corruption (duplicate keys etc.) is detected at ingest, not
here — `StoredRecord` already represents post-parse state. On-disk
projection drift detection is deferred to follow-up B (it needs the
advisory lock to avoid races with `--fix-markdown`).

**Test**: construct a `StoredRecord` with `actor_chain = []` directly;
must flag with target = record id.

### 6.2 Broken actor chains (`broken_actor_chain`) — error

For each record, run `cairn_core::verifier::verify_signed_intent` on the
trailing signed intent. Failures (signature mismatch, expired credential,
chain-rooting violation) flag the record id.

**Test**: tamper one record's signature byte after upsert; lint flags
exactly that record.

### 6.3 Missing provenance (`missing_provenance`) — warning

Source-link hygiene only (consent-boundary invariants are deferred to
follow-up A — see §6.5):
- A record with empty `source_refs` whose kind is not in
  `provenance_optional` (`raw/observation` only at v0.1) → warning.
- A `source_ref` pointing at a `sources/` path that no record under
  `records` claims → warning (dangling forward link).

Suggested fix: `"rerun ingest with --source set"` /
`"remove the dangling source_ref"`.

**Test**: record with no `source_refs` and a non-optional kind → flag.

### 6.4 Stale schema (`stale_schema`) — warning / error

- `record.schema_version == current` → no finding.
- `record.schema_version` exactly one minor behind → warning.
- More than one minor behind → error.

Suggested fix: `"run cairn migrate --to <current>"` (today not
implemented; finding is the canary).

### 6.5 Missing consent for sensors — **deferred** (`deferred_check` info)

This check requires the consent receipt timeline (`ConsentLookup` +
`covering_grant`), the per-record `consent_model` gate, and migrations
that touch ingest. That work is **follow-up #253** (§11).

PR-1 emits exactly one `info` finding pointing at #253:
- `kind = deferred_check`
- `severity = info`
- `message = "sensor-consent enforcement requires the receipt timeline
  introduced in #253"`
- `tracking_issue = 253`

The check engine therefore has full coverage of its advertised checks;
the deferred surface is honest in lint output.

### 6.6 Hot memory over budget (`hot_memory_over_budget`) — warning

Static, no `assemble_hot` call. Uses the runtime's own unit
(`config.vault.hot_memory.max_bytes`) and the pure
`MarkdownProjector::project(stored).content.len()` so no on-disk read
is involved:

```
hot_records = records.filter(|r| r.hot_scope && active)
estimated_bytes = sum(MarkdownProjector::project(r).content.len()
                      for r in hot_records)
if estimated_bytes > config.vault.hot_memory.max_bytes: emit finding
```

Target = `path: ".cairn/config.yaml"`. Message includes both
`estimated_bytes` and `max_bytes`.

If a shared sizing helper already exists in the assembler, this PR
imports it; otherwise it adds one in `cairn-core::domain::hot_memory`
and the assembler picks it up in a follow-up rebase. Either way the
two paths converge on a single function.

### 6.7 Derived-index drift (`index_drift`) — error

```rust
pub struct IndexStats {
    pub records_active: u64,
    pub fts5_rows: u64,
    pub vec_rows: u64,
}
```

`fts5_rows != records_active` or `vec_rows != records_active` → flag.
Counts-only.

Adds one method to `MemoryStore`:
`async fn index_stats(&self) -> Result<IndexStats, StoreError>`.
Fixture store returns hand-rolled values; SQLite store runs three
`SELECT COUNT(*)` queries.

## 7. CLI wiring

`cairn-cli/src/verbs/lint.rs::run` (default branch, no `--fix-*`):

1. Open the SQLite store via the shared bootstrap path; resolve config
   and current schema version.
2. Pull active records via `list_active_stored`.
3. Pull `index_stats` (three counts).
4. Build `LintInputs`; call `cairn_core::verbs::lint::run_checks`.
5. If `--write-report`, project the `LintData` to markdown and write
   `.cairn/lint-report.md` via the same atomic-rename helper used by
   `fix_markdown_handler`.
6. Print human or JSON; exit 1 on any `error`-severity finding, else 0.

**Concurrency.** PR-1 does not coordinate with `--fix-*` or WAL apply.
Lint reads from a SQLite read-only transaction (consistent for store
data); concurrent ingest/fix-markdown can in theory cause a transient
`index_drift` warning if a write commits between counts. Documented in
the `lint` man page entry as: *"run lint when no other writers are
active for byte-stable output; transient findings are safe to re-run."*
Follow-up #254 introduces the advisory lock that closes this.

**Live store dependency.** Wiring depends on #46 (SQLite store wired
into CLI dispatch). If #46 has not landed by the time this PR is ready,
this PR rebases on it; lint is not shipped before the store is wired.

## 8. Testing strategy

- **Unit tests** (`cairn-core`): `rstest` table per check, asserting
  exact `LintFinding` produced from a hand-rolled `LintInputs`.
- **Property tests** (`proptest`):
  - `run_checks` is order-independent over `records`.
  - `LintFindingTarget` constructor rejects more than one field set.
- **Integration tests** (`cairn-cli`, `tests/lint.rs`): real on-disk
  SQLite store + `tempfile::tempdir()` vault. Three scenarios as
  required ship gates:
  1. Clean vault → zero findings, exit 0.
  2. Defect-matrix vault: one seeded defect per implemented check
     (6.1, 6.2, 6.3, 6.4, 6.6, 6.7) plus the §6.5 deferred-info
     finding → exactly the expected findings, exit 1.
  3. `--write-report` on the defect-matrix vault → `.cairn/lint-report.md`
     written atomically, contents match the rendered findings.
- **Snapshot tests** (`insta`): JSON envelope, human report markdown,
  CLI human output. Reviewed via `cargo insta review`.
- **Read-only assertion**: integration tests record a vault hash before
  and after each lint run and assert byte-equality.

## 9. Documentation

- Regenerate verb reference: `cargo run -p cairn-cli --bin cairn-docgen
  -- --write` (CLAUDE.md §8).
- Update `docs/design/traceability.md` to map §8.0 row 7 + §14 to this
  PR and to follow-up issues A/B/C.

## 10. Verification

Per CLAUDE.md §8 — full checklist before pushing. Critical items:
- `./scripts/check-core-boundary.sh`.
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`.
- `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`.

## 11. Follow-up issues (file alongside this PR)

**#253 — Consent receipt timeline + per-record gate.** Adds the
`consent_timeline` table + ordered events keyed by `(consent_ref,
seq)`, the `ConsentLookup` trait with `timeline()` / `covering_grant()`,
the per-row `records.consent_model` column, and the ingest path changes
that populate them. Wires §6.5 of this spec — at issue completion the
deferred-check info finding is replaced with the full sub-check matrix
(sensor binding, scope binding, issuance / expiry / revoke window,
state-at-issue) under the per-record gate. Brief §14 amendment likely.

**#254 — Lint / fix / WAL advisory lock + projection drift check.**
Adds `.cairn/lint.lock`, makes `--fix-markdown`, `--fix-folders`, and
WAL apply cooperate on it. Adds the on-disk projection drift check as a
new `index_drift` warning sub-classification (parse-failure path stays
in `malformed_record` error). Closes the transient-finding window
documented in §7 above.

**#255 — Phase-B consent enforcement.** Once #253 has been
deployed broadly, flips the `consent_model` default for new inserts to
`receipt_timeline`, adds the schema check constraint that rejects
mismatched ingests, and gates Phase B on a `cairn.writer.min_version`
metadata key set by the operator. Brief §14 amendment.

These three issues are file-and-link, not implement, as part of this
PR. The implementation plan generated from this spec assumes all three
exist as tracking links.
