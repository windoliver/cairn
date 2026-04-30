//! Map `async-openai` errors to [`LlmError`].

use async_openai::error::OpenAIError;
use cairn_core::contract::LlmError;

/// Convert an [`OpenAIError`] to [`LlmError`].
///
/// Uses structural pattern matching on the [`OpenAIError`] variants for precise
/// mapping, falling back to message-text heuristics for edge cases.
///
/// # 401 with empty body
/// When a server returns HTTP 401 with no JSON body, `async-openai` cannot
/// deserialise the error object and surfaces `OpenAIError::JSONDeserialize`
/// with an empty content string. We treat that as [`LlmError::AuthDenied`].
pub(crate) fn map_openai_error(e: &OpenAIError) -> LlmError {
    match e {
        // ApiError: check the message and code fields for auth indicators.
        OpenAIError::ApiError(api_err) => {
            let msg_lower = api_err.message.to_lowercase();
            let code_lower = api_err.code.as_deref().unwrap_or("").to_lowercase();
            if msg_lower.contains("unauthorized")
                || msg_lower.contains("forbidden")
                || msg_lower.contains("invalid_api_key")
                || code_lower.contains("unauthorized")
                || code_lower.contains("invalid_api_key")
            {
                return LlmError::AuthDenied;
            }
            LlmError::ProviderUnreachable {
                detail: api_err.to_string(),
            }
        }

        // Reqwest error: check the HTTP status if available.
        OpenAIError::Reqwest(req_err) => {
            if let Some(status) = req_err.status() {
                // 401 Unauthorized or 403 Forbidden
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return LlmError::AuthDenied;
                }
            }
            let detail = req_err.to_string();
            LlmError::ProviderUnreachable { detail }
        }

        // JSONDeserialize with empty body content: the server returned a
        // non-success HTTP status with no JSON body.  401 is the most common
        // case (auth rejection before routing), so we map it to AuthDenied.
        OpenAIError::JSONDeserialize(_parse_err, content) if content.is_empty() => {
            LlmError::AuthDenied
        }

        // JSONDeserialize whose body content names an auth status. Some
        // providers (OpenRouter as of 2026-04) return the HTTP status as a
        // JSON integer rather than a string, breaking strict deserialisation,
        // but the body itself is unambiguous.
        OpenAIError::JSONDeserialize(_parse_err, content) if looks_like_auth_failure(content) => {
            LlmError::AuthDenied
        }

        // All other variants fall through to ProviderUnreachable.
        _ => {
            let msg = e.to_string();
            LlmError::ProviderUnreachable { detail: msg }
        }
    }
}

/// Heuristic: does this raw body look like an HTTP-401/403 auth failure?
fn looks_like_auth_failure(body: &str) -> bool {
    // Cheap check: "401" / "403" appearing as a JSON status field, or any
    // of the standard textual auth markers.
    let lower = body.to_ascii_lowercase();
    let status_markers = [
        "\"code\":401",
        "\"code\": 401",
        "\"status\":401",
        "\"status\": 401",
        "\"code\":403",
        "\"code\": 403",
        "\"status\":403",
        "\"status\": 403",
    ];
    if status_markers.iter().any(|m| lower.contains(m)) {
        return true;
    }
    lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
        || lower.contains("invalid_api_key")
}
