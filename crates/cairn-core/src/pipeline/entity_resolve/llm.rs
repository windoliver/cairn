//! Tier 3 — LLM pairwise dedup, gated on `LLMProvider`.

use serde_json::{Value, json};

use crate::contract::llm_provider::{CompletionOutput, CompletionRequest, LLMProvider, LlmError};
use crate::domain::graph::EntityNode;
use crate::pipeline::entity_resolve::{EntityResolutionError, Resolution};

/// JSON Schema sent to `LLMProvider::complete` for Tier-3 enforcement.
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
            // Defence-in-depth: do not include raw payload in the error
            // detail — callers may log this at warn/error and the body
            // can echo `reasoning` / entity names, breaching brief §14.
            return Err(EntityResolutionError::LlmInvalidResponse {
                detail: format!(
                    "expected JSON response (schema was provided); got Text ({} bytes); raw payload elided to preserve §14 privacy invariant",
                    raw.len()
                ),
            });
        }
    };

    // Resolver-side schema enforcement. The provider is supposed to honour
    // `req.schema`, but a non-conforming adapter could return well-typed
    // JSON with out-of-range `confidence` or extra properties; an unchecked
    // payload would otherwise authorise a hard-to-undo entity merge.
    let validator = jsonschema::validator_for(&dedup_schema()).map_err(|e| {
        EntityResolutionError::LlmInvalidResponse {
            detail: format!("internal: dedup_schema is invalid JSON Schema: {e}"),
        }
    })?;
    if validator.validate(&value).is_err() {
        return Err(EntityResolutionError::LlmInvalidResponse {
            detail: "tier-3 response failed schema validation; payload elided to preserve §14 privacy invariant".to_owned(),
        });
    }

    // Schema-validated above; field reads here are belt-and-braces but
    // surface a useful error rather than panic if validator is mis-cached.
    let same = value.get("same").and_then(Value::as_bool).ok_or_else(|| {
        EntityResolutionError::LlmInvalidResponse {
            detail: "post-validation: missing or non-boolean `same`".into(),
        }
    })?;
    #[allow(clippy::cast_possible_truncation)]
    // Schema enforced confidence ∈ [0, 1]; f64→f32 precision loss is bounded.
    let confidence = value
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|f| f as f32)
        .ok_or_else(|| EntityResolutionError::LlmInvalidResponse {
            detail: "post-validation: missing or non-numeric `confidence`".into(),
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

    #[tokio::test]
    async fn rejects_confidence_above_one_even_if_provider_skipped_schema() {
        // Defence-in-depth: a non-conforming provider returns a payload
        // that bypasses the schema (e.g. `confidence: 100`). The
        // resolver must NOT merge — schema is re-validated locally.
        let provider = CannedJsonLlm(json!({
            "same": true,
            "confidence": 100.0,
            "reasoning": "bug — out of range"
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: out-of-range confidence must surface as LlmInvalidResponse");
        assert!(matches!(
            err,
            EntityResolutionError::LlmInvalidResponse { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_missing_reasoning_field() {
        // Schema requires `reasoning`; a payload without it must not
        // authorise a merge even if `same` and `confidence` are valid.
        let provider = CannedJsonLlm(json!({
            "same": true,
            "confidence": 0.95
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: missing required field must surface as LlmInvalidResponse");
        assert!(matches!(
            err,
            EntityResolutionError::LlmInvalidResponse { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_extra_properties() {
        // Schema sets additionalProperties: false; extra keys must
        // be rejected by the validator before reaching field reads.
        let provider = CannedJsonLlm(json!({
            "same": true,
            "confidence": 0.95,
            "reasoning": "ok",
            "evil_extra": "should be rejected"
        }));
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: extra properties must surface as LlmInvalidResponse");
        assert!(matches!(
            err,
            EntityResolutionError::LlmInvalidResponse { .. }
        ));
    }

    #[tokio::test]
    async fn invalid_response_detail_does_not_leak_raw_text() {
        // Privacy invariant (brief §14): when the provider returns Text
        // despite a schema, the LlmInvalidResponse detail must NOT
        // contain the raw payload (which can echo entity names or
        // reasoning). Verify the elision phrasing.
        let provider = TextDespiteSchemaLlm;
        let n = node("01HZE7JV5N0000000000000001", "AuthService");
        let err = llm_dedup(&provider, "auth_service", &n, 0.7)
            .await
            .expect_err("invariant: Text payload surfaces as LlmInvalidResponse");
        if let EntityResolutionError::LlmInvalidResponse { detail } = err {
            assert!(
                !detail.contains("not json"),
                "raw payload leaked into error detail: {detail}"
            );
            assert!(
                detail.contains("elided") || detail.contains("omitted"),
                "expected elision marker in detail: {detail}"
            );
        } else {
            panic!("expected LlmInvalidResponse");
        }
    }
}
