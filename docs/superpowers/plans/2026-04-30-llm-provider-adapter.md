# LLMProvider Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `complete()` method to `LLMProvider` in `cairn-core`, create the `cairn-llm-openai-compat` adapter crate over `async-openai`, and enforce JSON-schema output validation so invalid model output never reaches the store.

**Architecture:** New types (`CompletionRequest`, `CompletionOutput`, `LlmError`) land in `cairn-core` (zero I/O). A new `cairn-llm-openai-compat` crate implements the trait via `async-openai`, converting `LlmConfig` to an `OpenAIConfig`, calling the chat completions endpoint, and running `jsonschema::validate` before returning `CompletionOutput::Json`. Missing provider config fails closed via `build_llm_provider`.

**Tech Stack:** Rust 1.95 / edition 2024, `async-openai 0.36`, `jsonschema` (already workspace), `bon` (already workspace), `wiremock 0.6` (dev), `async_trait`, `thiserror`, `insta`.

---

## File Map

### Modified
| File | What changes |
|---|---|
| `Cargo.toml` | add `async-openai` + `wiremock` to `[workspace.dependencies]`; add `cairn-llm-openai-compat` intra-workspace dep entry |
| `crates/cairn-core/Cargo.toml` | add `bon` to `[dependencies]` |
| `crates/cairn-core/src/contract/llm_provider.rs` | add `LlmError`, `CompletionRequest`, `CompletionOutput`; add `complete()` to trait; update `StubLlm` |
| `crates/cairn-core/src/contract/mod.rs` | re-export `LlmError`, `CompletionRequest`, `CompletionOutput` |

### Created
| File | Purpose |
|---|---|
| `crates/cairn-llm-openai-compat/Cargo.toml` | crate manifest |
| `crates/cairn-llm-openai-compat/src/lib.rs` | `pub use`; `build_llm_provider` |
| `crates/cairn-llm-openai-compat/src/provider.rs` | `OpenAiCompatProvider` struct + `LLMProvider` impl |
| `crates/cairn-llm-openai-compat/src/config.rs` | `LlmConfig → OpenAIConfig` conversion |
| `crates/cairn-llm-openai-compat/src/error.rs` | `async-openai` error → `LlmError` mapping |
| `crates/cairn-llm-openai-compat/tests/mock_provider.rs` | wiremock happy + error path tests |
| `crates/cairn-llm-openai-compat/tests/missing_config.rs` | `build_llm_provider` config tests |

---

## Task 1: Add `LlmError` to `cairn-core`

**Files:**
- Modify: `crates/cairn-core/src/contract/llm_provider.rs`

- [ ] **Step 1.1 — Write failing display tests**

Add inside the existing `#[cfg(test)] mod tests` block at the bottom of
`crates/cairn-core/src/contract/llm_provider.rs`:

```rust
    #[test]
    fn lm_error_not_configured_display() {
        let e = LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".into(),
        };
        assert_eq!(
            e.to_string(),
            "llm.not_configured: cairn config set llm.provider ollama"
        );
    }

    #[test]
    fn lm_error_invalid_json_display() {
        let e = LlmError::InvalidJsonOutput {
            detail: "missing field `kind`".into(),
            raw: "{}".into(),
        };
        assert!(e.to_string().contains("llm.invalid_json_output"));
    }
```

- [ ] **Step 1.2 — Run to confirm compile failure**

```bash
cargo test -p cairn-core contract::llm_provider 2>&1 | head -20
```

Expected: error `cannot find type LlmError`.

- [ ] **Step 1.3 — Add `LlmError` to `llm_provider.rs`**

Add _before_ the `LLMProviderCapabilities` struct (after the existing `use` lines):

```rust
/// Errors returned by [`LLMProvider::complete`] (ADR 0001 error codes).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// No provider configured; LLM-dependent verbs fail closed (exit 78).
    #[error("llm.not_configured: {remediation}")]
    NotConfigured { remediation: String },
    /// Provider host/port refused connection or DNS failed or timed out.
    #[error("llm.provider_unreachable: {detail}")]
    ProviderUnreachable { detail: String },
    /// Provider returned HTTP 401 or 403.
    #[error("llm.auth_denied")]
    AuthDenied,
    /// Provider is reachable but lacks a required capability (e.g. json_mode).
    #[error("llm.capability_missing: {capability}")]
    CapabilityMissing { capability: String },
    /// Provider returned output that failed JSON parse or schema validation.
    #[error("llm.invalid_json_output: {detail}")]
    InvalidJsonOutput { detail: String, raw: String },
    /// Completion exceeded the configured token or time budget.
    #[error("llm.budget_exceeded")]
    BudgetExceeded,
}
```

- [ ] **Step 1.4 — Run tests**

```bash
cargo test -p cairn-core contract::llm_provider 2>&1 | tail -10
```

Expected: `lm_error_not_configured_display ... ok`, `lm_error_invalid_json_display ... ok`.

- [ ] **Step 1.5 — Commit**

```bash
git add crates/cairn-core/src/contract/llm_provider.rs
git commit -m "feat(core): add LlmError enum to LLMProvider contract (brief §4.0)"
```

---

## Task 2: Add `CompletionRequest` and `CompletionOutput`

**Files:**
- Modify: `crates/cairn-core/Cargo.toml`
- Modify: `crates/cairn-core/src/contract/llm_provider.rs`

- [ ] **Step 2.1 — Write failing round-trip tests**

Add to the `#[cfg(test)] mod tests` block in `llm_provider.rs`:

```rust
    #[test]
    fn completion_request_builder_minimal() {
        let req = CompletionRequest::builder()
            .prompt("hello".to_string())
            .build();
        assert_eq!(req.prompt, "hello");
        assert!(req.schema.is_none());
        assert!(req.model.is_none());
        assert!(req.budget.is_none());
    }

    #[test]
    fn completion_request_builder_with_schema() {
        let schema = serde_json::json!({ "type": "object" });
        let req = CompletionRequest::builder()
            .prompt("hello".to_string())
            .schema(schema.clone())
            .build();
        assert_eq!(req.schema.as_ref().unwrap(), &schema);
    }

    #[test]
    fn completion_output_is_text() {
        let out = CompletionOutput::Text("hi".into());
        assert!(matches!(out, CompletionOutput::Text(_)));
    }

    #[test]
    fn completion_output_is_json() {
        let out = CompletionOutput::Json(serde_json::json!({"k": "v"}));
        assert!(matches!(out, CompletionOutput::Json(_)));
    }
```

- [ ] **Step 2.2 — Run to confirm failure**

```bash
cargo test -p cairn-core contract::llm_provider 2>&1 | head -10
```

Expected: error `cannot find type CompletionRequest`.

- [ ] **Step 2.3 — Add `bon` to `cairn-core` Cargo.toml**

In `crates/cairn-core/Cargo.toml`, add to `[dependencies]`:

```toml
bon = { workspace = true }
```

- [ ] **Step 2.4 — Add types to `llm_provider.rs`**

Add _after_ `LlmError` and _before_ `LLMProviderCapabilities`:

```rust
use crate::config::ExtractBudget;

/// Input to [`LLMProvider::complete`].
#[derive(Debug, Clone, bon::Builder)]
pub struct CompletionRequest {
    /// The user/system prompt to send to the model.
    pub prompt: String,
    /// Optional JSON Schema. When `Some`, triggers JSON-mode enforcement:
    /// the adapter sends `response_format: json_schema` and validates the
    /// returned value against this schema before returning.
    #[builder(default)]
    pub schema: Option<serde_json::Value>,
    /// Override the model configured in `LlmConfig`. `None` uses the default.
    #[builder(default)]
    pub model: Option<String>,
    /// Token and wall-clock budget. `None` means unlimited.
    #[builder(default)]
    pub budget: Option<ExtractBudget>,
}

/// Output from [`LLMProvider::complete`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CompletionOutput {
    /// Free-form text response (no schema was requested).
    Text(String),
    /// Validated JSON response (schema was provided and output matched).
    Json(serde_json::Value),
}
```

- [ ] **Step 2.5 — Run tests**

```bash
cargo test -p cairn-core contract::llm_provider 2>&1 | tail -15
```

Expected: all four new tests pass, existing tests still pass.

- [ ] **Step 2.6 — Commit**

```bash
git add crates/cairn-core/Cargo.toml crates/cairn-core/src/contract/llm_provider.rs
git commit -m "feat(core): add CompletionRequest, CompletionOutput types (brief §4.0)"
```

---

## Task 3: Add `complete()` to `LLMProvider` trait + update `StubLlm`

**Files:**
- Modify: `crates/cairn-core/src/contract/llm_provider.rs`

- [ ] **Step 3.1 — Write failing trait-dispatch test**

Add to `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn stub_complete_returns_text() {
        let provider: Box<dyn LLMProvider> = Box::new(StubLlm);
        let req = CompletionRequest::builder()
            .prompt("hello".to_string())
            .build();
        let out = provider.complete(&req).await.unwrap();
        assert!(matches!(out, CompletionOutput::Text(_)));
    }
```

- [ ] **Step 3.2 — Run to confirm failure**

```bash
cargo test -p cairn-core stub_complete_returns_text 2>&1 | head -15
```

Expected: error `method complete not found` or trait incomplete.

- [ ] **Step 3.3 — Add `complete()` to the trait**

In `llm_provider.rs`, add to the `LLMProvider` trait (after `supported_contract_versions`):

```rust
    /// Single LLM completion call.
    ///
    /// When `req.schema` is `Some`, the adapter MUST enforce JSON-schema
    /// validation before returning `CompletionOutput::Json`. Invalid output
    /// returns `LlmError::InvalidJsonOutput` — never reaches the store.
    async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> Result<CompletionOutput, LlmError>;
```

- [ ] **Step 3.4 — Update `StubLlm` to implement `complete()`**

In the `#[cfg(test)] mod tests` block, inside the `impl LLMProvider for StubLlm`:

```rust
    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Text("stub".into()))
    }
```

- [ ] **Step 3.5 — Run tests**

```bash
cargo test -p cairn-core 2>&1 | tail -15
```

Expected: all tests pass including `stub_complete_returns_text`.

- [ ] **Step 3.6 — Update re-exports in `contract/mod.rs`**

Change the existing line:
```rust
pub use llm_provider::{LLMProvider, LLMProviderCapabilities, LLMProviderPlugin};
```
to:
```rust
pub use llm_provider::{
    CompletionOutput, CompletionRequest, LlmError, LLMProvider, LLMProviderCapabilities,
    LLMProviderPlugin,
};
```

- [ ] **Step 3.7 — Run full cairn-core tests**

```bash
cargo test -p cairn-core 2>&1 | tail -5
```

Expected: `test result: ok`.

- [ ] **Step 3.8 — Commit**

```bash
git add crates/cairn-core/src/contract/llm_provider.rs crates/cairn-core/src/contract/mod.rs
git commit -m "feat(core): add complete() to LLMProvider trait (brief §4.0, issue #144)"
```

---

## Task 4: Add workspace deps + scaffold `cairn-llm-openai-compat`

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cairn-llm-openai-compat/Cargo.toml`
- Create: `crates/cairn-llm-openai-compat/src/lib.rs`

- [ ] **Step 4.1 — Add `async-openai` and `wiremock` to workspace deps**

In the root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
async-openai = { version = "0.36", default-features = false, features = ["rustls"] }
wiremock = "0.6"
```

Also add the intra-workspace dep entry:

```toml
cairn-llm-openai-compat = { path = "crates/cairn-llm-openai-compat", version = "0.0.1" }
```

- [ ] **Step 4.2 — Create `crates/cairn-llm-openai-compat/Cargo.toml`**

```toml
[package]
name = "cairn-llm-openai-compat"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "OpenAI-compatible LLMProvider adapter for Cairn (ADR 0001)."

[dependencies]
cairn-core   = { workspace = true }
async-openai = { workspace = true }
jsonschema   = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true }
tracing      = { workspace = true }
thiserror    = { workspace = true }
async-trait  = { workspace = true }

[dev-dependencies]
wiremock            = { workspace = true }
tokio               = { workspace = true, features = ["rt", "macros"] }
insta               = { workspace = true }
cairn-test-fixtures = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 4.3 — Create `crates/cairn-llm-openai-compat/src/lib.rs`**

```rust
//! OpenAI-compatible [`LLMProvider`] adapter (ADR 0001, brief §4.0).
//!
//! Entry point: [`build_llm_provider`].

#![doc = include_str!("../../../README.md")]

mod config;
mod error;
mod provider;

pub use provider::OpenAiCompatProvider;

use cairn_core::{
    config::LlmConfig,
    contract::{LLMProvider, LlmError},
};

/// Construct a boxed [`LLMProvider`] from the resolved config.
///
/// Returns [`LlmError::NotConfigured`] immediately (no network call) when
/// `config.provider` is `None`.
pub fn build_llm_provider(config: &LlmConfig) -> Result<Box<dyn LLMProvider>, LlmError> {
    if config.provider.is_none() {
        return Err(LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".into(),
        });
    }
    let provider = provider::OpenAiCompatProvider::from_config(config)?;
    Ok(Box::new(provider))
}
```

- [ ] **Step 4.4 — Verify the crate compiles**

```bash
cargo check -p cairn-llm-openai-compat 2>&1 | tail -10
```

Expected: errors only about missing modules `config`, `error`, `provider` (not yet created). If you see other errors, fix before continuing.

- [ ] **Step 4.5 — Commit scaffold**

```bash
git add Cargo.toml crates/cairn-llm-openai-compat/
git commit -m "chore: scaffold cairn-llm-openai-compat crate (ADR 0001)"
```

---

## Task 5: Config conversion (`config.rs`) + missing-provider tests

**Files:**
- Create: `crates/cairn-llm-openai-compat/src/config.rs`
- Create: `crates/cairn-llm-openai-compat/tests/missing_config.rs`

- [ ] **Step 5.1 — Write missing-provider tests**

Create `crates/cairn-llm-openai-compat/tests/missing_config.rs`:

```rust
use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::LlmError,
};
use cairn_llm_openai_compat::build_llm_provider;

#[test]
fn no_provider_returns_not_configured() {
    let config = LlmConfig::default(); // provider is None
    let err = build_llm_provider(&config).unwrap_err();
    assert!(
        matches!(err, LlmError::NotConfigured { .. }),
        "expected NotConfigured, got {err}"
    );
    assert!(err.to_string().contains("llm.not_configured"));
}

#[test]
fn provider_set_no_base_url_constructs_ok() {
    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        model: Some("gpt-4o-mini".into()),
        ..LlmConfig::default()
    };
    // No base_url → defaults to https://api.openai.com/v1
    // Does not make a network call — just constructs the client.
    assert!(build_llm_provider(&config).is_ok());
}

#[test]
fn provider_set_ollama_base_url_constructs_ok() {
    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some("http://localhost:11434/v1".into()),
        model: Some("llama3.2".into()),
        ..LlmConfig::default()
    };
    assert!(build_llm_provider(&config).is_ok());
}
```

- [ ] **Step 5.2 — Run to confirm failure**

```bash
cargo test -p cairn-llm-openai-compat --test missing_config 2>&1 | head -15
```

Expected: compile errors (modules not yet created).

- [ ] **Step 5.3 — Create `src/config.rs`**

```rust
//! Convert [`LlmConfig`] into an [`async_openai::config::OpenAIConfig`].

use async_openai::config::OpenAIConfig;
use cairn_core::config::LlmConfig;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Build an `OpenAIConfig` from Cairn's `LlmConfig`.
///
/// `base_url` defaults to `https://api.openai.com/v1` when not set.
/// `api_key` defaults to `"cairn"` (placeholder) when not set —
/// some providers (Ollama) require a non-empty key but ignore its value.
pub(crate) fn to_openai_config(config: &LlmConfig) -> OpenAIConfig {
    let base_url = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_BASE_URL)
        .to_owned();
    let api_key = config.api_key.clone().unwrap_or_else(|| "cairn".into());
    OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::config::LlmConfig;

    #[test]
    fn default_base_url_when_none() {
        let cfg = to_openai_config(&LlmConfig::default());
        assert_eq!(cfg.api_base(), DEFAULT_BASE_URL);
    }

    #[test]
    fn custom_base_url_propagated() {
        let config = LlmConfig {
            base_url: Some("http://localhost:11434/v1".into()),
            ..LlmConfig::default()
        };
        let cfg = to_openai_config(&config);
        assert_eq!(cfg.api_base(), "http://localhost:11434/v1");
    }
}
```

- [ ] **Step 5.4 — Create stub `src/error.rs` and `src/provider.rs` to unblock compilation**

Create `crates/cairn-llm-openai-compat/src/error.rs`:

```rust
//! Map `async-openai` errors to [`LlmError`].

use cairn_core::contract::LlmError;

/// Convert an `async_openai::error::OpenAIError` to [`LlmError`].
pub(crate) fn map_openai_error(e: async_openai::error::OpenAIError) -> LlmError {
    use async_openai::error::OpenAIError;
    match e {
        OpenAIError::ApiError(ref api_err) => {
            let status = api_err.status;
            match status {
                Some(s) if s == 401 || s == 403 => LlmError::AuthDenied,
                Some(_) => LlmError::CapabilityMissing {
                    capability: format!("http_{}", api_err.message),
                },
                None => LlmError::ProviderUnreachable {
                    detail: api_err.message.clone(),
                },
            }
        }
        OpenAIError::Reqwest(ref re) => {
            if re.is_connect() || re.is_timeout() {
                LlmError::ProviderUnreachable {
                    detail: re.to_string(),
                }
            } else {
                LlmError::CapabilityMissing {
                    capability: re.to_string(),
                }
            }
        }
        other => LlmError::ProviderUnreachable {
            detail: other.to_string(),
        },
    }
}
```

Create `crates/cairn-llm-openai-compat/src/provider.rs`:

```rust
//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over `async-openai`.

use async_openai::{Client, config::OpenAIConfig};
use cairn_core::{
    config::LlmConfig,
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities,
        LLMProviderPlugin, LlmError,
    },
    contract::version::{ContractVersion, VersionRange},
};

use crate::config::to_openai_config;

/// OpenAI-compatible [`LLMProvider`] adapter.
pub struct OpenAiCompatProvider {
    client: Client<OpenAIConfig>,
    model: String,
    capabilities: LLMProviderCapabilities,
}

impl OpenAiCompatProvider {
    /// Construct from a resolved [`LlmConfig`].
    pub fn from_config(config: &LlmConfig) -> Result<Self, LlmError> {
        let openai_cfg = to_openai_config(config);
        let model = config
            .model
            .clone()
            .unwrap_or_else(|| "gpt-4o-mini".into());
        Ok(Self {
            client: Client::with_config(openai_cfg),
            model,
            capabilities: LLMProviderCapabilities {
                json_mode: true, // assumed true at P0; cairn status probe out of scope
                streaming: false,
                tool_calls: false,
            },
        })
    }

    /// Test-only constructor that accepts explicit capabilities.
    #[cfg(test)]
    pub(crate) fn with_capabilities(
        base_url: &str,
        model: &str,
        capabilities: LLMProviderCapabilities,
    ) -> Self {
        let openai_cfg = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key("test-key");
        Self {
            client: Client::with_config(openai_cfg),
            model: model.into(),
            capabilities,
        }
    }
}

impl LLMProviderPlugin for OpenAiCompatProvider {
    const NAME: &'static str = "openai-compatible";
    const SUPPORTED_VERSIONS: VersionRange = VersionRange::new(
        ContractVersion::new(0, 1, 0),
        ContractVersion::new(0, 2, 0),
    );
}

#[async_trait::async_trait]
impl LLMProvider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn capabilities(&self) -> &LLMProviderCapabilities {
        &self.capabilities
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    async fn complete(
        &self,
        _req: &CompletionRequest,
    ) -> Result<CompletionOutput, LlmError> {
        // Full implementation in Tasks 8–10.
        Err(LlmError::CapabilityMissing {
            capability: "not yet implemented".into(),
        })
    }
}
```

- [ ] **Step 5.5 — Run missing_config tests**

```bash
cargo test -p cairn-llm-openai-compat --test missing_config 2>&1 | tail -10
```

Expected: all three tests pass.

- [ ] **Step 5.6 — Commit**

```bash
git add crates/cairn-llm-openai-compat/src/
git add crates/cairn-llm-openai-compat/tests/missing_config.rs
git commit -m "feat(llm): build_llm_provider, config conversion, missing-provider tests"
```

---

## Task 6: `complete()` — text path

**Files:**
- Modify: `crates/cairn-llm-openai-compat/src/provider.rs`
- Create: `crates/cairn-llm-openai-compat/tests/mock_provider.rs`

- [ ] **Step 6.1 — Write failing text-path test**

Create `crates/cairn-llm-openai-compat/tests/mock_provider.rs`:

```rust
use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::{CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities},
};
use cairn_llm_openai_compat::{build_llm_provider, OpenAiCompatProvider};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Minimal valid OpenAI chat completion response body.
fn chat_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
    })
}

#[tokio::test]
async fn text_completion_no_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("Hello!")))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let req = CompletionRequest::builder()
        .prompt("Say hello".to_string())
        .build();
    let out = provider.complete(&req).await.unwrap();
    assert!(matches!(out, CompletionOutput::Text(ref s) if s == "Hello!"));
}
```

- [ ] **Step 6.2 — Run to confirm failure**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider text_completion_no_schema 2>&1 | tail -10
```

Expected: test fails (stub returns `CapabilityMissing`).

- [ ] **Step 6.3 — Implement text path in `provider.rs`**

Replace the stub `complete()` body with:

```rust
    async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> Result<CompletionOutput, LlmError> {
        use async_openai::types::{
            ChatCompletionRequestUserMessageArgs,
            CreateChatCompletionRequestArgs,
        };

        // Guard: json_mode required when schema is provided.
        if req.schema.is_some() && !self.capabilities.json_mode {
            return Err(LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            });
        }

        let model = req.model.as_deref().unwrap_or(&self.model);

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder
            .model(model)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content(req.prompt.as_str())
                .build()
                .map_err(|e| LlmError::ProviderUnreachable { detail: e.to_string() })?
                .into()]);

        if let Some(budget) = &req.budget {
            if let Some(max_tokens) = budget.max_tokens {
                builder.max_tokens(max_tokens);
            }
        }

        // JSON schema mode — full path implemented in Task 7.
        if req.schema.is_some() {
            todo!("json schema path — implemented in Task 7")
        }

        let request = builder
            .build()
            .map_err(|e| LlmError::ProviderUnreachable { detail: e.to_string() })?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(crate::error::map_openai_error)?;

        let content = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        Ok(CompletionOutput::Text(content))
    }
```

- [ ] **Step 6.4 — Run text-path test**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider text_completion_no_schema 2>&1 | tail -5
```

Expected: `text_completion_no_schema ... ok`.

- [ ] **Step 6.5 — Commit**

```bash
git add crates/cairn-llm-openai-compat/src/provider.rs \
        crates/cairn-llm-openai-compat/tests/mock_provider.rs
git commit -m "feat(llm): complete() text path with wiremock test"
```

---

## Task 7: `complete()` — JSON schema path

**Files:**
- Modify: `crates/cairn-llm-openai-compat/src/provider.rs`
- Modify: `crates/cairn-llm-openai-compat/tests/mock_provider.rs`

- [ ] **Step 7.1 — Write failing JSON schema tests**

Append to `tests/mock_provider.rs`:

```rust
#[tokio::test]
async fn json_completion_schema_match() {
    let server = MockServer::start().await;
    let body_content = r#"{"kind":"feedback","confidence":0.9}"#;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(body_content)))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "confidence"],
        "properties": {
            "kind": { "type": "string" },
            "confidence": { "type": "number" }
        }
    });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let out = provider.complete(&req).await.unwrap();
    assert!(
        matches!(out, CompletionOutput::Json(ref v) if v["kind"] == "feedback"),
        "expected Json with kind=feedback, got {out:?}"
    );
}

#[tokio::test]
async fn json_completion_schema_mismatch() {
    let server = MockServer::start().await;
    // Returns JSON that is missing required "confidence" field.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"kind":"feedback"}"#)),
        )
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "confidence"],
        "properties": {
            "kind": { "type": "string" },
            "confidence": { "type": "number" }
        }
    });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::InvalidJsonOutput { .. }),
        "expected InvalidJsonOutput, got {err:?}"
    );
}

#[tokio::test]
async fn json_completion_unparseable() {
    let server = MockServer::start().await;
    // Returns text that is not valid JSON at all.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("not json {")))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({ "type": "object" });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidJsonOutput { .. }));
}
```

- [ ] **Step 7.2 — Run to confirm failures**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider 2>&1 | grep -E "FAILED|ok|error"
```

Expected: `json_completion_*` tests fail (hit `todo!`).

- [ ] **Step 7.3 — Implement JSON schema path in `provider.rs`**

Replace `todo!("json schema path — implemented in Task 7")` with the JSON branch. The full updated `complete()` method:

```rust
    async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> Result<CompletionOutput, LlmError> {
        use async_openai::types::{
            ChatCompletionRequestUserMessageArgs,
            CreateChatCompletionRequestArgs,
            ResponseFormat,
        };

        if req.schema.is_some() && !self.capabilities.json_mode {
            return Err(LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            });
        }

        let model = req.model.as_deref().unwrap_or(&self.model);

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder
            .model(model)
            .messages([ChatCompletionRequestUserMessageArgs::default()
                .content(req.prompt.as_str())
                .build()
                .map_err(|e| LlmError::ProviderUnreachable { detail: e.to_string() })?
                .into()]);

        if let Some(budget) = &req.budget {
            if let Some(max_tokens) = budget.max_tokens {
                builder.max_tokens(max_tokens);
            }
        }

        if req.schema.is_some() {
            // Request JSON object mode — all OpenAI-compat endpoints that
            // advertise json_mode support at minimum ResponseFormat::JsonObject.
            builder.response_format(ResponseFormat::JsonObject);
        }

        let request = builder
            .build()
            .map_err(|e| LlmError::ProviderUnreachable { detail: e.to_string() })?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(crate::error::map_openai_error)?;

        let content = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        if let Some(schema) = &req.schema {
            let value: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| LlmError::InvalidJsonOutput {
                    detail: e.to_string(),
                    raw: content.clone(),
                })?;

            let compiled = jsonschema::validator_for(schema).map_err(|e| {
                LlmError::InvalidJsonOutput {
                    detail: format!("invalid schema: {e}"),
                    raw: content.clone(),
                }
            })?;

            if let Err(mut errors) = compiled.validate(&value) {
                let detail = errors
                    .next()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "schema validation failed".into());
                return Err(LlmError::InvalidJsonOutput { detail, raw: content });
            }

            return Ok(CompletionOutput::Json(value));
        }

        Ok(CompletionOutput::Text(content))
    }
```

- [ ] **Step 7.4 — Run JSON schema tests**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider 2>&1 | tail -10
```

Expected: all four tests pass (`text_completion_no_schema`, `json_completion_schema_match`, `json_completion_schema_mismatch`, `json_completion_unparseable`).

- [ ] **Step 7.5 — Commit**

```bash
git add crates/cairn-llm-openai-compat/src/provider.rs \
        crates/cairn-llm-openai-compat/tests/mock_provider.rs
git commit -m "feat(llm): complete() JSON schema path with validation tests"
```

---

## Task 8: `complete()` — error paths (HTTP 401, unreachable, capability_missing)

**Files:**
- Modify: `crates/cairn-llm-openai-compat/tests/mock_provider.rs`
- Modify: `crates/cairn-llm-openai-compat/src/error.rs`

- [ ] **Step 8.1 — Write failing error-path tests**

Append to `tests/mock_provider.rs`:

```rust
use cairn_core::contract::LlmError;

#[tokio::test]
async fn endpoint_returns_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("bad-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let req = CompletionRequest::builder().prompt("hi".to_string()).build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::AuthDenied),
        "expected AuthDenied, got {err:?}"
    );
}

#[tokio::test]
async fn json_mode_not_supported_no_http_call() {
    // Build a provider with json_mode=false, no server needed.
    let provider = OpenAiCompatProvider::with_capabilities(
        "http://127.0.0.1:1", // unreachable — should never be called
        "any-model",
        LLMProviderCapabilities {
            json_mode: false,
            streaming: false,
            tool_calls: false,
        },
    );
    let schema = serde_json::json!({ "type": "object" });
    let req = CompletionRequest::builder()
        .prompt("hi".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::CapabilityMissing { ref capability } if capability == "json_mode"),
        "expected CapabilityMissing(json_mode), got {err:?}"
    );
}
```

- [ ] **Step 8.2 — Run to confirm failures**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider endpoint_returns_401 2>&1 | tail -10
cargo test -p cairn-llm-openai-compat --test mock_provider json_mode_not_supported 2>&1 | tail -10
```

Expected: `endpoint_returns_401` fails (error mapping may not yet match); `json_mode_not_supported_no_http_call` may already pass (guard already in `complete()`). Note which fails.

- [ ] **Step 8.3 — Fix `error.rs` if 401 mapping is wrong**

Check the actual `async_openai::error::OpenAIError` variants for your version with:

```bash
cargo doc -p async-openai --open 2>/dev/null || cargo doc -p async-openai 2>&1 | tail -5
```

If the `ApiError` variant doesn't expose `.status` as an `Option<u16>`, update `map_openai_error` to match the actual API. A safe fallback that works across versions:

```rust
pub(crate) fn map_openai_error(e: async_openai::error::OpenAIError) -> LlmError {
    let msg = e.to_string();
    if msg.contains("401") || msg.contains("403") || msg.contains("Unauthorized") || msg.contains("Forbidden") {
        return LlmError::AuthDenied;
    }
    if msg.contains("connect") || msg.contains("timeout") || msg.contains("Connection") {
        return LlmError::ProviderUnreachable { detail: msg };
    }
    LlmError::ProviderUnreachable { detail: msg }
}
```

Adapt to match the actual error type structure you see in the docs.

- [ ] **Step 8.4 — Run all mock_provider tests**

```bash
cargo test -p cairn-llm-openai-compat --test mock_provider 2>&1 | tail -15
```

Expected: all tests pass.

- [ ] **Step 8.5 — Commit**

```bash
git add crates/cairn-llm-openai-compat/src/error.rs \
        crates/cairn-llm-openai-compat/tests/mock_provider.rs
git commit -m "feat(llm): error path tests and mapping (401, unreachable, capability_missing)"
```

---

## Task 9: `cairn-core` JSON validation unit tests + snapshot tests

**Files:**
- Modify: `crates/cairn-core/src/contract/llm_provider.rs`
- Modify: `crates/cairn-llm-openai-compat/src/provider.rs`

- [ ] **Step 9.1 — Add pure validation unit tests to `cairn-core`**

Add to the `#[cfg(test)] mod tests` block in `crates/cairn-core/src/contract/llm_provider.rs`:

```rust
    // These tests exercise the jsonschema crate directly to verify schema
    // enforcement logic is sound before the adapter layer is involved.
    // cairn-core does not depend on jsonschema at runtime — these are dev-dep tests.

    #[test]
    fn valid_object_passes_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["kind"],
            "properties": { "kind": { "type": "string" } }
        });
        let value = serde_json::json!({ "kind": "feedback" });
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&value).is_ok());
    }

    #[test]
    fn missing_required_field_fails_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["kind", "confidence"],
            "properties": {
                "kind": { "type": "string" },
                "confidence": { "type": "number" }
            }
        });
        let value = serde_json::json!({ "kind": "feedback" }); // missing "confidence"
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&value).is_err());
    }

    #[test]
    fn wrong_type_fails_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "confidence": { "type": "number" } }
        });
        let value = serde_json::json!({ "confidence": "not-a-number" });
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&value).is_err());
    }
```

- [ ] **Step 9.2 — Add `jsonschema` to `cairn-core` dev-deps**

In `crates/cairn-core/Cargo.toml`, add to `[dev-dependencies]`:

```toml
jsonschema = { workspace = true }
```

- [ ] **Step 9.3 — Run cairn-core tests**

```bash
cargo test -p cairn-core 2>&1 | tail -10
```

Expected: all tests pass including the three new validation tests.

- [ ] **Step 9.4 — Add snapshot tests for `LlmError` display strings**

Add to `crates/cairn-llm-openai-compat/src/provider.rs` (at the bottom, inside a `#[cfg(test)] mod tests` block):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::contract::LlmError;

    #[test]
    fn lm_error_display_snapshots() {
        insta::assert_snapshot!("not_configured", LlmError::NotConfigured {
            remediation: "cairn config set llm.provider ollama".into(),
        }.to_string());
        insta::assert_snapshot!("auth_denied", LlmError::AuthDenied.to_string());
        insta::assert_snapshot!("budget_exceeded", LlmError::BudgetExceeded.to_string());
        insta::assert_snapshot!("capability_missing", LlmError::CapabilityMissing {
            capability: "json_mode".into(),
        }.to_string());
    }
}
```

- [ ] **Step 9.5 — Run and accept snapshots**

```bash
cargo test -p cairn-llm-openai-compat lm_error_display_snapshots 2>&1 | tail -5
cargo insta review  # accept all four new snapshots
```

- [ ] **Step 9.6 — Run all adapter tests to verify nothing regressed**

```bash
cargo test -p cairn-llm-openai-compat 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 9.7 — Commit**

```bash
git add crates/cairn-core/Cargo.toml \
        crates/cairn-core/src/contract/llm_provider.rs \
        crates/cairn-llm-openai-compat/src/provider.rs \
        crates/cairn-llm-openai-compat/src/provider.rs.snap 2>/dev/null || true
# Add any generated .snap files:
git add crates/cairn-llm-openai-compat/src/snapshots/ 2>/dev/null || true
git commit -m "test(llm): core JSON validation unit tests + LlmError display snapshots"
```

---

## Task 10: Full verification + core-boundary check

**Files:** None created; verification only.

- [ ] **Step 10.1 — Run core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: passes (cairn-core must not import async-openai or any adapter crate).

- [ ] **Step 10.2 — Run full workspace tests**

```bash
cargo nextest run --workspace --locked --no-fail-fast 2>&1 | tail -20
```

Expected: `test result: ok` across all crates.

- [ ] **Step 10.3 — Run clippy**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tail -20
```

Fix any warnings before continuing.

- [ ] **Step 10.4 — Run fmt check**

```bash
cargo fmt --all --check 2>&1
```

Run `cargo fmt --all` if it reports diffs, then re-check.

- [ ] **Step 10.5 — Run supply-chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Fix any issues (unused deps, denied licenses, advisories).

- [ ] **Step 10.6 — Final commit**

```bash
git add -p  # stage any fmt/clippy fixes
git commit -m "chore: clippy + fmt fixes post #144 implementation" \
  --allow-empty  # only if nothing changed
```

---

## Self-Review Checklist

### Spec coverage
| Spec requirement | Task(s) |
|---|---|
| Add `complete()` to `LLMProvider` trait | Task 3 |
| `CompletionRequest` with prompt, schema, model, budget | Task 2 |
| `CompletionOutput` Text / Json variants | Task 2 |
| `LlmError` variants matching ADR 0001 codes | Task 1 |
| New `cairn-llm-openai-compat` crate | Task 4 |
| `build_llm_provider` from `LlmConfig` | Task 4–5 |
| `LlmConfig::provider == None` → `NotConfigured` | Task 5 |
| JSON-mode validation via `jsonschema` | Task 7 |
| Schema mismatch → `InvalidJsonOutput` | Task 7 |
| `json_mode=false` + schema → `CapabilityMissing`, no HTTP | Task 8 |
| Wiremock tests: text, schema match/mismatch, unparseable | Tasks 6–7 |
| Wiremock tests: 401, capability_missing | Task 8 |
| Missing-provider config tests | Task 5 |
| `cairn-core` pure JSON validation unit tests | Task 9 |
| `LlmError` display snapshot tests | Task 9 |
| `cairn-core` boundary still passes | Task 10 |
| Full workspace tests + clippy + fmt + supply-chain | Task 10 |
