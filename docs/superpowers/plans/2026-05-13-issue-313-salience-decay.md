# Issue 313 Salience Decay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build access-frequency salience strengthening, decay-curve salience erosion, durable pinning, and guardrailed auto-eviction through the existing forget path.

**Architecture:** Add pure salience policy functions in `cairn-core`, narrow store contract methods for salience metadata, SQLite persistence for `last_accessed_at_ms` and `pinned`, and CLI/workflow/read-path wiring around those primitives. Deletion remains centralized in the existing `forget_record` path.

**Tech Stack:** Rust 2024, `cairn-core`, `cairn-store-sqlite`, `cairn-cli`, `cairn-workflows`, `rusqlite`, `tokio`, `proptest`, `insta`.

---

### Task 1: Core Salience Policy

**Files:**
- Create: `crates/cairn-core/src/pipeline/salience.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`

- [ ] **Step 1: Write failing tests**

Add tests in `salience.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn access_never_decreases_and_stays_bounded(s in 0.0f32..=1.0) {
            let next = apply_access(s);
            prop_assert!(next >= s);
            prop_assert!((0.0..=1.0).contains(&next));
        }

        #[test]
        fn decay_never_increases_and_stays_bounded(s in 0.0f32..=1.0, days in 0u32..365) {
            let next = decay_salience(s, 0.05, days);
            prop_assert!(next <= s);
            prop_assert!((0.0..=1.0).contains(&next));
        }
    }

    #[test]
    fn access_uses_diminishing_returns() {
        assert!((apply_access(0.5) - 0.525).abs() < 0.000_001);
        assert!((apply_access(0.9) - 0.905).abs() < 0.000_001);
    }

    #[test]
    fn decay_matches_forgetting_curve() {
        let next = decay_salience(1.0, 0.05, 14);
        assert!((next - 0.496_585).abs() < 0.000_01);
    }
}
```

- [ ] **Step 2: Verify red**

Run: `cargo test -p cairn-core pipeline::salience`

Expected: compile failure because `pipeline::salience` does not exist.

- [ ] **Step 3: Implement minimal core module**

Add:

```rust
pub fn apply_access(salience: f32) -> f32 {
    let s = salience.clamp(0.0, 1.0);
    (s + 0.05 * (1.0 - s)).clamp(0.0, 1.0)
}

pub fn decay_salience(salience: f32, decay_rate: f32, days_since_last_access: u32) -> f32 {
    let s = salience.clamp(0.0, 1.0);
    let rate = if decay_rate.is_finite() { decay_rate.max(0.0) } else { 0.0 };
    let exponent = -rate * days_since_last_access as f32;
    (s * exponent.exp()).clamp(0.0, 1.0)
}
```

Expose `pub mod salience;` from `pipeline/mod.rs`.

- [ ] **Step 4: Verify green**

Run: `cargo test -p cairn-core pipeline::salience`

Expected: tests pass.

### Task 2: Config Defaults

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Test: existing config tests/snapshots

- [ ] **Step 1: Write failing config test**

Add a test asserting default salience policy values:

```rust
#[test]
fn default_salience_config_matches_issue_313() {
    let cfg = CairnConfig::default();
    assert_eq!(cfg.vault.salience.decay_rate, 0.05);
    assert_eq!(cfg.vault.salience.eviction_threshold, 0.10);
    assert_eq!(cfg.vault.salience.min_age_days, 30);
    assert_eq!(cfg.vault.salience.batch_limit, 500);
}
```

- [ ] **Step 2: Verify red**

Run: `cargo test -p cairn-core default_salience_config_matches_issue_313`

Expected: compile failure for missing `vault.salience`.

- [ ] **Step 3: Add config structs**

Add `SalienceConfig` under vault config with defaults:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SalienceConfig {
    #[serde(default = "default_decay_rate")]
    pub decay_rate: f32,
    #[serde(default = "default_eviction_threshold")]
    pub eviction_threshold: f32,
    #[serde(default = "default_min_age_days")]
    pub min_age_days: u32,
    #[serde(default = "default_batch_limit")]
    pub batch_limit: u32,
}
```

Wire it into `VaultConfig`.

- [ ] **Step 4: Verify green**

Run: `cargo test -p cairn-core default_salience_config_matches_issue_313`

Expected: pass.

### Task 3: Store Contract And SQLite Metadata

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0060_salience_access.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Create: `crates/cairn-store-sqlite/src/store/salience.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Test: `crates/cairn-store-sqlite/tests/salience_access.rs`

- [ ] **Step 1: Write failing store tests**

Tests:
- migration exposes `last_accessed_at_ms` and `pinned`.
- `record_access` increases salience and stamps last access.
- `pin_record` flips durable pin flag.
- decay skips pinned records.

- [ ] **Step 2: Verify red**

Run: `cargo test -p cairn-store-sqlite --test salience_access`

Expected: compile failure for missing contract methods.

- [ ] **Step 3: Add contract types and methods**

Add `AccessReason`, `AccessUpdate`, `DecayPolicy`, `DecayBatchOutcome`, and default fail-closed trait methods.

- [ ] **Step 4: Add migration and SQLite implementation**

Migration:

```sql
ALTER TABLE records ADD COLUMN last_accessed_at_ms INTEGER;
ALTER TABLE records ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1));

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (60, '0060_salience_access', '', strftime('%s','now') * 1000);
```

Implementation updates active, non-tombstoned rows and leaves record bodies unchanged.

- [ ] **Step 5: Verify green**

Run: `cargo test -p cairn-store-sqlite --test salience_access`

Expected: pass.

### Task 4: Forget Pin CLI

**Files:**
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
- Test: `crates/cairn-cli/tests/forget_record.rs` or new `forget_pin.rs`

- [ ] **Step 1: Write failing CLI test**

Run `cairn forget --pin <record_id> --json` against a temp vault and assert it marks the record pinned without tombstoning it.

- [ ] **Step 2: Verify red**

Run: `cargo test -p cairn-cli --test forget_pin`

Expected: clap rejects `--pin`.

- [ ] **Step 3: Implement `--pin`**

Add clap arg and dispatch to `store.pin_record(record_id, true)`. JSON output should be committed success with target record id and `pinned: true`.

- [ ] **Step 4: Verify green**

Run: `cargo test -p cairn-cli --test forget_pin`

Expected: pass.

### Task 5: Read-Path Access Tracking

**Files:**
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/src/verbs/assemble_hot.rs`
- Modify where concrete retrieve dispatch exists.
- Test: `crates/cairn-cli/tests/search_modes_golden.rs`, `crates/cairn-cli/tests/cli_assemble_hot.rs`, or focused new tests.

- [ ] **Step 1: Write failing read-path tests**

Assert search and assemble-hot increase salience for returned records.

- [ ] **Step 2: Verify red**

Run focused CLI tests and observe unchanged salience.

- [ ] **Step 3: Implement tracking calls**

Call `record_access` after successful hit loading using reasons `search`, `assemble_hot`, and `retrieve`.

- [ ] **Step 4: Verify green**

Run focused tests.

### Task 6: Decay Workflow And Auto-Eviction

**Files:**
- Modify: `crates/cairn-workflows/src/lib.rs`
- Create: `crates/cairn-workflows/src/salience_decay.rs`
- Modify: `crates/cairn-workflows/Cargo.toml` if needed.
- Test: `crates/cairn-workflows/tests/salience_decay.rs`

- [ ] **Step 1: Write failing workflow tests**

Create records covering each guardrail:
- below threshold and old enough gets forgotten.
- pinned record retained.
- too young retained.
- above threshold retained.
- consent-denied retained.

- [ ] **Step 2: Verify red**

Run: `cargo test -p cairn-workflows --test salience_decay`

Expected: missing workflow.

- [ ] **Step 3: Implement workflow**

Implement a `run_salience_decay` function that calls store decay batch, re-checks candidate guardrails, and calls existing `forget_record`.

- [ ] **Step 4: Verify green**

Run workflow test.

### Task 7: Lint Surfacing

**Files:**
- Modify: `crates/cairn-cli/src/verbs/lint.rs`
- Test: `crates/cairn-cli/tests/lint_cli.rs` or focused lint test.

- [ ] **Step 1: Write failing lint test**

Assert human or JSON lint output includes salience and pin state for inspected records.

- [ ] **Step 2: Verify red**

Run focused lint test.

- [ ] **Step 3: Implement lint output**

Add salience and pin metadata to record diagnostics without logging bodies.

- [ ] **Step 4: Verify green**

Run focused lint test.

### Task 8: Final Verification

**Files:** all touched files.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Focused tests**

Run:

```bash
cargo test -p cairn-core salience
cargo test -p cairn-store-sqlite --test salience_access
cargo test -p cairn-cli --test forget_pin
cargo test -p cairn-workflows --test salience_decay
```

- [ ] **Step 3: Boundary check**

Run: `scripts/check-core-boundary.sh`

- [ ] **Step 4: Workspace check if time permits**

Run: `cargo test --workspace`
