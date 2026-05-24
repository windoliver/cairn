//! Gate runner trait and result types for Skillify pipeline gates.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cairn_core::contract::llm_provider::LLMProvider;
use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillArtifactKind, SkillLintSkill, SkillLintSnapshot, SkillifyGate,
    SkillifyGateStatus,
};

use super::materialize::AuthoredSkillBundle;

/// Context passed to each gate runner.
pub struct GateRunContext<'a> {
    /// Vault root path.
    pub vault_root: &'a Path,
    /// Stable candidate id.
    pub candidate_id: &'a str,
    /// Candidate directory on disk.
    pub candidate_dir: PathBuf,
    /// Validated artifact bundle.
    pub bundle: &'a SkillArtifactBundle,
    /// Raw authored content.
    pub authored: &'a AuthoredSkillBundle,
    /// Optional LLM provider for eval gates.
    pub llm: Option<&'a dyn LLMProvider>,
    /// Current skill lint snapshot for DRY/resolvable checks.
    pub snapshot: &'a SkillLintSnapshot,
}

/// Result from one gate runner execution.
#[derive(Debug, Clone)]
pub struct GateRunResult {
    /// Artifact kind this gate evaluates.
    pub kind: SkillArtifactKind,
    /// Gate verdict.
    pub status: SkillifyGateStatus,
    /// Human-readable detail.
    pub message: Option<String>,
    /// Evidence references.
    pub evidence_refs: Vec<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// When `Some`, a transient dependency error caused this gate to fail
    /// (e.g. LLM provider unreachable). The pipeline propagates this as a
    /// `SkillifyPipelineError::Llm` so the scheduler retries instead of
    /// recording a permanent gate failure.
    pub transient_error_detail: Option<String>,
}

impl GateRunResult {
    /// Convert to a core `SkillifyGate` for state machine recording.
    #[must_use]
    pub fn into_gate(self) -> SkillifyGate {
        SkillifyGate {
            name: self.kind.as_str().to_owned(),
            status: self.status,
            message: self.message,
        }
    }

    /// Create a passing result.
    #[must_use]
    pub fn passed(kind: SkillArtifactKind, duration_ms: u64) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Passed,
            message: None,
            evidence_refs: Vec::new(),
            duration_ms,
            transient_error_detail: None,
        }
    }

    /// Create a failing result.
    #[must_use]
    pub fn failed(kind: SkillArtifactKind, message: String, duration_ms: u64) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Failed,
            message: Some(message),
            evidence_refs: Vec::new(),
            duration_ms,
            transient_error_detail: None,
        }
    }

    /// Create a blocked result.
    #[must_use]
    pub fn blocked(kind: SkillArtifactKind, message: String) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Blocked,
            message: Some(message),
            evidence_refs: Vec::new(),
            duration_ms: 0,
            transient_error_detail: None,
        }
    }

    /// Create a transient-error result. The pipeline propagates this as a
    /// retriable LLM error rather than a permanent gate failure.
    #[must_use]
    pub fn transient(kind: SkillArtifactKind, detail: String, duration_ms: u64) -> Self {
        Self {
            kind,
            status: SkillifyGateStatus::Blocked,
            message: Some(format!("transient dependency error: {detail}")),
            evidence_refs: Vec::new(),
            duration_ms,
            transient_error_detail: Some(detail),
        }
    }
}

/// Measures wall-clock duration for a gate run.
pub struct GateTimer {
    start: Instant,
}

impl GateTimer {
    /// Start a new timer.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Elapsed milliseconds since start.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

/// Trait for individual gate runner implementations.
#[async_trait::async_trait]
pub trait GateRunner: Send + Sync {
    /// Which artifact kind this runner validates.
    fn artifact_kind(&self) -> SkillArtifactKind;

    /// Execute the gate check.
    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult;
}

// -- Runner implementations --

/// Gate 1: Validates skill contract markdown frontmatter.
pub struct SkillContractRunner;

#[async_trait::async_trait]
impl GateRunner for SkillContractRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::SkillContract
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let md = &ctx.authored.skill_markdown;

        // Round 8 hardening: parse YAML frontmatter and require fields
        // to appear THERE, not anywhere in the body. The previous
        // substring check would pass `lane:` written inside the body
        // prose as a real frontmatter declaration.
        let Some(frontmatter) = extract_frontmatter(md) else {
            return GateRunResult::failed(
                self.artifact_kind(),
                "skill contract has no YAML frontmatter (--- ... ---)".to_owned(),
                timer.elapsed_ms(),
            );
        };

        // Round 9 hardening: validate each required field is present at the
        // top level AND has a non-empty value. Scalar fields (lane, uses,
        // files_to) must be non-empty strings; triggers must be a non-empty
        // list. Without these checks an empty `triggers:` or a key nested
        // under another mapping could pass the gate.
        let mut missing = Vec::new();
        for field in ["lane", "uses", "files_to"] {
            match top_level_scalar(frontmatter, field) {
                Some(v) if !v.is_empty() => {}
                _ => missing.push(field),
            }
        }
        // Triggers must be present and non-empty (inline `[a, b]` or block
        // list `- a\n  - b`).
        if top_level_list(frontmatter, "triggers").is_none_or(|v| v.is_empty()) {
            missing.push("triggers");
        }

        if !missing.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!(
                    "skill contract frontmatter missing or empty required fields: {}",
                    missing.join(", ")
                ),
                timer.elapsed_ms(),
            );
        }

        // Cross-check that the frontmatter agrees with the authored fields
        // the rest of the pipeline uses. A mismatch means the contract
        // markdown doesn't describe the actual artifacts.
        if let Some(lane_in_md) = top_level_scalar(frontmatter, "lane")
            && lane_in_md != ctx.authored.lane
        {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!(
                    "skill contract lane `{lane_in_md}` does not match authored lane `{}`",
                    ctx.authored.lane
                ),
                timer.elapsed_ms(),
            );
        }
        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Extract YAML frontmatter (`---\n…\n---`) from the start of a markdown body.
fn extract_frontmatter(body: &str) -> Option<&str> {
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Returns the scalar value for a TOP-LEVEL `key:` (no leading indent), or
/// `None` if absent, empty, list-shaped, or only present nested under
/// another mapping. Stricter than the legacy substring check so a nested or
/// commented-out key cannot satisfy the gate.
fn top_level_scalar(fm: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    for line in fm.lines() {
        // Require zero leading whitespace — nested keys (under another
        // mapping) are not top-level and don't count.
        if line.starts_with(&needle) {
            let rest = &line[needle.len()..];
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() || value.starts_with('[') || value.starts_with('{') {
                return None;
            }
            return Some(value.to_owned());
        }
    }
    None
}

/// Returns the list value for a TOP-LEVEL `key:`, either inline
/// (`triggers: ["a", "b"]`) or block-list. Returns `None` when the key is
/// absent or non-list; an empty list returns `Some(vec![])`.
fn top_level_list(fm: &str, key: &str) -> Option<Vec<String>> {
    let needle = format!("{key}:");
    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with(&needle) {
            i += 1;
            continue;
        }
        let rest = &line[needle.len()..];
        let inline = rest.trim();
        if !inline.is_empty() {
            // Inline list `[a, b]`.
            if let Some(arr) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let mut out = Vec::new();
                for item in arr.split(',') {
                    let v = item.trim().trim_matches('"').trim_matches('\'');
                    if !v.is_empty() {
                        out.push(v.to_owned());
                    }
                }
                return Some(out);
            }
            // Inline scalar or mapping — not a list.
            return None;
        }
        // Block list: subsequent indented lines starting with `- `.
        let mut out = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let next = lines[j];
            if let Some(item) = next.trim_start().strip_prefix("- ") {
                let v = item.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    out.push(v.to_owned());
                }
                j += 1;
            } else if next.trim().is_empty() {
                j += 1;
            } else {
                break;
            }
        }
        return Some(out);
    }
    None
}

/// Gate 2: Validates deterministic script exists and has a shebang.
pub struct DeterministicScriptRunner;

#[async_trait::async_trait]
impl GateRunner for DeterministicScriptRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::DeterministicScript
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        let content = match std::fs::read_to_string(&script_path) {
            Ok(c) => c,
            Err(e) => {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    format!("script not found: {e}"),
                    timer.elapsed_ms(),
                );
            }
        };

        if content.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "script is empty".to_owned(),
                timer.elapsed_ms(),
            );
        }

        if !content.starts_with("#!") {
            return GateRunResult::failed(
                self.artifact_kind(),
                "script missing shebang (#!) line".to_owned(),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 3: Runs unit test cases against the deterministic script.
pub struct UnitTestRunner;

#[async_trait::async_trait]
impl GateRunner for UnitTestRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::UnitTests
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(cases) = ctx
            .authored
            .unit_tests
            .get("cases")
            .and_then(serde_json::Value::as_array)
        else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "unit_tests missing 'cases' array — gate blocked pending correct artifact format"
                    .to_owned(),
            );
        };

        // Round 7 hardening: coverage gates must actually exercise the
        // script. An empty `cases` array structurally satisfies the JSON
        // shape but trivially passes the gate, leaving promotion gated by
        // a no-op.
        if cases.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "unit_tests cases array is empty — gate requires at least one case".to_owned(),
                timer.elapsed_ms(),
            );
        }

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case
                .get("input")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let Some(expected) = case
                .get("expected_stdout")
                .and_then(serde_json::Value::as_str)
            else {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    format!("case {i}: missing expected_stdout"),
                    timer.elapsed_ms(),
                );
            };
            let timeout_ms = case
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10_000);

            match run_script(&script_path, input, timeout_ms, &[]).await {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 4: Runs integration test cases with `CAIRN_INTEGRATION=1`.
pub struct IntegrationTestRunner;

#[async_trait::async_trait]
impl GateRunner for IntegrationTestRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::IntegrationTests
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(cases) = ctx
            .authored
            .integration_tests
            .get("cases")
            .and_then(serde_json::Value::as_array)
        else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "integration_tests missing 'cases' array — gate blocked pending correct artifact format"
                    .to_owned(),
            );
        };

        if cases.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "integration_tests cases array is empty — gate requires at least one case"
                    .to_owned(),
                timer.elapsed_ms(),
            );
        }

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case
                .get("input")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let Some(expected) = case
                .get("expected_stdout")
                .and_then(serde_json::Value::as_str)
            else {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    format!("case {i}: missing expected_stdout"),
                    timer.elapsed_ms(),
                );
            };
            let timeout_ms = case
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(30_000);

            match run_script(
                &script_path,
                input,
                timeout_ms,
                &[("CAIRN_INTEGRATION", "1")],
            )
            .await
            {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 5: Runs LLM-based rubric evals.
pub struct LlmEvalRunner;

#[async_trait::async_trait]
impl GateRunner for LlmEvalRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::LlmEvals
    }

    #[allow(
        clippy::too_many_lines,
        reason = "linear rubric loop with structured error handling for each LLM response variant; splitting helpers would obscure the transient/permanent distinction"
    )]
    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(llm) = ctx.llm else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "LLM provider required for eval gate — gate blocked without LLM".to_owned(),
            );
        };

        let Some(rubric) = ctx
            .authored
            .llm_evals
            .get("rubric")
            .and_then(serde_json::Value::as_array)
        else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "llm_evals missing 'rubric' array — gate blocked pending correct artifact format"
                    .to_owned(),
            );
        };

        if rubric.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "llm_evals rubric array is empty — gate requires at least one rubric item"
                    .to_owned(),
                timer.elapsed_ms(),
            );
        }

        for (i, item) in rubric.iter().enumerate() {
            let prompt = item
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let expected = item
                .get("expected_behavior")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let criteria = item
                .get("scoring_criteria")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            let judge_prompt = format!(
                "Evaluate whether this skill correctly handles the following intent.\n\
                 Intent: {prompt}\n\
                 Expected behavior: {expected}\n\
                 Scoring criteria: {criteria}\n\
                 Skill contract:\n{}\n\n\
                 Respond with JSON: {{\"pass\": true/false, \"reason\": \"...\"}}",
                ctx.authored.skill_markdown
            );

            let req = cairn_core::contract::llm_provider::CompletionRequest::builder()
                .prompt(judge_prompt)
                .schema(serde_json::json!({
                    "type": "object",
                    "required": ["pass", "reason"],
                    "properties": {
                        "pass": {"type": "boolean"},
                        "reason": {"type": "string"}
                    }
                }))
                .build();

            match llm.complete(&req).await {
                Ok(cairn_core::contract::llm_provider::CompletionOutput::Json(v)) => {
                    if !v
                        .get("pass")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        let reason = v
                            .get("reason")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("no reason");
                        return GateRunResult::failed(
                            self.artifact_kind(),
                            format!("rubric item {i} failed: {reason}"),
                            timer.elapsed_ms(),
                        );
                    }
                }
                Ok(_) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("rubric item {i}: LLM returned non-JSON"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    // Transient provider outages must be propagated as a
                    // retriable error so the scheduler retries instead of
                    // burning the candidate as permanently failed.
                    if matches!(
                        e,
                        cairn_core::contract::llm_provider::LlmError::ProviderUnreachable { .. }
                    ) {
                        return GateRunResult::transient(
                            self.artifact_kind(),
                            format!("rubric item {i}: LLM unreachable: {e}"),
                            timer.elapsed_ms(),
                        );
                    }
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("rubric item {i}: LLM error: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 6: Validates resolver trigger entries and checks for collisions.
pub struct ResolverTriggerRunner;

#[async_trait::async_trait]
impl GateRunner for ResolverTriggerRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::ResolverTrigger
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(trigger_arr) = ctx.authored.resolver_triggers.as_array() else {
            return GateRunResult::failed(
                self.artifact_kind(),
                "resolver_triggers must be a JSON array of strings".to_owned(),
                timer.elapsed_ms(),
            );
        };

        let triggers: Vec<&str> = trigger_arr
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();

        if triggers.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "resolver_triggers is empty".to_owned(),
                timer.elapsed_ms(),
            );
        }

        for trigger in &triggers {
            if trigger.trim().is_empty() {
                return GateRunResult::failed(
                    self.artifact_kind(),
                    "resolver_triggers contains blank entry".to_owned(),
                    timer.elapsed_ms(),
                );
            }
        }

        for existing_skill in &ctx.snapshot.skills {
            if existing_skill.lane == ctx.authored.lane {
                continue;
            }
            for existing_trigger in &existing_skill.resolver_triggers {
                for candidate_trigger in &triggers {
                    if existing_trigger
                        .trim()
                        .eq_ignore_ascii_case(candidate_trigger.trim())
                    {
                        return GateRunResult::failed(
                            self.artifact_kind(),
                            format!(
                                "trigger {:?} collides with skill {} (lane {})",
                                candidate_trigger, existing_skill.skill_id, existing_skill.lane
                            ),
                            timer.elapsed_ms(),
                        );
                    }
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 7: Evaluates resolver precision/recall against labelled intents.
pub struct ResolverEvalRunner;

#[async_trait::async_trait]
impl GateRunner for ResolverEvalRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::ResolverEval
    }

    #[allow(
        clippy::too_many_lines,
        reason = "linear confusion-matrix computation; splitting into helpers would obscure the precision/recall thresholds"
    )]
    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(intents) = ctx
            .authored
            .resolver_eval
            .get("intents")
            .and_then(serde_json::Value::as_array)
        else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "resolver_eval missing 'intents' array — gate blocked pending correct artifact format"
                    .to_owned(),
            );
        };

        let triggers: Vec<String> = ctx
            .authored
            .resolver_triggers
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        // Compute confusion-matrix metrics over the labelled intents so
        // the gate enforces both recall (don't miss positives) and
        // precision (don't fire on negatives). Round 4 hardening — the
        // previous recall-only check let a broad trigger pass whenever
        // positives kept recall ≥ 0.8, regardless of false positives.
        let mut positives = 0u32; // expected_lane == candidate lane
        let mut negatives = 0u32; // expected_lane != candidate lane
        let mut true_positives = 0u32;
        let mut false_positives = 0u32;

        for intent_obj in intents {
            let intent = intent_obj
                .get("intent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let expected_lane = intent_obj
                .get("expected_lane")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            let matched = triggers
                .iter()
                .any(|t| intent.to_lowercase().contains(&t.to_lowercase()));
            let is_positive = expected_lane == ctx.authored.lane;
            if is_positive {
                positives += 1;
                if matched {
                    true_positives += 1;
                }
            } else {
                negatives += 1;
                if matched {
                    false_positives += 1;
                }
            }
        }

        if positives + negatives == 0 {
            return GateRunResult::failed(
                self.artifact_kind(),
                "no intents to evaluate".to_owned(),
                timer.elapsed_ms(),
            );
        }

        // Require BOTH positive and negative coverage. Without negatives,
        // precision is trivially 1.0 (no false positives possible) and the
        // round 4 precision guard provides no protection against overbroad
        // routing — an LLM that authors only positive intents would
        // self-certify a broad trigger. Round 5 hardening.
        if positives == 0 {
            return GateRunResult::failed(
                self.artifact_kind(),
                "resolver_eval intents contain no positives for the candidate lane".to_owned(),
                timer.elapsed_ms(),
            );
        }
        if negatives == 0 {
            return GateRunResult::failed(
                self.artifact_kind(),
                "resolver_eval intents contain no negative examples — \
                 add intents for other lanes so precision can be evaluated"
                    .to_owned(),
                timer.elapsed_ms(),
            );
        }

        let recall = if positives == 0 {
            1.0
        } else {
            f64::from(true_positives) / f64::from(positives)
        };
        if recall < 0.8 {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!(
                    "recall {recall:.2} < 0.8 ({true_positives}/{positives} positives matched)"
                ),
                timer.elapsed_ms(),
            );
        }

        // Precision: among intents the trigger fired on, what fraction were
        // the candidate's positives? If the trigger never fires we treat
        // precision as 1.0 (no false positives is fine; recall caught any
        // missed positives above).
        let predicted_positive = true_positives + false_positives;
        let precision = if predicted_positive == 0 {
            1.0
        } else {
            f64::from(true_positives) / f64::from(predicted_positive)
        };
        if precision < 0.9 {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!(
                    "precision {precision:.2} < 0.9 ({true_positives}/{predicted_positive} fires were correct; {false_positives} false positives over {negatives} negatives)"
                ),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 8: Runs check-resolvable and DRY audit via lint snapshot merge.
pub struct CheckResolvableAndDryRunner;

#[async_trait::async_trait]
impl GateRunner for CheckResolvableAndDryRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::CheckResolvableAndDry
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let triggers: Vec<String> = ctx
            .authored
            .resolver_triggers
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let files_to = ctx
            .authored
            .filing_rules
            .get("files_to")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);

        let candidate_skill = SkillLintSkill {
            skill_id: ctx.candidate_id.to_owned(),
            lane: ctx.authored.lane.clone(),
            path: format!("bundle/skills/skill_{}.md", ctx.authored.slug),
            uses: Some(format!("bundle/scripts/{}.sh", ctx.authored.slug)),
            resolver_triggers: triggers,
            files_to,
            gate_report_passed: true,
            rollback_version_count: 1,
            existing_paths: vec![format!("bundle/scripts/{}.sh", ctx.authored.slug)],
        };

        let mut merged = ctx.snapshot.clone();
        merged.skills.push(candidate_skill);

        let issues = cairn_core::pipeline::skillify::lint_skill_snapshot(&merged);
        let candidate_issues: Vec<_> = issues
            .iter()
            .filter(|issue| issue.skill_id == ctx.candidate_id)
            .collect();

        if candidate_issues.is_empty() {
            GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
        } else {
            let messages: Vec<String> = candidate_issues
                .iter()
                .map(|issue| issue.message.clone())
                .collect();
            GateRunResult::failed(
                self.artifact_kind(),
                format!("lint issues: {}", messages.join("; ")),
                timer.elapsed_ms(),
            )
        }
    }
}

/// Gate 9: End-to-end smoke test — trigger → script → output.
pub struct E2eSmokeRunner;

#[async_trait::async_trait]
impl GateRunner for E2eSmokeRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::E2eSmoke
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(cases) = ctx
            .authored
            .smoke
            .get("cases")
            .and_then(serde_json::Value::as_array)
        else {
            return GateRunResult::blocked(
                self.artifact_kind(),
                "smoke missing 'cases' array — gate blocked pending correct artifact format"
                    .to_owned(),
            );
        };

        if cases.is_empty() {
            return GateRunResult::failed(
                self.artifact_kind(),
                "smoke cases array is empty — gate requires at least one case".to_owned(),
                timer.elapsed_ms(),
            );
        }

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let expected = case
                .get("expected_output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            match run_script(&script_path, "", 60_000, &[]).await {
                Ok(stdout) if stdout == expected => {}
                Ok(stdout) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("smoke case {i}: expected {expected:?}, got {stdout:?}"),
                        timer.elapsed_ms(),
                    );
                }
                Err(e) => {
                    return GateRunResult::failed(
                        self.artifact_kind(),
                        format!("smoke case {i}: {e}"),
                        timer.elapsed_ms(),
                    );
                }
            }
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Gate 10: Validates filing rules path safety.
pub struct FilingRulesRunner;

#[async_trait::async_trait]
impl GateRunner for FilingRulesRunner {
    fn artifact_kind(&self) -> SkillArtifactKind {
        SkillArtifactKind::FilingRules
    }

    async fn run(&self, ctx: &GateRunContext<'_>) -> GateRunResult {
        let timer = GateTimer::start();
        let Some(files_to) = ctx
            .authored
            .filing_rules
            .get("files_to")
            .and_then(serde_json::Value::as_str)
        else {
            return GateRunResult::failed(
                self.artifact_kind(),
                "filing_rules missing 'files_to' field".to_owned(),
                timer.elapsed_ms(),
            );
        };

        let path = std::path::Path::new(files_to);
        if path.is_absolute() {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` must be relative"),
                timer.elapsed_ms(),
            );
        }
        if !files_to.ends_with('/') {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` must end with /"),
                timer.elapsed_ms(),
            );
        }
        if path.components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("files_to `{files_to}` contains unsafe path components"),
                timer.elapsed_ms(),
            );
        }

        GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
    }
}

/// Execute a script via subprocess with optional stdin and env vars.
///
/// Containment (round 5 hardening — LLM-authored scripts must not run with
/// host privileges):
/// - environment is cleared and an allowlist re-populated (PATH, HOME=tmp,
///   plus caller-provided keys);
/// - working directory is an isolated tempdir created per call;
/// - on timeout, the subprocess and its entire process group are killed and
///   reaped via `kill -KILL -<pgid>` (Unix) plus `kill_on_drop(true)` as a
///   belt-and-suspenders;
/// - the script bundle is not made writable here — gates that need to
///   mutate the bundle (e.g. installing fixtures) are the integration test
///   runner's responsibility, not the LLM author's.
///
/// This is a meaningful step toward sandboxing but is NOT a full container
/// boundary. A motivated authored script can still exfiltrate via DNS,
/// HTTP, or anything PATH-reachable. Full isolation (firejail/bubblewrap/
/// nsjail) is tracked separately.
async fn run_script(
    script_path: &Path,
    input: &str,
    timeout_ms: u64,
    env: &[(&str, &str)],
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::process::Command;

    // Sandbox-lite: per-call temp working directory so scripts cannot
    // mutate the vault or the bundle under test (the bundle is referenced
    // by absolute path via script_path; relative I/O goes to scratch).
    let scratch = match tempfile::Builder::new()
        .prefix("cairn-skillify-script-")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(e) => return Err(format!("scratch dir: {e}")),
    };

    // Canonicalize the script path BEFORE we change the subprocess's cwd to
    // the scratch directory. If the caller passed a relative vault root
    // (e.g. --vault . or relative CAIRN_VAULT), `script_path` is also
    // relative; without canonicalization, bash would look for the script
    // under scratch and every script gate would fail with "not found".
    let resolved_script = match script_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return Err(format!(
                "script path canonicalize: {e} (for {})",
                script_path.display()
            ));
        }
    };

    let mut cmd = Command::new("bash");
    cmd.arg(&resolved_script);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    // Environment isolation: clear the inherited env and re-populate only
    // an allowlist. Without this an LLM-authored script could read
    // AWS_*, GITHUB_TOKEN, OPENAI_API_KEY, etc. from the worker process.
    cmd.env_clear();
    // PATH is required to find bash itself on most distros; copy a minimal
    // safe value from the running process so /usr/bin/bash etc. resolve.
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    } else {
        cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    }
    cmd.env("HOME", scratch.path());
    cmd.env("TMPDIR", scratch.path());
    cmd.current_dir(scratch.path());

    // On Unix, put the script in its own process group (PGID == child PID)
    // so timeout kill can signal the whole group, catching descendants the
    // script may have spawned. `process_group(0)` is stable since Rust
    // 1.64 and avoids `unsafe` (`pre_exec` would require it).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.as_std_mut().process_group(0);
    }

    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    #[cfg(unix)]
    let child_pgid: Option<i32> = child.id().and_then(|id| i32::try_from(id).ok());
    #[cfg(not(unix))]
    let child_pgid: Option<i32> = None;
    let mut stdout = child.stdout.take().ok_or("missing stdout handle")?;
    let mut stderr = child.stderr.take().ok_or("missing stderr handle")?;
    let mut stdin_handle = child.stdin.take();
    let input_bytes = input.as_bytes().to_vec();

    let timeout = tokio::time::Duration::from_millis(timeout_ms);

    // Wrap stdin write + stdout/stderr drain + wait() in ONE timeout. The
    // earlier design wrote stdin BEFORE the timeout, which deadlocked if
    // the script never read its stdin AND the input exceeded the pipe
    // buffer (Round 3 finding). Driving stdin write concurrently with the
    // reads/wait inside the timeout window means a misbehaving script
    // hitting the deadline always reaches the kill path.
    let wait = async {
        let stdin_task = async move {
            if let Some(mut stdin) = stdin_handle.take() {
                // Best-effort; if the script never reads stdin this may
                // block forever, but the outer timeout will kill the
                // process group and abort us.
                let _ = stdin.write_all(&input_bytes).await;
                drop(stdin); // EOF
            }
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_stdin, r_out, r_err, status) = tokio::join!(
            stdin_task,
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait(),
        );
        r_out.map_err(|e| format!("stdout read: {e}"))?;
        r_err.map_err(|e| format!("stderr read: {e}"))?;
        let status = status.map_err(|e| format!("wait failed: {e}"))?;
        Ok::<_, String>((out, err, status))
    };

    match tokio::time::timeout(timeout, wait).await {
        Ok(Ok((out, err, status))) => {
            if status.success() {
                Ok(String::from_utf8_lossy(&out).to_string())
            } else {
                let stderr = String::from_utf8_lossy(&err);
                Err(format!("script exited {status}: {stderr}"))
            }
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            // Timeout: kill the entire process group on Unix so descendants
            // the script may have spawned are reaped too. Use the `kill`
            // utility with a negative PID to signal the whole group — this
            // avoids `unsafe` (the workspace forbids it) while still
            // providing the necessary cleanup. `start_kill` falls through
            // for the bash parent and on non-Unix.
            #[cfg(unix)]
            if let Some(pgid) = child_pgid {
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", &format!("-{pgid}")])
                    .status();
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err("script timed out".to_owned())
        }
    }
}
