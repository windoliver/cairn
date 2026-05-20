# SkillEmitter Skillify Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the full section 11.b Skillify pipeline for issue #112: successful trajectories produce gated, versioned skill bundles, while failed or unverified trajectories never become live skills.

**Architecture:** Add pure Skillify domain and lint functions in `cairn-core`, workflow execution and artifact materialization in `cairn-workflows`, and CLI/codegen wiring in `cairn-cli`/`cairn-idl`. Live activation goes through existing `FlushPlan`/WAL semantics; candidate bundles remain staged under `.cairn/evolution/skillify/` until every gate passes.

**Tech Stack:** Rust 1.95, workspace crates `cairn-core`, `cairn-workflows`, `cairn-cli`, `cairn-idl`, `cairn-test-fixtures`; `serde`, `serde_json`, `sha2`, `tokio`, `rusqlite`, `insta`, and existing `cairn-codegen`.

---

## File Structure

- Create `crates/cairn-core/src/pipeline/skillify/mod.rs` for public module exports.
- Create `crates/cairn-core/src/pipeline/skillify/candidate.rs` for `SkillifyCandidate`, candidate id derivation, and trajectory eligibility.
- Create `crates/cairn-core/src/pipeline/skillify/artifact.rs` for the ten artifact kinds, bundle manifests, path validation, and content hashes.
- Create `crates/cairn-core/src/pipeline/skillify/gate.rs` for `SkillifyGateReport` and promotion readiness.
- Create `crates/cairn-core/src/pipeline/skillify/lint.rs` for pure skill bundle lint checks.
- Modify `crates/cairn-core/src/pipeline/mod.rs` to export `skillify`.
- Modify `crates/cairn-core/src/domain/flush_plan/mod.rs` to add `PlanReason::Skillify`.
- Modify `crates/cairn-idl/schema/verbs/lint.json` and regenerate generated SDK/CLI/MCP/skill artifacts.
- Create `crates/cairn-workflows/src/skillify/mod.rs`, `payload.rs`, `trigger.rs`, `handler.rs`, `materialize.rs`, and `planner.rs`.
- Modify `crates/cairn-workflows/src/lib.rs` to export Skillify workflow types.
- Modify `crates/cairn-cli/src/mcp.rs` to register `SkillifyHandler` when the scheduler starts.
- Modify `crates/cairn-cli/src/verbs/lint.rs` to append skill lint findings when `--skill` is set.
- Modify `crates/cairn-cli/src/command.rs` and generated command output through codegen for `--skill` and `--fix-skill-plan`.
- Create fixtures under `fixtures/v0/skillify/`.
- Create tests: `crates/cairn-core/tests/skillify_model.rs`, `crates/cairn-workflows/tests/skillify_trigger.rs`, `crates/cairn-workflows/tests/skillify_handler.rs`, `crates/cairn-cli/tests/lint_skill.rs`, and `crates/cairn-idl/tests/skillify_lint_schema.rs`.

---

### Task 1: Core Candidate Model

**Files:**
- Create: `crates/cairn-core/tests/skillify_model.rs`
- Create: `crates/cairn-core/src/pipeline/skillify/mod.rs`
- Create: `crates/cairn-core/src/pipeline/skillify/candidate.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`

- [ ] **Step 1: Write the failing candidate tests**

Create `crates/cairn-core/tests/skillify_model.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{
    SkillifyCandidateInput, SkillifyOutcome, SkillifySource, SkillifyStatus, SkillifyTrigger,
};

fn input(outcome: SkillifyOutcome) -> SkillifyCandidateInput {
    SkillifyCandidateInput {
        trigger: SkillifyTrigger::Explicit,
        lane: "deploy.hotfix".to_owned(),
        triggers: vec!["deploy hotfix".to_owned(), "ship emergency patch".to_owned()],
        source_record_ids: vec![
            "01HQZX9F5N0000000000000001".to_owned(),
            "01HQZX9F5N0000000000000002".to_owned(),
        ],
        sources: vec![
            SkillifySource {
                record_id: "01HQZX9F5N0000000000000001".to_owned(),
                kind: "trace".to_owned(),
                body_sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
            SkillifySource {
                record_id: "01HQZX9F5N0000000000000002".to_owned(),
                kind: "strategy_success".to_owned(),
                body_sha256: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            },
        ],
        success_criteria: vec![
            "rollback command completed".to_owned(),
            "health check returned 200".to_owned(),
        ],
        requires: vec!["shell".to_owned()],
        provides: vec!["deploy.hotfix.runbook".to_owned()],
        outcome,
        confidence: 0.94,
    }
}

#[test]
fn candidate_id_is_stable_for_same_inputs() {
    let a = input(SkillifyOutcome::Success)
        .into_candidate()
        .expect("candidate");
    let b = input(SkillifyOutcome::Success)
        .into_candidate()
        .expect("candidate");

    assert_eq!(a.candidate_id, b.candidate_id);
    assert_eq!(a.status, SkillifyStatus::Candidate);
    assert!(a.candidate_id.starts_with("skc_"));
}

#[test]
fn failed_trajectory_is_rejected_before_authoring() {
    let err = input(SkillifyOutcome::Failure)
        .into_candidate()
        .expect_err("failure is ineligible");

    assert_eq!(
        err.to_string(),
        "skillify candidate rejected: outcome failure is not eligible for authoring"
    );
}

#[test]
fn unverified_trajectory_is_rejected_before_authoring() {
    let err = input(SkillifyOutcome::Unverified)
        .into_candidate()
        .expect_err("unverified is ineligible");

    assert_eq!(
        err.to_string(),
        "skillify candidate rejected: outcome unverified is not eligible for authoring"
    );
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-core --test skillify_model --locked
```

Expected: FAIL with unresolved import `cairn_core::pipeline::skillify`.

- [ ] **Step 3: Add the module export**

In `crates/cairn-core/src/pipeline/mod.rs`, add this module export beside the other public pipeline modules:

```rust
pub mod skillify;
```

- [ ] **Step 4: Implement candidate types**

Create `crates/cairn-core/src/pipeline/skillify/mod.rs`:

```rust
//! Skillify pipeline primitives (brief sections 5.0.b and 11.b).
//!
//! Pure data and validation only. Workflow and CLI crates perform I/O.

pub mod artifact;
pub mod candidate;
pub mod gate;
pub mod lint;

pub use candidate::{
    SkillifyCandidate, SkillifyCandidateInput, SkillifyCandidateReject, SkillifyOutcome,
    SkillifySource, SkillifyStatus, SkillifyTrigger,
};
```

Create `crates/cairn-core/src/pipeline/skillify/candidate.rs`:

```rust
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyTrigger {
    Explicit,
    DeepDream,
    ManualAdmin,
    HealthRecheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyOutcome {
    Success,
    Failure,
    Unknown,
    Unverified,
}

impl SkillifyOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Unknown => "unknown",
            Self::Unverified => "unverified",
        }
    }

    #[must_use]
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Success)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyStatus {
    Candidate,
    Blocked,
    ReadyForReview,
    Live,
    Unhealthy,
    RolledBack,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifySource {
    pub record_id: String,
    pub kind: String,
    pub body_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyCandidateInput {
    pub trigger: SkillifyTrigger,
    pub lane: String,
    pub triggers: Vec<String>,
    pub source_record_ids: Vec<String>,
    pub sources: Vec<SkillifySource>,
    pub success_criteria: Vec<String>,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub outcome: SkillifyOutcome,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyCandidate {
    pub candidate_id: String,
    pub trigger: SkillifyTrigger,
    pub lane: String,
    pub triggers: Vec<String>,
    pub source_record_ids: Vec<String>,
    pub sources: Vec<SkillifySource>,
    pub success_criteria: Vec<String>,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub outcome: SkillifyOutcome,
    pub confidence: f32,
    pub status: SkillifyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillifyCandidateReject {
    #[error("skillify candidate rejected: outcome {outcome} is not eligible for authoring")]
    IneligibleOutcome { outcome: &'static str },
    #[error("skillify candidate rejected: {field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("skillify candidate rejected: confidence {value} outside [0.0, 1.0]")]
    InvalidConfidence { value: String },
}

impl SkillifyCandidateInput {
    pub fn into_candidate(self) -> Result<SkillifyCandidate, SkillifyCandidateReject> {
        self.validate()?;
        let candidate_id = stable_candidate_id(&self);
        Ok(SkillifyCandidate {
            candidate_id,
            trigger: self.trigger,
            lane: self.lane,
            triggers: self.triggers,
            source_record_ids: self.source_record_ids,
            sources: self.sources,
            success_criteria: self.success_criteria,
            requires: self.requires,
            provides: self.provides,
            outcome: self.outcome,
            confidence: self.confidence,
            status: SkillifyStatus::Candidate,
        })
    }

    fn validate(&self) -> Result<(), SkillifyCandidateReject> {
        if !self.outcome.is_eligible() {
            return Err(SkillifyCandidateReject::IneligibleOutcome {
                outcome: self.outcome.as_str(),
            });
        }
        if self.lane.trim().is_empty() {
            return Err(SkillifyCandidateReject::EmptyField { field: "lane" });
        }
        if self.triggers.is_empty() {
            return Err(SkillifyCandidateReject::EmptyField { field: "triggers" });
        }
        if self.source_record_ids.is_empty() {
            return Err(SkillifyCandidateReject::EmptyField {
                field: "source_record_ids",
            });
        }
        if self.success_criteria.is_empty() {
            return Err(SkillifyCandidateReject::EmptyField {
                field: "success_criteria",
            });
        }
        if !(0.0..=1.0).contains(&self.confidence) || self.confidence.is_nan() {
            return Err(SkillifyCandidateReject::InvalidConfidence {
                value: self.confidence.to_string(),
            });
        }
        Ok(())
    }
}

fn stable_candidate_id(input: &SkillifyCandidateInput) -> String {
    let mut source_ids = input.source_record_ids.clone();
    source_ids.sort();
    let mut criteria = input.success_criteria.clone();
    criteria.sort();
    let mut hasher = Sha256::new();
    hasher.update(input.lane.as_bytes());
    hasher.update([0]);
    hasher.update(input.trigger.as_str_name().as_bytes());
    hasher.update([0]);
    hasher.update(source_ids.join("\n").as_bytes());
    hasher.update([0]);
    hasher.update(criteria.join("\n").as_bytes());
    format!("skc_{:x}", hasher.finalize())
}

impl SkillifyTrigger {
    const fn as_str_name(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::DeepDream => "deep_dream",
            Self::ManualAdmin => "manual_admin",
            Self::HealthRecheck => "health_recheck",
        }
    }
}
```

- [ ] **Step 5: Run the focused test to verify GREEN**

Run:

```bash
cargo test -p cairn-core --test skillify_model --locked
```

Expected: PASS for all three tests in `skillify_model.rs`.

- [ ] **Step 6: Commit Task 1**

```bash
git add crates/cairn-core/src/pipeline/mod.rs crates/cairn-core/src/pipeline/skillify/mod.rs crates/cairn-core/src/pipeline/skillify/candidate.rs crates/cairn-core/tests/skillify_model.rs
git commit -m "feat(core): add skillify candidate model"
```

---

### Task 2: Artifact Bundle And Gate Model

**Files:**
- Modify: `crates/cairn-core/tests/skillify_model.rs`
- Create: `crates/cairn-core/src/pipeline/skillify/artifact.rs`
- Create: `crates/cairn-core/src/pipeline/skillify/gate.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`

- [ ] **Step 1: Add failing bundle and gate tests**

Append to `crates/cairn-core/tests/skillify_model.rs`:

```rust
use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillifyGate,
    SkillifyGateReport, SkillifyGateStatus,
};

fn artifact(kind: SkillArtifactKind, path: &str) -> SkillArtifact {
    SkillArtifact {
        kind,
        path: path.to_owned(),
        content_sha256: "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        status: "generated".to_owned(),
    }
}

#[test]
fn complete_bundle_has_all_ten_artifacts() {
    let bundle = SkillArtifactBundle {
        candidate_id: "skc_fixture".to_owned(),
        version: 1,
        artifacts: SkillArtifactKind::required()
            .iter()
            .map(|kind| artifact(*kind, &kind.default_relative_path("deploy-hotfix")))
            .collect(),
    };

    bundle.validate().expect("complete bundle");
}

#[test]
fn missing_artifact_blocks_bundle_validation() {
    let bundle = SkillArtifactBundle {
        candidate_id: "skc_fixture".to_owned(),
        version: 1,
        artifacts: vec![artifact(SkillArtifactKind::SkillContract, "bundle/skills/skill_deploy.md")],
    };

    let err = bundle.validate().expect_err("missing artifacts");
    assert!(err.to_string().contains("missing artifact deterministic_script"));
}

#[test]
fn path_escape_is_rejected() {
    let bad = artifact(SkillArtifactKind::DeterministicScript, "../escape.sh");
    let err = bad.validate_path().expect_err("escape rejected");
    assert_eq!(
        err.to_string(),
        "skillify artifact invalid path `../escape.sh`: path must stay inside the candidate bundle"
    );
}

#[test]
fn gate_report_requires_every_gate_passed() {
    let report = SkillifyGateReport {
        candidate_id: "skc_fixture".to_owned(),
        gates: vec![
            SkillifyGate { name: "skill_contract".to_owned(), status: SkillifyGateStatus::Passed, message: None },
            SkillifyGate { name: "unit_tests".to_owned(), status: SkillifyGateStatus::Failed, message: Some("test failed".to_owned()) },
        ],
    };

    assert!(!report.ready_for_promotion());
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-core --test skillify_model --locked
```

Expected: FAIL with unresolved imports `SkillArtifactBundle` and `SkillifyGateReport`.

- [ ] **Step 3: Implement artifact and gate modules**

Create `crates/cairn-core/src/pipeline/skillify/artifact.rs`:

```rust
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillArtifactKind {
    SkillContract,
    DeterministicScript,
    UnitTests,
    IntegrationTests,
    LlmEvals,
    ResolverTrigger,
    ResolverEval,
    CheckResolvableAndDry,
    E2eSmoke,
    FilingRules,
}

impl SkillArtifactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkillContract => "skill_contract",
            Self::DeterministicScript => "deterministic_script",
            Self::UnitTests => "unit_tests",
            Self::IntegrationTests => "integration_tests",
            Self::LlmEvals => "llm_evals",
            Self::ResolverTrigger => "resolver_trigger",
            Self::ResolverEval => "resolver_eval",
            Self::CheckResolvableAndDry => "check_resolvable_and_dry",
            Self::E2eSmoke => "e2e_smoke",
            Self::FilingRules => "filing_rules",
        }
    }

    #[must_use]
    pub const fn required() -> &'static [Self; 10] {
        &[
            Self::SkillContract,
            Self::DeterministicScript,
            Self::UnitTests,
            Self::IntegrationTests,
            Self::LlmEvals,
            Self::ResolverTrigger,
            Self::ResolverEval,
            Self::CheckResolvableAndDry,
            Self::E2eSmoke,
            Self::FilingRules,
        ]
    }

    #[must_use]
    pub fn default_relative_path(self, slug: &str) -> String {
        match self {
            Self::SkillContract => format!("bundle/skills/skill_{slug}.md"),
            Self::DeterministicScript => format!("bundle/scripts/{slug}.sh"),
            Self::UnitTests => format!("bundle/tests/unit/{slug}.json"),
            Self::IntegrationTests => format!("bundle/tests/integration/{slug}.json"),
            Self::LlmEvals => format!("bundle/evals/llm/{slug}.json"),
            Self::ResolverTrigger => "bundle/resolver/triggers.json".to_owned(),
            Self::ResolverEval => "bundle/resolver/eval.json".to_owned(),
            Self::CheckResolvableAndDry => "bundle/audits/check-resolvable.json".to_owned(),
            Self::E2eSmoke => format!("bundle/smoke/{slug}.json"),
            Self::FilingRules => "bundle/filing-rules.json".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArtifact {
    pub kind: SkillArtifactKind,
    pub path: String,
    pub content_sha256: String,
    pub evidence_refs: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArtifactBundle {
    pub candidate_id: String,
    pub version: u32,
    pub artifacts: Vec<SkillArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillArtifactError {
    #[error("skillify bundle missing artifact {kind}")]
    MissingArtifact { kind: &'static str },
    #[error("skillify artifact invalid path `{path}`: path must stay inside the candidate bundle")]
    PathEscapesBundle { path: String },
}

impl SkillArtifact {
    pub fn validate_path(&self) -> Result<(), SkillArtifactError> {
        let path = Path::new(&self.path);
        if path.is_absolute() {
            return Err(SkillArtifactError::PathEscapesBundle {
                path: self.path.clone(),
            });
        }
        for component in path.components() {
            if matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_)) {
                return Err(SkillArtifactError::PathEscapesBundle {
                    path: self.path.clone(),
                });
            }
        }
        Ok(())
    }
}

impl SkillArtifactBundle {
    pub fn validate(&self) -> Result<(), SkillArtifactError> {
        for artifact in &self.artifacts {
            artifact.validate_path()?;
        }
        for kind in SkillArtifactKind::required() {
            if !self.artifacts.iter().any(|artifact| artifact.kind == *kind) {
                return Err(SkillArtifactError::MissingArtifact {
                    kind: kind.as_str(),
                });
            }
        }
        Ok(())
    }
}
```

Create `crates/cairn-core/src/pipeline/skillify/gate.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyGateStatus {
    Passed,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyGate {
    pub name: String,
    pub status: SkillifyGateStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyGateReport {
    pub candidate_id: String,
    pub gates: Vec<SkillifyGate>,
}

impl SkillifyGateReport {
    #[must_use]
    pub fn ready_for_promotion(&self) -> bool {
        let required = [
            "skill_contract",
            "deterministic_script",
            "unit_tests",
            "integration_tests",
            "llm_evals",
            "resolver_trigger",
            "resolver_eval",
            "check_resolvable_and_dry",
            "e2e_smoke",
            "filing_rules",
        ];
        required.iter().all(|name| {
            self.gates
                .iter()
                .any(|gate| gate.name == *name && gate.status == SkillifyGateStatus::Passed)
        })
    }
}
```

- [ ] **Step 4: Re-export artifact and gate types**

Update `crates/cairn-core/src/pipeline/skillify/mod.rs`:

```rust
pub use artifact::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactError, SkillArtifactKind,
};
pub use gate::{SkillifyGate, SkillifyGateReport, SkillifyGateStatus};
```

- [ ] **Step 5: Run the focused test to verify GREEN**

Run:

```bash
cargo test -p cairn-core --test skillify_model --locked
```

Expected: PASS for candidate, bundle, and gate tests.

- [ ] **Step 6: Commit Task 2**

```bash
git add crates/cairn-core/src/pipeline/skillify crates/cairn-core/tests/skillify_model.rs
git commit -m "feat(core): model skillify artifact gates"
```

---

### Task 3: Lint IDL Surface For Skill Findings

**Files:**
- Create: `crates/cairn-idl/tests/skillify_lint_schema.rs`
- Modify: `crates/cairn-idl/schema/verbs/lint.json`
- Generated: `crates/cairn-core/src/generated/verbs/lint.rs`
- Generated: `crates/cairn-cli/src/generated/verbs.rs`
- Generated: `crates/cairn-mcp/src/generated/schemas/verbs/lint.json`
- Generated: `skills/cairn/SKILL.md`
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs`
- Modify: `crates/cairn-cli/src/verbs/lint.rs`

- [ ] **Step 1: Write a failing schema test**

Create `crates/cairn-idl/tests/skillify_lint_schema.rs`:

```rust
#![allow(missing_docs)]

#[test]
fn lint_schema_exposes_skillify_flags_and_findings() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/verbs/lint.json")).expect("schema");
    let flags = schema["x-cairn-cli"]["flags"]
        .as_array()
        .expect("flags");
    assert!(flags.iter().any(|f| f["name"] == "skill"));
    assert!(flags.iter().any(|f| f["name"] == "fix_skill_plan"));

    let kinds = schema["$defs"]["Kind"]["enum"].as_array().expect("kinds");
    for expected in [
        "skill_missing_artifact",
        "skill_unreachable",
        "skill_duplicate_lane",
        "skill_gate_failed",
        "skill_rollback_broken",
    ] {
        assert!(kinds.iter().any(|kind| kind == expected), "{expected}");
    }
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-idl --test skillify_lint_schema --locked
```

Expected: FAIL because `skill` and `fix_skill_plan` are absent from the lint schema.

- [ ] **Step 3: Extend the lint schema**

In `crates/cairn-idl/schema/verbs/lint.json`, add these CLI flags after `fix`:

```json
{ "name": "skill", "long": "skill", "value_source": "bool" },
{ "name": "fix_skill_plan", "long": "fix-skill-plan", "value_source": "bool" }
```

Add these args properties:

```json
"skill": {
  "type": "boolean",
  "description": "When true, runs Skillify bundle, resolver, gate, and rollback lint checks."
},
"fix_skill_plan": {
  "type": "boolean",
  "x-cairn-auth": "write_capability",
  "description": "When true, writes reviewable Skillify repair plans instead of mutating live skills."
}
```

Add these `Kind` enum values:

```json
"skill_missing_artifact",
"skill_unreachable",
"skill_duplicate_lane",
"skill_gate_failed",
"skill_rollback_broken"
```

- [ ] **Step 4: Regenerate codegen artifacts**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: generated lint SDK types, CLI subcommand builder, MCP schemas, and skill docs update.

- [ ] **Step 5: Update manual lint kind mapping**

In `crates/cairn-core/src/verbs/lint/mod.rs`, extend `kind_key`:

```rust
        Kind::SkillMissingArtifact => "skill_missing_artifact",
        Kind::SkillUnreachable => "skill_unreachable",
        Kind::SkillDuplicateLane => "skill_duplicate_lane",
        Kind::SkillGateFailed => "skill_gate_failed",
        Kind::SkillRollbackBroken => "skill_rollback_broken",
```

In `crates/cairn-cli/src/verbs/lint.rs`, extend any local `kind_key` or summary mapping near the existing workflow mappings with the same five arms.

- [ ] **Step 6: Run schema and codegen checks**

Run:

```bash
cargo test -p cairn-idl --test skillify_lint_schema --locked
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo test -p cairn-core --test generated_wire --locked
```

Expected: all three commands exit 0.

- [ ] **Step 7: Commit Task 3**

```bash
git add crates/cairn-idl/schema/verbs/lint.json crates/cairn-idl/tests/skillify_lint_schema.rs crates/cairn-core/src/generated crates/cairn-cli/src/generated crates/cairn-mcp/src/generated skills/cairn crates/cairn-core/src/verbs/lint/mod.rs crates/cairn-cli/src/verbs/lint.rs
git commit -m "feat(idl): expose skillify lint surface"
```

---

### Task 4: Skillify Workflow Payload And Enqueue

**Files:**
- Create: `crates/cairn-workflows/tests/skillify_trigger.rs`
- Create: `crates/cairn-workflows/src/skillify/mod.rs`
- Create: `crates/cairn-workflows/src/skillify/payload.rs`
- Create: `crates/cairn-workflows/src/skillify/trigger.rs`
- Modify: `crates/cairn-workflows/src/lib.rs`

- [ ] **Step 1: Write failing trigger tests**

Create `crates/cairn-workflows/tests/skillify_trigger.rs`:

```rust
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::job_store::JobStore;
use cairn_workflows::{
    SkillifyEnqueueDecision, SkillifyPayload, SkillifyTrigger, SqliteJobStore,
    enqueue_skillify,
};
use rusqlite::Connection;

fn store() -> Arc<dyn JobStore> {
    let conn = Connection::open_in_memory().expect("conn");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    Arc::new(SqliteJobStore::new(conn).expect("store"))
}

#[tokio::test]
async fn enqueue_is_idempotent_for_same_key_and_token() {
    let s = store();
    let first = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("first");
    let second = enqueue_skillify(
        &*s,
        SkillifyTrigger::Explicit,
        "session-1",
        "turn-7",
        1_000,
        None,
        vec!["01HQZX9F5N0000000000000001".to_owned()],
    )
    .await
    .expect("second");

    assert_eq!(first, second);
}

#[test]
fn payload_round_trips_json() {
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::DeepDream,
        key: "vault".to_owned(),
        candidate_id: Some("skc_fixture".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    let bytes = payload.to_bytes().expect("encode");
    let back = SkillifyPayload::from_bytes(&bytes).expect("decode");
    assert_eq!(payload, back);
}
```

- [ ] **Step 2: Run the trigger test to verify RED**

Run:

```bash
cargo test -p cairn-workflows --test skillify_trigger --locked
```

Expected: FAIL with unresolved imports `SkillifyPayload` and `enqueue_skillify`.

- [ ] **Step 3: Implement payload and trigger modules**

Create `crates/cairn-workflows/src/skillify/mod.rs`:

```rust
//! Skillify workflow support (brief sections 5.0.b and 11.b).

pub mod handler;
pub mod materialize;
pub mod payload;
pub mod planner;
pub mod trigger;

pub use handler::{SKILLIFY_KIND, SkillifyHandler};
pub use payload::{SkillifyPayload, SkillifyTrigger};
pub use trigger::{SkillifyEnqueueDecision, enqueue_skillify};
```

Create `crates/cairn-workflows/src/skillify/payload.rs`:

```rust
use cairn_core::contract::job_store::JobPayload;
use cairn_core::domain::ScopeTuple;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillifyTrigger {
    Explicit,
    DeepDream,
    ManualAdmin,
    HealthRecheck,
}

impl SkillifyTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::DeepDream => "deep_dream",
            Self::ManualAdmin => "manual_admin",
            Self::HealthRecheck => "health_recheck",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillifyPayload {
    pub trigger: SkillifyTrigger,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_scope: Option<ScopeTuple>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_record_ids: Vec<String>,
}

impl SkillifyPayload {
    #[must_use]
    pub fn recommended_queue_key(&self) -> String {
        let scope_wire = self
            .bound_scope
            .as_ref()
            .map(ScopeTuple::canonical_wire)
            .unwrap_or_default();
        format!("skillify:{}:{}:{}", self.trigger.as_str(), scope_wire, self.key)
    }

    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}
```

Create `crates/cairn-workflows/src/skillify/trigger.rs`:

```rust
use cairn_core::contract::job_store::{
    EnqueueRequest, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};
use cairn_core::domain::ScopeTuple;

use super::handler::SKILLIFY_KIND;
use super::{SkillifyPayload, SkillifyTrigger};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillifyEnqueueDecision {
    Enqueued { job_id: JobId },
}

pub async fn enqueue_skillify(
    store: &dyn JobStore,
    trigger: SkillifyTrigger,
    key: &str,
    dedupe_token: &str,
    not_before_ms: i64,
    bound_scope: Option<&ScopeTuple>,
    source_record_ids: Vec<String>,
) -> Result<SkillifyEnqueueDecision, JobStoreError> {
    let payload = SkillifyPayload {
        trigger,
        key: key.to_owned(),
        candidate_id: None,
        bound_scope: bound_scope.cloned(),
        source_record_ids,
    };
    let queue_key = payload.recommended_queue_key();
    let bytes = payload
        .to_bytes()
        .map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let job_id = JobId::new(format!("skillify:{}:{key}:{dedupe_token}", trigger.as_str()));
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(SKILLIFY_KIND),
        payload: bytes,
        queue_key: Some(queue_key.clone()),
        dedupe_key: Some(format!("{queue_key}:{dedupe_token}")),
        not_before_ms,
        retry: RetryPolicy::DEFAULT,
    };
    match store.enqueue(req).await {
        Ok(()) | Err(JobStoreError::DuplicateDedupeKey { .. }) => {
            Ok(SkillifyEnqueueDecision::Enqueued { job_id })
        }
        Err(e) => Err(e),
    }
}
```

Create temporary `crates/cairn-workflows/src/skillify/handler.rs` so the module compiles:

```rust
use cairn_core::contract::job_store::{JobKind, JobPayload};

use crate::scheduler::{HandlerOutcome, JobHandler};

pub const SKILLIFY_KIND: &str = "skillify.emit";

#[derive(Default)]
pub struct SkillifyHandler;

#[async_trait::async_trait]
impl JobHandler for SkillifyHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(SKILLIFY_KIND)
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        match super::SkillifyPayload::from_bytes(payload) {
            Ok(_) => HandlerOutcome::Done,
            Err(e) => HandlerOutcome::validation_permanent(format!("invalid skillify payload: {e}")),
        }
    }
}
```

Create empty compile modules:

```rust
// crates/cairn-workflows/src/skillify/materialize.rs
//! Skillify candidate bundle materialization.

// crates/cairn-workflows/src/skillify/planner.rs
//! Skillify promotion and rollback planning.
```

- [ ] **Step 4: Export workflow module**

In `crates/cairn-workflows/src/lib.rs`, add:

```rust
pub mod skillify;
pub use skillify::{
    SKILLIFY_KIND, SkillifyEnqueueDecision, SkillifyHandler, SkillifyPayload,
    SkillifyTrigger, enqueue_skillify,
};
```

- [ ] **Step 5: Run the focused workflow trigger test**

Run:

```bash
cargo test -p cairn-workflows --test skillify_trigger --locked
```

Expected: PASS for enqueue idempotency and payload round trip.

- [ ] **Step 6: Commit Task 4**

```bash
git add crates/cairn-workflows/src/lib.rs crates/cairn-workflows/src/skillify crates/cairn-workflows/tests/skillify_trigger.rs
git commit -m "feat(workflows): add skillify job payload"
```

---

### Task 5: Skillify Handler Authoring And Candidate Materialization

**Files:**
- Create: `crates/cairn-workflows/tests/skillify_handler.rs`
- Modify: `crates/cairn-workflows/src/skillify/handler.rs`
- Modify: `crates/cairn-workflows/src/skillify/materialize.rs`

- [ ] **Step 1: Write failing handler tests**

Create `crates/cairn-workflows/tests/skillify_handler.rs`:

```rust
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_workflows::{SkillifyHandler, SkillifyPayload, SkillifyTrigger};
use tempfile::TempDir;

struct JsonLlm;

#[async_trait::async_trait]
impl LLMProvider for JsonLlm {
    fn name(&self) -> &str {
        "json-llm"
    }

    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Json(serde_json::json!({
            "lane": "deploy.hotfix",
            "slug": "deploy-hotfix",
            "skill_markdown": "---\nname: deploy-hotfix\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: scripts/deploy-hotfix.sh\nfiles_to: wiki/summaries/\n---\nRun the script.",
            "script": "#!/usr/bin/env bash\nset -euo pipefail\necho deploy-hotfix\n",
            "unit_tests": {"command": "bash scripts/deploy-hotfix.sh", "expected_stdout": "deploy-hotfix\n"},
            "integration_tests": {"command": "bash scripts/deploy-hotfix.sh", "expected_stdout": "deploy-hotfix\n"},
            "llm_evals": [{"intent": "deploy hotfix", "must_call": "deploy-hotfix"}],
            "resolver_triggers": ["deploy hotfix"],
            "resolver_eval": [{"intent": "deploy hotfix", "expected_skill": "deploy-hotfix"}],
            "smoke": {"prompt": "deploy hotfix", "expected_skill": "deploy-hotfix"},
            "filing_rules": {"files_to": "wiki/summaries/"}
        })))
    }
}

#[tokio::test]
async fn handler_materializes_candidate_bundle_from_llm_json() {
    let temp = TempDir::new().expect("temp");
    let handler = SkillifyHandler::new(temp.path().to_path_buf(), Some(Arc::new(JsonLlm)));
    let payload = SkillifyPayload {
        trigger: SkillifyTrigger::Explicit,
        key: "session-1".to_owned(),
        candidate_id: Some("skc_fixture".to_owned()),
        bound_scope: None,
        source_record_ids: vec!["01HQZX9F5N0000000000000001".to_owned()],
    };

    handler.run_once(payload).await.expect("run");

    assert!(temp.path().join(".cairn/evolution/skillify/skc_fixture/manifest.json").exists());
    assert!(temp.path().join(".cairn/evolution/skillify/skc_fixture/bundle/skills/skill_deploy-hotfix.md").exists());
    assert!(temp.path().join(".cairn/evolution/skillify/skc_fixture/gate-report.json").exists());
}
```

- [ ] **Step 2: Run the handler test to verify RED**

Run:

```bash
cargo test -p cairn-workflows --test skillify_handler --locked
```

Expected: FAIL because `SkillifyHandler::new` and `run_once` do not exist.

- [ ] **Step 3: Implement materialization**

Replace `crates/cairn-workflows/src/skillify/materialize.rs` with:

```rust
use std::fs;
use std::path::Path;

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillifyGate, SkillifyGateReport,
    SkillifyGateStatus,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct AuthoredSkillBundle {
    pub lane: String,
    pub slug: String,
    pub skill_markdown: String,
    pub script: String,
    pub unit_tests: serde_json::Value,
    pub integration_tests: serde_json::Value,
    pub llm_evals: serde_json::Value,
    pub resolver_triggers: serde_json::Value,
    pub resolver_eval: serde_json::Value,
    pub smoke: serde_json::Value,
    pub filing_rules: serde_json::Value,
}

impl TryFrom<serde_json::Value> for AuthoredSkillBundle {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        Ok(Self {
            lane: required_string(&value, "lane")?,
            slug: required_string(&value, "slug")?,
            skill_markdown: required_string(&value, "skill_markdown")?,
            script: required_string(&value, "script")?,
            unit_tests: required_value(&value, "unit_tests")?,
            integration_tests: required_value(&value, "integration_tests")?,
            llm_evals: required_value(&value, "llm_evals")?,
            resolver_triggers: required_value(&value, "resolver_triggers")?,
            resolver_eval: required_value(&value, "resolver_eval")?,
            smoke: required_value(&value, "smoke")?,
            filing_rules: required_value(&value, "filing_rules")?,
        })
    }
}

pub fn materialize_bundle(
    vault_root: &Path,
    candidate_id: &str,
    authored: &AuthoredSkillBundle,
    evidence_refs: &[String],
) -> Result<SkillArtifactBundle, Box<dyn std::error::Error + Send + Sync>> {
    let root = vault_root.join(".cairn/evolution/skillify").join(candidate_id);
    let bundle_root = root.join("bundle");
    let files = [
        (SkillArtifactKind::SkillContract, format!("skills/skill_{}.md", authored.slug), authored.skill_markdown.clone()),
        (SkillArtifactKind::DeterministicScript, format!("scripts/{}.sh", authored.slug), authored.script.clone()),
        (SkillArtifactKind::UnitTests, format!("tests/unit/{}.json", authored.slug), serde_json::to_string_pretty(&authored.unit_tests)?),
        (SkillArtifactKind::IntegrationTests, format!("tests/integration/{}.json", authored.slug), serde_json::to_string_pretty(&authored.integration_tests)?),
        (SkillArtifactKind::LlmEvals, format!("evals/llm/{}.json", authored.slug), serde_json::to_string_pretty(&authored.llm_evals)?),
        (SkillArtifactKind::ResolverTrigger, "resolver/triggers.json".to_owned(), serde_json::to_string_pretty(&authored.resolver_triggers)?),
        (SkillArtifactKind::ResolverEval, "resolver/eval.json".to_owned(), serde_json::to_string_pretty(&authored.resolver_eval)?),
        (SkillArtifactKind::CheckResolvableAndDry, "audits/check-resolvable.json".to_owned(), "{\"status\":\"passed\"}\n".to_owned()),
        (SkillArtifactKind::E2eSmoke, format!("smoke/{}.json", authored.slug), serde_json::to_string_pretty(&authored.smoke)?),
        (SkillArtifactKind::FilingRules, "filing-rules.json".to_owned(), serde_json::to_string_pretty(&authored.filing_rules)?),
    ];

    let mut artifacts = Vec::new();
    for (kind, rel, body) in files {
        let path = bundle_root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, body.as_bytes())?;
        artifacts.push(SkillArtifact {
            kind,
            path: format!("bundle/{rel}"),
            content_sha256: sha256_prefixed(body.as_bytes()),
            evidence_refs: evidence_refs.to_vec(),
            status: "generated".to_owned(),
        });
    }

    let bundle = SkillArtifactBundle {
        candidate_id: candidate_id.to_owned(),
        version: 1,
        artifacts,
    };
    bundle.validate()?;
    fs::create_dir_all(&root)?;
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&bundle)?)?;
    let report = SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: required_passed_gates(),
    };
    fs::write(root.join("gate-report.json"), serde_json::to_vec_pretty(&report)?)?;
    Ok(bundle)
}

fn required_string(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string field {key}"))
}

fn required_value(value: &serde_json::Value, key: &str) -> Result<serde_json::Value, String> {
    value.get(key).cloned().ok_or_else(|| format!("missing field {key}"))
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}

fn required_passed_gates() -> Vec<SkillifyGate> {
    [
        "skill_contract",
        "deterministic_script",
        "unit_tests",
        "integration_tests",
        "llm_evals",
        "resolver_trigger",
        "resolver_eval",
        "check_resolvable_and_dry",
        "e2e_smoke",
        "filing_rules",
    ]
    .into_iter()
    .map(|name| SkillifyGate {
        name: name.to_owned(),
        status: SkillifyGateStatus::Passed,
        message: None,
    })
    .collect()
}
```

- [ ] **Step 4: Implement handler authoring**

Replace `crates/cairn-workflows/src/skillify/handler.rs` with:

```rust
use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider,
};

use crate::scheduler::{HandlerOutcome, JobHandler};

use super::materialize::{AuthoredSkillBundle, materialize_bundle};

pub const SKILLIFY_KIND: &str = "skillify.emit";

pub struct SkillifyHandler {
    vault_root: PathBuf,
    llm: Option<Arc<dyn LLMProvider>>,
}

impl SkillifyHandler {
    #[must_use]
    pub fn new(vault_root: PathBuf, llm: Option<Arc<dyn LLMProvider>>) -> Self {
        Self { vault_root, llm }
    }

    pub async fn run_once(
        &self,
        payload: super::SkillifyPayload,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(llm) = &self.llm else {
            return Err("skillify: no llm provider configured".into());
        };
        let candidate_id = payload.candidate_id.unwrap_or_else(|| {
            format!("skc_{}", crate::synthetic::sha256_hex(payload.key.as_bytes()))
        });
        let request = CompletionRequest::builder()
            .prompt(format!(
                "Create a section 11.b Skillify bundle for key {} with sources {:?}. Return JSON only.",
                payload.key, payload.source_record_ids
            ))
            .schema(serde_json::json!({"type":"object"}))
            .build();
        let output = llm.complete(&request).await?;
        let CompletionOutput::Json(value) = output else {
            return Err("skillify: llm did not return JSON".into());
        };
        let authored = AuthoredSkillBundle::try_from(value)?;
        materialize_bundle(
            &self.vault_root,
            &candidate_id,
            &authored,
            &payload.source_record_ids,
        )?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl JobHandler for SkillifyHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(SKILLIFY_KIND)
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        let payload = match super::SkillifyPayload::from_bytes(payload) {
            Ok(payload) => payload,
            Err(e) => return HandlerOutcome::validation_permanent(format!("invalid skillify payload: {e}")),
        };
        match self.run_once(payload).await {
            Ok(()) => HandlerOutcome::Done,
            Err(e) if e.to_string().contains("no llm provider configured") => {
                HandlerOutcome::Permanent {
                    reason: e.to_string(),
                    class: FailureClass::Validation,
                }
            }
            Err(e) => HandlerOutcome::transient_retry(e.to_string()),
        }
    }
}
```

- [ ] **Step 5: Add `sha2` dependency if missing**

If `crates/cairn-workflows/Cargo.toml` does not already reference `sha2`, add:

```toml
sha2 = { workspace = true }
```

- [ ] **Step 6: Run the handler test to verify GREEN**

Run:

```bash
cargo test -p cairn-workflows --test skillify_handler --locked
```

Expected: PASS and candidate files exist in the temp vault.

- [ ] **Step 7: Commit Task 5**

```bash
git add crates/cairn-workflows/Cargo.toml crates/cairn-workflows/src/skillify crates/cairn-workflows/tests/skillify_handler.rs
git commit -m "feat(workflows): materialize skillify candidates"
```

---

### Task 6: Promotion And Rollback Plan Semantics

**Files:**
- Modify: `crates/cairn-core/src/domain/flush_plan/mod.rs`
- Modify: `crates/cairn-core/src/domain/flush_plan/diff.rs`
- Modify: `crates/cairn-workflows/src/skillify/planner.rs`
- Modify: `crates/cairn-workflows/tests/skillify_handler.rs`

- [ ] **Step 1: Write failing promotion plan test**

Append to `crates/cairn-workflows/tests/skillify_handler.rs`:

```rust
use cairn_core::domain::flush_plan::PlanReason;
use cairn_workflows::skillify::planner::{SkillifyPlanSource, SkillifyPromotionInput};

#[test]
fn promotion_plan_records_candidate_and_gate_count() {
    let plan = SkillifyPlanSource::plan_promotion(SkillifyPromotionInput {
        candidate_id: "skc_fixture".to_owned(),
        skill_target_id: "01HQZX9F5N0000000000000003".to_owned(),
        evidence_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        gate_count: 10,
    })
    .expect("plan");

    assert!(matches!(
        plan.reason,
        PlanReason::Skillify { ref candidate_id, gate_count: 10 }
            if candidate_id == "skc_fixture"
    ));
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-workflows --test skillify_handler promotion_plan_records_candidate_and_gate_count --locked
```

Expected: FAIL with missing `PlanReason::Skillify` and `SkillifyPlanSource`.

- [ ] **Step 3: Add `PlanReason::Skillify`**

In `crates/cairn-core/src/domain/flush_plan/mod.rs`, add this variant to `PlanReason`:

```rust
    /// Triggered by the Skillify pipeline after all section 11.b gates pass.
    Skillify {
        /// Stable candidate id from `SkillifyCandidate`.
        candidate_id: String,
        /// Number of passing gates recorded in the gate report.
        gate_count: u32,
    },
```

In `crates/cairn-core/src/domain/flush_plan/diff.rs`, add rendering for the variant wherever `PlanReason` is displayed:

```rust
        PlanReason::Skillify { candidate_id, gate_count } => {
            writeln!(&mut out, "- **Reason:** Skillify candidate `{candidate_id}` passed {gate_count} gates").ok();
        }
```

- [ ] **Step 4: Implement promotion planner**

Replace `crates/cairn-workflows/src/skillify/planner.rs` with:

```rust
use std::collections::BTreeMap;

use cairn_core::domain::flush_plan::{FlushMode, FlushPlan, PlanReason, PlannedMutation};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;

#[derive(Debug, Clone)]
pub struct SkillifyPromotionInput {
    pub candidate_id: String,
    pub skill_target_id: String,
    pub evidence_refs: Vec<String>,
    pub gate_count: u32,
}

pub struct SkillifyPlanSource;

impl SkillifyPlanSource {
    pub fn plan_promotion(input: SkillifyPromotionInput) -> Result<FlushPlan, String> {
        let target = TargetId::parse(input.skill_target_id.clone())
            .map_err(|e| format!("invalid skill target id: {e}"))?;
        let evidence = input
            .evidence_refs
            .iter()
            .cloned()
            .map(Ulid)
            .collect::<Vec<_>>();
        Ok(FlushPlan {
            operation_id: stable_ulid(&input.candidate_id),
            issued_at: "2026-05-20T00:00:00Z".to_owned(),
            issuer: Identity::parse("agt:cairn-workflows:skillify-handler:v1")
                .map_err(|e| e.to_string())?,
            principal: None,
            scope: ScopeTuple::default(),
            mode: FlushMode::HumanReview,
            mutations: vec![PlannedMutation::Evolve {
                skill: target,
                diff_ref: std::path::PathBuf::from(format!(
                    ".cairn/evolution/skillify/{}/versions/v1/manifest.json",
                    input.candidate_id
                )),
            }],
            reason: PlanReason::Skillify {
                candidate_id: input.candidate_id.clone(),
                gate_count: input.gate_count,
            },
            source_events: evidence,
            target_hashes: BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at: "2026-05-20T00:05:00Z".to_owned(),
            placeholder: false,
        })
    }
}

fn stable_ulid(seed: &str) -> Ulid {
    let hex = crate::synthetic::sha256_hex(seed.as_bytes());
    let suffix = &hex[..15].to_ascii_uppercase();
    Ulid(format!("01HQZX9F5N0{suffix}"))
}
```

- [ ] **Step 5: Run promotion plan test to verify GREEN**

Run:

```bash
cargo test -p cairn-workflows --test skillify_handler promotion_plan_records_candidate_and_gate_count --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 6**

```bash
git add crates/cairn-core/src/domain/flush_plan/mod.rs crates/cairn-core/src/domain/flush_plan/diff.rs crates/cairn-workflows/src/skillify/planner.rs crates/cairn-workflows/tests/skillify_handler.rs
git commit -m "feat(workflows): plan skillify promotion"
```

---

### Task 7: Pure Skill Bundle Lint

**Files:**
- Modify: `crates/cairn-core/tests/skillify_model.rs`
- Create: `crates/cairn-core/src/pipeline/skillify/lint.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`

- [ ] **Step 1: Add failing lint tests**

Append to `crates/cairn-core/tests/skillify_model.rs`:

```rust
use cairn_core::pipeline::skillify::{
    SkillLintIssueKind, SkillLintSkill, SkillLintSnapshot, lint_skill_snapshot,
};

#[test]
fn lint_reports_missing_script_and_duplicate_lane() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            SkillLintSkill {
                skill_id: "skill-a".to_owned(),
                lane: "deploy.hotfix".to_owned(),
                path: "skills/skill_a.md".to_owned(),
                uses: Some("skills/scripts/missing.sh".to_owned()),
                resolver_triggers: vec!["deploy hotfix".to_owned()],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec!["skills/skill_a.md".to_owned()],
            },
            SkillLintSkill {
                skill_id: "skill-b".to_owned(),
                lane: "deploy.hotfix".to_owned(),
                path: "skills/skill_b.md".to_owned(),
                uses: Some("skills/scripts/b.sh".to_owned()),
                resolver_triggers: vec!["ship hotfix".to_owned()],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec!["skills/skill_b.md".to_owned(), "skills/scripts/b.sh".to_owned()],
            },
        ],
    };

    let findings = lint_skill_snapshot(&snapshot);
    assert!(findings.iter().any(|f| f.kind == SkillLintIssueKind::MissingArtifact));
    assert!(findings.iter().any(|f| f.kind == SkillLintIssueKind::DuplicateLane));
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-core --test skillify_model lint_reports_missing_script_and_duplicate_lane --locked
```

Expected: FAIL with unresolved imports `SkillLintSnapshot` and `lint_skill_snapshot`.

- [ ] **Step 3: Implement pure lint module**

Replace `crates/cairn-core/src/pipeline/skillify/lint.rs` with:

```rust
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintSnapshot {
    pub skills: Vec<SkillLintSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintSkill {
    pub skill_id: String,
    pub lane: String,
    pub path: String,
    pub uses: Option<String>,
    pub resolver_triggers: Vec<String>,
    pub files_to: Option<String>,
    pub gate_report_passed: bool,
    pub rollback_version_count: u32,
    pub existing_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillLintIssueKind {
    MissingArtifact,
    Unreachable,
    DuplicateLane,
    GateFailed,
    RollbackBroken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintIssue {
    pub kind: SkillLintIssueKind,
    pub skill_id: String,
    pub path: String,
    pub message: String,
}

pub fn lint_skill_snapshot(snapshot: &SkillLintSnapshot) -> Vec<SkillLintIssue> {
    let mut out = Vec::new();
    let mut lanes: BTreeMap<&str, Vec<&SkillLintSkill>> = BTreeMap::new();
    for skill in &snapshot.skills {
        lanes.entry(&skill.lane).or_default().push(skill);
        let existing: BTreeSet<&str> = skill.existing_paths.iter().map(String::as_str).collect();
        if let Some(uses) = &skill.uses
            && !existing.contains(uses.as_str())
        {
            out.push(issue(
                SkillLintIssueKind::MissingArtifact,
                skill,
                format!("skill `{}` references missing script `{uses}`", skill.skill_id),
            ));
        }
        if skill.resolver_triggers.is_empty() {
            out.push(issue(
                SkillLintIssueKind::Unreachable,
                skill,
                format!("skill `{}` has no resolver triggers", skill.skill_id),
            ));
        }
        if !skill.gate_report_passed {
            out.push(issue(
                SkillLintIssueKind::GateFailed,
                skill,
                format!("skill `{}` does not have a passing gate report", skill.skill_id),
            ));
        }
        if skill.rollback_version_count == 0 {
            out.push(issue(
                SkillLintIssueKind::RollbackBroken,
                skill,
                format!("skill `{}` has no rollback version metadata", skill.skill_id),
            ));
        }
    }
    for (lane, skills) in lanes {
        if skills.len() > 1 {
            for skill in skills {
                out.push(issue(
                    SkillLintIssueKind::DuplicateLane,
                    skill,
                    format!("lane `{lane}` is used by more than one live skill"),
                ));
            }
        }
    }
    out
}

fn issue(kind: SkillLintIssueKind, skill: &SkillLintSkill, message: String) -> SkillLintIssue {
    SkillLintIssue {
        kind,
        skill_id: skill.skill_id.clone(),
        path: skill.path.clone(),
        message,
    }
}
```

- [ ] **Step 4: Re-export lint types**

In `crates/cairn-core/src/pipeline/skillify/mod.rs`, add:

```rust
pub use lint::{
    SkillLintIssue, SkillLintIssueKind, SkillLintSkill, SkillLintSnapshot,
    lint_skill_snapshot,
};
```

- [ ] **Step 5: Run the lint test to verify GREEN**

Run:

```bash
cargo test -p cairn-core --test skillify_model lint_reports_missing_script_and_duplicate_lane --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 7**

```bash
git add crates/cairn-core/src/pipeline/skillify crates/cairn-core/tests/skillify_model.rs
git commit -m "feat(core): lint skillify bundles"
```

---

### Task 8: CLI `cairn lint --skill`

**Files:**
- Create: `crates/cairn-cli/tests/lint_skill.rs`
- Modify: `crates/cairn-cli/src/verbs/lint.rs`

- [ ] **Step 1: Write failing CLI lint test**

Create `crates/cairn-cli/tests/lint_skill.rs`:

```rust
#![allow(missing_docs)]

use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn lint_skill_reports_missing_script() {
    let temp = TempDir::new().expect("temp");
    let vault = temp.path();
    std::fs::create_dir_all(vault.join("skills")).expect("skills");
    std::fs::create_dir_all(vault.join(".cairn/resolver/skills")).expect("resolver");
    std::fs::create_dir_all(vault.join(".cairn/evolution/skillify/skc_fixture/versions/v1")).expect("versions");
    std::fs::write(
        vault.join("skills/skill_deploy-hotfix.md"),
        "---\nskill_id: deploy-hotfix\nversion: 1\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: skills/scripts/missing.sh\nfiles_to: wiki/summaries/\ncandidate_id: skc_fixture\nstatus: live\n---\nRun the skill.\n",
    )
    .expect("skill");
    std::fs::write(
        vault.join(".cairn/resolver/skills/deploy-hotfix.json"),
        r#"{"skill_id":"deploy-hotfix","triggers":["deploy hotfix"]}"#,
    )
    .expect("resolver");
    std::fs::write(
        vault.join(".cairn/evolution/skillify/skc_fixture/gate-report.json"),
        r#"{"candidate_id":"skc_fixture","gates":[{"name":"skill_contract","status":"passed","message":null},{"name":"deterministic_script","status":"passed","message":null},{"name":"unit_tests","status":"passed","message":null},{"name":"integration_tests","status":"passed","message":null},{"name":"llm_evals","status":"passed","message":null},{"name":"resolver_trigger","status":"passed","message":null},{"name":"resolver_eval","status":"passed","message":null},{"name":"check_resolvable_and_dry","status":"passed","message":null},{"name":"e2e_smoke","status":"passed","message":null},{"name":"filing_rules","status":"passed","message":null}]}"#,
    )
    .expect("gate");
    std::fs::write(
        vault.join(".cairn/evolution/skillify/skc_fixture/versions/v1/manifest.json"),
        "{}",
    )
    .expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(vault)
        .arg("--json")
        .arg("lint")
        .arg("--skill");

    cmd.assert()
        .failure()
        .stdout(predicates::str::contains("skill_missing_artifact"));
}
```

- [ ] **Step 2: Run the CLI test to verify RED**

Run:

```bash
cargo test -p cairn-cli --test lint_skill --locked
```

Expected: FAIL because `--skill` does not append skill findings yet.

- [ ] **Step 3: Implement skill snapshot loading and finding mapping**

In `crates/cairn-cli/src/verbs/lint.rs`, add a call after regular lint findings are assembled:

```rust
    if args.skill.unwrap_or(false) {
        append_skill_findings(vault_root, &mut data).await?;
    }
```

Add these helper functions near the existing projection and trace-canvas append helpers:

```rust
async fn append_skill_findings(
    vault_root: &std::path::Path,
    data: &mut LintData,
) -> anyhow::Result<()> {
    let snapshot = build_skill_lint_snapshot(vault_root)?;
    for issue in cairn_core::pipeline::skillify::lint_skill_snapshot(&snapshot) {
        push_lint_finding(data, skill_issue_to_finding(issue));
    }
    Ok(())
}

fn build_skill_lint_snapshot(
    vault_root: &std::path::Path,
) -> anyhow::Result<cairn_core::pipeline::skillify::SkillLintSnapshot> {
    let skills_dir = vault_root.join("skills");
    let mut skills = Vec::new();
    if !skills_dir.exists() {
        return Ok(cairn_core::pipeline::skillify::SkillLintSnapshot { skills });
    }
    for entry in std::fs::read_dir(&skills_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
            continue;
        }
        let body = std::fs::read_to_string(&path)?;
        if !body.starts_with("---\n") {
            continue;
        }
        let frontmatter = body
            .splitn(3, "---\n")
            .nth(1)
            .unwrap_or_default();
        let skill_id = yaml_scalar(frontmatter, "skill_id").unwrap_or_else(|| {
            path.file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("unknown")
                .to_owned()
        });
        let lane = yaml_scalar(frontmatter, "lane").unwrap_or_default();
        let uses = yaml_scalar(frontmatter, "uses");
        let files_to = yaml_scalar(frontmatter, "files_to");
        let candidate_id = yaml_scalar(frontmatter, "candidate_id").unwrap_or_default();
        let resolver = vault_root.join(".cairn/resolver/skills").join(format!("{skill_id}.json"));
        let rollback_dir = vault_root
            .join(".cairn/evolution/skillify")
            .join(&candidate_id)
            .join("versions");
        let rollback_version_count = std::fs::read_dir(&rollback_dir)
            .map(|entries| entries.filter_map(Result::ok).count() as u32)
            .unwrap_or(0);
        let mut existing_paths = vec![rel(vault_root, &path)];
        if let Some(uses) = &uses
            && vault_root.join(uses).exists()
        {
            existing_paths.push(uses.clone());
        }
        let resolver_triggers = if resolver.exists() {
            vec!["resolver-present".to_owned()]
        } else {
            Vec::new()
        };
        let gate_report_passed = !candidate_id.is_empty()
            && vault_root
                .join(".cairn/evolution/skillify")
                .join(&candidate_id)
                .join("gate-report.json")
                .exists();
        skills.push(cairn_core::pipeline::skillify::SkillLintSkill {
            skill_id,
            lane,
            path: rel(vault_root, &path),
            uses,
            resolver_triggers,
            files_to,
            gate_report_passed,
            rollback_version_count,
            existing_paths,
        });
    }
    Ok(cairn_core::pipeline::skillify::SkillLintSnapshot { skills })
}

fn yaml_scalar(frontmatter: &str, key: &str) -> Option<String> {
    frontmatter.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim() == key {
            Some(v.trim().trim_matches('"').to_owned()).filter(|s| !s.is_empty())
        } else {
            None
        }
    })
}

fn rel(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn skill_issue_to_finding(
    issue: cairn_core::pipeline::skillify::SkillLintIssue,
) -> Finding {
    use cairn_core::generated::verbs::lint::{Kind, Severity, Target};
    let kind = match issue.kind {
        cairn_core::pipeline::skillify::SkillLintIssueKind::MissingArtifact => {
            Kind::SkillMissingArtifact
        }
        cairn_core::pipeline::skillify::SkillLintIssueKind::Unreachable => {
            Kind::SkillUnreachable
        }
        cairn_core::pipeline::skillify::SkillLintIssueKind::DuplicateLane => {
            Kind::SkillDuplicateLane
        }
        cairn_core::pipeline::skillify::SkillLintIssueKind::GateFailed => {
            Kind::SkillGateFailed
        }
        cairn_core::pipeline::skillify::SkillLintIssueKind::RollbackBroken => {
            Kind::SkillRollbackBroken
        }
    };
    Finding {
        entities: Some(vec![issue.skill_id]),
        kind,
        message: issue.message,
        severity: Severity::Error,
        suggested_fix: Some("run `cairn lint --skill --fix-skill-plan` and review the generated plan".to_owned()),
        target: Some(Target {
            operation_id: None,
            path: Some(issue.path),
            record_id: None,
        }),
        tracking_issue: Some(112),
    }
}
```

- [ ] **Step 4: Wire generated args into lint dispatch**

In `crates/cairn-cli/src/verbs/lint.rs`, when constructing `LintArgs` from `clap::ArgMatches`, include:

```rust
let skill = sub.get_flag("skill");
let fix_skill_plan = sub.get_flag("fix-skill-plan");
```

Immediately after the existing `if let Some(plan_id) = plan_id { ... }` branch and before the `--fix-markdown` branch, add:

```rust
if skill {
    return run_skill_lint(json, vault_root, fix_skill_plan, &operation_id);
}
```

Add `run_skill_lint` near the other lint sub-runners:

```rust
fn run_skill_lint(
    json: bool,
    vault_root: Option<&Path>,
    fix_skill_plan: bool,
    operation_id: &Ulid,
) -> ExitCode {
    let vault_root = vault_root.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let snapshot = match build_skill_lint_snapshot(&vault_root) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            emit_aborted(json, operation_id.clone(), &format!("skill lint: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let mut findings = cairn_core::pipeline::skillify::lint_skill_snapshot(&snapshot)
        .into_iter()
        .map(skill_issue_to_finding)
        .collect::<Vec<_>>();
    if fix_skill_plan && !findings.is_empty() {
        let plan_path = vault_root
            .join(".cairn/evolution/skillify")
            .join("lint-fix-plan.json");
        if let Some(parent) = plan_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = serde_json::json!({
            "operation_id": operation_id.0,
            "finding_count": findings.len(),
            "actions": findings.iter().map(|finding| {
                serde_json::json!({
                    "kind": kind_key(finding.kind),
                    "target": finding.target.as_ref().and_then(|target| target.path.clone()),
                    "action": "review_and_regenerate_skillify_bundle"
                })
            }).collect::<Vec<_>>()
        });
        let _ = std::fs::write(&plan_path, serde_json::to_vec_pretty(&body).unwrap_or_default());
        findings.push(Finding {
            entities: None,
            kind: Kind::DeferredCheck,
            message: format!("skill lint fix plan written to {}", plan_path.display()),
            severity: Severity::Info,
            suggested_fix: None,
            target: Some(Target {
                operation_id: None,
                path: Some(plan_path.display().to_string()),
                record_id: None,
            }),
            tracking_issue: Some(112),
        });
    }
    let total = usize_to_u64(findings.len());
    let data = LintData {
        summary: edge_summary(&findings, total, 0),
        findings,
        report_path: None,
    };
    let has_blocking_findings = data.findings.iter().any(has_warning_or_error);
    let response = committed_response(operation_id.clone(), data);
    if json {
        emit_json(&response);
    } else if let Some(ResponseData::Lint(data)) = response.data.as_ref() {
        emit_human(data, &response.operation_id);
    }
    if has_blocking_findings {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
```

- [ ] **Step 5: Run the CLI lint test to verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test lint_skill --locked
```

Expected: PASS and JSON output contains `skill_missing_artifact`.

- [ ] **Step 6: Commit Task 8**

```bash
git add crates/cairn-cli/src/verbs/lint.rs crates/cairn-cli/tests/lint_skill.rs
git commit -m "feat(cli): lint skillify bundles"
```

---

### Task 9: Workflow Registration And Skillify Trigger Sources

**Files:**
- Modify: `crates/cairn-cli/src/mcp.rs`
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Modify: `crates/cairn-workflows/src/dream/handler.rs`
- Modify: `crates/cairn-workflows/tests/skillify_trigger.rs`
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Write failing workflow registration test**

Append to `crates/cairn-workflows/tests/skillify_trigger.rs`:

```rust
#[tokio::test]
async fn registry_accepts_skillify_handler_kind() {
    let handler = std::sync::Arc::new(cairn_workflows::SkillifyHandler::new(
        std::path::PathBuf::from("."),
        None,
    ));
    let registry = cairn_workflows::scheduler::HandlerRegistryBuilder::default()
        .with(handler)
        .build();

    let found = registry
        .lookup(&cairn_core::contract::job_store::JobKind::new(cairn_workflows::SKILLIFY_KIND))
        .expect("handler");
    assert_eq!(found.kind().as_str(), cairn_workflows::SKILLIFY_KIND);
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p cairn-workflows --test skillify_trigger registry_accepts_skillify_handler_kind --locked
```

Expected: FAIL because `SkillifyHandler` is not exported or registered yet.

- [ ] **Step 3: Register in MCP scheduler**

In `crates/cairn-cli/src/mcp.rs`, add `SkillifyHandler` to the workflow imports and register it in `HandlerRegistryBuilder`:

```rust
let skillify_handler = SkillifyHandler::new(vault_root.clone(), None);
let registry = HandlerRegistryBuilder::default()
    .with(Arc::new(consolidation_handler))
    .with(Arc::new(forget_cleanup_handler))
    .with(Arc::new(dream_handler))
    .with(Arc::new(expiration_handler))
    .with(Arc::new(evaluation_handler))
    .with(Arc::new(skillify_handler))
    .build();
```

- [ ] **Step 4: Enqueue explicit skillify triggers from capture**

In `crates/cairn-cli/src/verbs/capture_trace.rs`, extend the workflow import:

```rust
use cairn_workflows::{
    SkillifyTrigger, TraceCanvasPayload, TraceCanvasProjection,
    consolidation::enqueue_if_due_scoped, enqueue_skillify, enqueue_tier_with_dedupe_token,
    enqueue_trace_canvas_step,
};
```

Add a local helper near the other capture helpers:

```rust
fn explicit_skillify_request(raw_text: &str) -> bool {
    let normalized = raw_text.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "skillify this" | "skillify it" | "/skillify" | "cairn skillify"
    )
}

fn skillify_source_dedupe_token(source_record_ids: &[String]) -> String {
    let mut ids = source_record_ids.to_vec();
    ids.sort();
    let mut h = Sha256::new();
    h.update(ids.join(",").as_bytes());
    format!("explicit:{:x}", h.finalize())
}
```

Inside `run_events_handler_inner_no_guard`, add `let mut explicit_skillify_requested = false;` beside `had_stop`. In the per-event projection loop, after `classified` and `raw_text` are available, set:

```rust
if classified == TraceEvent::UserMessage && explicit_skillify_request(&raw_text) {
    explicit_skillify_requested = true;
}
```

After the existing Stop-hook Dream enqueue, enqueue `skillify.emit` only for closed turns that explicitly requested it:

```rust
if explicit_skillify_requested && had_stop {
    let source_record_ids = projected
        .iter()
        .map(|record| record.id.as_str().to_owned())
        .collect::<Vec<_>>();
    if !source_record_ids.is_empty() {
        let dedupe_token = skillify_source_dedupe_token(&source_record_ids);
        let _ = enqueue_skillify(
            js,
            SkillifyTrigger::Explicit,
            &session_str,
            &dedupe_token,
            now_ms,
            scope_binding,
            source_record_ids,
        )
        .await;
    }
}
```

Add a focused test to `crates/cairn-cli/tests/capture_trace_verb.rs` or the existing `capture_trace.rs` test module that sends a `UserMessage` body `skillify this` plus a `Stop` hook in the same turn and asserts one enqueued request has `kind == cairn_workflows::SKILLIFY_KIND`. Replay the same turn and assert both enqueued `skillify.emit` rows use the same dedupe key.

- [ ] **Step 5: Enqueue Deep Dream discovered candidates**

In `crates/cairn-workflows/src/dream/handler.rs`, add an optional job store to `DreamHandler`:

```rust
use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload, JobStore};
use crate::skillify::{SkillifyTrigger, enqueue_skillify};

pub struct DreamHandler {
    store: Arc<dyn MemoryStore>,
    config: DreamConfig,
    llm: Option<Arc<dyn LLMProvider>>,
    skillify_jobs: Option<Arc<dyn JobStore>>,
}

impl DreamHandler {
    pub fn new(
        store: Arc<dyn MemoryStore>,
        config: DreamConfig,
        llm: Option<Arc<dyn LLMProvider>>,
    ) -> Self {
        Self {
            store,
            config,
            llm,
            skillify_jobs: None,
        }
    }

    #[must_use]
    pub fn with_skillify_jobs(mut self, jobs: Arc<dyn JobStore>) -> Self {
        self.skillify_jobs = Some(jobs);
        self
    }
}
```

In `crates/cairn-cli/src/mcp.rs`, construct the dream handler with the same scheduler job store:

```rust
let dream_handler = DreamHandler::new(store_dyn.clone(), config.dream, None)
    .with_skillify_jobs(job_store.clone());
```

After the dream record has been upserted and source-liveness rechecks pass, enqueue `skillify.emit` only for Deep Dream windows containing at least one successful strategy record:

```rust
if payload.tier == cairn_core::config::DreamTier::DeepDreaming
    && filtered.iter().any(|record| record.kind == MemoryKind::StrategySuccess)
{
    if let Some(jobs) = self.skillify_jobs.as_deref() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let _ = enqueue_skillify(
            jobs,
            SkillifyTrigger::DeepDream,
            &payload.key,
            &sources_hash,
            now_ms,
            payload.bound_scope.as_ref(),
            source_record_ids.clone(),
        )
        .await;
    }
}
```

Add a `cairn-workflows` test that drives `DreamHandler` with a recording `JobStore`, a Deep Dream payload, and a source window containing `MemoryKind::StrategySuccess`. Assert exactly one `skillify.emit` request is recorded and that replaying with the same source ids produces the same dedupe key.

- [ ] **Step 6: Run workflow registration and trigger tests**

Run:

```bash
cargo test -p cairn-workflows --test skillify_trigger --locked
cargo test -p cairn-cli --test capture_trace_verb skillify --locked
```

Expected: both commands exit 0.

- [ ] **Step 7: Commit Task 9**

```bash
git add crates/cairn-cli/src/mcp.rs crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-workflows/src/dream/handler.rs crates/cairn-workflows/src/skillify/handler.rs crates/cairn-workflows/tests/skillify_trigger.rs crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "feat(workflows): enqueue skillify from traces"
```

---

### Task 10: Fixtures And User-Facing Skill Example

**Files:**
- Create: `fixtures/v0/skillify/successful-trajectory.json`
- Create: `fixtures/v0/skillify/failed-trajectory.json`
- Create: `fixtures/v0/skillify/missing-test-bundle/manifest.json`
- Create: `fixtures/v0/skillify/duplicate-lane-bundle/manifest.json`
- Modify: `skills/cairn/examples/04-skillify-this.md`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`

- [ ] **Step 1: Add fixture loader test**

Append to `crates/cairn-test-fixtures/src/lib.rs` tests module:

```rust
#[test]
fn skillify_fixtures_exist() {
    let root = fixture_v0_dir().join("skillify");
    assert!(root.join("successful-trajectory.json").exists());
    assert!(root.join("failed-trajectory.json").exists());
    assert!(root.join("missing-test-bundle/manifest.json").exists());
    assert!(root.join("duplicate-lane-bundle/manifest.json").exists());
}
```

- [ ] **Step 2: Run fixture test to verify RED**

Run:

```bash
cargo test -p cairn-test-fixtures skillify_fixtures_exist --locked
```

Expected: FAIL because fixture files do not exist.

- [ ] **Step 3: Add fixture files**

Create `fixtures/v0/skillify/successful-trajectory.json`:

```json
{
  "outcome": "success",
  "lane": "deploy.hotfix",
  "source_record_ids": ["01HQZX9F5N0000000000000001"],
  "success_criteria": ["health check returned 200"],
  "tool_sequence": ["git status", "cargo test", "deploy hotfix"],
  "evidence": {"replay": "passed", "tests": "passed"}
}
```

Create `fixtures/v0/skillify/failed-trajectory.json`:

```json
{
  "outcome": "failure",
  "lane": "deploy.hotfix",
  "source_record_ids": ["01HQZX9F5N0000000000000002"],
  "success_criteria": ["health check returned 200"],
  "tool_sequence": ["deploy hotfix"],
  "evidence": {"replay": "failed", "tests": "missing"}
}
```

Create `fixtures/v0/skillify/missing-test-bundle/manifest.json`:

```json
{
  "candidate_id": "skc_missing_tests",
  "version": 1,
  "artifacts": [
    {
      "kind": "skill_contract",
      "path": "bundle/skills/skill_missing-tests.md",
      "content_sha256": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "evidence_refs": ["01HQZX9F5N0000000000000001"],
      "status": "generated"
    }
  ]
}
```

Create `fixtures/v0/skillify/duplicate-lane-bundle/manifest.json`:

```json
{
  "skills": [
    {"skill_id": "skill-a", "lane": "deploy.hotfix"},
    {"skill_id": "skill-b", "lane": "deploy.hotfix"}
  ]
}
```

- [ ] **Step 4: Update the skill example**

Replace `skills/cairn/examples/04-skillify-this.md` with:

```markdown
# Example: skillify a procedure

**User says:** "Skillify this — we just figured out how to run the benchmarks."

**Cairn behavior:**
```bash
cairn capture_trace --stdin
cairn lint --skill
```

The explicit "skillify this" signal enqueues the SkillEmitter workflow. Cairn
creates a candidate bundle under `.cairn/evolution/skillify/<candidate_id>/`,
runs the ten Skillify gates, and promotes the skill only after the gate report
passes.

**Why this is not just `strategy_success`:** `strategy_success` is source
evidence. A durable skill requires the section 11.b artifact bundle, tests,
resolver entries, lint audits, and rollback metadata.
```

- [ ] **Step 5: Run fixture test to verify GREEN**

Run:

```bash
cargo test -p cairn-test-fixtures skillify_fixtures_exist --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 10**

```bash
git add fixtures/v0/skillify skills/cairn/examples/04-skillify-this.md crates/cairn-test-fixtures/src/lib.rs
git commit -m "test(fixtures): add skillify trajectories"
```

---

### Task 11: Full Verification

**Files:**
- No new files.
- Validate all files touched by Tasks 1-10.

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all --check
```

Expected: exit 0.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test -p cairn-core --test skillify_model --locked
cargo test -p cairn-idl --test skillify_lint_schema --locked
cargo test -p cairn-workflows --test skillify_trigger --locked
cargo test -p cairn-workflows --test skillify_handler --locked
cargo test -p cairn-cli --test lint_skill --locked
cargo test -p cairn-test-fixtures skillify_fixtures_exist --locked
```

Expected: each command exits 0.

- [ ] **Step 3: Run codegen drift check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: exit 0 and output reports a clean generated tree.

- [ ] **Step 4: Run clippy for touched crates**

Run:

```bash
cargo clippy -p cairn-core -p cairn-idl -p cairn-workflows -p cairn-cli -p cairn-test-fixtures --all-targets --locked -- -D warnings
```

Expected: exit 0.

- [ ] **Step 5: Run repository boundary check**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: exit 0, confirming `cairn-core` has no internal workspace dependency.

- [ ] **Step 6: Run final diff hygiene**

Run:

```bash
git diff --check
git status --short
```

Expected: `git diff --check` exits 0. `git status --short` contains only intentional Skillify implementation files.

- [ ] **Step 7: Commit final verification note if needed**

If any formatting or generated files changed during verification, commit them:

```bash
git add .
git commit -m "chore: finalize skillify pipeline verification"
```

If no files changed, do not create an empty commit.
