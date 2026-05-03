//! Verify gaps left by the original e2e: budget-exceeded, real schema-mismatch,
//! provider-diversity, rate-limit behavior. Manual run only — needs
//! `OPENROUTER_API_KEY`.

use cairn_core::{
    config::{ExtractBudget, LlmConfig, LlmProvider},
    contract::{CompletionOutput, CompletionRequest, LlmError},
};
use cairn_llm_openai_compat::build_llm_provider;

fn cfg(model: &str) -> LlmConfig {
    LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        model: Some(model.into()),
        api_key: Some(std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY")),
    }
}

#[allow(clippy::too_many_lines)] // Manual gap-verification example.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let banner = |s: &str| println!("\n=== {s} ===");

    // ── Gap A: BudgetExceeded against a real model ────────────────────────────
    banner("A. BudgetExceeded — max_tokens=5 with prompt that needs much more");
    let provider = build_llm_provider(&cfg("openai/gpt-4o-mini"))?;
    let req = CompletionRequest::builder()
        .prompt(
            "Write a 500-word essay about the history of cryptography. \
             Begin immediately, no preamble."
                .to_string(),
        )
        .budget(ExtractBudget {
            max_tokens: Some(5),
            max_wall_ms: None,
            max_turns: None,
        })
        .build();
    match provider.complete(&req).await {
        Err(LlmError::BudgetExceeded) => println!("    OK  → LlmError::BudgetExceeded"),
        Err(other) => println!("    FAIL → expected BudgetExceeded, got {other:?}"),
        Ok(out) => println!("    FAIL → expected BudgetExceeded, got {out:?}"),
    }

    // ── Gap B: Real schema-mismatch (non-strict path) ─────────────────────────
    // OpenRouter routes some models to providers that don't honor strict
    // structured outputs.  We use a model that famously diverges and ask for
    // JSON that the provider may not enforce server-side.
    banner("B. Schema-mismatch via a non-strict model — exercises client validation");
    let provider = build_llm_provider(&cfg("meta-llama/llama-3.1-8b-instruct"))?;
    let schema = serde_json::json!({
        "type": "object",
        "required": ["uuid_field"],
        "properties": {
            "uuid_field": {
                "type": "string",
                "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
            }
        },
        "additionalProperties": false
    });
    let req = CompletionRequest::builder()
        .prompt(
            "Return a JSON object with a single field `uuid_field` set to the \
             literal string \"banana\". Output only JSON."
                .to_string(),
        )
        .schema(schema)
        .build();
    match provider.complete(&req).await {
        Err(LlmError::InvalidJsonOutput { detail, raw }) => {
            println!("    OK  → InvalidJsonOutput");
            println!("        detail: {detail}");
            println!("        raw:    {raw}");
        }
        Err(other) => println!("    NOTE → got {other:?} (model may not support response_format)"),
        Ok(CompletionOutput::Json(v)) => println!("    NOTE → provider enforced strictly: {v}"),
        Ok(other) => println!("    UNEXPECTED → {other:?}"),
    }

    // ── Gap C: Provider quirks — Anthropic via OpenRouter ────────────────────
    banner("C. Anthropic Claude via OpenRouter — text completion");
    let provider = build_llm_provider(&cfg("anthropic/claude-3.5-haiku"))?;
    let req = CompletionRequest::builder()
        .prompt("Reply with exactly the word: pong".to_string())
        .build();
    match provider.complete(&req).await {
        Ok(CompletionOutput::Text(s)) => println!("    OK  → Text({:?})", s.trim()),
        Err(e) => println!("    FAIL → {e:?}"),
        Ok(other) => println!("    UNEXPECTED → {other:?}"),
    }

    // ── Gap D: Provider quirks — Google Gemini via OpenRouter ────────────────
    banner("D. Google Gemini via OpenRouter — JSON schema");
    let provider = build_llm_provider(&cfg("google/gemini-2.0-flash-001"))?;
    let schema = serde_json::json!({
        "type": "object",
        "required": ["greeting"],
        "properties": { "greeting": { "type": "string" } },
        "additionalProperties": false
    });
    let req = CompletionRequest::builder()
        .prompt("Return JSON with `greeting` set to \"hello\". Only JSON.".to_string())
        .schema(schema)
        .build();
    match provider.complete(&req).await {
        Ok(CompletionOutput::Json(v)) => println!("    OK  → Json({v})"),
        Err(e) => println!("    NOTE → {e:?} (Gemini-via-OR may not support strict json_schema)"),
        Ok(other) => println!("    UNEXPECTED → {other:?}"),
    }

    // ── Gap E: Rate-limit behavior — burst 10 concurrent requests ────────────
    banner("E. Rate-limit / concurrency — 10 parallel small requests");
    let provider = std::sync::Arc::new(build_llm_provider(&cfg("openai/gpt-4o-mini"))?);
    let mut joins = Vec::new();
    for i in 0..10 {
        let p = provider.clone();
        joins.push(tokio::spawn(async move {
            let req = CompletionRequest::builder()
                .prompt(format!("Say number {i}"))
                .build();
            (i, p.complete(&req).await)
        }));
    }
    let mut ok_count = 0;
    let mut err_count = 0;
    for j in joins {
        let (i, res) = j.await.unwrap();
        match res {
            Ok(_) => ok_count += 1,
            Err(e) => {
                err_count += 1;
                println!("    req {i}: ERR — {e}");
            }
        }
    }
    println!("    {ok_count}/10 succeeded, {err_count} failed");

    println!("\n=== gap audit complete ===");
    Ok(())
}
