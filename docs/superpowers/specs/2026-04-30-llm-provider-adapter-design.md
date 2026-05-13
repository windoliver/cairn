# LLMProvider Adapter & JSON-mode Contract Tests — Design Spec

**Issue:** [#144](https://github.com/windoliver/cairn/issues/144)
**Date:** 2026-04-30
**Design sources:** brief §4.0, §4.1, §5.2.a, §20 Q2; ADR 0001
**Status:** Approved

---

## 1. Scope

Implement the `complete(req) → text | json` method on the `LLMProvider` trait,
create the `cairn-llm-openai-compat` adapter crate, and add JSON-mode
enforcement + contract tests so extractor prompts cannot write invalid drafts to
storage.

Out of scope: `LLMExtractor` implementation (tracked in #74), embedding support,
`cairn status` Ollama probe, config env-var resolution beyond what is required to
construct the adapter.

---

## 2. Architecture

Two change sites:

### 2.1 `cairn-core` — types only, zero I/O

Add to `crates/cairn-core/src/contract/llm_provider.rs`:

- `CompletionRequest` — bon-derived builder; carries prompt, optional JSON
  Schema, optional model override, optional `ExtractBudget`.
- `CompletionOutput` — `Text(String)` or `Json(serde_json::Value)`.
- `LlmError` — thiserror enum with variants pinned to ADR 0001 error codes:
  `NotConfigured`, `ProviderUnreachable`, `AuthDenied`, `CapabilityMissing`,
  `InvalidJsonOutput`, `BudgetExceeded`.
- `complete(&self, req: &CompletionRequest) -> Result<CompletionOutput, LlmError>`
  added to the `LLMProvider` trait.

`cairn-core` must not depend on `async-openai`, `reqwest`, or any network crate.
The `cairn-core` boundary check (`scripts/check-core-boundary.sh`) must continue
to pass.

### 2.2 `crates/cairn-llm-openai-compat/` — new workspace crate

Implements `LLMProvider` + `LLMProviderPlugin` over `async-openai`.
`LLMProviderPlugin::NAME = "openai-compatible"` (matches `LlmProvider::OpenaiCompatible`
in config).

Single public entry point:

```rust
pub fn build_llm_provider(
    config: &LlmConfig,
) -> Result<Box<dyn LLMProvider>, LlmError>;
```

Returns `LlmError::NotConfigured` when `config.provider` is `None`.
`cairn-cli` and `cairn-sdk` call this at startup to wire the provider into the
verb layer.

---

## 3. Components

### 3.1 `CompletionRequest`

```rust
#[derive(Debug, Clone, bon::Builder)]
pub struct CompletionRequest {
    pub prompt: String,
    #[builder(default)]
    pub schema: Option<serde_json::Value>,  // JSON Schema; triggers json_mode
    #[builder(default)]
    pub model: Option<String>,              // overrides LlmConfig::model
    #[builder(default)]
    pub budget: Option<ExtractBudget>,      // max_tokens / max_wall_ms
}
```

### 3.2 `CompletionOutput`

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CompletionOutput {
    Text(String),
    Json(serde_json::Value),
}
```

### 3.3 `LlmError`

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    #[error("llm.not_configured: {remediation}")]
    NotConfigured { remediation: String },
    #[error("llm.provider_unreachable: {detail}")]
    ProviderUnreachable { detail: String },
    #[error("llm.auth_denied")]
    AuthDenied,
    #[error("llm.capability_missing: {capability}")]
    CapabilityMissing { capability: String },
    #[error("llm.invalid_json_output: {detail}")]
    InvalidJsonOutput { detail: String, raw: String },
    #[error("llm.budget_exceeded")]
    BudgetExceeded,
}
```

Error code strings match ADR 0001 §"Error codes (stable, machine-readable)".

### 3.4 `OpenAiCompatProvider`

```rust
pub struct OpenAiCompatProvider {
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
    model: String,
    capabilities: LLMProviderCapabilities,
}
```

Constructed from `&LlmConfig` via the sync `build_llm_provider` function.
At P0, `capabilities.json_mode = true` by default for `openai-compatible`
endpoints (all mainstream targets support it). `streaming` and `tool_calls`
are `false` at P0. The live `cairn status` probe that verifies `json_mode`
against the endpoint is out of scope for this issue.

For testing the `json_mode=false` branch, `OpenAiCompatProvider` exposes a
`#[cfg(test)]` constructor that accepts an explicit `LLMProviderCapabilities`
so the capability can be set to `false` without a live endpoint.

---

## 4. Data flow

### 4.1 Schema-mode (json_mode) path

```
caller
  │  CompletionRequest { prompt, schema: Some(s), budget }
  ▼
OpenAiCompatProvider::complete()
  │  capabilities.json_mode == true?
  │  yes → ChatCompletion with response_format: { type: "json_schema", json_schema: s }
  │         + max_tokens from budget.max_tokens
  ▼
raw text from endpoint
  │  serde_json::from_str → Value   (fail → InvalidJsonOutput)
  │  jsonschema::validate(&value, &s)  (fail → InvalidJsonOutput)
  ▼
Ok(CompletionOutput::Json(value))
```

If `capabilities.json_mode == false` and `schema` is `Some`: return
`LlmError::CapabilityMissing { capability: "json_mode" }` immediately — no HTTP
call.

### 4.2 Text path

Schema is `None`; `response_format` omitted from request; returns
`CompletionOutput::Text(string)`.

### 4.3 Missing provider path

```
build_llm_provider(config)
  │  config.provider == None
  ▼
Err(LlmError::NotConfigured { remediation: "cairn config set llm.provider ollama" })
  │
  ▼  (in cairn-cli verb handler)
CapabilityUnavailable, exit 78
```

### 4.4 Error mapping from `async-openai`

| `async-openai` / HTTP condition | `LlmError` variant |
|---|---|
| connect error / timeout | `ProviderUnreachable` |
| HTTP 401 / 403 | `AuthDenied` |
| HTTP 4xx other | `CapabilityMissing` |
| JSON parse failure | `InvalidJsonOutput` |
| Schema validation failure | `InvalidJsonOutput` |
| Token limit exceeded | `BudgetExceeded` |

---

## 5. New crate: `cairn-llm-openai-compat`

### 5.1 Cargo.toml dependencies

```toml
[dependencies]
cairn-core    = { workspace = true }
async-openai  = { version = "0.36", default-features = false, features = ["rustls"] }
jsonschema    = { workspace = true }
serde_json    = { workspace = true }
tokio         = { workspace = true }
thiserror     = { workspace = true }
tracing       = { workspace = true }

[dev-dependencies]
wiremock      = "0.6"
tokio         = { workspace = true, features = ["rt", "macros"] }
insta         = { workspace = true }
cairn-test-fixtures = { workspace = true }
```

`async-openai` added to `[workspace.dependencies]` in root `Cargo.toml` with
`default-features = false, features = ["rustls"]`. Crate is added to `members`
implicitly via `members = ["crates/*"]`.

### 5.2 Crate layout

```
crates/cairn-llm-openai-compat/
├── Cargo.toml
├── src/
│   ├── lib.rs          — pub use; build_llm_provider fn
│   ├── provider.rs     — OpenAiCompatProvider struct + LLMProvider impl
│   ├── config.rs       — LlmConfig → OpenAIConfig conversion
│   └── error.rs        — async-openai error → LlmError mapping
└── tests/
    ├── mock_provider.rs   — wiremock-based happy/error path tests
    └── missing_config.rs  — build_llm_provider with absent provider config
```

---

## 6. Testing

### 6.1 Mocked provider tests (`tests/mock_provider.rs`)

All use `wiremock::MockServer` to simulate an OpenAI-compatible endpoint.

| Test | Stimulus | Expected |
|---|---|---|
| `text_completion_no_schema` | valid chat response, no schema | `CompletionOutput::Text` |
| `json_completion_schema_match` | valid JSON response, schema matches | `CompletionOutput::Json` |
| `json_completion_schema_mismatch` | valid JSON but fails schema | `LlmError::InvalidJsonOutput` |
| `json_completion_unparseable` | response body not JSON | `LlmError::InvalidJsonOutput` |
| `endpoint_returns_401` | HTTP 401 | `LlmError::AuthDenied` |
| `endpoint_unreachable` | connection refused | `LlmError::ProviderUnreachable` |
| `json_mode_not_supported_capability_missing` | `json_mode=false`, schema passed | `LlmError::CapabilityMissing` — no HTTP call |

### 6.2 Missing-provider config tests (`tests/missing_config.rs`)

| Test | Config | Expected |
|---|---|---|
| `no_provider_returns_not_configured` | `LlmConfig::default()` | `LlmError::NotConfigured` |
| `provider_set_no_base_url_constructs` | `provider=OpenaiCompatible`, `base_url=None` | `Ok(...)`, defaults to `https://api.openai.com/v1` |
| `provider_set_ollama_base_url_constructs` | `provider=OpenaiCompatible`, `base_url=Some("http://localhost:11434/v1")` | `Ok(...)` |

### 6.3 JSON validation unit tests (in `cairn-core`)

Pure `jsonschema::validate` round-trips in `contract/llm_provider.rs` unit
tests — no HTTP, no adapter. Verifies the schema-enforcement logic is correct
before the adapter layer is involved. At least: valid object passes, missing
required field fails, wrong type fails.

### 6.4 Snapshot tests

`insta` snapshots on `LlmError` display strings (lock human-facing messages).

---

## 7. Invariants touched

- **Invariant 2 (stand-alone P0):** adapter has zero Python deps; `async-openai`
  with `rustls` adds no system TLS dependency.
- **Invariant 3 (CLI is ground truth):** `complete()` is one function in
  `cairn-core`; the adapter is a plugin, not a parallel implementation.
- **Invariant 4 (seven contracts):** `OpenAiCompatProvider` implements
  `LLMProvider`; no new contract is added.
- **Invariant 6 (fail closed):** `LlmError::NotConfigured` returned before any
  HTTP call when `llm.provider` is absent.
- **Invariant 8 (no unwrap in cairn-core):** `LlmError` variants carry typed
  fields; all error paths use `?`.

---

## 8. Out of scope

- `LLMExtractor` implementation (tracked in #74, which depends on this issue).
- `cairn status` Ollama probe (`GET :11434/api/tags`).
- Env-var config resolution beyond constructing the adapter (ADR 0001 config
  precedence matrix is a `cairn-cli` concern, not adapter-level).
- Streaming completions (`capabilities.streaming = false` at P0).
- Tool calls (`capabilities.tool_calls = false` at P0).
- Embedding endpoint.
