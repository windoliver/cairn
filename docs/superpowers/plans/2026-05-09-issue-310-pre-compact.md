# Issue 310 PreCompact Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a typed `PreCompact` flow that computes a budget-bounded hot-memory reinjection payload, snapshots the pre-compaction transcript, and advertises the hook capability safely.

**Architecture:** Keep `assemble_hot` as a renderer and add a separate pre-compaction orchestrator in core. Reconcile the issue/spec wording with the repo's actual flat capability-string model, then thread the new flow through config, status advertisement, trace capture, and skill guidance with fail-closed behavior.

**Tech Stack:** Rust workspace, JSON Schema + codegen (`cairn-idl`), clap CLI, SQLite-backed trace capture tests, markdown skill docs.

---

### Task 1: Reconcile config + capability schema surface

**Files:**
- Modify: `crates/cairn-idl/schema/capabilities/capabilities.json`
- Modify: `crates/cairn-core/src/config/mod.rs`
- Modify: `crates/cairn-core/src/status/tests.rs`
- Modify: `docs/superpowers/specs/2026-05-09-issue-310-pre-compact-design.md`
- Regenerate: `crates/cairn-core/src/generated/common/mod.rs`
- Regenerate: `crates/cairn-core/src/generated/schemas/capabilities/capabilities.json`
- Regenerate: `crates/cairn-core/src/generated/schemas/prelude/status.json`
- Regenerate: `crates/cairn-mcp/src/generated/schemas/capabilities/capabilities.json`
- Regenerate: `crates/cairn-mcp/src/generated/schemas/prelude/status.json`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/cairn-core/src/config/mod.rs
#[test]
fn hot_memory_pre_compact_defaults_round_trip() {
    let cfg = CairnConfig::default();
    let yaml = serde_yaml::to_string(&cfg).unwrap();
    let back: CairnConfig = serde_yaml::from_str(&yaml).unwrap();

    assert_eq!(back.hot_memory.pre_compact_recipe.as_deref(), Some("handoff"));
    assert_eq!(back.hot_memory.pre_compact_safety_ratio, 0.30);
}

#[test]
fn hot_memory_pre_compact_ratio_above_one_rejected() {
    let yaml = r#"
hot_memory:
  max_bytes: 25600
  recipe: [purpose]
  pre_compact_recipe: handoff
  pre_compact_safety_ratio: 1.5
"#;

    let err = serde_yaml::from_str::<CairnConfig>(yaml).unwrap_err();
    assert!(err.to_string().contains("pre_compact_safety_ratio"));
}
```

```rust
// crates/cairn-core/src/status/tests.rs
#[test]
fn pre_compact_capability_not_advertised_before_wiring() {
    let gates = CapabilityGates {
        config: cap_set_default(false, false),
        store: None,
        vault_bound: true,
        model_present: false,
        embedding_provider_ready: false,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    };

    let caps = advertise(&gates);
    assert!(
        !caps.contains(&Capabilities::CairnMcpV1SensorsPreCompact),
        "pre_compact must stay hidden until the runtime is fully wired"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-core hot_memory_pre_compact_defaults_round_trip && cargo test -p cairn-core pre_compact_capability_not_advertised_before_wiring`

Expected: FAIL with missing `pre_compact_*` config fields and missing `Capabilities::CairnMcpV1SensorsPreCompact`.

- [ ] **Step 3: Write the minimal schema + config implementation**

```json
// crates/cairn-idl/schema/capabilities/capabilities.json
{ "const": "cairn.mcp.v1.sensors.pre_compact", "x-cairn-since": "v0.1" }
```

```rust
// crates/cairn-core/src/config/mod.rs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotMemoryConfig {
    pub recipe: Vec<HotMemoryRecipeStep>,
    pub max_bytes: u32,
    pub pre_compact_recipe: Option<String>,
    pub pre_compact_safety_ratio: f64,
}

impl Default for HotMemoryConfig {
    fn default() -> Self {
        Self {
            recipe: vec![
                HotMemoryRecipeStep::Purpose,
                HotMemoryRecipeStep::Index,
                HotMemoryRecipeStep::PinnedFeedback,
                HotMemoryRecipeStep::TopSalienceProject,
                HotMemoryRecipeStep::ActivePlaybook,
                HotMemoryRecipeStep::RecentUserSignal,
            ],
            max_bytes: 25_600,
            pre_compact_recipe: Some("handoff".to_owned()),
            pre_compact_safety_ratio: 0.30,
        }
    }
}
```

```rust
// crates/cairn-core/src/generated/common/mod.rs (after codegen)
#[serde(rename = "cairn.mcp.v1.sensors.pre_compact")]
CairnMcpV1SensorsPreCompact,
```

```md
<!-- docs/superpowers/specs/2026-05-09-issue-310-pre-compact-design.md -->
Replace `status.capabilities.sensors.pre_compact = true` wording with the
repo's flat capability string: `cairn.mcp.v1.sensors.pre_compact`.
```

- [ ] **Step 4: Regenerate schemas and run the tests to green**

Run: `cargo run -p cairn-idl --bin cairn-codegen && cargo test -p cairn-core hot_memory_pre_compact_defaults_round_trip && cargo test -p cairn-core pre_compact_capability_not_advertised_before_wiring`

Expected: PASS, and generated capability enums/schemas include `cairn.mcp.v1.sensors.pre_compact`.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/cairn-idl/schema/capabilities/capabilities.json \
  crates/cairn-core/src/config/mod.rs \
  crates/cairn-core/src/status/tests.rs \
  crates/cairn-core/src/generated/common/mod.rs \
  crates/cairn-core/src/generated/schemas/capabilities/capabilities.json \
  crates/cairn-core/src/generated/schemas/prelude/status.json \
  crates/cairn-mcp/src/generated/schemas/capabilities/capabilities.json \
  crates/cairn-mcp/src/generated/schemas/prelude/status.json \
  docs/superpowers/specs/2026-05-09-issue-310-pre-compact-design.md
git commit -m "feat(config): add pre-compact capability and config surface"
```

### Task 2: Add typed PreCompact core models and budget math

**Files:**
- Create: `crates/cairn-core/src/pipeline/pre_compact.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`
- Modify: `crates/cairn-core/src/domain/capture.rs`
- Test: `crates/cairn-core/src/pipeline/pre_compact.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/cairn-core/src/pipeline/pre_compact.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_budget_from_target_and_ratio() {
        let budget = compute_budget(8_000, 25_600, 0.30);
        assert_eq!(budget, 2_400);
    }

    #[test]
    fn caps_budget_at_hot_memory_max_bytes() {
        let budget = compute_budget(8_000, 1_000, 0.30);
        assert_eq!(budget, 1_000);
    }

    #[test]
    fn zero_target_yields_zero_budget() {
        let budget = compute_budget(0, 25_600, 0.30);
        assert_eq!(budget, 0);
    }
}
```

```rust
// crates/cairn-core/src/domain/capture.rs
#[test]
fn pre_compact_hook_payload_validates() {
    let payload = CapturePayload::Hook {
        hook_name: "PreCompact".into(),
        body: None,
        tool_name: None,
    };
    assert!(matches!(payload, CapturePayload::Hook { .. }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-core computes_budget_from_target_and_ratio && cargo test -p cairn-core caps_budget_at_hot_memory_max_bytes && cargo test -p cairn-core zero_target_yields_zero_budget`

Expected: FAIL because `pipeline::pre_compact` and `compute_budget` do not exist.

- [ ] **Step 3: Write the minimal implementation**

```rust
// crates/cairn-core/src/pipeline/pre_compact.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactEvent {
    pub session_id: SessionId,
    pub token_count_before: u32,
    pub compaction_target: u32,
    pub last_user_turn_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactOutput {
    pub reinjection_text: String,
    pub output_bytes: u64,
    pub budget_bytes: u64,
    pub recipe: String,
}

pub fn compute_budget(compaction_target: u32, max_bytes: u32, ratio: f64) -> u64 {
    let hinted = ((compaction_target as f64) * ratio).floor() as u64;
    hinted.min(u64::from(max_bytes))
}
```

```rust
// crates/cairn-core/src/pipeline/mod.rs
pub mod pre_compact;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cairn-core computes_budget_from_target_and_ratio && cargo test -p cairn-core caps_budget_at_hot_memory_max_bytes && cargo test -p cairn-core zero_target_yields_zero_budget`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/cairn-core/src/pipeline/pre_compact.rs \
  crates/cairn-core/src/pipeline/mod.rs \
  crates/cairn-core/src/domain/capture.rs
git commit -m "feat(core): add pre-compact event models and budget math"
```

### Task 3: Implement fail-closed PreCompact orchestration around assemble_hot + snapshot persistence

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/assembler.rs`
- Create: `crates/cairn-core/src/pipeline/pre_compact.rs`
- Modify: `crates/cairn-core/src/pipeline/capture_trace.rs`
- Test: `crates/cairn-core/src/pipeline/pre_compact.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/cairn-core/src/pipeline/pre_compact.rs
#[test]
fn pre_compact_runs_assemble_hot_and_snapshot_in_order() {
    let mut calls = Vec::new();

    let out = run_pre_compact(
        sample_event(),
        sample_cfg(),
        |_| {
            calls.push("assemble_hot");
            Ok(AssembleHotData { bytes: 4, prefix: "MEM".into(), segments: Some(vec![]) })
        },
        |_| {
            calls.push("snapshot");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(calls, vec!["assemble_hot", "snapshot"]);
    assert_eq!(out.reinjection_text, "MEM");
    assert_eq!(out.output_bytes, 4);
}

#[test]
fn pre_compact_snapshot_failure_rejects_hook() {
    let err = run_pre_compact(
        sample_event(),
        sample_cfg(),
        |_| Ok(AssembleHotData { bytes: 4, prefix: "MEM".into(), segments: Some(vec![]) }),
        |_| Err("disk full".to_owned()),
    )
    .unwrap_err();

    assert!(err.to_string().contains("snapshot"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-core pre_compact_runs_assemble_hot_and_snapshot_in_order && cargo test -p cairn-core pre_compact_snapshot_failure_rejects_hook`

Expected: FAIL because `run_pre_compact` and typed errors do not exist.

- [ ] **Step 3: Write the minimal orchestration**

```rust
// crates/cairn-core/src/pipeline/pre_compact.rs
#[derive(Debug, thiserror::Error)]
pub enum PreCompactError {
    #[error("assemble_hot: {0}")]
    AssembleHot(String),
    #[error("snapshot: {0}")]
    Snapshot(String),
}

pub fn run_pre_compact<AH, SNAP>(
    event: PreCompactEvent,
    cfg: &HotMemoryConfig,
    mut assemble_hot: AH,
    mut snapshot: SNAP,
) -> Result<PreCompactOutput, PreCompactError>
where
    AH: FnMut(u64) -> Result<AssembleHotData, String>,
    SNAP: FnMut(&PreCompactEvent) -> Result<(), String>,
{
    let budget = compute_budget(
        event.compaction_target,
        cfg.max_bytes,
        cfg.pre_compact_safety_ratio,
    );
    let recipe = cfg
        .pre_compact_recipe
        .clone()
        .unwrap_or_else(|| "handoff".to_owned());
    let data = assemble_hot(budget).map_err(PreCompactError::AssembleHot)?;
    snapshot(&event).map_err(PreCompactError::Snapshot)?;

    Ok(PreCompactOutput {
        reinjection_text: data.prefix,
        output_bytes: data.bytes,
        budget_bytes: budget,
        recipe,
    })
}
```

```rust
// crates/cairn-core/src/verbs/assemble_hot/assembler.rs
pub fn assemble_hot_with_budget(
    config: &HotMemoryConfig,
    budget: u64,
) -> Result<AssembleHotData, AssembleHotError> {
    let mut budgeted = config.clone();
    budgeted.max_bytes = budget.min(u64::from(u32::MAX)) as u32;
    assemble_hot(&budgeted)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cairn-core pre_compact_runs_assemble_hot_and_snapshot_in_order && cargo test -p cairn-core pre_compact_snapshot_failure_rejects_hook`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/cairn-core/src/pipeline/pre_compact.rs \
  crates/cairn-core/src/pipeline/capture_trace.rs \
  crates/cairn-core/src/verbs/assemble_hot/assembler.rs
git commit -m "feat(core): orchestrate pre-compact reinjection and snapshots"
```

### Task 4: Wire status advertisement, CLI trace capture, and skill guidance

**Files:**
- Modify: `crates/cairn-core/src/domain/trace.rs`
- Modify: `crates/cairn-core/src/status/mod.rs`
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`
- Modify: `crates/cairn-core/src/status/tests.rs`
- Modify: `skills/cairn/SKILL.md`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/cairn-core/src/status/tests.rs
#[test]
fn pre_compact_capability_advertised_when_wired() {
    let gates = CapabilityGates {
        config: cap_set_default(false, false),
        store: None,
        vault_bound: true,
        model_present: false,
        embedding_provider_ready: false,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    };

    let caps = advertise(&gates);
    assert!(caps.contains(&Capabilities::CairnMcpV1SensorsPreCompact));
}
```

```rust
// crates/cairn-cli/tests/capture_trace_verb.rs
#[tokio::test]
async fn pre_compact_event_is_captured_fail_closed() {
    let response = run_handler(&store, vault.path(), fixture_jsonl("PreCompact")).await.unwrap();
    assert!(response.failed_turns.is_empty(), "PreCompact should persist cleanly");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-core pre_compact_capability_advertised_when_wired && cargo test -p cairn-cli pre_compact_event_is_captured_fail_closed`

Expected: FAIL because the wiring flag is still false and CLI capture flow has no `PreCompact` handling.

- [ ] **Step 3: Write the minimal implementation**

```rust
// crates/cairn-core/src/domain/trace.rs
pub enum TraceEvent {
    UserMessage,
    AgentMessage,
    PreTool,
    PostTool,
    ToolOutput,
    PreCompact,
    Stop,
    TurnSummary,
}
```

```rust
// crates/cairn-core/src/status/wiring.rs
pub const SENSORS_PRE_COMPACT_WIRED: bool = true;
```

```rust
// crates/cairn-core/src/status/mod.rs
if wiring::SENSORS_PRE_COMPACT_WIRED {
    out.push(Capabilities::CairnMcpV1SensorsPreCompact);
}
```

```rust
// crates/cairn-core/src/pipeline/capture_trace.rs
CapturePayload::Hook { hook_name, .. } => match hook_name.as_str() {
    "UserPromptSubmit" => Ok(TraceEvent::UserMessage),
    "PreToolUse" => Ok(TraceEvent::PreTool),
    "PostToolUse" => Ok(TraceEvent::PostTool),
    "PreCompact" => Ok(TraceEvent::PreCompact),
    "Stop" => Ok(TraceEvent::Stop),
    _ => Err(TraceProjectError::Unclassifiable),
}
```

```md
<!-- skills/cairn/SKILL.md -->
- before harness compaction, call `cairn assemble_hot --session SESSION_ID --budget BUDGET --json`
- if the harness supports `PreCompact`, splice the returned text into the post-compaction prefix before continuing
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p cairn-core pre_compact_capability_advertised_when_wired && cargo test -p cairn-cli pre_compact_event_is_captured_fail_closed`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add \
  crates/cairn-core/src/domain/trace.rs \
  crates/cairn-core/src/status/mod.rs \
  crates/cairn-core/src/status/wiring.rs \
  crates/cairn-cli/src/verbs/capture_trace.rs \
  crates/cairn-cli/src/verbs/status.rs \
  crates/cairn-cli/tests/capture_trace_verb.rs \
  crates/cairn-core/src/status/tests.rs \
  skills/cairn/SKILL.md
git commit -m "feat(cli): wire pre-compact capability and trace handling"
```

### Task 5: Full verification and cleanup

**Files:**
- Verify only: workspace files touched above

- [ ] **Step 1: Run targeted crate tests**

```bash
cargo test -p cairn-core pre_compact
cargo test -p cairn-cli capture_trace
```

- [ ] **Step 2: Run codegen drift and formatting checks**

```bash
cargo fmt --all --check
cargo test -p cairn-idl
git diff --exit-code
```

Expected: no formatting drift, codegen committed, and no unexpected untracked changes.

- [ ] **Step 3: Run an end-to-end status sanity check**

```bash
cargo test -p cairn-core status::tests
cargo test -p cairn-mcp init_status_parity
```

Expected: `cairn.mcp.v1.sensors.pre_compact` appears only in the fully wired case and parity remains intact.

- [ ] **Step 4: Review user-facing docs and failure semantics**

```md
Checklist:
- spec wording matches flat capability string
- skill docs mention the pre-compaction reinjection path
- fail-closed snapshot behavior is covered by tests
```

- [ ] **Step 5: Commit final polish**

```bash
git add -A
git commit -m "test: verify pre-compact end-to-end behavior"
```
