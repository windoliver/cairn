//! Local redaction and source-side drop policy.

use regex::Regex;

/// Policy result for text-bearing sensor payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    /// Text was accepted after redaction.
    Sanitized(String),
    /// Text was rejected and must not be emitted.
    Rejected(String),
}

/// Sanitize text payloads before hashing or event construction.
#[must_use]
pub fn sanitize_text_payload(input: &str) -> PolicyAction {
    if contains_private_key_block(input) {
        return PolicyAction::Rejected("private key block".to_owned());
    }

    let mut text = input.to_owned();
    text = redact_regex(
        &text,
        r"(?i)\b([A-Z0-9_]*(TOKEN|API_KEY|SECRET|PASSWORD)[A-Z0-9_]*)=([^\s]+)",
        "$1=[REDACTED]",
    );
    text = redact_regex(
        &text,
        r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9._~+/=-]+",
        "Authorization: Bearer [REDACTED]",
    );

    PolicyAction::Sanitized(text)
}

fn redact_regex(input: &str, pattern: &str, replacement: &str) -> String {
    match Regex::new(pattern) {
        Ok(regex) => regex.replace_all(input, replacement).into_owned(),
        Err(_) => input.to_owned(),
    }
}

fn contains_private_key_block(input: &str) -> bool {
    let upper = input.to_ascii_uppercase();
    upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY-----")
}
