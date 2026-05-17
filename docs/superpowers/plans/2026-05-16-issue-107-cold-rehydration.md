# Issue 107 Cold Rehydration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `retrieve --session --rehydrate` an explicit, budgeted, body-free retrieval path.

**Architecture:** Thread the generated session `rehydrate` flag through the CLI request object and policy trace builder. Keep the data source as the existing authorized SQLite session retrieval path for this slice, with a trace hook that future cold-bundle restore can replace.

**Tech Stack:** Rust 2024, `cairn-cli` integration tests, generated `RetrieveArgs`, existing signed response envelopes.

---

### Task 1: Rehydration Trace Tests

**Files:**
- Modify: `crates/cairn-cli/tests/issue_61_signed_verbs.rs`

- [x] **Step 1: Write failing integration tests**

Add tests that call `retrieve --session --rehydrate --json` and default `retrieve --session --json` against an ingested trace session. Assert `read.rehydrate` appears only on the explicit path and that its detail is body-free.

- [x] **Step 2: Run tests to verify RED**

Run: `cargo test -p cairn-cli --test issue_61_signed_verbs retrieve_session_rehydrate`

Expected: the rehydrate test fails because no `read.rehydrate` trace is emitted.

### Task 2: Thread Rehydrate Flag

**Files:**
- Modify: `crates/cairn-cli/src/verbs/retrieve.rs`

- [x] **Step 1: Add request fields**

Add `rehydrate: bool` and a start timestamp to the session retrieval flow.

- [x] **Step 2: Emit policy trace**

Extend the existing budget report so `read_policy_trace` emits `read.rehydrate` only when rehydration is requested. Keep details body-free and limited to counts, elapsed time, budget, and source tier.

- [x] **Step 3: Run targeted tests**

Run: `cargo test -p cairn-cli --test issue_61_signed_verbs retrieve_session_rehydrate`

Expected: the explicit rehydration test passes.

### Task 3: Verify Existing Retrieval Behavior

**Files:**
- Modify only if tests reveal a regression: `crates/cairn-cli/src/verbs/retrieve.rs`

- [x] **Step 1: Run neighboring retrieve tests**

Run: `cargo test -p cairn-cli --test issue_61_signed_verbs retrieve_session`

Expected: existing session budget, cursor, limit, and redaction tests pass.

- [x] **Step 2: Run formatting/checks for touched crate**

Run: `cargo fmt --all --check`

Expected: formatting passes.
