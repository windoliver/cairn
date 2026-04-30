//! Map `async-openai` errors to [`LlmError`].

use async_openai::error::OpenAIError;
use cairn_core::contract::LlmError;

/// Convert an [`OpenAIError`] to [`LlmError`].
///
/// Heuristic mapping based on error message text — sufficient for P0.
/// Tasks 6–8 will tighten this once we inspect HTTP status codes directly.
// Used in Tasks 6–8 when complete() is wired up.
#[allow(dead_code)]
pub(crate) fn map_openai_error(e: &OpenAIError) -> LlmError {
    let msg = e.to_string();
    if msg.contains("401")
        || msg.contains("403")
        || msg.contains("Unauthorized")
        || msg.contains("Forbidden")
    {
        return LlmError::AuthDenied;
    }
    if msg.contains("connect")
        || msg.contains("timeout")
        || msg.contains("Connection refused")
        || msg.contains("dns error")
    {
        return LlmError::ProviderUnreachable { detail: msg };
    }
    LlmError::ProviderUnreachable { detail: msg }
}
