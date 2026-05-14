# Issue #257 — Source-Link Hygiene for Provenance Lint

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:test-driven-development` while executing this plan. Steps use checkbox syntax for tracking.

**Goal:** Implement `provenance.source_ids` end-to-end and replace the deferred provenance lint placeholder with real source-link hygiene checks: presence, resolution, hash integrity, forgotten-source detection, and redact-on-forget enforcement.

**Architecture:** Add a typed source-id field to `cairn-core::domain::Provenance`, propagate it through ingest and all record constructors, extend lint inputs with read-only source/forget snapshots, and implement the five provenance lint rules as pure `cairn-core` checks. Adapter crates perform all filesystem / SQLite reads needed to build those snapshots.

**Tech Stack:** Rust 2024, `tokio`, SQLite store, `thiserror`, `rstest`, `proptest`, `insta`.

**Brief sources:** §3 (vault layout and immutable sources), §5.6 (forget / source_forget), §6.5 (mandatory provenance), §8 (`lint` read-only surface).

**Spec:** [../specs/2026-05-12-issue-257-source-link-hygiene-design.md](../specs/2026-05-12-issue-257-source-link-hygiene-design.md)

---

## File Structure

**New:**
- `crates/cairn-core/src/domain/source_id.rs` — typed source-id newtype + parser/serde.
- `crates/cairn-core/src/verbs/lint/checks/provenance.rs` — real source-link hygiene checks (replaces deferred placeholder behavior).
- `crates/cairn-cli/tests/lint_source_link_hygiene.rs` — integration coverage for real vault/source drift scenarios, if current tests do not already have a better home.

**Modified:**
- `crates/cairn-core/src/domain/mod.rs` — export `SourceId`.
- `crates/cairn-core/src/domain/provenance.rs` — add `source_ids`, validation, serde tests.
- `crates/cairn-core/src/domain/record.rs` and any builders / fixtures — supply `source_ids`.
- `crates/cairn-core/tests/memory_record.rs` — schema-level regressions.
- `crates/cairn-core/src/verbs/lint/mod.rs` — extend `LintInputs` with source/forget/config snapshots and wire real provenance check.
- `crates/cairn-core/src/generated/verbs/lint.rs` and IDL sources if new finding kinds are required.
- `crates/cairn-cli/src/verbs/ingest.rs` — persist `provenance.source_ids`.
- `crates/cairn-cli/src/verbs/lint.rs` — collect source files, forget state, redact policy, and pass them into `LintInputs`.
- `crates/cairn-store-sqlite` read-paths, only if existing APIs cannot provide the read-only forget snapshot needed by lint.
- `crates/cairn-test-fixtures` helpers and snapshots touched by the new required field.

**Deleted / replaced behavior:**
- the deferred-info implementation in `crates/cairn-core/src/verbs/lint/checks/provenance.rs`.

---

## Task 1: Add typed `SourceId` and require `provenance.source_ids`

**Files:**
- Create: `crates/cairn-core/src/domain/source_id.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Modify: `crates/cairn-core/src/domain/provenance.rs`
- Modify: `crates/cairn-core/tests/memory_record.rs`

- [ ] **Step 1: Write the failing tests**

Add tests that prove:
- `Provenance` deserialization fails when `source_ids` is absent.
- `Provenance::validate()` fails when `source_ids` is empty.
- a `SourceId` parser rejects empty strings and round-trips valid values.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:
```bash
cargo nextest run -p cairn-core provenance memory_record
```

Expected: failures due to missing `source_ids` support.

- [ ] **Step 3: Implement `SourceId` and provenance schema changes**

Add the newtype, export it from `domain/mod.rs`, add `pub source_ids: Vec<SourceId>` to `Provenance`, and extend validation / custom serde so the field is structurally required.

- [ ] **Step 4: Re-run focused tests and verify GREEN**

Run the same focused `cairn-core` tests and confirm they pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/source_id.rs \
        crates/cairn-core/src/domain/mod.rs \
        crates/cairn-core/src/domain/provenance.rs \
        crates/cairn-core/tests/memory_record.rs
git commit -m "feat(core): require provenance source ids"
```

---

## Task 2: Propagate `source_ids` through record constructors, ingest, and fixtures

**Files:**
- Modify all `Provenance { ... }` construction sites in `cairn-core`, `cairn-cli`, `cairn-test-fixtures`, and tests.
- Modify `crates/cairn-cli/src/verbs/ingest.rs`.

- [ ] **Step 1: Write / tighten failing tests first**

Add or update ingest-focused tests asserting persisted records carry at least one `source_id` pointing at the created immutable source artifact.

- [ ] **Step 2: Run targeted tests and verify RED**

Run the smallest relevant set, for example:
```bash
cargo nextest run -p cairn-cli ingest
```

Expected: failures or compile errors where `source_ids` is not yet supplied.

- [ ] **Step 3: Implement propagation**

Thread the canonical source identifier through ingest and every helper that builds a `Provenance`. Fix fixtures and snapshots so they reflect the new required schema.

- [ ] **Step 4: Re-run targeted tests and verify GREEN**

Run the same ingest / fixture tests plus:
```bash
cargo nextest run -p cairn-test-fixtures
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest.rs \
        crates/cairn-core \
        crates/cairn-test-fixtures
git commit -m "feat(ingest): persist provenance source ids"
```

---

## Task 3: Extend lint finding kinds and replace the deferred provenance stub

**Files:**
- Modify IDL / generated lint enums if new finding kinds are not already present.
- Modify `crates/cairn-core/src/verbs/lint/mod.rs`
- Modify `crates/cairn-core/src/verbs/lint/checks/provenance.rs`

- [ ] **Step 1: Write failing rule-shape tests**

Add focused unit tests for provenance lint that expect:
- `source_link_missing`
- `source_link_dangling`
- `source_hash_mismatch`
- `source_after_forget`
- `source_redact_skipped`

Each test should assert kind, severity, and core reproduction metadata.

- [ ] **Step 2: Run targeted tests and verify RED**

Run:
```bash
cargo nextest run -p cairn-core provenance lint
```

Expected: tests fail because the deferred placeholder still emits `deferred_check`.

- [ ] **Step 3: Implement lint finding enum growth if needed**

Update the canonical lint finding kind definitions, regenerate code if required, and refresh any snapshots impacted by the new enum values.

- [ ] **Step 4: Replace the deferred stub with real pure checks**

Refactor `provenance.rs` to operate over a richer `LintInputs` snapshot instead of emitting a single info finding.

- [ ] **Step 5: Re-run targeted tests and verify GREEN**

Run the same focused lint tests and confirm the real finding kinds appear.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/verbs/lint \
        crates/cairn-core/src/generated/verbs/lint.rs \
        crates/cairn-idl
git commit -m "feat(lint): add provenance source-link findings"
```

---

## Task 4: Add CLI / adapter snapshot plumbing for source files, forget state, and redact policy

**Files:**
- Modify `crates/cairn-cli/src/verbs/lint.rs`
- Modify `cairn-store-sqlite` read-only APIs only if needed

- [ ] **Step 1: Write failing integration tests**

Create real-vault tests that:
- remove a source file after ingest and expect `source_link_dangling`;
- mutate source bytes and expect `source_hash_mismatch`;
- mark a source forgotten and expect `source_after_forget`;
- enable `source.redact_on_forget`, leave bytes intact, and expect `source_redact_skipped`.

- [ ] **Step 2: Run targeted tests and verify RED**

Run the new CLI integration tests only.

- [ ] **Step 3: Implement read-only snapshot assembly**

Teach CLI lint to gather:
- active records;
- source-id to path mapping;
- file readability / bytes for hash comparison;
- forget-state snapshot from the store / journal;
- effective `source.redact_on_forget` config.

Keep all I/O at the adapter boundary so `cairn-core` still receives a pure snapshot.

- [ ] **Step 4: Re-run targeted tests and verify GREEN**

Confirm the integration matrix passes and the findings are stable.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/lint.rs \
        crates/cairn-store-sqlite
git commit -m "feat(cli): wire source-link lint snapshots"
```

---

## Task 5: Full verification and cleanup

- [ ] Run focused suites:

```bash
cargo nextest run -p cairn-core
cargo nextest run -p cairn-cli
cargo nextest run -p cairn-store-sqlite
cargo nextest run -p cairn-test-fixtures
```

- [ ] Run required repo checks for touched areas:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
./scripts/check-core-boundary.sh
```

- [ ] Regenerate code if lint IDL changed:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

- [ ] Review final diffs for accidental unrelated changes.

- [ ] Prepare a summary listing:
- schema changes (`Provenance`, `SourceId`);
- ingest behavior updates;
- new lint finding kinds and rule coverage;
- verification commands actually run.
