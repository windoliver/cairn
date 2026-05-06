//! Issue #187 AC — Tier 3 skips gracefully when `LLMProvider` returns
//! `NotConfigured` / `CapabilityMissing`.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::graph::{EntityId, EntityNode};
use cairn_core::pipeline::entity_resolve::{EntityResolver, Resolution, ResolverConfig};

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

struct NotConfiguredLlm;

#[async_trait]
impl LLMProvider for NotConfiguredLlm {
    fn name(&self) -> &'static str {
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

struct CapMissingLlm;

#[async_trait]
impl LLMProvider for CapMissingLlm {
    fn name(&self) -> &'static str {
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

fn node(id: &str, name_norm: &str) -> EntityNode {
    EntityNode {
        id: EntityId::from(id),
        name: name_norm.to_owned(),
        name_norm: name_norm.to_owned(),
        summary: None,
        created_at: 0,
        embedding_id: None,
    }
}

/// Tune the config so Tier 2 cannot satisfy the threshold (0.999) and
/// Tier 3 always fires (`low_band` 0.0). Forces the LLM call to occur,
/// which means the silent-skip path is exercised.
fn force_tier3() -> ResolverConfig {
    ResolverConfig {
        fuzzy_threshold: 0.999,
        llm_low_band: 0.0,
        llm_min_confidence: 0.7,
        ..ResolverConfig::default()
    }
}

#[tokio::test]
async fn graceful_skip_on_not_configured() {
    let llm = Arc::new(NotConfiguredLlm);
    let r =
        EntityResolver::new(force_tier3(), Some(llm)).expect("invariant: tuned config validates");
    let existing = vec![node("01HZE7JV5N0000000000000001", "auth service backend")];
    let res = r
        .resolve("auth service frontend", &existing)
        .await
        .expect("invariant: NotConfigured maps to Resolution::New, not Err");
    assert!(matches!(res, Resolution::New), "expected New, got {res:?}");
}

#[tokio::test]
async fn graceful_skip_on_capability_missing() {
    let llm = Arc::new(CapMissingLlm);
    let r =
        EntityResolver::new(force_tier3(), Some(llm)).expect("invariant: tuned config validates");
    let existing = vec![node("01HZE7JV5N0000000000000001", "auth service backend")];
    let res = r
        .resolve("auth service frontend", &existing)
        .await
        .expect("invariant: CapabilityMissing maps to Resolution::New, not Err");
    assert!(matches!(res, Resolution::New), "expected New, got {res:?}");
}
