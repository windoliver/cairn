//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over a direct
//! `reqwest` HTTP path.
//!
//! `async-openai` discards the HTTP status when its `WrappedError`
//! deserialisation fails (e.g. empty bodies), which prevents accurate
//! 401/429/5xx classification. We use it for typed request/response
//! structs only and own the transport so the `LlmError` mapping is
//! status-driven.

use async_openai::types::chat::{
    ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequest,
    CreateChatCompletionRequestArgs, CreateChatCompletionResponse, FinishReason, ResponseFormat,
    ResponseFormatJsonSchema,
};
use cairn_core::{
    config::LlmConfig,
    contract::version::{ContractVersion, VersionRange},
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities,
        LLMProviderPlugin, LlmError,
    },
};

use crate::retry::{RetryPolicy, Retryable, with_retries};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_API_KEY: &str = "cairn"; // Some providers (Ollama) ignore the value but require non-empty.

/// OpenAI-compatible [`LLMProvider`] adapter.
pub struct OpenAiCompatProvider {
    /// HTTP client. Direct `reqwest` so we own status mapping.
    http: reqwest::Client,
    /// Endpoint base URL (e.g. `https://api.openai.com/v1`).
    base_url: String,
    /// Bearer-auth API key sent on every request.
    api_key: String,
    /// Model name resolved at construction time.
    model: String,
    /// Static capability advertisement.
    capabilities: LLMProviderCapabilities,
}

impl OpenAiCompatProvider {
    /// Construct from a resolved [`LlmConfig`].
    ///
    /// # Errors
    /// Returns [`LlmError::ProviderUnreachable`] only on `reqwest::Client`
    /// build failure (very unusual — OS-level TLS/dns init issue).
    pub fn from_config(config: &LlmConfig) -> Result<Self, LlmError> {
        let http =
            reqwest::Client::builder()
                .build()
                .map_err(|e| LlmError::ProviderUnreachable {
                    detail: format!("http client init: {e}"),
                })?;
        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        let api_key = config
            .api_key
            .clone()
            .unwrap_or_else(|| DEFAULT_API_KEY.into());
        let model = config.model.clone().unwrap_or_else(|| DEFAULT_MODEL.into());
        Ok(Self {
            http,
            base_url,
            api_key,
            model,
            capabilities: LLMProviderCapabilities {
                json_mode: true,
                streaming: false,
                tool_calls: false,
            },
        })
    }

    /// Test-only constructor with explicit capabilities.
    ///
    /// # Stability
    /// Only available with the `testing` feature. Semver-exempt — do not rely
    /// on this in production code.
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn with_capabilities(
        base_url: &str,
        model: &str,
        capabilities: LLMProviderCapabilities,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: "test-key".into(),
            model: model.into(),
            capabilities,
        }
    }

    /// Issue one POST to `/chat/completions`. The closure surface — returning
    /// [`Retryable`] — keeps the retry layer the sole source of pacing.
    async fn post_chat(
        &self,
        request: &CreateChatCompletionRequest,
    ) -> Result<CreateChatCompletionResponse, Retryable> {
        let url = format!("{}/chat/completions", self.base_url);
        let response = match self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Connection refused / DNS / TLS failures are not retried
                // — they are stable terminal errors for this endpoint.
                let retryable = e.is_timeout();
                return Err(Retryable {
                    err: LlmError::ProviderUnreachable {
                        detail: e.to_string(),
                    },
                    retryable,
                });
            }
        };

        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Err(Retryable {
                    err: LlmError::ProviderUnreachable {
                        detail: format!("response body: {e}"),
                    },
                    retryable: true,
                });
            }
        };

        if status.is_success() {
            return serde_json::from_slice::<CreateChatCompletionResponse>(&bytes).map_err(|e| {
                Retryable {
                    err: LlmError::ProviderUnreachable {
                        detail: format!("response parse: {e}"),
                    },
                    retryable: false,
                }
            });
        }

        let code = status.as_u16();
        if code == 401 || code == 403 {
            return Err(Retryable {
                err: LlmError::AuthDenied,
                retryable: false,
            });
        }

        let body_preview = truncate_body(&bytes);
        if code == 429 || (500..600).contains(&code) {
            return Err(Retryable {
                err: LlmError::ProviderUnreachable {
                    detail: format!("HTTP {code}: {body_preview}"),
                },
                retryable: true,
            });
        }

        Err(Retryable {
            err: LlmError::ProviderUnreachable {
                detail: format!("HTTP {code}: {body_preview}"),
            },
            retryable: false,
        })
    }
}

/// Return up to 256 bytes of the response body as a UTF-8-lossy preview.
fn truncate_body(bytes: &[u8]) -> String {
    const MAX: usize = 256;
    let slice = if bytes.len() > MAX {
        &bytes[..MAX]
    } else {
        bytes
    };
    String::from_utf8_lossy(slice).to_string()
}

impl LLMProviderPlugin for OpenAiCompatProvider {
    const NAME: &'static str = "openai-compatible";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
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

    #[tracing::instrument(skip(self, req), err, fields(model, schema_mode = req.schema.is_some()))]
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        // JSON schema path requires json_mode capability — guard it now.
        if req.schema.is_some() && !self.capabilities.json_mode {
            return Err(LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            });
        }

        // Validate the schema *before* any network call. The compiled
        // validator is reused on the response.
        let validator = if let Some(schema) = &req.schema {
            Some(
                jsonschema::validator_for(schema).map_err(|e| LlmError::InvalidJsonOutput {
                    detail: format!("invalid schema: {e}"),
                    raw: String::new(),
                })?,
            )
        } else {
            None
        };

        // Choose the model: per-request override or the instance default.
        let model = req.model.as_deref().unwrap_or(&self.model);
        tracing::Span::current().record("model", model);

        // Build the user message from the prompt string.
        let user_msg = ChatCompletionRequestUserMessageArgs::default()
            .content(req.prompt.as_str())
            .build()
            .map_err(|e| LlmError::ProviderUnreachable {
                detail: e.to_string(),
            })?;

        // Build the chat completion request.
        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(model).messages([user_msg.into()]);

        // Apply token budget when set.
        if let Some(budget) = &req.budget
            && let Some(max_tok) = budget.max_tokens
        {
            builder.max_completion_tokens(max_tok);
        }

        // JSON schema path: pass the caller's schema to the endpoint so it returns
        // structured JSON conforming to that schema.
        if let Some(schema) = &req.schema {
            builder.response_format(ResponseFormat::JsonSchema {
                json_schema: ResponseFormatJsonSchema {
                    name: "output".to_string(),
                    description: None,
                    schema: Some(schema.clone()),
                    strict: Some(true),
                },
            });
        }

        let request = builder.build().map_err(|e| LlmError::ProviderUnreachable {
            detail: e.to_string(),
        })?;

        // Retry transient failures (network timeout, 429, 5xx). The closure
        // pre-classifies each error so retry/no-retry is keyed on HTTP status.
        let response = with_retries(RetryPolicy::standard(), || self.post_chat(&request)).await?;

        // Extract the first choice.
        let choice =
            response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| LlmError::ProviderUnreachable {
                    detail: "provider returned empty choices".into(),
                })?;

        // Treat a truncated response as a budget overrun.
        if matches!(choice.finish_reason, Some(FinishReason::Length)) {
            return Err(LlmError::BudgetExceeded);
        }

        let content = choice
            .message
            .content
            .ok_or_else(|| LlmError::ProviderUnreachable {
                detail: "provider returned null content".into(),
            })?;

        // JSON schema path: parse and validate the response body with the
        // compiled validator from the preflight.
        if let Some(validator) = validator {
            let value: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| LlmError::InvalidJsonOutput {
                    detail: e.to_string(),
                    raw: content.clone(),
                })?;
            validator
                .validate(&value)
                .map_err(|e| LlmError::InvalidJsonOutput {
                    detail: e.to_string(),
                    raw: content.clone(),
                })?;
            return Ok(CompletionOutput::Json(value));
        }

        Ok(CompletionOutput::Text(content))
    }
}

#[cfg(test)]
mod tests {
    use cairn_core::contract::LlmError;

    #[test]
    fn lm_error_display_snapshots() {
        insta::assert_snapshot!(
            "not_configured",
            LlmError::NotConfigured {
                remediation: "cairn config set llm.provider ollama".into(),
            }
            .to_string()
        );
        insta::assert_snapshot!("auth_denied", LlmError::AuthDenied.to_string());
        insta::assert_snapshot!("budget_exceeded", LlmError::BudgetExceeded.to_string());
        insta::assert_snapshot!(
            "capability_missing",
            LlmError::CapabilityMissing {
                capability: "json_mode".into(),
            }
            .to_string()
        );
    }
}
