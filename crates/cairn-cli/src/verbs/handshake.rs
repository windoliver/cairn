//! `cairn handshake` handler — fresh challenge mint (§8.0.a, issue #52).
//!
//! Emits a typed `HandshakeResponse` conforming to the generated handshake
//! schema. Every call produces a unique nonce (§8.0.a point d).
//!
//! # Behaviour
//!
//! - When `--issuer` is supplied **and** the active vault is bound, the
//!   nonce is persisted into `outstanding_challenges` via a real `SQLite`
//!   transaction so a later signed envelope from the same issuer can
//!   redeem it. This is the production path that actually enables
//!   challenge-mode replay (brief §4.2).
//! - When `--issuer` is missing **or** the vault is unbound, the call
//!   emits a warning to stderr and prints an ephemeral nonce that
//!   cannot be redeemed server-side. This preserves the pre-#52 P0
//!   surface for callers that are not ready to bind to an identity yet
//!   (notably MCP `initialize` flows that have no caller identity).

use std::path::Path;
use std::process::ExitCode;

use cairn_core::generated::common::Nonce16Base64;
use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};
use cairn_core::time::checked_now_ms;

use super::envelope::{emit_json, new_nonce};

const CHALLENGE_TTL_MS: i64 = 60_000;

/// Run `cairn handshake` without store binding (ephemeral fallback).
/// Kept for callers that still drive the verb without resolving a vault
/// (e.g. `--help` flows). Production callers should use [`run_with_context`].
#[must_use]
pub fn run(json: bool) -> ExitCode {
    run_with_context(json, None, None)
}

/// Run `cairn handshake` with optional vault root + issuer.
///
/// When both are provided, opens the store at `<vault>/.cairn/cairn.db`
/// and inserts a row into `outstanding_challenges` inside a single
/// transaction. The returned nonce can be redeemed by a signed envelope
/// from `issuer` until `expires_at_ms`.
///
/// When either is missing, the function falls back to the pre-#52
/// ephemeral mint (random nonce, no persistence) and prints a one-line
/// warning to stderr.
#[must_use]
pub fn run_with_context(json: bool, vault_root: Option<&Path>, issuer: Option<&str>) -> ExitCode {
    // Fail closed on a degraded clock: minting a persisted challenge with a
    // saturated `now_ms` would either purge every live row in
    // `outstanding_challenges` (overflow case → cutoff at `i64::MAX`) or
    // emit a 1970-era `expires_at` (pre-epoch case). Round-2 review #2.
    let now_ms = match checked_now_ms() {
        Ok(ms) => ms,
        Err(e) => {
            eprintln!("cairn handshake: clock error — {e}");
            return ExitCode::from(69); // EX_UNAVAILABLE
        }
    };

    if let (Some(root), Some(issuer)) = (vault_root, issuer) {
        return mint_persisted(json, root, issuer, now_ms);
    }

    // Ephemeral fallback. Mirrors the pre-#52 behaviour so help/CI
    // smoke flows that drive `cairn handshake` without `--issuer` keep
    // returning a well-formed response.
    //
    // Round-6 review #2: stdout shape stays identical to the persisted
    // response (downstream wire-compat tests rely on it), so emit the
    // warning to stderr in BOTH human and JSON modes. Machine callers
    // that care whether a nonce is redeemable read stderr; humans see
    // the warning above the human-readable block.
    let nonce = new_nonce();
    let expires_at_ms = now_ms.saturating_add(CHALLENGE_TTL_MS);
    let resp = response(nonce, expires_at_ms);
    eprintln!(
        "warning: --issuer not provided (or vault unbound) — challenge will not be \
         redeemable. Pass `--issuer ID` against a bound vault to persist."
    );
    if json {
        emit_json(&resp);
    } else {
        print_human(&resp, /* persisted */ false);
    }
    ExitCode::SUCCESS
}

fn mint_persisted(json: bool, vault_root: &Path, issuer: &str, now_ms: i64) -> ExitCode {
    let cairn_dir = vault_root.join(".cairn");
    if !cairn_dir.exists() {
        eprintln!(
            "cairn handshake: vault at {} is not bootstrapped (no .cairn/) — \
             run `cairn bootstrap` first",
            vault_root.display()
        );
        return ExitCode::from(78); // EX_CONFIG
    }
    // The store auto-creates `.cairn/cairn.db` on first open; do not
    // pre-check existence here. A fresh vault should be able to mint
    // challenges without first running an unrelated mutating verb.
    let db_path = cairn_dir.join("cairn.db");

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn handshake: could not build tokio runtime — {e}");
            return ExitCode::from(1);
        }
    };

    let issuer_owned = issuer.to_owned();
    let outcome = runtime.block_on(async move {
        let store = cairn_store_sqlite::open(&db_path).await?;
        store
            .with_tx(move |tx| {
                let chal = tx.mint_challenge(&issuer_owned, now_ms, CHALLENGE_TTL_MS)?;
                Ok::<_, cairn_store_sqlite::StoreError>(chal)
            })
            .await
    });

    let chal = match outcome {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cairn handshake: store error — {e}");
            return ExitCode::from(1);
        }
    };

    let resp = response(Nonce16Base64(chal.nonce_b64), chal.expires_at_ms);

    // Round-14 review #1: substrate ships in this PR but no signed
    // verb dispatch consumes the persisted nonce yet — `cairn ingest`
    // / `forget` / `capture_trace` do not call
    // `StoreTx::prepare_wal_with_replay`. Persistence is correct for
    // forward compat (issue acceptance criterion); operators should
    // see one stderr line so a falsely "redeemed" nonce is not a
    // surprise.
    eprintln!(
        "warning: replay-admit dispatch is not wired in this build — \
         the persisted challenge will only be redeemable once a \
         follow-up issue routes signed mutating verbs through \
         `prepare_wal_with_replay`."
    );

    if json {
        emit_json(&resp);
    } else {
        print_human(&resp, /* persisted */ true);
    }
    ExitCode::SUCCESS
}

fn response(nonce: Nonce16Base64, expires_at_ms: i64) -> HandshakeResponse {
    // The IDL exposes `expires_at` as `u64` (epoch-ms ≥ 0). The store
    // computes it from a signed `i64` (matching SQLite's INTEGER); a
    // negative value is impossible here unless the wall clock is
    // before 1970. Coerce defensively.
    let expires_at = u64::try_from(expires_at_ms).unwrap_or(0);
    HandshakeResponse {
        contract: "cairn.mcp.v1".to_owned(),
        challenge: HandshakeResponseChallenge { nonce, expires_at },
    }
}

fn print_human(resp: &HandshakeResponse, persisted: bool) {
    println!("contract:   {}", resp.contract);
    println!("nonce:      {}", resp.challenge.nonce.0);
    if persisted {
        println!(
            "expires_at: {} (epoch-ms; persisted in outstanding_challenges, single-use)",
            resp.challenge.expires_at
        );
    } else {
        println!(
            "expires_at: {} (epoch-ms, ephemeral — not persisted)",
            resp.challenge.expires_at
        );
    }
}
