# Issue #7 Identity Envelope Status Handshake Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify and close the integrated P0 identity, signed envelope, replay, status, and handshake substrate for GitHub issue #7.

**Architecture:** Treat #7 as a parent-epic acceptance pass over the landed #50-#53 implementations on `origin/main`. Run focused verification against the existing identity, verifier, replay, handshake, status, CLI, MCP, and SDK tests; add code only after a failing test proves a remaining parent-epic gap.

**Tech Stack:** Rust 1.95 workspace, `cargo nextest`, `cargo run -p cairn-idl --bin cairn-codegen -- --check`, `scripts/check-core-boundary.sh`, GitHub issue evidence.

---

## File Structure

- Read: `docs/superpowers/specs/2026-05-08-issue-7-identity-envelope-status-handshake-design.md`
- Read: `docs/superpowers/specs/2026-04-27-issue-50-identity-provisioning-design.md`
- Read: `docs/superpowers/specs/2026-05-02-issue-51-envelope-verifier-design.md`
- Read: `docs/superpowers/specs/2026-05-06-issue-53-status-capability-parity-design.md`
- Read: `crates/cairn-core/src/verifier/mod.rs`
- Read: `crates/cairn-core/src/status/mod.rs`
- Read: `crates/cairn-store-sqlite/src/replay/mod.rs`
- Read: `crates/cairn-store-sqlite/src/replay/challenge.rs`
- Read: `crates/cairn-cli/src/verbs/handshake.rs`
- Read: `crates/cairn-mcp/src/prelude_tools.rs`
- Verify: `crates/cairn-core/src/verifier/tests.rs`
- Verify: `crates/cairn-core/src/status/tests.rs`
- Verify: `crates/cairn-core/tests/status_phase_pinning.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/tests.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/concurrency_tests.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/handshake_roundtrip_tests.rs`
- Verify: `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`
- Verify: `crates/cairn-cli/tests/handshake_tests.rs`
- Verify: `crates/cairn-cli/tests/sdk_cli_parity.rs`
- Verify: `crates/cairn-cli/tests/status_snapshot.rs`
- Verify: `crates/cairn-cli/tests/status_snapshot_insta.rs`
- Verify: `crates/cairn-mcp/tests/handshake_tool.rs`
- Verify: `crates/cairn-mcp/tests/init_status_parity.rs`
- Verify: `crates/cairn-sdk/tests/surface.rs`

Use an isolated Cargo cache and target directory for execution so unrelated worktrees cannot hold the shared package-cache lock. Each task uses its own `/tmp/codex-issue7-taskN-*` paths so worker verification cannot contend with other tasks:

```bash
export CARGO_HOME=/tmp/codex-issue7-taskN-cargo-home
export CARGO_TARGET_DIR=/tmp/codex-issue7-taskN-target
```

Do not commit changes under `/tmp`; these paths are execution scratch space only.

---

### Task 1: Verify Core Identity, Verifier, And Status Gates

**Files:**
- Verify: `crates/cairn-core/src/verifier/tests.rs`
- Verify: `crates/cairn-core/src/verifier/proptests.rs`
- Verify: `crates/cairn-core/src/status/tests.rs`
- Verify: `crates/cairn-core/tests/status_phase_pinning.rs`
- Verify: `crates/cairn-core/tests/envelope_errors.rs`
- Verify: `crates/cairn-core/tests/usr_prefix_banned.rs`

- [ ] **Step 1: Run core verifier/status/identity tests**

```bash
CARGO_HOME=/tmp/codex-issue7-task1-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task1-target \
cargo nextest run -p cairn-core --locked verifier status identity envelope_errors usr_prefix_banned
```

Expected: PASS. These tests cover signed envelope validation, identity prefix correctness, status decision rules, phase pinning, and wire error mapping.

- [ ] **Step 1b: Run integration-test binaries whose names do not match the broad filters**

```bash
CARGO_HOME=/tmp/codex-issue7-task1-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task1-target \
cargo nextest run -p cairn-core --locked \
  forget_session_pinned_to_v0_2_phase \
  forget_scope_pinned_to_v0_3_phase \
  replay_capabilities_held_back_at_every_phase \
  retrieve_capabilities_held_back_at_every_phase \
  no_usr_colon_in_workspace_rust_sources
```

Expected: PASS. These tests explicitly cover `crates/cairn-core/tests/status_phase_pinning.rs` and `crates/cairn-core/tests/usr_prefix_banned.rs`; nextest does not select those integration-test binaries by filename when only broad substring filters are used.

- [ ] **Step 1c: Run the full `envelope_errors` integration-test binary**

```bash
CARGO_HOME=/tmp/codex-issue7-task1-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task1-target \
cargo nextest run -p cairn-core --locked -E 'binary(envelope_errors)'
```

Expected: PASS. The broad Step 1 filter selects only `fallthrough_invalid_identity`; this command runs every snapshot test in `crates/cairn-core/tests/envelope_errors.rs`.

- [ ] **Step 2: If this command fails, stop execution and switch to `superpowers:systematic-debugging`**

Record the exact failing test names and failure messages. Do not edit production code until a failing test is understood and can be reproduced by a narrower command.

- [ ] **Step 3: Re-run the narrow failing test after any fix**

Example command shape for a verifier failure:

```bash
CARGO_HOME=/tmp/codex-issue7-task1-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task1-target \
cargo nextest run -p cairn-core --locked rejects_tampered_signature
```

Expected: PASS after the targeted fix.

---

### Task 2: Verify Replay Ledger, Sequence CAS, Challenge, And WAL Coupling

**Files:**
- Verify: `crates/cairn-store-sqlite/src/replay/mod.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/challenge.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/tests.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/concurrency_tests.rs`
- Verify: `crates/cairn-store-sqlite/src/replay/handshake_roundtrip_tests.rs`
- Verify: `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`

- [ ] **Step 1: Run replay and pre-write rejection tests**

```bash
CARGO_HOME=/tmp/codex-issue7-task2-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task2-target \
cargo nextest run -p cairn-store-sqlite --locked replay envelope_blocks_wal
```

Expected: PASS. These tests cover duplicate `operation_id` / nonce rejection, sequence strict advance, out-of-order rejection without state advance, single-use challenge consumption, TTL rejection, concurrency behavior, and verifier rejection before mutable identity WAL writes.

- [ ] **Step 1b: Run the pre-write rejection integration-test binary**

```bash
CARGO_HOME=/tmp/codex-issue7-task2-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task2-target \
cargo nextest run -p cairn-store-sqlite --locked --test envelope_blocks_wal
```

Expected: PASS. The broad Step 1 filter selects replay-related tests but does not select every test in `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`; this command runs the verifier-rejection-before-write tests directly.

- [ ] **Step 2: If replay admission fails, isolate the mode**

Run the smallest matching filter:

```bash
CARGO_HOME=/tmp/codex-issue7-task2-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task2-target \
cargo nextest run -p cairn-store-sqlite --locked duplicate_operation_id_rejected_as_replay
```

or:

```bash
CARGO_HOME=/tmp/codex-issue7-task2-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task2-target \
cargo nextest run -p cairn-store-sqlite --locked challenge_mode_single_use
```

Expected: the narrow command reproduces the same failure before any code change.

- [ ] **Step 3: Patch only the owning layer**

If the failure is in replay transaction ordering, patch `crates/cairn-store-sqlite/src/replay/mod.rs`. If the failure is in challenge minting or expiry, patch `crates/cairn-store-sqlite/src/replay/challenge.rs`. If the failure is in pre-write verifier rejection, patch the verifier or resolver path named in the failing assertion, not unrelated store code.

- [ ] **Step 4: Re-run Task 2 Step 1**

Expected: PASS.

---

### Task 3: Verify CLI, SDK, And MCP Status/Handshake Parity

**Files:**
- Verify: `crates/cairn-cli/tests/handshake_tests.rs`
- Verify: `crates/cairn-cli/tests/sdk_cli_parity.rs`
- Verify: `crates/cairn-cli/tests/status_snapshot.rs`
- Verify: `crates/cairn-cli/tests/status_snapshot_insta.rs`
- Verify: `crates/cairn-mcp/tests/handshake_tool.rs`
- Verify: `crates/cairn-mcp/tests/init_status_parity.rs`
- Verify: `crates/cairn-sdk/tests/surface.rs`
- Verify: `crates/cairn-sdk/tests/search_dispatch.rs`

- [ ] **Step 1: Run CLI prelude and status tests**

```bash
CARGO_HOME=/tmp/codex-issue7-task3-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task3-target \
cargo nextest run -p cairn-cli --locked handshake status sdk_cli_parity
```

Expected: PASS. This covers fresh CLI handshake nonces, CLI/SDK shape parity, status snapshots, and status capability stability.

- [ ] **Step 2: Run MCP prelude/status tests**

```bash
CARGO_HOME=/tmp/codex-issue7-task3-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task3-target \
cargo nextest run -p cairn-mcp --locked handshake init_status
```

Expected: PASS. This covers MCP initialize/status parity and handshake tool capability gating.

- [ ] **Step 3: Run SDK surface tests for handshake and capability fail-closed behavior**

```bash
CARGO_HOME=/tmp/codex-issue7-task3-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task3-target \
cargo nextest run -p cairn-sdk --locked handshake CapabilityUnavailable status
```

Expected: PASS. This covers SDK fresh handshake nonces, capability rejection paths, and SDK status output.

- [ ] **Step 4: If status parity fails, patch the single source of truth first**

Patch `crates/cairn-core/src/status/mod.rs` or `crates/cairn-core/src/status/wiring.rs` before touching surface adapters. Only patch `crates/cairn-cli/src/verbs/status.rs`, `crates/cairn-mcp/src/handler.rs`, or `crates/cairn-sdk/src/transport.rs` if the failure proves that a surface populated `CapabilityGates` incorrectly.

- [ ] **Step 5: If handshake freshness fails, patch the minting surface**

Patch `crates/cairn-cli/src/verbs/handshake.rs` for CLI-only failure, `crates/cairn-sdk/src/transport.rs` for SDK-only failure, or `crates/cairn-mcp/src/prelude_tools.rs` for MCP-only failure. Re-run the exact failing test before rerunning Task 3.

---

### Task 4: Run Contract Drift And Boundary Checks

**Files:**
- Verify: `crates/cairn-idl/schema/prelude/status.json`
- Verify: `crates/cairn-idl/schema/prelude/handshake.json`
- Verify: `crates/cairn-idl/schema/envelope/signed_intent.json`
- Verify: `scripts/check-core-boundary.sh`

- [ ] **Step 1: Run codegen drift check**

```bash
CARGO_HOME=/tmp/codex-issue7-task4-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task4-target \
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: PASS with generated files unchanged.

- [ ] **Step 2: Run core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: PASS. `cairn-core` must not depend on adapter crates.

- [ ] **Step 3: If either command fails, patch the drift directly**

For codegen drift, run:

```bash
CARGO_HOME=/tmp/codex-issue7-task4-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task4-target \
cargo run -p cairn-idl --bin cairn-codegen --locked --
```

Then inspect `git diff` and keep only generated changes that correspond to intentional schema edits. For boundary failures, remove the adapter dependency from `cairn-core`; do not add a lint exception.

---

### Task 5: Full Verification For Touched Areas

**Files:**
- Verify: workspace

- [ ] **Step 1: Run formatting check**

```bash
CARGO_HOME=/tmp/codex-issue7-task5-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task5-target \
cargo fmt --all --check
```

Expected: PASS.

- [ ] **Step 2: Run clippy for touched crates**

If no code changed after Tasks 1-4, run:

```bash
CARGO_HOME=/tmp/codex-issue7-task5-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task5-target \
cargo clippy -p cairn-core -p cairn-store-sqlite -p cairn-cli -p cairn-mcp -p cairn-sdk --all-targets --locked -- -D warnings
```

Expected: PASS.

If code changed in other crates, include those crates in the `-p` list before running.

- [ ] **Step 3: Run cargo check for touched crates**

```bash
CARGO_HOME=/tmp/codex-issue7-task5-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task5-target \
cargo check -p cairn-core -p cairn-store-sqlite -p cairn-cli -p cairn-mcp -p cairn-sdk --all-targets --locked
```

Expected: PASS.

- [ ] **Step 4: Run doctests for touched crates**

```bash
CARGO_HOME=/tmp/codex-issue7-task5-cargo-home \
CARGO_TARGET_DIR=/tmp/codex-issue7-task5-target \
cargo test --doc -p cairn-core -p cairn-store-sqlite -p cairn-cli -p cairn-mcp -p cairn-sdk --locked
```

Expected: PASS.

---

### Task 6: GitHub Issue Evidence

**Files:**
- No file changes unless a verified gap required a patch.

- [ ] **Step 1: Summarize acceptance evidence**

Prepare a concise issue comment for #7 with these sections:

```markdown
Issue #7 integration pass completed against branch `codex/issue-7-full-identity`.

Brief sections verified: §4.2, §8.0.a, §8.0.b, §14.

Acceptance evidence:
- Mutating envelopes reject invalid/expired/replayed inputs before disk writes: covered by `cairn-core` verifier tests, `cairn-store-sqlite` `envelope_blocks_wal`, and replay ledger tests.
- `status` capability output is deterministic within a process/surface and shared across CLI/MCP/SDK: covered by status unit tests, status snapshots, `sdk_cli_parity`, and `init_status_parity`.
- Consecutive handshakes mint fresh challenges: covered by CLI, SDK, and MCP handshake tests.
- Capability list matches runtime behavior: covered by `cairn-core::status::advertise`, CLI/MCP/SDK capability rejection tests, and codegen drift check.

Verification run:
- Include each command from Tasks 1-5 with its observed outcome before posting.
```

- [ ] **Step 2: Post evidence only after tests pass**

Use the GitHub connector to comment on `windoliver/cairn#7`. Do not claim the issue is complete if any command from Tasks 1-5 failed or was skipped.

---

## Self-Review

- Spec coverage: Task 1 covers identity and signed envelope validation; Task 2 covers replay, sequence, challenge, and pre-write rejection; Task 3 covers status, capabilities, and handshake across CLI/MCP/SDK; Task 4 covers wire-contract drift and core boundary; Task 5 covers final build hygiene; Task 6 records issue evidence.
- Placeholder scan: the plan has no unresolved markers, angle-bracket placeholders, or unspecified file paths. The GitHub evidence template instructs the executor to include runtime command outcomes because those are generated by executing the plan.
- Type consistency: file names and module names match the current `origin/main` tree.
