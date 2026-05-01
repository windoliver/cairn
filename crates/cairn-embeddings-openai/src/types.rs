//! Wire-format types for the `OpenAI` embedding endpoint.
//!
//! Reference: <https://platform.openai.com/docs/api-reference/embeddings>.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EmbedRequest<'a> {
    pub model: &'a str,
    pub input: EmbedInput<'a>,
    /// Always `"float"` for our path; `OpenAI` also supports `"base64"`.
    pub encoding_format: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum EmbedInput<'a> {
    One(&'a str),
    Many(&'a [&'a str]),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EmbedResponse {
    pub data: Vec<EmbedDatum>,
    /// Echoed model id; we do not consume it but accepting it makes the
    /// deserializer reject unexpectedly missing fields.
    #[allow(dead_code)]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EmbedDatum {
    pub embedding: Vec<f32>,
    /// Position in the input array; useful for batch ordering checks.
    #[allow(dead_code)]
    pub index: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Reserved for richer error reporting once the CLI surfaces it.
pub(crate) struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // Same as above — wired in via the type and ready to bubble up.
pub(crate) struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub code: Option<String>,
}
