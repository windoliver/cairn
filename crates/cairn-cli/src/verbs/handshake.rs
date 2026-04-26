//! `cairn handshake` handler — fresh challenge mint (§8.0.a).
//!
//! Every call produces a different nonce. The nonce is single-use and would
//! be stored in `outstanding_challenges` once the store is wired (`issue #9`).

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};

use super::envelope::{emit_json, new_nonce};

const CHALLENGE_TTL_MS: u64 = 60_000;

/// Run `cairn handshake`. Exits 0 on success.
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
        println!("expires_at: {} (epoch-ms, TTL 60 s)", resp.challenge.expires_at);
    }
    ExitCode::SUCCESS
}
