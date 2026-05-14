# Issue 257 Source-Link Hygiene Design

## Context

Issue [#257](https://github.com/windoliver/cairn/issues/257) turns the brief's
provenance requirements into runtime-checkable lint invariants. The design brief
sections in scope are:

- §3 Vault layout: `sources/` is immutable and records link back to source
  documents.
- §5.6 Forget Phase A: source-forget journal entries must affect later reads and
  diagnostics.
- §6.5 Provenance: every record carries mandatory provenance, now including
  `source_ids` as part of the provenance bundle.
- §8: `lint` is a read-only verb whose findings must be identical across CLI,
  SDK, MCP, and skill surfaces.

The implementation must honor the repo invariants from `AGENTS.md`: CLI is
ground truth, `cairn-core` owns the pure logic, adapters perform I/O, and lint
never mutates the vault.

## Goal

Ship the full v0.1 source-link hygiene slice:

1. Add `provenance.source_ids` as a typed, required field on every
   `MemoryRecord`.
2. Populate that field for records created by ingest and other record-producing
   paths.
3. Replace the current deferred provenance lint placeholder with real
   read-only checks that validate source presence, resolution, hash integrity,
   forget-state consistency, and forget-redaction enforcement.

Acceptance target: a clean fixture vault yields zero findings from this rule
family, while targeted fixture drift produces deterministic error findings with
actionable metadata.

## Non-Goals

- Auto-repair or `lint --fix` behavior.
- Cross-vault or federated source resolution.
- Source-signature verification beyond the existing `source_hash`.
- Reworking unrelated provenance semantics such as actor-chain validation.

## Approaches Considered

1. **Recommended: typed provenance field plus layered lint implementation.**
   Add `source_ids` directly to `cairn-core::domain::Provenance`, wire it
   through serde, ingest, fixtures, and lint inputs, then implement the five
   source-link checks against typed data. This matches brief §6.5 and keeps the
   wire format honest.

2. **Adapter-only metadata lookup.**
   Keep `source_ids` outside the typed core model and recover it from loose
   frontmatter or store-specific metadata. This reduces immediate churn but
   contradicts the brief and weakens the core schema.

3. **Schema first, partial lint later.**
   Add `source_ids` now and leave forget/redaction checks deferred. This lowers
   risk, but it does not satisfy the full issue scope.

Option 1 is the implementation target.

## Proposed Design

### 1. Core schema

Add `source_ids` to `crates/cairn-core/src/domain/provenance.rs` as a required,
non-empty vector of typed source identifiers. The field belongs inside
`Provenance`, alongside `source_hash`, because both describe the evidence a
record was derived from.

The implementation should introduce a dedicated newtype instead of using raw
`String` values at crate boundaries. The parser can stay intentionally small in
v0.1: it needs to reject empty IDs, round-trip through serde, and remain
forward-compatible with whatever vault source naming scheme is already emitted
by ingest.

Validation rules:

- `source_ids` must be present on the wire.
- `source_ids` must not be empty.
- every entry must be non-empty and parse as a valid source identifier.

`DomainError::MissingProvenance` remains the error family for missing or empty
source-link data so callers keep a uniform provenance failure path.

### 2. Record-producing paths

Every place that constructs a `Provenance` value must now supply `source_ids`.
That includes:

- CLI ingest flows;
- trace / turn helpers in `cairn-core`;
- fixtures and property tests;
- any sample records in CLI or store tests.

For the v0.1 ingest contract, each persisted record should carry at least one
source identifier referencing the immutable source artifact created for that
ingest operation. If a single ingest call yields multiple records, they may
share the same source identifier when they were derived from the same source
document.

### 3. Lint architecture

Replace `crates/cairn-core/src/verbs/lint/checks/provenance.rs` from a deferred
placeholder into the real source-link hygiene check runner. `cairn-core`
remains pure, so the CLI must gather a read-only vault snapshot and pass the
needed artifacts into `LintInputs`.

`LintInputs` should grow only with data the check actually needs:

- active records;
- vault root or a precomputed source-file index;
- consent/forget snapshot from the journal;
- effective config needed for `source.redact_on_forget`.

The check should emit findings in stable order for deterministic snapshots and
cross-surface parity.

### 4. Finding model

Use the existing lint finding structure and extend the generated kind enum with
the issue's concrete source-link failure kinds:

- `source_link_missing`
- `source_link_dangling`
- `source_hash_mismatch`
- `source_after_forget`
- `source_redact_skipped`

All five are `error` severity in v0.1. Findings should include the minimum
reproduction metadata needed by operators:

- `target.record_id` or equivalent target id;
- offending `source_id` where applicable;
- expected source path for resolution failures;
- expected and actual hash for hash mismatches;
- forget operation id when a record survives a source-forget decision.

### 5. The five checks

#### 5.1 `source_link_present`

For each active record, error when `provenance.source_ids` is missing or empty.
Because `source_ids` becomes required in the typed schema, this catches legacy
rows, malformed persisted JSON, or direct-SQL drift that bypassed normal
validation.

#### 5.2 `source_link_resolves`

For each `source_id`, resolve the corresponding path under `sources/` and error
if the file is absent or unreadable. Resolution logic should be centralized in
one helper so path normalization and fixture setup stay consistent.

#### 5.3 `source_hash_match`

For each resolved source file, recompute the content hash and compare it to
`provenance.source_hash`. A mismatch means the supposedly immutable source bytes
were edited out of band or the record points at the wrong source. The lint path
must be binary-safe and should compare raw bytes by default; if the current
ingest implementation already canonicalizes certain text cases, the same rules
must be reused here rather than re-invented.

#### 5.4 `source_not_forgotten`

Cross-reference each `source_id` against consent-journal rows representing
`source_forget`. If a record still references a forgotten source and the record
is otherwise active, emit an error naming both the record and the forget
operation. The check should consume a read-only forget snapshot prepared by the
adapter layer rather than opening SQLite directly from `cairn-core`.

#### 5.5 `source_redact_on_forget_honored`

When effective config enables `source.redact_on_forget`, assert that each
forgotten source has been content-redacted. In v0.1, "redacted" means the
original source bytes are no longer present while the artifact remains
identifiable enough for provenance diagnostics. The exact redaction predicate
must reuse whichever source-file representation the forget implementation writes,
so lint checks policy adherence instead of inventing a second format.

## Data Flow

1. CLI `lint` resolves vault config, active records, source paths, and
   forget-state inputs.
2. CLI hands that snapshot to `cairn-core::verbs::lint`.
3. `cairn-core` runs the five pure provenance checks.
4. Findings return through the existing canonical lint envelope to CLI / SDK /
   MCP / skill surfaces unchanged.

This preserves the "CLI is ground truth" invariant while keeping the rule logic
pure and testable.

## Testing Strategy

The implementation should be test-first and layered.

### Core unit tests

- `Provenance` serde rejects missing `source_ids`.
- `Provenance::validate` rejects empty `source_ids`.
- source-id parser rejects empty / malformed values and round-trips valid ones.
- provenance lint helper emits one deterministic finding per violated rule.

### Core integration / verb tests

- records with valid source links produce no provenance findings;
- records with empty `source_ids` emit `source_link_missing`;
- records pointing at missing source artifacts emit `source_link_dangling`.

### Adapter / CLI integration tests

Using `tempfile::tempdir()` vaults and real source files:

- mutate a source file out of band and assert `source_hash_mismatch`;
- add a `source_forget` journal row while a record still references that source
  and assert `source_after_forget`;
- enable `source.redact_on_forget`, leave original bytes in place, and assert
  `source_redact_skipped`.

### Regression coverage

- snapshot the lint finding shapes;
- update existing fixtures that construct `Provenance` so schema drift is caught
  at compile time;
- keep `scripts/check-core-boundary.sh` green by ensuring new I/O stays outside
  `cairn-core`.

## Risks and Mitigations

- **Schema churn risk.** Adding a required provenance field touches many tests
  and fixtures. Mitigation: land the typed field first behind failing tests, and
  let compile errors surface every missed constructor.
- **Ambiguous source-id/path mapping.** If current ingest does not expose a
  single canonical mapping, lint could guess wrong. Mitigation: centralize
  source-id resolution in one helper used by both ingest and lint-facing adapter
  code.
- **Forget/redaction coupling risk.** The lint rule must match the real forget
  representation. Mitigation: reuse existing store/config semantics instead of
  encoding a parallel notion of "redacted."

## Implementation Boundaries

- `cairn-core`: new source-id type, provenance schema updates, pure lint logic,
  unit tests.
- `cairn-cli`: lint input assembly, source resolution snapshot, config plumbing,
  integration tests.
- `cairn-store-sqlite`: only if needed to expose read-only forget-state queries
  or source-related snapshots for CLI lint.
- `cairn-idl`: lint finding kind additions if the canonical enum needs to grow.

## Ready-to-Plan Outcome

After this design is approved, the implementation plan should proceed in this
order:

1. add failing schema tests for `provenance.source_ids`;
2. wire the new field through constructors and fixtures;
3. replace deferred provenance lint with failing rule-specific tests;
4. implement the rule logic and adapter snapshots incrementally;
5. regenerate IDL if lint finding enums change;
6. run focused verification, then the broader checklist as the slice stabilizes.
