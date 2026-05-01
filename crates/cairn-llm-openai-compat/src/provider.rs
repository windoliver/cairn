//! [`OpenAiCompatProvider`] — implements [`LLMProvider`] over a direct
//! `reqwest` HTTP path.
//!
//! `async-openai` discards the HTTP status when its `WrappedError`
//! deserialisation fails (e.g. empty bodies), which prevents accurate
//! 401/429/5xx classification. We use it for typed request/response
//! structs only and own the transport so the `LlmError` mapping is
//! status-driven.

use std::time::Duration;

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

/// TCP connect timeout. A stalled DNS/connect should fail fast and
/// surface as a `ProviderUnreachable` (retryable) within seconds, not
/// hang the caller.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Total per-attempt timeout (connect + send + receive). Bounds the
/// retry budget so the outer policy stays the source of pacing.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
/// Cap the body preview we inspect on a non-success status. A degraded
/// provider can stream gigabytes with a 401 — the preview must never
/// dominate the error path.
const ERROR_BODY_PREVIEW: usize = 256;
/// Time budget for collecting the error-body preview. Status mapping is
/// authoritative; the preview is best-effort context.
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(2);

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
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
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
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("invariant: reqwest client builder cannot fail with default config");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: "test-key".into(),
            model: model.into(),
            capabilities,
        }
    }

    /// Issue one POST to `/chat/completions`. The closure surface — returning
    /// [`Retryable`] — keeps the retry layer the sole source of pacing.
    ///
    /// Status is classified before the body is consumed: a degraded provider
    /// that streams a huge or stalled body on a 401/429/5xx must not be able
    /// to delay or hide the status mapping.
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
            Err(e) => return Err(classify_send_error(&e)),
        };

        let status = response.status();

        if !status.is_success() {
            let code = status.as_u16();
            // Bound the preview read so a stalled/huge body cannot delay
            // status-driven mapping. Status is authoritative either way.
            let body_preview = tokio::time::timeout(ERROR_BODY_TIMEOUT, collect_preview(response))
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            return Err(map_error_status(code, &body_preview));
        }

        // Success: parse the typed body. Body read inherits the
        // request-level timeout configured on the client.
        let bytes = response.bytes().await.map_err(|e| Retryable {
            err: LlmError::ProviderUnreachable {
                detail: format!("response body: {e}"),
            },
            // A timeout/reset mid-body is plausibly transient.
            retryable: e.is_timeout() || e.is_connect(),
        })?;
        serde_json::from_slice::<CreateChatCompletionResponse>(&bytes).map_err(|e| Retryable {
            err: LlmError::ProviderUnreachable {
                detail: format!("response parse: {e}"),
            },
            retryable: false,
        })
    }
}

/// Classify a send-side `reqwest::Error`.
///
/// - Timeouts and connection errors (DNS, refused, reset) → transient.
///   Retried up to the policy's budget.
/// - TLS verification / handshake errors → terminal. Repeating these
///   never helps, and we prefer surfacing the misconfiguration quickly.
/// - Anything else (request building, redirect loops, decode) → terminal.
fn classify_send_error(e: &reqwest::Error) -> Retryable {
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    let looks_tls = lower.contains("certificate")
        || lower.contains("invalid peer")
        || lower.contains("tls")
        || lower.contains("handshake");
    let retryable = !looks_tls && (e.is_timeout() || e.is_connect());
    Retryable {
        err: LlmError::ProviderUnreachable { detail: msg },
        retryable,
    }
}

/// Map a non-success HTTP status to an [`LlmError`].
fn map_error_status(code: u16, body_preview: &str) -> Retryable {
    if code == 401 || code == 403 {
        return Retryable {
            err: LlmError::AuthDenied,
            retryable: false,
        };
    }
    let detail = if body_preview.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code}: {body_preview}")
    };
    if code == 429 || (500..600).contains(&code) {
        return Retryable {
            err: LlmError::ProviderUnreachable { detail },
            retryable: true,
        };
    }
    Retryable {
        err: LlmError::ProviderUnreachable { detail },
        retryable: false,
    }
}

/// Read at most [`ERROR_BODY_PREVIEW`] bytes from the response and return
/// a UTF-8-lossy preview. Returns `None` on stream error so the caller
/// can fall back to a status-only message.
async fn collect_preview(response: reqwest::Response) -> Option<String> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(ERROR_BODY_PREVIEW);
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                let take = ERROR_BODY_PREVIEW
                    .saturating_sub(buf.len())
                    .min(bytes.len());
                buf.extend_from_slice(&bytes[..take]);
                if buf.len() >= ERROR_BODY_PREVIEW {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
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
