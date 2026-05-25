# Issue #128: Skillify 10-Step Pipeline and SkillPack Packaging

**Issue:** [#128](https://github.com/windoliver/cairn/issues/128)
**Design sections:** brief §11.b (Skillify), §11.1 (Evolvable artifacts), §11.3 (Promotion predicate)
**Parent:** #28 (P2 EvolutionWorkflow / Skillify / SkillPacks)
**Dependency:** #127 (EvolutionWorkflow state machine) — closed

---

## Summary

Implement the full Skillify 5-stage pipeline state machine with 10 gate runners,
SkillPack packaging with dependency metadata and compatibility ranges, and
fail-closed enforcement at every stage boundary. The pipeline turns
failures/successful trajectories into tested, versioned skill artifacts and
packages related skills into portable SkillPacks.

---

## Existing Code

The following infrastructure is already built and will be extended:

| Layer | Module | What exists |
|-------|--------|-------------|
| Core data models | `cairn-core::pipeline::skillify` | `SkillArtifactKind` (10 kinds), `SkillArtifact`, `SkillArtifactBundle`, `SkillifyCandidate`, `SkillifyGate/Report`, `SkillLintSnapshot`, `lint_skill_snapshot()` |
| Workflow handler | `cairn-workflows::skillify` | `SkillifyHandler` (single LLM call), `SkillifyPayload`, `enqueue_skillify()`, `materialize_bundle()`, `SkillifyPlanSource` |
| Evolution | `cairn-core::pipeline::evolution` | `EvolutionRun` state machine, `EvolutionGateReport`, `EvolutionLineage` (from #127) |
| CLI | `cairn-cli::skill` | `cairn skill install` with agent integrations |

---

## Architecture

Three-layer split following established crate patterns:

```
cairn-core (pure data + transitions)
  pipeline::skillify::stage   — SkillifyStage enum, SkillifyPipelineState
  pipeline::skillify::pack    — SkillPackManifest, SkillPackEntry, validation
  pipeline::skillify::spec    — SkillSpecDraft (STAGE 1 output)

cairn-workflows (async orchestration + I/O)
  skillify::pipeline          — SkillifyPipeline orchestrator
  skillify::gate_runner       — GateRunner trait + 10 implementations
  skillify::gate_registry     — GateRunnerRegistry
  skillify::packer            — pack_skills() archive builder
  skillify::health            — daily health check runner

cairn-cli (user surface)
  verbs::skillpack            — `cairn skillpack pack|install|inspect`
```

---

## Section 1: Pipeline State Machine

**File:** `crates/cairn-core/src/pipeline/skillify/stage.rs`

### SkillifyStage

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyStage {
    Extract,
    Author,
    Gate,
    Promote,
    HealthCheck,
    Failed,
    Blocked,
}
```

### SkillifyPipelineState

Pure state machine. No I/O, no async.

```rust
pub struct SkillifyPipelineState {
    candidate_id: String,
    stage: SkillifyStage,
    spec: Option<SkillSpecDraft>,
    bundle: Option<SkillArtifactBundle>,
    gate_report: SkillifyGateReport,       // uses SkillifyGate (core type)
    promotion_plan_ref: Option<String>,
    failure_reason: Option<String>,
}
```

**Transition methods** (each returns `Result<(), SkillifyStageError>`):

| Method | From | To | Preconditions |
|--------|------|----|---------------|
| `advance_to_author(spec)` | Extract | Author | spec validated |
| `advance_to_gate(bundle)` | Author | Gate | bundle validated via `SkillArtifactBundle::validate()` |
| `record_gate(kind, result)` | Gate | Gate | accumulates results |
| `advance_to_promote()` | Gate | Promote | all 10 required gates present and passed |
| `advance_to_health(plan_ref)` | Promote | HealthCheck | promotion plan ref provided |
| `fail(reason)` | any non-terminal | Failed | reason non-empty |
| `block(reason)` | any non-terminal | Blocked | reason non-empty |

**`SkillifyStageError`:**
- `InvalidTransition { from, to }` — illegal state transition
- `GatesNotSatisfied { missing, failed }` — attempt to promote with failing gates
- `MissingPrecondition { field }` — required data not set for transition

### SkillSpecDraft

**File:** `crates/cairn-core/src/pipeline/skillify/spec.rs`

```rust
pub struct SkillSpecDraft {
    pub lane: String,
    pub slug: String,
    pub decision_tree: serde_json::Value,
    pub triggers: Vec<String>,
    pub success_criteria: Vec<String>,
    pub source_refs: Vec<String>,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
}
```

Validation: lane non-empty, slug is a safe path token, triggers non-empty,
at least one source ref.

---

## Section 2: Gate Runner Framework

**File:** `crates/cairn-workflows/src/skillify/gate_runner.rs`

### GateRunner trait

```rust
#[async_trait]
pub trait GateRunner: Send + Sync {
    fn artifact_kind(&self) -> SkillArtifactKind;
    async fn run(&self, ctx: &GateRunContext) -> GateRunResult;
}
```

### GateRunContext

```rust
pub struct GateRunContext<'a> {
    pub vault_root: &'a Path,
    pub candidate_id: &'a str,
    pub candidate_dir: PathBuf,
    pub bundle: &'a SkillArtifactBundle,
    pub authored: &'a AuthoredSkillBundle,
    pub llm: Option<&'a dyn LLMProvider>,
    pub snapshot: &'a SkillLintSnapshot,
}
```

### GateRunResult

Workflows-local type. The pipeline converts to `SkillifyGate` (core type) when
updating the state machine via `record_gate()`.

```rust
pub struct GateRunResult {
    pub kind: SkillArtifactKind,
    pub status: SkillifyGateStatus,
    pub message: Option<String>,
    pub evidence_refs: Vec<String>,
    pub duration_ms: u64,
}

impl GateRunResult {
    pub fn into_gate(self) -> SkillifyGate { /* maps kind → name, drops duration */ }
}
```

### 10 Runner Implementations

| # | Struct | Kind | Logic |
|---|--------|------|-------|
| 1 | `SkillContractRunner` | SkillContract | Parse skill markdown, validate frontmatter fields (lane, triggers, uses, files_to). Fail if any required field missing or malformed. |
| 2 | `DeterministicScriptRunner` | DeterministicScript | Verify script file exists at declared path, starts with shebang (`#!`), is non-empty. Does not execute — execution is the unit test runner's job. |
| 3 | `UnitTestRunner` | UnitTests | Parse unit test JSON artifact. Each test case has `input`, `expected_output`, `timeout_ms`. Execute script via `tokio::process::Command` with input on stdin, compare stdout to expected. Fail if any case fails or times out (default 10s). |
| 4 | `IntegrationTestRunner` | IntegrationTests | Same structure as unit tests but with `fixtures` field pointing to real data files in the bundle. Execute with `CAIRN_INTEGRATION=1` env var. Fail on mismatch or timeout (default 30s). |
| 5 | `LlmEvalRunner` | LlmEvals | Parse eval JSON with rubric items. Each item: `prompt`, `expected_behavior`, `scoring_criteria`. Call `LLMProvider::complete()` with a judge prompt. Parse pass/fail per rubric item. Fail if LLM unavailable or any rubric item fails. |
| 6 | `ResolverTriggerRunner` | ResolverTrigger | Parse trigger JSON. Validate each trigger is non-empty, no duplicates within the candidate, no collision with existing snapshot triggers for different lanes. |
| 7 | `ResolverEvalRunner` | ResolverEval | Parse eval JSON with labelled intents: `{ intent, expected_lane, expected_match }`. Run each intent through the candidate's trigger set using substring/keyword matching. Compute precision/recall. Fail if recall < 0.8 or precision < 0.9 (configurable thresholds). |
| 8 | `CheckResolvableAndDryRunner` | CheckResolvableAndDry | Merge candidate into the existing `SkillLintSnapshot`. Run `lint_skill_snapshot()`. Fail if any `DuplicateLane` or `Unreachable` issues found for the candidate skill. |
| 9 | `E2eSmokeRunner` | E2eSmoke | Parse smoke JSON with end-to-end cases: `{ trigger_phrase, expected_script_call, expected_output }`. Simulate: match trigger → resolve to skill → execute script → compare output. Fail on any mismatch. Timeout: 60s per case. |
| 10 | `FilingRulesRunner` | FilingRules | Parse filing rules JSON. Validate `files_to` is a valid relative directory path (reuses `valid_relative_dir()` from lint). Check the target directory exists or is a valid vault prefix. |

### GateRunnerRegistry

```rust
pub struct GateRunnerRegistry {
    runners: Vec<Box<dyn GateRunner>>,
}

impl GateRunnerRegistry {
    pub fn default_suite() -> Self { /* all 10 runners */ }

    pub async fn run_all(&self, ctx: &GateRunContext<'_>) -> Vec<GateRunResult> {
        // Execute in dependency order:
        // 1. SkillContract (validates the foundation)
        // 2. DeterministicScript (validates the script exists)
        // 3. FilingRules, ResolverTrigger (independent validation)
        // 4. UnitTests (requires script)
        // 5. IntegrationTests (requires script)
        // 6. LlmEvals (requires contract + script)
        // 7. ResolverEval (requires triggers)
        // 8. CheckResolvableAndDry (requires all above)
        // 9. E2eSmoke (full pipeline, requires everything)
        //
        // If a dependency fails, downstream gates are Blocked.
    }
}
```

---

## Section 3: SkillPack Data Model

**File:** `crates/cairn-core/src/pipeline/skillify/pack.rs`

### SkillPackManifest

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackManifest {
    pub pack_id: String,
    pub name: String,
    pub version: String,
    pub cairn_compat: String,
    pub description: String,
    pub skills: Vec<SkillPackEntry>,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub content_sha256: String,
}
```

### SkillPackEntry

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackEntry {
    pub candidate_id: String,
    pub lane: String,
    pub slug: String,
    pub bundle_version: u32,
    pub artifact_sha256: String,
}
```

### SkillPackError

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillPackError {
    #[error("pack skill `{candidate_id}` not found in archive")]
    MissingSkill { candidate_id: String },

    #[error("duplicate lane `{lane}` in pack")]
    DuplicateLane { lane: String },

    #[error("pack requires Cairn {required} but running {running}")]
    IncompatibleCairn { required: String, running: String },

    #[error("dependency `{dep}` not provided by any skill in pack")]
    DependencyMissing { dep: String },

    #[error("content integrity check failed: expected {expected}, got {actual}")]
    IntegrityFailure { expected: String, actual: String },

    #[error("invalid pack name: {reason}")]
    InvalidName { reason: String },
}
```

### Validation

`SkillPackManifest::validate(cairn_version: &str) -> Result<(), SkillPackError>`:

1. Name is non-empty, alphanumeric + hyphens only
2. No duplicate lanes across entries
3. `cairn_compat` is a simple floor constraint (`>=X.Y.Z`), not full semver range resolution. Parsed by splitting on `.` and comparing `(major, minor, patch)` tuples.
4. Every `requires` entry matched by some `provides` entry across all skills
5. `content_sha256` verified against provided digest (caller supplies computed hash)

### Pack ID derivation

Deterministic: `skp_<sha256(name + version + sorted(candidate_ids)))>`

### Archive format

`.cairnpack` files are gzip-compressed tar archives (using `flate2` + `tar` crates,
both already in the ecosystem and well-audited):
```
manifest.json              — SkillPackManifest
skills/<candidate_id>/     — one directory per skill
  manifest.json            — SkillArtifactBundle
  gate-report.json         — SkillifyGateReport
  bundle/                  — artifact files
    skills/skill_<slug>.md
    scripts/<slug>.sh
    tests/unit/<slug>.json
    tests/integration/<slug>.json
    evals/llm/<slug>.json
    resolver/triggers.json
    resolver/eval.json
    audits/check-resolvable.json
    smoke/<slug>.json
    filing-rules.json
```

### Packaging (`cairn-workflows`)

**File:** `crates/cairn-workflows/src/skillify/packer.rs`

```rust
pub struct SkillPackBuilder {
    name: String,
    version: String,
    cairn_compat: String,
    description: String,
}

impl SkillPackBuilder {
    pub fn add_candidate(&mut self, candidate_id: &str) -> Result<(), SkillPackError>;
    pub fn build(self, vault_root: &Path) -> Result<SkillPackArchive, SkillPackBuildError>;
}

pub struct SkillPackArchive {
    pub manifest: SkillPackManifest,
    pub archive_path: PathBuf,
}
```

`build()`:
1. For each candidate: read materialized bundle, verify gate report passes
2. Aggregate requires/provides
3. Validate manifest
4. Create tar.gz archive
5. Compute content_sha256 and write final manifest

### Install (`cairn-cli`)

`cairn skillpack install <path>`:
1. Read and decompress archive
2. Parse and validate manifest against running Cairn version
3. For each skill entry: extract to `.cairn/evolution/skillify/<candidate_id>/`
4. Run `lint_skill_snapshot()` against all installed skills
5. If lint fails → rollback extraction, report errors
6. Print install receipt

`cairn skillpack inspect <path>`:
- Print manifest summary (name, version, skills, dependencies)

`cairn skillpack pack --name <name> --version <ver> --candidates <id1,id2,...>`:
- Build and write `.cairnpack` archive

---

## Section 4: Pipeline Orchestrator and Fail-Closed Enforcement

**File:** `crates/cairn-workflows/src/skillify/pipeline.rs`

### SkillifyPipeline

```rust
pub struct SkillifyPipeline {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
    gate_registry: GateRunnerRegistry,
}
```

### run()

```rust
pub async fn run(
    &self,
    payload: SkillifyPayload,
) -> Result<SkillifyPipelineResult, SkillifyPipelineError>
```

**STAGE 1 — Extract:**
- Build extraction prompt from `payload.source_record_ids`
- Call LLM to produce `SkillSpecDraft` JSON
- Validate draft via `SkillSpecDraft::validate()`
- Write `skill-spec.draft.json` to candidate dir
- If no LLM: transition to Blocked, write blocked marker, return early
- If LLM fails: transition to Failed, return early

**STAGE 2 — Author:**
- Build authoring prompt from the validated spec
- Call LLM to produce `AuthoredSkillBundle` JSON (10 artifact fields)
- Parse via `AuthoredSkillBundle::try_from(value)`
- Materialize via existing `materialize_bundle()`
- If LLM returns invalid JSON: transition to Failed

**STAGE 3 — Gate:**
- Build `GateRunContext` with vault root, candidate dir, bundle, authored content,
  LLM reference, and current skill lint snapshot
- Call `gate_registry.run_all(ctx)`
- Record each result in the state machine
- Write updated `gate-report.json`
- **Fail-closed:** if any gate has status `Failed` or `Blocked`, call
  `state.fail(summary)`. No promotion plan created. The candidate stays
  materialized but is marked as failed.

**STAGE 4 — Promote:**
- Only reached when all 10 gates passed
- Call `SkillifyPlanSource::plan_promotion()` to build a FlushPlan
- Transition state to Promoted
- Write promotion plan to candidate dir

**STAGE 5 — HealthCheck (separate entry point):**
- `pub async fn health_check(&self, candidate_id: &str) -> HealthCheckResult`
- Re-runs all gate runners against a live (promoted) skill
- If any gate regresses:
  - Update `gate-report.json` with failures
  - Set candidate status to `Unhealthy`
  - Flag in lint output
- Designed to be called by a daily scheduled job (separate from the main pipeline)

### SkillifyPipelineResult

```rust
pub struct SkillifyPipelineResult {
    pub candidate_id: String,
    pub final_stage: SkillifyStage,
    pub gate_report: SkillifyGateReport,
    pub promotion_plan: Option<FlushPlan>,
    pub errors: Vec<String>,
    pub duration_ms: u64,
}
```

### SkillifyPipelineError

```rust
#[derive(Debug, thiserror::Error)]
pub enum SkillifyPipelineError {
    #[error("no LLM provider configured")]
    NoLlm,
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Materialize(#[from] SkillifyMaterializeError),
    #[error(transparent)]
    Stage(#[from] SkillifyStageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

### Handler refactor

The existing `SkillifyHandler::run_once()` is refactored to:
1. Construct `SkillifyPipeline` (reuses `self.vault_root` and `self.llm`)
2. Call `pipeline.run(payload).await`
3. Map `SkillifyPipelineResult` to `HandlerOutcome`

The `SkillifyHandler` struct, `SKILLIFY_KIND`, and `JobHandler` impl remain
unchanged. The handler is the job-store entry point; the pipeline owns the
multi-stage logic.

### Fail-closed invariants

| Condition | Behavior | Stage |
|-----------|----------|-------|
| No LLM configured | Pipeline state → Blocked. Blocked marker written. Job permanent failure. | 1 |
| LLM returns non-JSON | Pipeline state → Failed. No artifacts written. Job permanent failure. | 1, 2 |
| LLM returns invalid spec/bundle | Pipeline state → Failed. Candidate dir cleaned up. Job permanent failure. | 1, 2 |
| LLM provider unreachable | Job transient retry. State not advanced. | 1, 2 |
| Any gate runner returns Failed | Pipeline state → Failed. Gate report with failures written. No promotion. | 3 |
| Any gate runner panics | Caught via `catch_unwind` or spawn boundary. Treated as gate Failed. | 3 |
| Gate runner times out | Treated as gate Failed with timeout message. | 3 |
| All gates pass | Pipeline advances to Promote. FlushPlan created. | 4 |
| Health check gate regression | Status → Unhealthy. Flagged in lint. No automatic rollback. | 5 |
| SkillPack install lint failure | Install rejected. Extracted files removed. Error reported. | install |
| SkillPack incompatible Cairn version | Install rejected before extraction. | install |
| SkillPack dependency missing | Validation fails. Pack not built. | pack |

---

## Test Plan

### Unit tests (cairn-core)

- [ ] `SkillifyPipelineState` transition happy path (Extract → Author → Gate → Promote → HealthCheck)
- [ ] `SkillifyPipelineState` illegal transitions (e.g., Extract → Promote)
- [ ] `SkillifyPipelineState` fail/block from every non-terminal state
- [ ] `advance_to_promote()` fails when gates are missing or failed
- [ ] `SkillSpecDraft::validate()` rejects empty lanes, unsafe slugs, empty triggers
- [ ] `SkillPackManifest::validate()` rejects duplicate lanes
- [ ] `SkillPackManifest::validate()` rejects missing dependencies
- [ ] `SkillPackManifest::validate()` rejects incompatible Cairn version
- [ ] `SkillPackManifest::validate()` passes valid manifests
- [ ] Pack ID derivation is deterministic
- [ ] `SkillPackEntry` serialization round-trip

### Unit tests (cairn-workflows)

- [ ] Each gate runner passes with valid input
- [ ] Each gate runner fails with invalid input
- [ ] `SkillContractRunner` rejects missing frontmatter fields
- [ ] `DeterministicScriptRunner` rejects missing/empty scripts
- [ ] `UnitTestRunner` executes test cases and catches failures
- [ ] `IntegrationTestRunner` sets CAIRN_INTEGRATION env var
- [ ] `LlmEvalRunner` calls LLM and parses rubric responses
- [ ] `LlmEvalRunner` fails when no LLM configured
- [ ] `ResolverTriggerRunner` catches duplicate triggers
- [ ] `ResolverEvalRunner` computes precision/recall correctly
- [ ] `CheckResolvableAndDryRunner` detects duplicate lanes in merged snapshot
- [ ] `E2eSmokeRunner` executes full pipeline simulation
- [ ] `FilingRulesRunner` validates relative directory paths
- [ ] `GateRunnerRegistry::run_all()` blocks downstream on dependency failure
- [ ] `SkillifyPipeline::run()` with passing mock LLM completes all 5 stages
- [ ] `SkillifyPipeline::run()` with no LLM blocks at STAGE 1
- [ ] `SkillifyPipeline::run()` with failing gate stops at STAGE 3
- [ ] `SkillPackBuilder` produces valid archives
- [ ] `SkillPackBuilder` rejects candidates with failing gates

### Integration tests

- [ ] Pipeline end-to-end with mock LLM (JsonLlm) and tempdir vault
- [ ] SkillPack pack → install round-trip in tempdir
- [ ] SkillPack install rejects incompatible versions
- [ ] Health check detects regression in a promoted skill
- [ ] Handler backward compatibility — existing payloads still work

### Snapshot tests (insta)

- [ ] `SkillSpecDraft` serialization
- [ ] `SkillPackManifest` serialization
- [ ] `GateRunResult` collection serialization
- [ ] `SkillifyPipelineResult` serialization

---

## Files Changed

### New files
- `crates/cairn-core/src/pipeline/skillify/stage.rs`
- `crates/cairn-core/src/pipeline/skillify/spec.rs`
- `crates/cairn-core/src/pipeline/skillify/pack.rs`
- `crates/cairn-workflows/src/skillify/gate_runner.rs`
- `crates/cairn-workflows/src/skillify/gate_registry.rs`
- `crates/cairn-workflows/src/skillify/pipeline.rs`
- `crates/cairn-workflows/src/skillify/packer.rs`
- `crates/cairn-workflows/src/skillify/health.rs`
- `crates/cairn-workflows/tests/skillify_pipeline.rs`
- `crates/cairn-workflows/tests/skillify_gate_runners.rs`
- `crates/cairn-workflows/tests/skillify_packer.rs`
- `crates/cairn-core/tests/skillify_stage.rs`
- `crates/cairn-core/tests/skillify_pack.rs`

### Modified files
- `crates/cairn-core/src/pipeline/skillify/mod.rs` — re-export new modules
- `crates/cairn-workflows/src/skillify/mod.rs` — re-export new modules
- `crates/cairn-workflows/src/skillify/handler.rs` — delegate to pipeline
- `crates/cairn-cli/src/verbs/mod.rs` — add skillpack subcommand
- `crates/cairn-idl/` — if IDL schema changes needed for new types

---

## Invariants Touched

- **#3 CLI is ground truth** — `cairn skillpack` subcommands added to CLI
- **#4 Seven contracts** — no new contracts; `LLMProvider` and `MemoryStore` used as-is
- **#6 Fail closed on capability** — gate failures block promotion; SkillPack installs fail on lint errors
- **#8 No unwrap in core** — all new core code returns typed errors
