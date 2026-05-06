//! Tier 3 — LLM pairwise dedup, gated on `LLMProvider`.

use serde_json::{Value, json};

use crate::contract::llm_provider::{CompletionOutput, CompletionRequest, LLMProvider, LlmError};
use crate::domain::graph::EntityNode;
use crate::pipeline::entity_resolve::{EntityResolutionError, Resolution};

/// JSON Schema sent to `LLMProvider::complete` for Tier-3 enforcement.
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub(super) fn dedup_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["same", "confidence", "reasoning"],
        "properties": {
            "same":       { "type": "boolean" },
            "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
            "reasoning":  { "type": "string", "maxLength": 512 }
        }
    })
}

/// Build the Tier-3 prompt verbatim per issue #187.
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub(super) fn dedup_prompt(candidate_name: &str, top_match_name: &str) -> String {
    format!(
        "Are these two entities the same real-world concept?\n  A: {candidate_name}\n  B: {top_match_name}\nRespond as JSON: {{ \"same\": <bool>, \"confidence\": <float 0..1>, \"reasoning\": <string> }}"
    )
}

/// Single Tier-3 LLM call. Returns:
///
/// - `Resolution::Merge(top_match.id)` when the model returns
///   `same: true` and `confidence >= min_confidence`.
/// - `Resolution::New` when the model returns `same: false`,
///   confidence below threshold, or `LlmError::NotConfigured` /
///   `CapabilityMissing` (the silent-skip contract).
/// - `EntityResolutionError::Llm` for any other `LlmError`.
/// - `EntityResolutionError::LlmInvalidResponse` when the payload
///   parsed as `Json` but missing/wrong-typed required fields
///   (defence-in-depth — should be unreachable when the provider
///   honours the schema arg).
#[allow(dead_code)] // Task 6 wires this into EntityResolver; suppress dead_code until then.
pub(super) async fn llm_dedup(
    provider: &dyn LLMProvider,
    candidate_name: &str,
    top_match: &EntityNode,
    min_confidence: f32,
) -> Result<Resolution, EntityResolutionError> {
    let req = CompletionRequest::builder()
        .prompt(dedup_prompt(candidate_name, &top_match.name))
        .schema(dedup_schema())
        .build();

    let out = match provider.complete(&req).await {
        Ok(o) => o,
        Err(LlmError::NotConfigured { .. } | LlmError::CapabilityMissing { .. }) => {
            return Ok(Resolution::New);
        }
        Err(other) => return Err(EntityResolutionError::Llm { source: other }),
    };

    let value = match out {
        CompletionOutput::Json(v) => v,
        CompletionOutput::Text(raw) => {
            return Err(EntityResolutionError::LlmInvalidResponse {
                detail: format!("expected JSON response (schema was provided), got Text: {raw}"),
            });
        }
    };

    let same = value.get("same").and_then(Value::as_bool).ok_or_else(|| {
        EntityResolutionError::LlmInvalidResponse {
            detail: "missing or non-boolean `same`".into(),
        }
    })?;
    #[allow(clippy::cast_possible_truncation)]
    // Schema guarantees confidence is in [0, 1]; precision loss on f64→f32 is bounded.
    let confidence = value
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|f| f as f32)
        .ok_or_else(|| EntityResolutionError::LlmInvalidResponse {
            detail: "missing or non-numeric `confidence`".into(),
        })?;

    if same && confidence >= min_confidence {
        Ok(Resolution::Merge(top_match.id.clone()))
    } else {
        Ok(Resolution::New)
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub impls return literal strings; the trait defines &str.
mod tests {
    use super::*;
    use crate::contract::llm_provider::LLMProviderCapabilities;
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::graph::EntityId;
    use async_trait::async_trait;

    fn caps() -> &'static LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }

    fn versions() -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    fn node(id: &str, name: &str) -> EntityNode {
        EntityNode {
            id: EntityId::from(id),
            name: name.to_owned(),
            name_norm: name.to_owned(),
            summary: None,
            created_at: 0,
            embedding_id: None,
        }
    }

    /// Stub LLM that returns a fixed JSON value.
    struct CannedJsonLlm(Value);

    #[async_trait]
    impl LLMProvider for CannedJsonLlm {
        fn name(&self) -> &str {
            "canned-json"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Ok(CompletionOutput::Json(self.0.clone()))
        }
    }

    /// Stub LLM that always returns `NotConfigured`.
    struct NotConfiguredLlm;

    #[async_trait]
    impl LLMProvider for NotConfiguredLlm {
        fn name(&self) -> &str {
            "not-configured"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Err(LlmError::NotConfigured {
                remediation: "test".into(),
            })
        }
    }

    /// Stub LLM that always returns `CapabilityMissing`.
    struct CapMissingLlm;

    #[async_trait]
    impl LLMProvider for CapMissingLlm {
        fn name(&self) -> &str {
            "cap-missing"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Err(LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            })
        }
    }

    /// Stub LLM that returns `ProviderUnreachable`.
    struct UnreachableLlm;

    #[async_trait]
    impl LLMProvider for UnreachableLlm {
        fn name(&self) -> &str {
            "unreachable"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Err(LlmError::ProviderUnreachable {
                detail: "test".into(),
            })
        }
    }

    /// Stub LLM that returns a Text payload despite a schema being supplied.
    struct TextDespiteSchemaLlm;

    #[async_trait]
    impl LLMProvider for TextDespiteSchemaLlm {
        fn name(&self) -> &str {
            "text-despite-schema"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Ok(CompletionOutput::Text("not json".into()))
        }
    }

    #[test]
    fn prompt_includes_both_names() {
        let p = dedup_prompt("AuthService", "auth_service");
        assert!(p.contains("A: AuthService"), "got: {p}");
        assert!(p.contains("B: auth_service"), "got: {p}");
    }

    #[test]
    fn schema_validates_well_formed_payload() {
        let schema = dedup_schema();
        let payload = json!({ "same": true, "confidence": 0.9, "reasoning": "same name" });
        let validator = jsonschema::validator_for(&schema)
            .expect("invariant: dedup_schema must be a valid JSON Schema");
        assert!(validator.validate(&payload).is_ok());
    }

    #[test]
    fn schema_rejects_missing_required() {
        let schema = dedup_schema();
        let payload = json!({ "same": true, "reasoning": "..." });
        let validator = jsonschema::validator_for(&schema)
            .expect("invariant: dedup_schema must be a valid JSON Schema");
        assert!(validator.validate(&payload).is_err());
    }

    #[tokio::test]
    async fn merges_when_same_true_and_above_threshold() {
        let provider = CannedJsonLlm(json!({
            "same": true,
            "confidence": 0.9,
            "reasoning": "stub"
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let r = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect("invariant: llm_dedup with canned same:true returns Ok");
        assert!(matches!(r, Resolution::Merge(id) if id.as_str() == "01HZE7JV5N0000000000000001"));
    }

    #[tokio::test]
    async fn declines_merge_when_same_true_below_threshold() {
        let provider = CannedJsonLlm(json!({
            "same": true,
            "confidence": 0.5,
            "reasoning": "stub"
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let r = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect("invariant: llm_dedup with canned response returns Ok");
        assert!(matches!(r, Resolution::New));
    }

    #[tokio::test]
    async fn declines_merge_when_same_false() {
        let provider = CannedJsonLlm(json!({
            "same": false,
            "confidence": 0.95,
            "reasoning": "stub"
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let r = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect("invariant: llm_dedup with canned response returns Ok");
        assert!(matches!(r, Resolution::New));
    }

    #[tokio::test]
    async fn silent_skip_on_not_configured() {
        let provider = NotConfiguredLlm;
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let r = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect("invariant: NotConfigured maps to Resolution::New, not Err");
        assert!(matches!(r, Resolution::New));
    }

    #[tokio::test]
    async fn silent_skip_on_capability_missing() {
        let provider = CapMissingLlm;
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let r = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect("invariant: CapabilityMissing maps to Resolution::New, not Err");
        assert!(matches!(r, Resolution::New));
    }

    #[tokio::test]
    async fn propagates_provider_unreachable() {
        let provider = UnreachableLlm;
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: ProviderUnreachable propagates as EntityResolutionError::Llm");
        assert!(matches!(err, EntityResolutionError::Llm { .. }));
    }

    #[tokio::test]
    async fn invalid_response_when_text_despite_schema() {
        let provider = TextDespiteSchemaLlm;
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: Text-when-schema-given surfaces as LlmInvalidResponse");
        assert!(matches!(
            err,
            EntityResolutionError::LlmInvalidResponse { .. }
        ));
    }
}
