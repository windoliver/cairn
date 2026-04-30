//! `cairn llm probe` — diagnostic verb that wires the configured
//! [`LLMProvider`] adapter and issues a single [`complete`] call.
//!
//! Maps every [`LlmError`] variant to the sysexits-style exit code pinned by
//! ADR 0001 §"Error codes (stable, machine-readable)".
//!
//! [`complete`]: cairn_core::contract::LLMProvider::complete
//! [`LLMProvider`]: cairn_core::contract::LLMProvider
//! [`LlmError`]: cairn_core::contract::LlmError

use std::process::ExitCode;

use cairn_core::{
    config::CairnConfig,
    contract::{CompletionOutput, CompletionRequest, LlmError},
};

/// `EX_UNAVAILABLE` (sysexits.h).
pub const EX_UNAVAILABLE: u8 = 69;
/// `EX_NOPERM` (sysexits.h).
pub const EX_NOPERM: u8 = 77;
/// `EX_CONFIG` (sysexits.h).
pub const EX_CONFIG: u8 = 78;

/// Outcome of `cairn llm probe`. Exposed so tests can assert exit codes
/// without spawning a subprocess.
#[must_use]
pub fn exit_code_for(err: &LlmError) -> u8 {
    match err {
        LlmError::NotConfigured { .. } => EX_CONFIG,
        LlmError::ProviderUnreachable { .. } | LlmError::CapabilityMissing { .. } => EX_UNAVAILABLE,
        LlmError::AuthDenied => EX_NOPERM,
        // `LlmError` is `#[non_exhaustive]`; future variants fall through here.
        LlmError::InvalidJsonOutput { .. } | LlmError::BudgetExceeded | _ => 1,
    }
}

/// Run the probe.
///
/// Builds the provider from `config.llm`, issues a tiny `complete()` call,
/// and either prints the model's reply or surfaces the typed error.
///
/// `prompt_override` lets tests inject a fixed prompt; production passes
/// `None` and a default ("Reply with the single word: ping") is used.
pub async fn run_probe(
    config: &CairnConfig,
    json: bool,
    prompt_override: Option<&str>,
) -> ExitCode {
    let provider = match cairn_llm_openai_compat::build_llm_provider(&config.llm) {
        Ok(p) => p,
        Err(e) => {
            emit_error(&e, json);
            return ExitCode::from(exit_code_for(&e));
        }
    };

    let prompt = prompt_override
        .unwrap_or("Reply with the single word: ping")
        .to_owned();
    let req = CompletionRequest::builder().prompt(prompt).build();

    match provider.complete(&req).await {
        Ok(CompletionOutput::Text(s)) => {
            emit_success(&s, "text", json);
            ExitCode::SUCCESS
        }
        Ok(CompletionOutput::Json(v)) => {
            emit_success(&v.to_string(), "json", json);
            ExitCode::SUCCESS
        }
        Ok(_) => {
            // `CompletionOutput` is `#[non_exhaustive]`. New variants reach
            // the user as a generic success — adapt this arm when added.
            emit_success("(unknown variant)", "unknown", json);
            ExitCode::SUCCESS
        }
        Err(e) => {
            emit_error(&e, json);
            ExitCode::from(exit_code_for(&e))
        }
    }
}

fn emit_success(reply: &str, kind: &str, json: bool) {
    if json {
        let payload = serde_json::json!({
            "ok": true,
            "kind": kind,
            "reply": reply,
        });
        println!("{payload}");
    } else {
        println!("ok ({kind}): {reply}");
    }
}

fn emit_error(err: &LlmError, json: bool) {
    if json {
        let payload = serde_json::json!({
            "ok": false,
            "code": code_str(err),
            "message": err.to_string(),
        });
        eprintln!("{payload}");
    } else {
        eprintln!("error: {err}");
    }
}

fn code_str(err: &LlmError) -> &'static str {
    match err {
        LlmError::NotConfigured { .. } => "llm.not_configured",
        LlmError::ProviderUnreachable { .. } => "llm.provider_unreachable",
        LlmError::AuthDenied => "llm.auth_denied",
        LlmError::CapabilityMissing { .. } => "llm.capability_missing",
        LlmError::InvalidJsonOutput { .. } => "llm.invalid_json_output",
        LlmError::BudgetExceeded => "llm.budget_exceeded",
        _ => "llm.unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_adr_0001() {
        assert_eq!(
            exit_code_for(&LlmError::NotConfigured {
                remediation: "x".into()
            }),
            78
        );
        assert_eq!(
            exit_code_for(&LlmError::ProviderUnreachable { detail: "x".into() }),
            69
        );
        assert_eq!(exit_code_for(&LlmError::AuthDenied), 77);
        assert_eq!(
            exit_code_for(&LlmError::CapabilityMissing {
                capability: "json_mode".into()
            }),
            69
        );
        assert_eq!(
            exit_code_for(&LlmError::InvalidJsonOutput {
                detail: "x".into(),
                raw: "x".into()
            }),
            1
        );
        assert_eq!(exit_code_for(&LlmError::BudgetExceeded), 1);
    }

    #[test]
    fn code_str_namespaced() {
        assert_eq!(
            code_str(&LlmError::NotConfigured {
                remediation: String::new()
            }),
            "llm.not_configured"
        );
        assert_eq!(code_str(&LlmError::AuthDenied), "llm.auth_denied");
    }
}
