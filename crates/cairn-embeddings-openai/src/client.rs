//! [`OpenAiEmbedder`] — implementation of [`EmbeddingModel`] backed by the
//! `OpenAI` HTTP embedding endpoint.
//!
//! The `EmbeddingModel` trait is synchronous (callers wrap us in
//! `tokio::task::spawn_blocking`), so the client owns a fresh
//! current-thread tokio runtime per call. That is cheap and avoids the
//! "block-in-async" deadlock that would happen if the trait method were
//! invoked from a multi-thread runtime via `Handle::current().block_on`.

use std::time::Duration;

use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingError, EmbeddingModel};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use crate::error::OpenAiEmbeddingError;
use crate::types::{EmbedInput, EmbedRequest, EmbedResponse};

const DEFAULT_BASE: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_MS: u64 = 200;

/// HTTP client for `OpenAI`'s `/v1/embeddings` endpoint.
///
/// Construct via [`OpenAiEmbedder::from_env`] (reads `OPENAI_API_KEY` and
/// optionally `OPENAI_BASE_URL`) or [`OpenAiEmbedder::new`] for tests.
#[derive(Debug, Clone)]
pub struct OpenAiEmbedder {
    base_url: String,
    model_label: &'static str,
    kind: EmbeddingModelKind,
    http: reqwest::Client,
}

impl OpenAiEmbedder {
    /// Construct from env. Reads `OPENAI_API_KEY` and (optionally) `OPENAI_BASE_URL`.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiEmbeddingError::MissingKey`] when no key is present
    /// (or it is empty), or [`OpenAiEmbeddingError::Network`] if the HTTP
    /// client cannot be built.
    pub fn from_env(kind: EmbeddingModelKind) -> Result<Self, OpenAiEmbeddingError> {
        let key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(OpenAiEmbeddingError::MissingKey)?;
        let base = std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_owned());
        Self::new(&key, &base, kind)
    }

    /// Construct with explicit credentials. Used by integration tests against
    /// a wiremock server.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiEmbeddingError::Network`] if the HTTP client cannot
    /// be built (e.g. invalid header bytes in the API key, TLS init failure).
    pub fn new(
        api_key: &str,
        base_url: &str,
        kind: EmbeddingModelKind,
    ) -> Result<Self, OpenAiEmbeddingError> {
        let model_label = match kind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large => "text-embedding-3-large",
            EmbeddingModelKind::OpenAiTextEmbedding3Small => "text-embedding-3-small",
            other => {
                return Err(OpenAiEmbeddingError::Network(format!(
                    "OpenAiEmbedder cannot serve {other:?}"
                )));
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| OpenAiEmbeddingError::Network(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| OpenAiEmbeddingError::Network(e.to_string()))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            model_label,
            kind,
            http,
        })
    }

    fn url(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    fn embed_inner_blocking<I: serde::Serialize>(
        &self,
        body: &I,
    ) -> Result<EmbedResponse, OpenAiEmbeddingError> {
        // The `EmbeddingModel` trait is sync; the store invokes it from
        // `spawn_blocking`. Spinning up a fresh current-thread runtime per
        // call is cheap and isolates us from any caller-side runtime.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| OpenAiEmbeddingError::Network(format!("rt build: {e}")))?;
        rt.block_on(self.embed_inner_async(body))
    }

    async fn embed_inner_async<I: serde::Serialize>(
        &self,
        body: &I,
    ) -> Result<EmbedResponse, OpenAiEmbeddingError> {
        let url = self.url();
        let mut last_err: Option<OpenAiEmbeddingError> = None;
        for attempt in 0..=MAX_RETRIES {
            let resp = self.http.post(&url).json(body).send().await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(OpenAiEmbeddingError::Network(e.to_string()));
                    if attempt < MAX_RETRIES {
                        backoff_sleep(attempt).await;
                        continue;
                    }
                    break;
                }
            };
            let status = resp.status();
            if status.is_success() {
                return resp
                    .json::<EmbedResponse>()
                    .await
                    .map_err(|e| OpenAiEmbeddingError::Parse(e.to_string()));
            }
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(OpenAiEmbeddingError::AuthFailed {
                    status: status.as_u16(),
                });
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                last_err = Some(OpenAiEmbeddingError::RateLimited);
                if attempt < MAX_RETRIES {
                    backoff_sleep(attempt).await;
                    continue;
                }
                break;
            }
            if status.is_server_error() {
                last_err = Some(OpenAiEmbeddingError::Server {
                    status: status.as_u16(),
                    retries: attempt,
                });
                if attempt < MAX_RETRIES {
                    backoff_sleep(attempt).await;
                    continue;
                }
                break;
            }
            // Other 4xx: surface body text and stop.
            let body_text = resp.text().await.unwrap_or_default();
            return Err(OpenAiEmbeddingError::Network(format!(
                "HTTP {}: {body_text}",
                status.as_u16()
            )));
        }
        Err(last_err.unwrap_or_else(|| OpenAiEmbeddingError::Network("unknown".to_owned())))
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, OpenAiEmbeddingError> {
        let req = EmbedRequest {
            model: self.model_label,
            input: EmbedInput::One(text),
            encoding_format: "float",
        };
        let resp = self.embed_inner_blocking(&req)?;
        if resp.data.len() != 1 {
            return Err(OpenAiEmbeddingError::BadResponseShape {
                expected: 1,
                got: resp.data.len(),
            });
        }
        // SAFETY (invariant): the length check above guarantees exactly one
        // element, so the iterator's first item is present.
        Ok(resp
            .data
            .into_iter()
            .next()
            .expect("invariant: len == 1 verified above")
            .embedding)
    }

    /// Batch embed (used by `cairn-bench` and bulk reindex paths).
    ///
    /// # Errors
    ///
    /// See [`OpenAiEmbeddingError`].
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OpenAiEmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let req = EmbedRequest {
            model: self.model_label,
            input: EmbedInput::Many(texts),
            encoding_format: "float",
        };
        let resp = self.embed_inner_blocking(&req)?;
        if resp.data.len() != texts.len() {
            return Err(OpenAiEmbeddingError::BadResponseShape {
                expected: texts.len(),
                got: resp.data.len(),
            });
        }
        Ok(resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

async fn backoff_sleep(attempt: u32) {
    // Bind to u64 *before* multiplying to avoid the cast-lossless / cast-precision
    // lints; `attempt` is a small retry counter so the widening is exact.
    let attempt_u64 = u64::from(attempt);
    let exp = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(attempt));
    // Add a small deterministic jitter (≤ half the base) so concurrent callers
    // don't synchronize their retries. No RNG dep — the value is bounded and
    // monotonic in `attempt`, which is plenty for backoff de-synchronization.
    let jitter = (attempt_u64.saturating_mul(17)) % (BACKOFF_BASE_MS / 2);
    tokio::time::sleep(Duration::from_millis(exp.saturating_add(jitter))).await;
}

impl EmbeddingModel for OpenAiEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.kind
    }

    fn dim(&self) -> usize {
        self.kind.dim()
    }

    fn embed_query(&self, q: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(q).map_err(EmbeddingError::from)
    }

    fn embed_document(&self, d: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(d).map_err(EmbeddingError::from)
    }
}
