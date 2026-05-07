//! Prelude tools — the MCP `handshake` surface (brief §8.0.a).
//!
//! The eight core verbs are frozen in [`crate::generated::TOOLS`] and ship
//! through the IDL.  `status` and `handshake` are *protocol preludes* per
//! brief §8.0.a — they are not counted among the eight verbs.
//!
//! - `status` is delivered through MCP's built-in `initialize` request: the
//!   Cairn `StatusResponse` rides in
//!   `serverCapabilities.experimental["cairn.status"]`. See
//!   the `get_info` impl on [`crate::handler::CairnMcpHandler`].
//! - `handshake` is exposed as a regular MCP tool registered alongside the
//!   eight core verbs but kept in this hand-written slice so the
//!   IDL-generated [`crate::generated::TOOLS`] count stays at eight.
//!
//! `handshake` requires a wired `SqliteMemoryStore` because it must persist
//! the minted nonce in `outstanding_challenges` so a later signed envelope
//! from the same `issuer` can redeem it (brief §4.2). Handlers constructed
//! without a sqlite store therefore omit `handshake` from `tools/list` —
//! brief §15 fail-closed: never advertise a capability that cannot be
//! honored end-to-end.

use std::sync::Arc;

use cairn_core::generated::common::Nonce16Base64;
use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};
use cairn_store_sqlite::SqliteMemoryStore;
use rmcp::model::{CallToolResult, Content};

use crate::generated::ToolDecl;

/// Default challenge TTL in milliseconds — matches the brief default.
const CHALLENGE_TTL_MS: i64 = 60_000;

const HANDSHAKE_INPUT_SCHEMA: &[u8] = br#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "required": ["issuer"],
  "properties": {
    "issuer": {
      "type": "string",
      "minLength": 1,
      "description": "Caller's signing-key identity. The minted nonce is keyed by this issuer so the caller's next signed envelope can redeem it."
    }
  }
}"#;

const HANDSHAKE_DESCRIPTION: &str = "`handshake` — protocol prelude: mint a fresh single-use challenge nonce (brief §8.0.a).

Inserts a row into `outstanding_challenges` keyed by the supplied `issuer`, returning a 16-byte base64 nonce + absolute expiry. The nonce is single-use and consumed by the next signed mutation from the same issuer.

Two back-to-back calls always return distinct nonces. Not one of the eight core verbs — handshake is a protocol prelude.";

/// Hand-written tool declarations for protocol preludes registered alongside
/// the IDL-generated [`crate::generated::TOOLS`] slice. Kept separate so the
/// canonical eight-verb count is preserved.
pub const PRELUDE_TOOLS: &[ToolDecl] = &[ToolDecl {
    name: "handshake",
    description: HANDSHAKE_DESCRIPTION,
    input_schema: HANDSHAKE_INPUT_SCHEMA,
    capability: None,
    auth: "rebac",
    auth_overrides: &[],
    capability_overrides: &[],
}];

/// `true` iff `name` is one of the prelude tools above.
#[must_use]
pub fn is_prelude_tool(name: &str) -> bool {
    PRELUDE_TOOLS.iter().any(|d| d.name == name)
}

/// Dispatch a `tools/call` request whose tool name is in [`PRELUDE_TOOLS`].
///
/// Returns `CallToolResult` (with `is_error` set) for routing /
/// argument failures so the rmcp transport surfaces the failure as an
/// MCP error-in-result rather than a JSON-RPC error frame.
pub async fn dispatch(
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    sqlite_store: Option<Arc<SqliteMemoryStore>>,
) -> CallToolResult {
    if name == "handshake" {
        return handshake(arguments, sqlite_store).await;
    }
    CallToolResult::error(vec![Content::text(format!(
        "cairn: unknown prelude tool '{name}'"
    ))])
}

async fn handshake(
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    sqlite_store: Option<Arc<SqliteMemoryStore>>,
) -> CallToolResult {
    let Some(store) = sqlite_store else {
        return CallToolResult::error(vec![Content::text(
            "cairn handshake: capability unavailable — handshake requires a \
             wired sqlite store; this handler was constructed without one",
        )]);
    };

    let issuer = match arguments
        .as_ref()
        .and_then(|m| m.get("issuer"))
        .and_then(|v| v.as_str())
    {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => {
            return CallToolResult::error(vec![Content::text(
                "cairn handshake: invalid args — `issuer` must be a non-empty string",
            )]);
        }
    };

    let now_ms = unix_now_ms();
    let issuer_for_tx = issuer.clone();
    let mint_outcome = store
        .with_tx(move |tx| tx.mint_challenge(&issuer_for_tx, now_ms, CHALLENGE_TTL_MS))
        .await;

    let chal = match mint_outcome {
        Ok(c) => c,
        Err(e) => {
            return CallToolResult::error(vec![Content::text(format!(
                "cairn handshake: store error: {e}"
            ))]);
        }
    };

    // `expires_at_ms` is `i64` from sqlite (matching INTEGER); the wire IDL
    // uses `u64` (epoch-ms ≥ 0). Coerce defensively — only fails when the
    // wall clock is before 1970, which we treat as "expires immediately".
    let expires_at = u64::try_from(chal.expires_at_ms).unwrap_or(0);
    let resp = HandshakeResponse {
        contract: "cairn.mcp.v1".to_owned(),
        challenge: HandshakeResponseChallenge {
            nonce: Nonce16Base64(chal.nonce_b64),
            expires_at,
        },
    };

    match serde_json::to_string(&resp) {
        Ok(s) => CallToolResult::success(vec![Content::text(s)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!(
            "cairn handshake: serialize error: {e}"
        ))]),
    }
}

fn unix_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(i64::MAX)
}
