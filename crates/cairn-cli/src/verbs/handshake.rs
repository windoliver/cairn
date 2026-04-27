//! `cairn handshake` handler — P0 stub.
//!
//! Challenge authentication requires persisting the issued nonce into
//! `outstanding_challenges` so the server can consume it as a single-use
//! token on the signed intent path. That storage lands in issue #9.
//!
//! Emitting an ephemeral challenge that can never be validated server-side
//! would mislead callers into believing challenge-auth is available, so this
//! handler returns `Internal aborted` until the store is wired.

use std::process::ExitCode;

use super::envelope::{emit_json, human_error, new_operation_id};

/// Run `cairn handshake`. Exits 1 until challenge storage is wired (issue #9).
#[must_use]
pub fn run(json: bool) -> ExitCode {
    let op = new_operation_id();
    let msg = "challenge storage not wired in P0 — handshake authentication lands in #9";
    if json {
        emit_json(&serde_json::json!({
            "contract": "cairn.mcp.v1",
            "status": "aborted",
            "error": { "code": "Internal", "message": msg },
            "operation_id": op.0,
            "policy_trace": [],
        }));
    } else {
        human_error("handshake", "Internal", msg, &op);
    }
    ExitCode::FAILURE
}
