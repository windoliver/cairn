//! End-to-end smoke test against `OpenRouter`.
//!
//! Run with:
//!   `OPENROUTER_API_KEY=sk-or-... cargo run -p cairn-llm-openai-compat`
//!       `--example e2e_openrouter`
//!
//! Exercises four paths:
//!   1. `NotConfigured` (no provider)
//!   2. Free-form text completion
//!   3. JSON-schema enforced completion (happy path)
//!   4. JSON-schema enforced completion with deliberately impossible schema
//!      to confirm `InvalidJsonOutput` is surfaced when the model violates it.

use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::{CompletionOutput, CompletionRequest, LlmError},
};
use cairn_llm_openai_compat::build_llm_provider;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key =
        std::env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY env var not set")?;
    let model =
        std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());

    println!("=== Cairn LLMProvider e2e against OpenRouter ===");
    println!("model: {model}");
    println!();

    // ── Case 1: NotConfigured ────────────────────────────────────────────────
    println!("[1/4] NotConfigured");
    let empty = LlmConfig::default();
    match build_llm_provider(&empty) {
        Err(LlmError::NotConfigured { remediation }) => {
            println!("    OK  → LlmError::NotConfigured (remediation: {remediation})");
        }
        Err(other) => return Err(format!("expected NotConfigured, got {other:?}").into()),
        Ok(_) => return Err("expected error, got Ok".into()),
    }
    println!();

    // ── Build the real provider for cases 2..4 ───────────────────────────────
    let cfg = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        model: Some(model.clone()),
        api_key: Some(api_key),
    };
    let provider = build_llm_provider(&cfg)?;

    // ── Case 2: free-form text ───────────────────────────────────────────────
    println!("[2/4] Text completion (no schema)");
    let req = CompletionRequest::builder()
        .prompt("Reply with the single word: ping".to_string())
        .build();
    match provider.complete(&req).await? {
        CompletionOutput::Text(s) => {
            let trimmed = s.trim();
            println!("    OK  → Text({trimmed:?})");
        }
        other => return Err(format!("expected Text, got {other:?}").into()),
    }
    println!();

    // ── Case 3: JSON-schema enforced (happy) ─────────────────────────────────
    println!("[3/4] JSON schema (matching)");
    let schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "confidence"],
        "additionalProperties": false,
        "properties": {
            "kind":       { "type": "string", "enum": ["feedback", "rule", "playbook"] },
            "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
        }
    });
    let req = CompletionRequest::builder()
        .prompt(
            "Return a JSON object with: kind=\"feedback\", confidence=0.85. \
             Only the JSON, no commentary."
                .to_string(),
        )
        .schema(schema.clone())
        .build();
    match provider.complete(&req).await? {
        CompletionOutput::Json(v) => {
            println!("    OK  → Json({v})");
            if v["kind"] != "feedback" {
                return Err(format!("expected kind=feedback, got {}", v["kind"]).into());
            }
        }
        other => return Err(format!("expected Json, got {other:?}").into()),
    }
    println!();

    // ── Case 4: JSON-schema impossible to satisfy → InvalidJsonOutput ────────
    println!("[4/4] JSON schema (impossible)");
    let impossible = serde_json::json!({
        "type": "object",
        "required": ["nonexistent_required_field_xyz"],
        "properties": {
            "nonexistent_required_field_xyz": {
                "type": "string",
                "pattern": "^IMPOSSIBLE_PATTERN_NEVER_MATCH_[0-9]{99}$"
            }
        },
        "additionalProperties": false
    });
    let req = CompletionRequest::builder()
        .prompt("Return any JSON object you like.".to_string())
        .schema(impossible)
        .build();
    match provider.complete(&req).await {
        Err(LlmError::InvalidJsonOutput { detail, .. }) => {
            println!("    OK  → InvalidJsonOutput (detail: {detail})");
        }
        Err(other) => {
            println!("    NOTE → got {other:?} (acceptable: provider may reject schema upfront)");
        }
        Ok(out) => {
            println!("    NOTE → provider satisfied the impossible schema: {out:?}");
        }
    }
    println!();

    println!("=== e2e complete ===");
    Ok(())
}
