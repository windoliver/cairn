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

/// Should the caller retry this error? True for 429 and 5xx.
///
/// `async-openai` may wrap a 429/5xx in either `Reqwest` (when the status
/// can be inspected directly) or `JSONDeserialize` (when the error body
/// can't be parsed as `WrappedError`). Both paths are checked.
///
/// # Empty bodies
/// When `async-openai` cannot deserialize and the body is empty, the
/// underlying HTTP status is lost. We classify that case as transient
/// — better to retry a few times (the bounded [`RetryPolicy`] caps the
/// cost) than to give up on what might be a 429/5xx, and after retry
/// exhaustion it falls through to [`LlmError::ProviderUnreachable`]
/// rather than mis-claiming auth.
///
/// [`RetryPolicy`]: crate::retry::RetryPolicy
pub(crate) fn is_rate_limit_or_server_error(e: &OpenAIError) -> bool {
    match e {
        OpenAIError::Reqwest(req_err) => req_err.status().is_some_and(|s| {
            let code = s.as_u16();
            code == 429 || (500..600).contains(&code)
        }),
        OpenAIError::JSONDeserialize(_, content) => {
            content.is_empty() || looks_like_transient_error(content)
        }
        OpenAIError::ApiError(api_err) => {
            let code_lower = api_err.code.as_deref().unwrap_or("").to_lowercase();
            let msg_lower = api_err.message.to_lowercase();
            code_lower.contains("rate_limit")
                || msg_lower.contains("rate limit")
                || msg_lower.contains("too many requests")
        }
        _ => false,
    }
}

/// Heuristic: does this raw body name a transient HTTP status (429 / 5xx)?
fn looks_like_transient_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let markers = [
        "\"code\":429",
        "\"code\": 429",
        "\"status\":429",
        "\"status\": 429",
        "\"code\":500",
        "\"code\": 500",
        "\"code\":502",
        "\"code\": 502",
        "\"code\":503",
        "\"code\": 503",
        "\"code\":504",
        "\"code\": 504",
    ];
    if markers.iter().any(|m| lower.contains(m)) {
        return true;
    }
    lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
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
