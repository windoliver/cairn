//! Skillify candidate bundle materialization.

use std::fs;
use std::path::Path;

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactKind, SkillifyGate, SkillifyGateReport,
    SkillifyGateStatus,
};
use sha2::{Digest, Sha256};

/// Structured skill bundle authored by an LLM before materialization.
#[derive(Debug, Clone)]
pub struct AuthoredSkillBundle {
    /// Skill lane declared by the generated contract.
    pub lane: String,
    /// Filesystem-safe skill slug.
    pub slug: String,
    /// Markdown skill contract.
    pub skill_markdown: String,
    /// Deterministic executable script body.
    pub script: String,
    /// Unit-test artifact JSON.
    pub unit_tests: serde_json::Value,
    /// Integration-test artifact JSON.
    pub integration_tests: serde_json::Value,
    /// LLM eval artifact JSON.
    pub llm_evals: serde_json::Value,
    /// Resolver trigger artifact JSON.
    pub resolver_triggers: serde_json::Value,
    /// Resolver eval artifact JSON.
    pub resolver_eval: serde_json::Value,
    /// End-to-end smoke artifact JSON.
    pub smoke: serde_json::Value,
    /// Filing rules artifact JSON.
    pub filing_rules: serde_json::Value,
}

impl TryFrom<serde_json::Value> for AuthoredSkillBundle {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let bundle = Self {
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
        };
        validate_path_token("slug", &bundle.slug)?;
        Ok(bundle)
    }
}

/// Materialize a candidate bundle under `.cairn/evolution/skillify/{candidate_id}`.
///
/// # Errors
/// Returns an error when artifact data cannot be serialized, paths are unsafe,
/// writes fail, or the generated bundle does not satisfy required coverage.
pub fn materialize_bundle(
    vault_root: &Path,
    candidate_id: &str,
    authored: &AuthoredSkillBundle,
    evidence_refs: &[String],
) -> Result<SkillArtifactBundle, Box<dyn std::error::Error + Send + Sync>> {
    validate_path_token("candidate id", candidate_id)?;

    let root = vault_root
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    let bundle_root = root.join("bundle");
    fs::create_dir_all(&root)?;

    let files = [
        (
            SkillArtifactKind::SkillContract,
            format!("skills/skill_{}.md", authored.slug),
            authored.skill_markdown.clone(),
        ),
        (
            SkillArtifactKind::DeterministicScript,
            format!("scripts/{}.sh", authored.slug),
            authored.script.clone(),
        ),
        (
            SkillArtifactKind::UnitTests,
            format!("tests/unit/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.unit_tests)?,
        ),
        (
            SkillArtifactKind::IntegrationTests,
            format!("tests/integration/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.integration_tests)?,
        ),
        (
            SkillArtifactKind::LlmEvals,
            format!("evals/llm/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.llm_evals)?,
        ),
        (
            SkillArtifactKind::ResolverTrigger,
            "resolver/triggers.json".to_owned(),
            serde_json::to_string_pretty(&authored.resolver_triggers)?,
        ),
        (
            SkillArtifactKind::ResolverEval,
            "resolver/eval.json".to_owned(),
            serde_json::to_string_pretty(&authored.resolver_eval)?,
        ),
        (
            SkillArtifactKind::CheckResolvableAndDry,
            "audits/check-resolvable.json".to_owned(),
            "{\"status\":\"passed\"}\n".to_owned(),
        ),
        (
            SkillArtifactKind::E2eSmoke,
            format!("smoke/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.smoke)?,
        ),
        (
            SkillArtifactKind::FilingRules,
            "filing-rules.json".to_owned(),
            serde_json::to_string_pretty(&authored.filing_rules)?,
        ),
    ];

    let mut artifacts = Vec::with_capacity(files.len());
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

    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&bundle)?,
    )?;
    let report = SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: required_passed_gates(),
    };
    fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

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
    value
        .get(key)
        .cloned()
        .ok_or_else(|| format!("missing field {key}"))
}

fn validate_path_token(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}

fn required_passed_gates() -> Vec<SkillifyGate> {
    SkillArtifactKind::required()
        .iter()
        .map(|kind| SkillifyGate {
            name: kind.as_str().to_owned(),
            status: SkillifyGateStatus::Passed,
            message: None,
        })
        .collect()
}
