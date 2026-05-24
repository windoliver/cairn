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
        let mut missing = Vec::new();

        if !md.contains("lane:") {
            missing.push("lane");
        }
        if !md.contains("triggers:") {
            missing.push("triggers");
        }
        if !md.contains("uses:") {
            missing.push("uses");
        }
        if !md.contains("files_to:") {
            missing.push("files_to");
        }

        if missing.is_empty() {
            GateRunResult::passed(self.artifact_kind(), timer.elapsed_ms())
        } else {
            GateRunResult::failed(
                self.artifact_kind(),
                format!(
                    "skill contract missing required fields: {}",
                    missing.join(", ")
                ),
                timer.elapsed_ms(),
            )
        }
    }
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

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case.get("input").and_then(serde_json::Value::as_str).unwrap_or("");
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

        let script_path = ctx
            .candidate_dir
            .join(format!("bundle/scripts/{}.sh", ctx.authored.slug));

        for (i, case) in cases.iter().enumerate() {
            let input = case.get("input").and_then(serde_json::Value::as_str).unwrap_or("");
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
                    if !v.get("pass").and_then(serde_json::Value::as_bool).unwrap_or(false) {
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
                                candidate_trigger,
                                existing_skill.skill_id,
                                existing_skill.lane
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

        let mut total = 0u32;
        let mut hits = 0u32;

        for intent_obj in intents {
            let intent = intent_obj
                .get("intent")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let expected_lane = intent_obj
                .get("expected_lane")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            total += 1;
            let matched = triggers
                .iter()
                .any(|t| intent.to_lowercase().contains(&t.to_lowercase()));
            if matched && expected_lane == ctx.authored.lane {
                hits += 1;
            }
        }

        if total == 0 {
            return GateRunResult::failed(
                self.artifact_kind(),
                "no intents to evaluate".to_owned(),
                timer.elapsed_ms(),
            );
        }

        let recall = f64::from(hits) / f64::from(total);
        if recall < 0.8 {
            return GateRunResult::failed(
                self.artifact_kind(),
                format!("recall {recall:.2} < 0.8 threshold ({hits}/{total} matched)"),
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
async fn run_script(
    script_path: &Path,
    input: &str,
    timeout_ms: u64,
    env: &[(&str, &str)],
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt as _;
    use tokio::process::Command;

    let mut cmd = Command::new("bash");
    cmd.arg(script_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("stdin write: {e}"))?;
    }

    let timeout = tokio::time::Duration::from_millis(timeout_ms);
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("script exited {}: {stderr}", output.status))
            }
        }
        Ok(Err(e)) => Err(format!("wait failed: {e}")),
        Err(_) => Err("script timed out".to_owned()),
    }
}
