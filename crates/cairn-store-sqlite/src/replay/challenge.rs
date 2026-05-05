//! Server-minted challenge nonces (issue #52, brief §4.2 + §8.0.a).
//!
//! [`mint_challenge`] inserts a fresh single-use nonce into
//! `outstanding_challenges` with a configurable TTL and returns the
//! base64-encoded nonce + the absolute expiry. The CLI / MCP `handshake`
//! prelude wraps this and emits the result to the caller.
//! [`purge_expired_challenges`] drops challenges whose TTL has elapsed
//! — callers that mint frequently should sweep occasionally to keep
//! the table bounded.

use base64::Engine as _;
use rand::RngCore as _;
use rusqlite::{Transaction, params};

use crate::error::StoreError;

/// Nonce length matching `Nonce16Base64` — 16 raw bytes ⇒ 24-char
/// standard-base64 string with `==` padding.
const NONCE_BYTES: usize = 16;

/// Minted challenge: the value the caller embeds in
/// `signed_intent.server_challenge` plus the absolute expiry epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedChallenge {
    /// 24-char base64 nonce (16 raw bytes + `==` padding) — the
    /// `Nonce16Base64` shape from the IDL.
    pub nonce_b64: String,
    /// Wall-clock unix-ms after which the challenge cannot be redeemed.
    pub expires_at_ms: i64,
}

/// Mint and persist a fresh challenge for `issuer`. Returns the
/// base64 nonce + absolute expiry timestamp.
///
/// `now_ms` is the wall-clock unix-ms; the caller must supply it so
/// tests can use a fixed clock.  `ttl_ms` is the lifetime of the
/// challenge in milliseconds; brief default is 60 000.
///
/// # Errors
///
/// Returns [`StoreError::Sqlite`] if the underlying insert fails — the
/// 0046 schema permits multiple outstanding challenges per issuer, so
/// PK collisions are statistically negligible (16 random bytes per
/// nonce).  A retry on PK collision could be wired in if a regression
/// surfaces, but P0 trusts the CSPRNG.
pub fn mint_challenge(
    tx: &Transaction<'_>,
    issuer: &str,
    now_ms: i64,
    ttl_ms: i64,
) -> Result<MintedChallenge, StoreError> {
    let mut bytes = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let expires_at_ms = now_ms.saturating_add(ttl_ms);

    tx.execute(
        "INSERT INTO outstanding_challenges (issuer, challenge, expires_at)
         VALUES (?1, ?2, ?3)",
        params![issuer, &bytes[..], expires_at_ms],
    )?;

    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(MintedChallenge {
        nonce_b64,
        expires_at_ms,
    })
}

/// Drop every challenge whose TTL has elapsed (`expires_at < now_ms`).
/// Callers that mint frequently should run this periodically; the
/// table is otherwise bounded only by the number of unspent challenges
/// times their TTL window. Returns the number of rows deleted.
///
/// # Errors
///
/// Returns [`StoreError::Sqlite`] if the underlying delete fails.
pub fn purge_expired_challenges(tx: &Transaction<'_>, now_ms: i64) -> Result<usize, StoreError> {
    let n = tx.execute(
        "DELETE FROM outstanding_challenges WHERE expires_at < ?1",
        params![now_ms],
    )?;
    Ok(n)
}
