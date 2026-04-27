//! `cairn handshake` handler — fresh challenge mint (§8.0.a).
//!
//! Emits a typed `HandshakeResponse` conforming to the generated handshake
//! schema. Every call produces a unique nonce (§8.0.a point d).
//!
//! **P0 caveat:** The issued nonce is ephemeral — it is NOT persisted into
//! `outstanding_challenges` and cannot be consumed server-side. Challenge-mode
//! signed intents will therefore be rejected until the store is wired in #9.
//! Callers should not rely on challenge authentication in this build.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};

use super::envelope::{emit_json, new_nonce};

const CHALLENGE_TTL_MS: u64 = 60_000;

/// Run `cairn handshake`. Exits 0 with a typed `HandshakeResponse`.
#[must_use]
pub fn run(json: bool) -> ExitCode {
    let nonce = new_nonce();
    #[allow(clippy::cast_possible_truncation)]
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is after Unix epoch")
        .as_millis() as u64;
    let expires_at = now_ms + CHALLENGE_TTL_MS;

    let resp = HandshakeResponse {
        contract: "cairn.mcp.v1".to_owned(),
        challenge: HandshakeResponseChallenge {
            nonce: nonce.clone(),
            expires_at,
        },
    };

    if json {
        emit_json(&resp);
    } else {
        println!("contract:   {}", resp.contract);
        println!("nonce:      {}", nonce.0);
        println!(
            "expires_at: {} (epoch-ms, TTL 60 s, P0: challenge not persisted)",
            resp.challenge.expires_at
        );
    }
    ExitCode::SUCCESS
}
