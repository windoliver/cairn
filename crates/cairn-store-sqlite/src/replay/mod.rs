//! Atomic replay ledger + WAL `PREPARE` coupling (issue #52, brief §4.2).
//!
//! [`consume_intent`] runs the per-mode replay check (`used` insert + per-issuer
//! sequence CAS, or `outstanding_challenges` consume) against an open
//! transaction. [`prepare_wal_with_replay`] couples that with a `wal_ops`
//! `PREPARED` row insert so the three writes — replay consume, sequence /
//! challenge bookkeeping, and WAL admission — land or roll back as a unit.
//!
//! ## Order inside the transaction
//!
//! The brief sketch in §4.2 inserts `used` first and `wal_ops` last, but the
//! 0003 trigger `used_issuer_matches_wal` reads `wal_ops.issuer` for the
//! same `operation_id` at insert time. `SQLite` triggers do not defer (only
//! foreign-key constraints can). To satisfy the trigger inside one
//! transaction we insert `wal_ops` first, then `used` — the atomicity
//! guarantee is identical (a rollback drops both rows).
//!
//! ## Mode selection
//!
//! [`SignedIntent`] declares an XOR between `sequence` and
//! `server_challenge` at the IDL boundary; this module re-checks it
//! defensively because in-process callers can construct a `SignedIntent`
//! with both fields set, bypassing the `RawSignedIntent::try_from` guards.

pub mod challenge;

use cairn_core::generated::envelope::SignedIntent;
use rusqlite::{OptionalExtension as _, Transaction, params};
use thiserror::Error;
use tracing::instrument;

use base64::Engine as _;

/// Errors that prevent admitting an envelope to the replay ledger.
///
/// Variants align 1:1 with the IDL-closed wire codes the verb layer
/// emits: [`ReplayError::Duplicate`] → `ReplayDetected`,
/// [`ReplayError::OutOfOrder`] → `OutOfOrderSequence`,
/// [`ReplayError::ChallengeMissing`] / [`ReplayError::ChallengeExpired`]
/// → `Unauthorized` (no dedicated wire code at v0.1; the brief permits
/// rejecting either as the more general unauthorized class).
/// [`ReplayError::ModeXorViolation`] is mapped to `InvalidArgs` since
/// the envelope itself is malformed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// `(operation_id)` or `(issuer, nonce)` is already in `used`.
    #[error("replay detected: operation_id {operation_id} or (issuer, nonce) already consumed")]
    Duplicate {
        /// The conflicting `operation_id` ULID.
        operation_id: String,
    },

    /// Sequence value is not strictly greater than the issuer's
    /// `issuer_seq.high_water`. `attempted` is the envelope value;
    /// `high_water` is the stored watermark.
    #[error(
        "out-of-order sequence for issuer {issuer}: high_water={high_water} attempted={attempted}"
    )]
    OutOfOrder {
        /// Envelope issuer string.
        issuer: String,
        /// Current `issuer_seq.high_water` value, or 0 if no row.
        high_water: u64,
        /// Sequence value the envelope tried to claim.
        attempted: u64,
    },

    /// Challenge mode requested but no matching outstanding challenge
    /// exists, or it has been consumed by an earlier call.
    #[error("server_challenge has no outstanding row for issuer {issuer}")]
    ChallengeMissing {
        /// Envelope issuer string.
        issuer: String,
    },

    /// Challenge nonce was found but its TTL has elapsed (`expires_at < now`).
    /// Surfaced as a separate variant so the verb layer can distinguish
    /// "spend a fresh challenge" from "your challenge is too old".
    #[error("server_challenge expired for issuer {issuer} (expires_at < now)")]
    ChallengeExpired {
        /// Envelope issuer string.
        issuer: String,
    },

    /// Envelope did not advertise exactly one of `sequence` or
    /// `server_challenge`. The IDL `RawSignedIntent::try_from` enforces
    /// this on the wire path; the check here defends in-process callers
    /// that constructed a `SignedIntent` directly.
    #[error("envelope must advertise exactly one of [sequence, server_challenge]")]
    ModeXorViolation,

    /// Envelope `sequence` exceeds `SQLite`'s `INTEGER` (`i64`) range. The
    /// IDL caps `sequence` at `2^53 − 1`, so this is a defensive check
    /// against in-process callers bypassing IDL validation.
    #[error("envelope sequence {value} exceeds SQLite i64 range")]
    SequenceOverflow {
        /// The offending sequence value.
        value: u64,
    },

    /// Underlying `SQLite` failure (driver/IO error, schema mismatch,
    /// or unexpected trigger abort whose message did not match a known
    /// invariant string).
    #[error("sqlite error during replay consume")]
    Sqlite(#[from] rusqlite::Error),
}

/// Transaction order for [`prepare_wal_with_replay`] writes.
///
/// `wal_ops_first → used` matches the 0046 trigger semantics: the
/// `used_issuer_matches_wal` BEFORE-INSERT trigger reads
/// `wal_ops.issuer` for the same `operation_id` at insert time and
/// would abort if the row were absent.
#[derive(Debug, Clone)]
pub struct WalPrepareInputs<'a> {
    /// `wal_ops.kind` — must be one of the closed CHECK values from
    /// migration 0002 (widened by 0041). Caller picks based on which
    /// verb produced the envelope.
    pub kind: &'a str,
    /// Optional `plan_ref` ULID — set when the verb stages a plan blob.
    pub plan_ref: Option<&'a str>,
    /// Serialized scope JSON — `wal_ops.scope_json` is `TEXT NOT NULL`.
    pub scope_json: &'a str,
    /// Full envelope JSON — `wal_ops.envelope` is `TEXT NOT NULL`.
    pub envelope_json: &'a str,
    /// Wall-clock unix-ms `now`. Used for `wal_ops.issued_at`,
    /// `wal_ops.updated_at`, `used.committed_at`, and the challenge
    /// expiry check.
    pub now_ms: i64,
    /// `wal_ops.expires_at` unix-ms. Caller derives from
    /// `intent.expires_at` after envelope verification.
    pub expires_at_ms: i64,
}

/// Atomically admit a verified envelope: insert the WAL `PREPARED` row,
/// then consume the replay ledger entry. Both writes happen inside the
/// caller's transaction; rolling back drops both.
///
/// # Errors
///
/// - [`ReplayError::Duplicate`] — `operation_id` or `(issuer, nonce)`
///   already consumed.
/// - [`ReplayError::OutOfOrder`] — sequence-mode envelope failed the
///   per-issuer CAS.
/// - [`ReplayError::ChallengeMissing`] — challenge-mode envelope
///   referenced a nonce that is not outstanding.
/// - [`ReplayError::ChallengeExpired`] — challenge nonce found but TTL
///   elapsed.
/// - [`ReplayError::ModeXorViolation`] — envelope is missing both
///   `sequence` and `server_challenge`, or carries both.
/// - [`ReplayError::SequenceOverflow`] — sequence > `i64::MAX`
///   (defensive; IDL caps at `2^53 − 1`).
/// - [`ReplayError::Sqlite`] — driver / schema / trigger error.
#[instrument(skip_all, fields(
    operation_id = %intent.operation_id.0,
    issuer = %intent.issuer.0,
    mode = mode_label(intent),
))]
pub fn prepare_wal_with_replay(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    inputs: &WalPrepareInputs<'_>,
) -> Result<(), ReplayError> {
    // Defensive XOR check before any write — see ReplayError::ModeXorViolation.
    if (u8::from(intent.sequence.is_some()) + u8::from(intent.server_challenge.is_some())) != 1 {
        return Err(ReplayError::ModeXorViolation);
    }

    let issued_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
        [],
        |r| r.get(0),
    )?;

    // Idempotent on operation_id — a retry after a crash that already
    // committed the wal_ops row finds the conflict and continues. The
    // replay-ledger consume below is the source of truth for "this
    // envelope has been admitted", so it is fine for wal_ops to be a
    // no-op here.
    let inserted = tx.execute(
        "INSERT INTO wal_ops (
             operation_id, issued_seq, kind, state, envelope, issuer,
             principal, target_hash, scope_json, plan_ref,
             expires_at, signature, issued_at, updated_at, reason
         ) VALUES (?1, ?2, ?3, 'PREPARED', ?4, ?5,
                   NULL, ?6, ?7, ?8, ?9, ?10, ?11, ?11, NULL)
         ON CONFLICT (operation_id) DO NOTHING",
        params![
            intent.operation_id.0,
            issued_seq,
            inputs.kind,
            inputs.envelope_json,
            intent.issuer.0,
            intent.target_hash,
            inputs.scope_json,
            inputs.plan_ref,
            inputs.expires_at_ms,
            intent.signature.0,
            inputs.now_ms,
        ],
    )?;

    // If the INSERT was a no-op, the replay ledger row already exists
    // (committed via the same transaction in a prior crash-recovered
    // run). The `used` INSERT below is also keyed on operation_id and
    // will surface as Duplicate if the prior consume committed.
    let _ = inserted;

    consume_intent(tx, intent, inputs.now_ms)
}

/// Run the per-mode replay-ledger consume against an existing
/// `wal_ops` row. Use [`prepare_wal_with_replay`] for the standard hot
/// path; this lower-level entry point exists for callers that already
/// hold a prepared `wal_ops` row (e.g., recovery / replay-only paths).
///
/// # Errors
///
/// Same as [`prepare_wal_with_replay`] minus `Sqlite` errors specific
/// to the `wal_ops` insert.
#[instrument(skip_all, fields(
    operation_id = %intent.operation_id.0,
    issuer = %intent.issuer.0,
    mode = mode_label(intent),
))]
pub fn consume_intent(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    now_ms: i64,
) -> Result<(), ReplayError> {
    match (intent.sequence, intent.server_challenge.as_ref()) {
        (Some(seq), None) => consume_sequence_mode(tx, intent, seq, now_ms),
        (None, Some(chal)) => consume_challenge_mode(tx, intent, &chal.0, now_ms),
        (Some(_), Some(_)) | (None, None) => Err(ReplayError::ModeXorViolation),
    }
}

fn mode_label(intent: &SignedIntent) -> &'static str {
    match (intent.sequence, intent.server_challenge.as_ref()) {
        (Some(_), None) => "sequence",
        (None, Some(_)) => "challenge",
        _ => "invalid",
    }
}

fn consume_sequence_mode(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    sequence: u64,
    now_ms: i64,
) -> Result<(), ReplayError> {
    let seq_i64 =
        i64::try_from(sequence).map_err(|_| ReplayError::SequenceOverflow { value: sequence })?;
    let nonce = decode_nonce(&intent.nonce.0).map_err(ReplayError::Sqlite)?;

    let result = tx.execute(
        "INSERT INTO used (operation_id, nonce, issuer, sequence, challenge, committed_at)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
        params![
            intent.operation_id.0,
            nonce,
            intent.issuer.0,
            seq_i64,
            now_ms,
        ],
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(map_sequence_insert_error(e, intent, sequence, tx)),
    }
}

fn consume_challenge_mode(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    challenge_b64: &str,
    now_ms: i64,
) -> Result<(), ReplayError> {
    let challenge_bytes = decode_nonce(challenge_b64).map_err(ReplayError::Sqlite)?;
    let nonce_bytes = decode_nonce(&intent.nonce.0).map_err(ReplayError::Sqlite)?;

    // Atomic single-use consume: DELETE … RETURNING. Empty result ⇒
    // either the challenge was never minted, or already consumed by an
    // earlier call. Distinguish "expired" from "missing" by re-probing
    // the challenge table after the delete returned empty — if we find
    // an expired row, the caller missed the TTL window.
    let consumed: Option<i64> = tx
        .query_row(
            "DELETE FROM outstanding_challenges
              WHERE issuer = ?1 AND challenge = ?2 AND expires_at >= ?3
              RETURNING rowid",
            params![intent.issuer.0, challenge_bytes, now_ms],
            |r| r.get(0),
        )
        .optional()?;

    if consumed.is_none() {
        // Re-check whether the row exists at all (regardless of expiry)
        // to differentiate Missing vs Expired. The row may have been
        // expired-but-not-yet-purged; treat any expires_at < now as
        // explicit expiry.
        let expired: Option<i64> = tx
            .query_row(
                "SELECT expires_at FROM outstanding_challenges
                  WHERE issuer = ?1 AND challenge = ?2",
                params![intent.issuer.0, challenge_bytes],
                |r| r.get(0),
            )
            .optional()?;
        return match expired {
            Some(_) => {
                // Found a row that the DELETE skipped — the only reason
                // the conditional clause excluded it is `expires_at < now`.
                Err(ReplayError::ChallengeExpired {
                    issuer: intent.issuer.0.clone(),
                })
            }
            None => Err(ReplayError::ChallengeMissing {
                issuer: intent.issuer.0.clone(),
            }),
        };
    }

    // Successful single-use consume — record the replay-ledger row.
    let result = tx.execute(
        "INSERT INTO used (operation_id, nonce, issuer, sequence, challenge, committed_at)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
        params![
            intent.operation_id.0,
            nonce_bytes,
            intent.issuer.0,
            challenge_bytes,
            now_ms,
        ],
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(map_challenge_insert_error(e, intent)),
    }
}

/// Translate the `INSERT INTO used` error from sequence mode into a
/// typed [`ReplayError`]. The two failure shapes are: a UNIQUE
/// violation on `(operation_id)` / `(issuer, nonce)` / `(issuer,
/// sequence)` (replay), or a `RAISE(ABORT, …)` from
/// `used_sequence_must_advance` (out-of-order). Anything else
/// propagates as `Sqlite`.
fn map_sequence_insert_error(
    err: rusqlite::Error,
    intent: &SignedIntent,
    sequence: u64,
    tx: &Transaction<'_>,
) -> ReplayError {
    if is_unique_violation(&err) {
        return ReplayError::Duplicate {
            operation_id: intent.operation_id.0.clone(),
        };
    }
    if is_constraint_message(&err, "must strictly advance") {
        let high_water: u64 = tx
            .query_row(
                "SELECT COALESCE(high_water, 0) FROM issuer_seq WHERE issuer = ?1",
                params![intent.issuer.0],
                |r| r.get::<_, i64>(0).map(|v| u64::try_from(v).unwrap_or(0)),
            )
            .unwrap_or(0);
        return ReplayError::OutOfOrder {
            issuer: intent.issuer.0.clone(),
            high_water,
            attempted: sequence,
        };
    }
    ReplayError::Sqlite(err)
}

fn map_challenge_insert_error(err: rusqlite::Error, intent: &SignedIntent) -> ReplayError {
    if is_unique_violation(&err) {
        return ReplayError::Duplicate {
            operation_id: intent.operation_id.0.clone(),
        };
    }
    ReplayError::Sqlite(err)
}

fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    ) && err.to_string().to_lowercase().contains("unique")
}

fn is_constraint_message(err: &rusqlite::Error, needle: &str) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    ) && err.to_string().contains(needle)
}

/// Decode a 16-byte base64 nonce (standard alphabet, optional `==`
/// padding) into raw bytes. The IDL guarantees the shape on the wire;
/// for in-process callers that bypass IDL validation we surface a
/// fail-closed `rusqlite::Error::InvalidParameterName` rather than
/// silently accepting malformed input.
fn decode_nonce(b64: &str) -> Result<Vec<u8>, rusqlite::Error> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("nonce base64 decode: {e}"),
            )))
        })
}

#[cfg(test)]
mod tests;
