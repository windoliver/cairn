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
// Match gbrain's embedding service: 60s timeout, 5 retries, 4s base
// backoff capped at 120s, 100-input sub-batches, 8000-char per-input
// truncation. See /tmp/gbrain/src/core/embedding.ts (commit 96852c0).
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const MAX_RETRIES: u32 = 5;
const BACKOFF_BASE_MS: u64 = 4_000;
const BACKOFF_MAX_MS: u64 = 120_000;
const MAX_CHARS_PER_INPUT: usize = 8_000;
const BATCH_SIZE: usize = 100;

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
    /// Output dimensionality requested from the API. Defaults to
    /// `kind.dim()`. `text-embedding-3-large` supports any dim from 1
    /// to 3072 via Matryoshka Representation Learning, which we use to
    /// match the local sqlite-vec table's compile-time dim (384) when
    /// the bench wants apples-to-apples ANN results without changing
    /// the schema.
    target_dim: u32,
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
            .filter(|s| !s.trim().is_empty())
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
        // Mark the Authorization header as sensitive so reqwest's Debug impl
        // redacts the bearer token (privacy invariant: never leak credentials
        // in logs or panic dumps — see CLAUDE.md §4 invariant 9).
        let mut auth = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|e| OpenAiEmbeddingError::Network(e.to_string()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
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
            target_dim: u32::try_from(kind.dim()).unwrap_or(1536),
        })
    }

    /// Override the requested embedding dimensionality. Only meaningful
    /// for `text-embedding-3-*` models (which honour the `dimensions`
    /// parameter via MRL). Used by `cairn-bench` to match the
    /// 384-dim sqlite-vec table without altering the canonical model dim.
    #[must_use]
    pub fn with_dim(mut self, dim: usize) -> Self {
        self.target_dim = u32::try_from(dim).unwrap_or(self.target_dim);
        self
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
                // Honor Retry-After when present (gbrain pattern). The
                // header is either a delta-seconds integer or an HTTP-date;
                // we parse only the integer form OpenAI sends in practice.
                let retry_after_ms = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(|secs| secs.saturating_mul(1_000));
                last_err = Some(OpenAiEmbeddingError::RateLimited);
                if attempt < MAX_RETRIES {
                    let delay = retry_after_ms.unwrap_or_else(|| backoff_delay_ms(attempt));
                    tokio::time::sleep(Duration::from_millis(delay)).await;
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
        let truncated = truncate_for_embedding(text);
        let req = EmbedRequest {
            model: self.model_label,
            input: EmbedInput::One(&truncated),
            encoding_format: "float",
            dimensions: self.target_dim,
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
        let embedding = resp
            .data
            .into_iter()
            .next()
            .expect("invariant: len == 1 verified above")
            .embedding;
        let want = self.target_dim as usize;
        if embedding.len() != want {
            return Err(OpenAiEmbeddingError::BadResponseShape {
                expected: want,
                got: embedding.len(),
            });
        }
        Ok(embedding)
    }

    /// Batch embed (used by `cairn-bench` and bulk reindex paths).
    ///
    /// Matches gbrain's strategy: truncate each input at 8000 chars and
    /// process in 100-input sub-batches so a single `OpenAI` request stays
    /// well under per-request token caps and re-runs after a transient
    /// rate-limit pick up where they left off (the caller's retry loop
    /// retries each sub-batch independently).
    ///
    /// # Errors
    ///
    /// See [`OpenAiEmbeddingError`].
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OpenAiEmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let truncated: Vec<String> = texts.iter().map(|t| truncate_for_embedding(t)).collect();
        let dims = self.target_dim;
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in truncated.chunks(BATCH_SIZE) {
            let chunk_refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
            let req = EmbedRequest {
                model: self.model_label,
                input: EmbedInput::Many(&chunk_refs),
                encoding_format: "float",
                dimensions: dims,
            };
            let resp = self.embed_inner_blocking(&req)?;
            if resp.data.len() != chunk_refs.len() {
                return Err(OpenAiEmbeddingError::BadResponseShape {
                    expected: chunk_refs.len(),
                    got: resp.data.len(),
                });
            }
            // Sort by `index` to defend against any out-of-order response.
            let mut data = resp.data;
            data.sort_by_key(|d| d.index);
            // Validate the index sequence is exactly 0..n with no gaps
            // or duplicates. Without this, an upstream proxy that
            // reorders or drops one entry and inserts a duplicate index
            // would silently align our `chunk_refs[i]` to the wrong
            // embedding — caching the result against the wrong content
            // hash and poisoning the bench.
            for (expected_idx, datum) in data.iter().enumerate() {
                if datum.index != expected_idx {
                    return Err(OpenAiEmbeddingError::BadResponseShape {
                        expected: chunk_refs.len(),
                        got: datum.index,
                    });
                }
            }
            // Validate every embedding matches `target_dim`. A skewed
            // OPENAI_BASE_URL or an API-version drift could return 3072
            // or 384 vectors silently; vec0 would then reject the row
            // at insertion time, far away from the source of truth.
            let want = self.target_dim as usize;
            for datum in &data {
                if datum.embedding.len() != want {
                    return Err(OpenAiEmbeddingError::BadResponseShape {
                        expected: want,
                        got: datum.embedding.len(),
                    });
                }
            }
            out.extend(data.into_iter().map(|d| d.embedding));
        }
        Ok(out)
    }
}

fn backoff_delay_ms(attempt: u32) -> u64 {
    // Cap at BACKOFF_MAX_MS (gbrain pattern). Attempt-derived jitter
    // ≤ half the base avoids RNG dep while still de-synchronizing
    // concurrent callers.
    let attempt_u64 = u64::from(attempt);
    let exp = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(attempt));
    let capped = exp.min(BACKOFF_MAX_MS);
    let jitter = (attempt_u64.saturating_mul(17)) % (BACKOFF_BASE_MS / 2);
    capped.saturating_add(jitter)
}

async fn backoff_sleep(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(backoff_delay_ms(attempt))).await;
}

/// Slice `text` to at most [`MAX_CHARS_PER_INPUT`] bytes, snapping back
/// to the previous UTF-8 character boundary so we never split a code
/// point. gbrain's TS implementation uses `text.slice(0, MAX_CHARS)`
/// which counts UTF-16 code units; we count bytes and the result is
/// equivalent for ASCII-dominated corpora and safe for non-ASCII.
fn truncate_for_embedding(text: &str) -> String {
    if text.len() <= MAX_CHARS_PER_INPUT {
        return text.to_owned();
    }
    let mut end = MAX_CHARS_PER_INPUT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl EmbeddingModel for OpenAiEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.kind
    }

    fn dim(&self) -> usize {
        usize::try_from(self.target_dim).unwrap_or_else(|_| self.kind.dim())
    }

    fn embed_query(&self, q: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(q).map_err(EmbeddingError::from)
    }

    fn embed_document(&self, d: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_one(d).map_err(EmbeddingError::from)
    }
}
