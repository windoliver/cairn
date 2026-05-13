# Issue 289 Real Apply Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire real `flush apply` execution for non-placeholder plans and land `Patch`/`Rename` mutations, including session metadata patching, typed failures, diff rendering, and SQLite-backed integration coverage.

**Architecture:** Keep placeholder plans on the existing metadata-only path, but route non-placeholder plans through a dedicated apply helper invoked from `crates/cairn-cli/src/verbs/flush.rs`. Represent patch and rename as new `PlannedMutation` variants in `cairn-core`, execute them against the SQLite-backed store inside one transaction boundary, and preserve record history by writing new versions instead of mutating active rows in place.

**Tech Stack:** Rust, Cargo, serde, proptest, insta snapshots, rusqlite-backed `SqliteMemoryStore`, existing CLI integration harness.

---

## File Map

- Modify: `crates/cairn-core/src/domain/flush_plan/mod.rs`
  Adds `PatchTarget`, `StrReplace`, `ReplaceOccurrence`, and the new `PlannedMutation::{Patch, Rename}` variants.
- Modify: `crates/cairn-core/src/domain/flush_plan/diff.rs`
  Renders record patches, session patches, and rename mutations deterministically for human review.
- Modify: `crates/cairn-core/tests/flush_plan_proptest.rs`
  Extends serde round-trip generators to cover the new mutation shapes.
- Modify: `crates/cairn-cli/src/verbs/flush.rs`
  Replaces the current non-placeholder metadata-only branch with a real apply delegation path while preserving placeholder behavior.
- Create: `crates/cairn-cli/src/verbs/flush_apply.rs`
  Holds the dedicated apply executor and string-replacement helpers so `flush.rs` does not become larger.
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
  Exports the new apply helper module if needed by the verb tree.
- Modify: `crates/cairn-store-sqlite/src/error.rs`
  Adds typed store/apply failures for patch and rename operations.
- Modify: `crates/cairn-store-sqlite/src/store/sessions.rs`
  Adds a helper for resolving an existing session metadata target or returning `SessionNotFound`.
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`
  Adds transactional helpers for patch/rename execution and inbound edge rewriting.
- Modify: `crates/cairn-cli/tests/flush_integration.rs`
  Covers placeholder-vs-real apply behavior and CLI-visible plan status.
- Modify: `crates/cairn-store-sqlite/tests/versioning.rs`
  Verifies patching preserves old versions and creates a new active version.
- Modify: `crates/cairn-store-sqlite/tests/entity_graph.rs`
  Verifies rename rewrites inbound graph edges atomically.
- Create: `crates/cairn-store-sqlite/tests/flush_apply_mutations.rs`
  Focused integration coverage for patch success/failure, session patching, and rename collision semantics.

### Task 1: Extend FlushPlan Types and Serde Coverage

**Files:**
- Modify: `crates/cairn-core/src/domain/flush_plan/mod.rs`
- Modify: `crates/cairn-core/tests/flush_plan_proptest.rs`
- Modify: `crates/cairn-test-fixtures/src/flush_plan.rs`

- [ ] **Step 1: Write the failing serde/property tests**

```rust
#[test]
fn patch_and_rename_round_trip_json() {
    use cairn_core::domain::flush_plan::{
        PatchTarget, PlannedMutation, ReplaceOccurrence, StrReplace,
    };
    use cairn_core::domain::{SessionId, TargetId};

    let patch = PlannedMutation::Patch {
        target: PatchTarget::Session(SessionId::parse("01JTS6R4J70000000000000000").unwrap()),
        str_replace: vec![StrReplace {
            old: "old-title".into(),
            new: "new-title".into(),
            occurrence: ReplaceOccurrence::First,
        }],
    };
    let rename = PlannedMutation::Rename {
        record_id: TargetId::parse("01JTS6R4J70000000000000001").unwrap(),
        new_id: TargetId::parse("01JTS6R4J70000000000000002").unwrap(),
    };

    for mutation in [patch, rename] {
        let bytes = serde_json::to_vec(&mutation).expect("serialize");
        let back: PlannedMutation = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(
            serde_json::to_value(&mutation).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-core patch_and_rename_round_trip_json -- --exact`

Expected: FAIL with missing `Patch`, `Rename`, `PatchTarget`, `StrReplace`, or `ReplaceOccurrence` symbols.

- [ ] **Step 3: Add the new mutation/data types in `flush_plan/mod.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum PatchTarget {
    Record(TargetId),
    Session(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceOccurrence {
    First,
    All,
    Nth(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrReplace {
    pub old: String,
    pub new: String,
    pub occurrence: ReplaceOccurrence,
}
```

```rust
Patch {
    target: PatchTarget,
    str_replace: Vec<StrReplace>,
},
Rename {
    record_id: TargetId,
    new_id: TargetId,
},
```

- [ ] **Step 4: Extend the property generator with the new variants**

```rust
fn arb_replace_occurrence() -> impl Strategy<Value = ReplaceOccurrence> {
    prop_oneof![
        Just(ReplaceOccurrence::First),
        Just(ReplaceOccurrence::All),
        (0usize..4).prop_map(ReplaceOccurrence::Nth),
    ]
}

fn arb_str_replace() -> impl Strategy<Value = StrReplace> {
    ("[a-z]{1,8}", "[a-z]{1,8}", arb_replace_occurrence()).prop_map(
        |(old, new, occurrence)| StrReplace { old, new, occurrence },
    )
}
```

```rust
(
    arb_target(),
    prop::collection::vec(arb_str_replace(), 1..3),
).prop_map(|(record_id, str_replace)| PlannedMutation::Patch {
    target: PatchTarget::Record(record_id),
    str_replace,
}),
(
    arb_target(),
    arb_target(),
).prop_map(|(record_id, new_id)| PlannedMutation::Rename { record_id, new_id }),
```

- [ ] **Step 5: Run the core test set again**

Run: `cargo test -p cairn-core patch_and_rename_round_trip_json flush_plan_json_round_trip persisted_plan_round_trip`

Expected: PASS for the new focused serde tests.

- [ ] **Step 6: Commit**

```bash
git add \
  crates/cairn-core/src/domain/flush_plan/mod.rs \
  crates/cairn-core/tests/flush_plan_proptest.rs \
  crates/cairn-test-fixtures/src/flush_plan.rs
git commit -m "feat: add patch and rename flush plan types"
```

### Task 2: Render Patch and Rename in Human Review Output

**Files:**
- Modify: `crates/cairn-core/src/domain/flush_plan/diff.rs`
- Modify: `crates/cairn-core/src/domain/flush_plan/snapshots/cairn_core__domain__flush_plan__tests__diff_delete_md.snap`
- Create: new snapshot files under `crates/cairn-core/src/domain/flush_plan/snapshots/`

- [ ] **Step 1: Write the failing diff-render tests**

```rust
#[test]
fn renders_session_patch_mutation() {
    let plan = FlushPlan {
        mutations: vec![PlannedMutation::Patch {
            target: PatchTarget::Session(SessionId::parse("01JTS6R4J70000000000000000").unwrap()),
            str_replace: vec![StrReplace {
                old: "draft".into(),
                new: "final".into(),
                occurrence: ReplaceOccurrence::First,
            }],
        }],
        ..sample_plan_base()
    };
    insta::assert_snapshot!("diff_patch_session_md", render(&plan));
}

#[test]
fn renders_rename_mutation() {
    let plan = FlushPlan {
        mutations: vec![PlannedMutation::Rename {
            record_id: TargetId::parse("01JTS6R4J70000000000000001").unwrap(),
            new_id: TargetId::parse("01JTS6R4J70000000000000002").unwrap(),
        }],
        ..sample_plan_base()
    };
    insta::assert_snapshot!("diff_rename_md", render(&plan));
}
```

- [ ] **Step 2: Run the diff tests to verify they fail**

Run: `cargo test -p cairn-core renders_session_patch_mutation renders_rename_mutation -- --exact`

Expected: FAIL with non-exhaustive match errors in `diff.rs` or missing snapshot outputs.

- [ ] **Step 3: Add the new match arms in `diff.rs`**

```rust
PlannedMutation::Patch { target, str_replace } => {
    writeln!(&mut out, "- **Kind:** patch").ok();
    match target {
        PatchTarget::Record(target) => {
            writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
        }
        PatchTarget::Session(session) => {
            writeln!(&mut out, "- **Session:** `{}`", session.as_str()).ok();
        }
    }
    for change in str_replace {
        writeln!(
            &mut out,
            "- **Replace:** `{}` -> `{}` ({:?})",
            change.old,
            change.new,
            change.occurrence
        ).ok();
    }
}
PlannedMutation::Rename { record_id, new_id } => {
    writeln!(&mut out, "- **Kind:** rename").ok();
    writeln!(&mut out, "- **Target:** `{}` -> `{}`", record_id.as_str(), new_id.as_str()).ok();
}
```

- [ ] **Step 4: Accept the new snapshots**

Run: `cargo test -p cairn-core renders_session_patch_mutation renders_rename_mutation -- --exact`

Expected: PASS and new snapshot files created for patch and rename rendering.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/cairn-core/src/domain/flush_plan/diff.rs \
  crates/cairn-core/src/domain/flush_plan/snapshots
git commit -m "feat: render patch and rename flush plan diffs"
```

### Task 3: Replace the Real-Plan Metadata-Only Apply Path

**Files:**
- Create: `crates/cairn-cli/src/verbs/flush_apply.rs`
- Modify: `crates/cairn-cli/src/verbs/flush.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write the failing apply-path CLI test**

```rust
#[test]
fn flush_apply_real_plan_records_full_apply_kind() {
    let (_tmp, vault) = fresh_vault();
    let operation_id = "01JTS6R4J7000000000000000A";
    write_non_placeholder_patch_plan(&vault, operation_id);

    let assert = Command::cargo_bin("cairn")
        .unwrap()
        .args(["flush", "apply", operation_id, "--vault", vault.to_str().unwrap()])
        .assert();

    assert.success();
    let persisted = read_applied_plan(&vault, operation_id);
    let PlanStatus::Applied { apply_kind, .. } = persisted.status else {
        panic!("expected applied status");
    };
    assert_eq!(apply_kind, ApplyKind::Full);
}
```

- [ ] **Step 2: Run the focused CLI test to verify it fails**

Run: `cargo test -p cairn-cli flush_apply_real_plan_records_full_apply_kind -- --exact`

Expected: FAIL because the current code writes `apply_kind = MetadataOnly`.

- [ ] **Step 3: Create `flush_apply.rs` with an execution entry point**

```rust
pub(crate) async fn apply_real_plan(
    store: &Arc<dyn MemoryStore>,
    sqlite: Option<&Arc<SqliteMemoryStore>>,
    plan: &FlushPlan,
) -> anyhow::Result<()> {
    for mutation in &plan.mutations {
        match mutation {
            PlannedMutation::Patch { .. } => {
                anyhow::bail!("patch apply not implemented yet");
            }
            PlannedMutation::Rename { .. } => {
                anyhow::bail!("rename apply not implemented yet");
            }
            _ => {}
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire `flush.rs` to use the helper for non-placeholder plans**

```rust
if persisted.plan.placeholder {
    persisted.status = PlanStatus::Applied {
        at: now_rfc3339(),
        apply_kind: ApplyKind::MetadataOnly,
    };
} else {
    flush_apply::apply_real_plan(&store, sqlite_store.as_ref(), &persisted.plan)
        .await
        .map_err(|e| format!("apply failed: {e}"))?;
    persisted.status = PlanStatus::Applied {
        at: now_rfc3339(),
        apply_kind: ApplyKind::Full,
    };
}
```

- [ ] **Step 5: Run the focused CLI test again**

Run: `cargo test -p cairn-cli flush_apply_real_plan_records_full_apply_kind -- --exact`

Expected: still FAIL, but now because patch execution is not implemented rather than because the plan is always metadata-only.

- [ ] **Step 6: Commit**

```bash
git add \
  crates/cairn-cli/src/verbs/flush_apply.rs \
  crates/cairn-cli/src/verbs/flush.rs \
  crates/cairn-cli/src/verbs/mod.rs \
  crates/cairn-cli/tests/flush_integration.rs
git commit -m "refactor: route real flush apply through dedicated executor"
```

### Task 4: Implement Patch Execution and Typed Failures

**Files:**
- Modify: `crates/cairn-store-sqlite/src/error.rs`
- Modify: `crates/cairn-store-sqlite/src/store/sessions.rs`
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`
- Modify: `crates/cairn-cli/src/verbs/flush_apply.rs`
- Create: `crates/cairn-store-sqlite/tests/flush_apply_mutations.rs`
- Modify: `crates/cairn-store-sqlite/tests/versioning.rs`

- [ ] **Step 1: Write the failing store integration tests for patching**

```rust
#[tokio::test]
async fn patch_creates_new_active_version_and_preserves_old_body() {
    let store = open_test_store().await;
    let original = sample_record_with_body("alpha beta gamma");
    store.upsert(&original).await.expect("seed");

    apply_patch_record(
        &store,
        original.target_id.clone(),
        vec![StrReplace {
            old: "beta".into(),
            new: "delta".into(),
            occurrence: ReplaceOccurrence::First,
        }],
    )
    .await
    .expect("patch");

    let active = store.get_active_by_target(&original.target_id).await.unwrap().unwrap();
    assert_eq!(active.record.body, "alpha delta gamma");
    let history = store.versions(&original.target_id).await.unwrap();
    assert_eq!(history.len(), 2);
}

#[tokio::test]
async fn patch_missing_substring_fails_atomically() {
    let store = open_test_store().await;
    let original = sample_record_with_body("alpha beta gamma");
    store.upsert(&original).await.expect("seed");

    let err = apply_patch_record(
        &store,
        original.target_id.clone(),
        vec![StrReplace {
            old: "missing".into(),
            new: "delta".into(),
            occurrence: ReplaceOccurrence::First,
        }],
    )
    .await
    .expect_err("expected missing substring");

    assert!(err.to_string().contains("substring"));
    let active = store.get_active_by_target(&original.target_id).await.unwrap().unwrap();
    assert_eq!(active.record.body, "alpha beta gamma");
}
```

- [ ] **Step 2: Run the patch integration tests to verify they fail**

Run: `cargo test -p cairn-store-sqlite patch_creates_new_active_version_and_preserves_old_body patch_missing_substring_fails_atomically -- --exact`

Expected: FAIL with missing apply helpers and missing typed patch errors.

- [ ] **Step 3: Add the store error variants**

```rust
PatchTargetMissing {
    target: String,
},
PatchSubstringMissing {
    target: String,
    old: String,
},
RenameTargetConflict {
    target: String,
    new_target: String,
},
```

- [ ] **Step 4: Implement deterministic replacement helpers in `flush_apply.rs`**

```rust
fn apply_one_replace(body: &str, change: &StrReplace) -> Result<String, StoreError> {
    match change.occurrence {
        ReplaceOccurrence::First => body.replacen(&change.old, &change.new, 1).pipe(|next| {
            if next == body {
                Err(StoreError::PatchSubstringMissing {
                    target: "<resolved later>".into(),
                    old: change.old.clone(),
                })
            } else {
                Ok(next)
            }
        }),
        ReplaceOccurrence::All => {
            if !body.contains(&change.old) {
                Err(StoreError::PatchSubstringMissing {
                    target: "<resolved later>".into(),
                    old: change.old.clone(),
                })
            } else {
                Ok(body.replace(&change.old, &change.new))
            }
        }
        ReplaceOccurrence::Nth(n) => replace_nth(body, &change.old, &change.new, n),
    }
}
```

- [ ] **Step 5: Implement transactional patch execution**

```rust
store.with_tx(move |tx| {
    let active = tx.get_active_by_target_sync(&target)?
        .ok_or_else(|| StoreError::PatchTargetMissing { target: target.as_str().into() })?;
    let mut next_body = active.record.body.clone();
    for change in &str_replace {
        next_body = apply_one_replace(&next_body, change)?;
    }
    let mut next_record = active.record.clone();
    next_record.body = next_body;
    next_record.validate()?;
    tx.upsert(&next_record)?;
    Ok(())
}).await?;
```

- [ ] **Step 6: Add session-target resolution using the existing session APIs**

```rust
pub(crate) fn session_metadata_target(session: &SessionId) -> TargetId {
    TargetId::parse(&format!("session:{}/meta", session.as_str())).expect("well-known target")
}
```

```rust
let target = match patch_target {
    PatchTarget::Record(target) => target.clone(),
    PatchTarget::Session(session) => resolve_existing_session_metadata_target(tx, session)?,
};
```

- [ ] **Step 7: Run the focused patch tests again**

Run: `cargo test -p cairn-store-sqlite patch_creates_new_active_version_and_preserves_old_body patch_missing_substring_fails_atomically -- --exact`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add \
  crates/cairn-store-sqlite/src/error.rs \
  crates/cairn-store-sqlite/src/store/sessions.rs \
  crates/cairn-store-sqlite/src/store/tx.rs \
  crates/cairn-cli/src/verbs/flush_apply.rs \
  crates/cairn-store-sqlite/tests/flush_apply_mutations.rs \
  crates/cairn-store-sqlite/tests/versioning.rs
git commit -m "feat: execute flush patch mutations atomically"
```

### Task 5: Implement Rename Execution and Inbound Edge Rewrites

**Files:**
- Modify: `crates/cairn-cli/src/verbs/flush_apply.rs`
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`
- Modify: `crates/cairn-store-sqlite/tests/entity_graph.rs`
- Modify: `crates/cairn-store-sqlite/tests/flush_apply_mutations.rs`
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write the failing rename tests**

```rust
#[tokio::test]
async fn rename_rejects_collision() {
    let store = open_test_store().await;
    let source = sample_record_with_target("01JTS6R4J70000000000000011");
    let dest = sample_record_with_target("01JTS6R4J70000000000000012");
    store.upsert(&source).await.expect("seed source");
    store.upsert(&dest).await.expect("seed dest");

    let err = apply_rename(&store, source.target_id.clone(), dest.target_id.clone())
        .await
        .expect_err("expected collision");

    assert!(err.to_string().contains("conflict"));
}

#[tokio::test]
async fn rename_rewrites_inbound_edges() {
    let store = open_test_store().await;
    let source = sample_graph_record("01JTS6R4J70000000000000021");
    let inbound = sample_graph_record("01JTS6R4J70000000000000022");
    store.upsert(&source).await.expect("seed source");
    store.upsert(&inbound).await.expect("seed inbound");
    store.put_edge(&edge_to(&inbound, &source)).await.expect("seed edge");

    apply_rename(
        &store,
        source.target_id.clone(),
        TargetId::parse("01JTS6R4J70000000000000023").unwrap(),
    )
    .await
    .expect("rename");

    let edges = store.neighbours(&inbound.record_id, EdgeDir::Out).await.unwrap();
    assert!(edges.iter().any(|edge| edge.to == RecordId::parse("01JTS6R4J70000000000000023").unwrap()));
}
```

- [ ] **Step 2: Run the rename tests to verify they fail**

Run: `cargo test -p cairn-store-sqlite rename_rejects_collision rename_rewrites_inbound_edges -- --exact`

Expected: FAIL with missing rename executor behavior.

- [ ] **Step 3: Implement transactional rename execution**

```rust
store.with_tx(move |tx| {
    let active = tx.get_active_by_target_sync(&record_id)?
        .ok_or_else(|| StoreError::PatchTargetMissing { target: record_id.as_str().into() })?;
    if tx.get_active_by_target_sync(&new_id)?.is_some() {
        return Err(StoreError::RenameTargetConflict {
            target: record_id.as_str().into(),
            new_target: new_id.as_str().into(),
        });
    }

    let mut next_record = active.record.clone();
    next_record.target_id = new_id.clone();
    tx.upsert(&next_record)?;
    tx.rewrite_inbound_edges(&active.record.record_id, &next_record.record_id)?;
    tx.tombstone(&active.record.record_id, TombstoneReason::Superseded)?;
    Ok(())
}).await?;
```

- [ ] **Step 4: Add an inbound-edge rewrite helper in `store/tx.rs`**

```rust
pub fn rewrite_inbound_edges(&self, old: &RecordId, new: &RecordId) -> Result<(), StoreError> {
    self.conn.execute(
        "UPDATE edges SET to_record_id = ?1 WHERE to_record_id = ?2",
        params![new.as_str(), old.as_str()],
    )?;
    Ok(())
}
```

- [ ] **Step 5: Run the focused rename tests again**

Run: `cargo test -p cairn-store-sqlite rename_rejects_collision rename_rewrites_inbound_edges -- --exact`

Expected: PASS.

- [ ] **Step 6: Run the end-to-end apply test set**

Run: `cargo test -p cairn-cli flush_apply_real_plan_records_full_apply_kind`

Expected: PASS with `ApplyKind::Full` for real plans and no regressions to placeholder tests.

- [ ] **Step 7: Commit**

```bash
git add \
  crates/cairn-cli/src/verbs/flush_apply.rs \
  crates/cairn-store-sqlite/src/store/tx.rs \
  crates/cairn-store-sqlite/tests/entity_graph.rs \
  crates/cairn-store-sqlite/tests/flush_apply_mutations.rs \
  crates/cairn-cli/tests/flush_integration.rs
git commit -m "feat: execute flush rename mutations with edge rewrites"
```

### Task 6: Final Verification and Cleanup

**Files:**
- Modify: any touched files from Tasks 1-5 only if required by failing verification

- [ ] **Step 1: Run the focused package suites**

Run: `cargo test -p cairn-core`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite flush_apply_mutations versioning entity_graph`

Expected: PASS.

Run: `cargo test -p cairn-cli flush_integration`

Expected: PASS.

- [ ] **Step 2: Run formatting**

Run: `cargo fmt --all`

Expected: no diff after formatting.

- [ ] **Step 3: Re-run the highest-signal tests after formatting**

Run: `cargo test -p cairn-core patch_and_rename_round_trip_json renders_session_patch_mutation renders_rename_mutation`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite patch_creates_new_active_version_and_preserves_old_body patch_missing_substring_fails_atomically rename_rejects_collision rename_rewrites_inbound_edges`

Expected: PASS.

Run: `cargo test -p cairn-cli flush_apply_real_plan_records_full_apply_kind`

Expected: PASS.

- [ ] **Step 4: Commit the final verified state**

```bash
git add \
  crates/cairn-core/src/domain/flush_plan/mod.rs \
  crates/cairn-core/src/domain/flush_plan/diff.rs \
  crates/cairn-core/tests/flush_plan_proptest.rs \
  crates/cairn-cli/src/verbs/flush.rs \
  crates/cairn-cli/src/verbs/flush_apply.rs \
  crates/cairn-store-sqlite/src/error.rs \
  crates/cairn-store-sqlite/src/store/sessions.rs \
  crates/cairn-store-sqlite/src/store/tx.rs \
  crates/cairn-cli/tests/flush_integration.rs \
  crates/cairn-store-sqlite/tests/flush_apply_mutations.rs \
  crates/cairn-store-sqlite/tests/versioning.rs \
  crates/cairn-store-sqlite/tests/entity_graph.rs
git commit -m "feat: add real flush apply for patch and rename"
```

## Self-Review

- Spec coverage check:
  - `Patch`/`Rename` types and serde are covered in Task 1.
  - human-review diff rendering is covered in Task 2.
  - non-placeholder real apply versus placeholder metadata-only behavior is covered in Task 3.
  - record patching, session metadata patching, atomic failures, and typed errors are covered in Task 4.
  - rename collision and inbound edge rewrites are covered in Task 5.
  - final verification and regression protection are covered in Task 6.
- Placeholder scan:
  - removed generic placeholders like "add tests" and replaced them with exact files, commands, and sample code.
- Type consistency:
  - plan consistently uses `PatchTarget`, `StrReplace`, `ReplaceOccurrence`, `ApplyKind::Full`, and `RenameTargetConflict`.
