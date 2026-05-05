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

    /// `INSERT INTO wal_ops` hit `ON CONFLICT DO NOTHING` but the
    /// pre-existing row was staged for a *different* envelope (mismatched
    /// `kind` / `signature` / `target_hash` / `envelope` / `scope_json`).
    /// Admitting under that row would consume sequence-or-challenge
    /// state against an unrelated WAL admission. Surface as a typed
    /// reject so the caller cannot silently advance replay state.
    #[error("wal_ops row for operation_id {operation_id} was prepared for a different envelope")]
    OperationMismatch {
        /// The conflicting `operation_id` ULID.
        operation_id: String,
    },

    /// Signed intent's `expires_at` lies at or before the admission
    /// clock. The verifier already enforced expiry at verification
    /// time, but a `VerifiedSignedIntent` token can outlive its
    /// signed window — queueing delay, writer-lock contention, or a
    /// caller that holds the proof. Re-check at the store boundary
    /// so the §4.2 leaked-key blast-radius bound holds end-to-end.
    /// Maps to the IDL wire code `ExpiredIntent` (round-10 review #1).
    #[error(
        "signed intent expired at admit: operation_id {operation_id} expires_at_ms={expires_at_ms} now_ms={now_ms}"
    )]
    IntentExpired {
        /// Envelope `operation_id` ULID.
        operation_id: String,
        /// Parsed `expires_at` unix-ms.
        expires_at_ms: i64,
        /// Trusted admission clock unix-ms.
        now_ms: i64,
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
///
/// Signed-payload columns (`scope_json`, `expires_at_ms`,
/// `envelope_json`) are **derived from the verified intent**, not
/// taken from the caller — round-4 review #2 closed the path where a
/// verified envelope could be staged with mismatched authorization
/// metadata. Likewise, the admission timestamp `now_ms` is **not**
/// in this struct: production callers go through
/// [`crate::store::tx::StoreTx::prepare_wal_with_replay`] which
/// derives the trusted store clock internally; tests inject a clock
/// via [`test_helpers::prepare_wal_with_replay`].
/// (Round-7 review #1.)
#[derive(Debug, Clone)]
pub struct WalPrepareInputs<'a> {
    /// `wal_ops.kind` — must be one of the closed CHECK values from
    /// migration 0002 (widened by 0041). Caller picks based on which
    /// verb produced the envelope.
    pub kind: &'a str,
    /// Optional `plan_ref` ULID — set when the verb stages a plan blob.
    pub plan_ref: Option<&'a str>,
}

/// Atomically admit a verified envelope: insert the WAL `PREPARED` row,
/// then consume the replay ledger entry. Both writes happen inside the
/// caller's transaction; rolling back drops both.
///
/// # Caller contract: bind `inputs.kind` / `inputs.plan_ref` to the signed payload
///
/// The IDL `SignedIntent` schema covers `target_hash` (a sha256 of the
/// record/plan/receipt) but **does not** include a verb-action /
/// `kind` field in the signed bytes (brief §4.2). This function copies
/// `inputs.kind` and `inputs.plan_ref` into `wal_ops` verbatim; it
/// cannot prove the issuer authorized the specific WAL action.
///
/// **The verb-layer caller is responsible for binding `inputs.kind`
/// and `inputs.plan_ref` to the `target_hash` it computed before
/// admitting.** Concretely: the caller derives `target_hash` over the
/// canonical bytes of the record / plan / receipt that this `kind`
/// produces, asserts the verifier's `intent.target_hash ==
/// computed_target_hash`, and only then calls this function. A future
/// IDL extension may move `kind` into the signed payload (§13.5
/// codegen) and let the store enforce the binding directly; round-11
/// review #1 documented as a known mitigation-by-contract.
///
/// # Caller contract: do not enable `feature = "test-helpers"` in production
///
/// `cairn_store_sqlite::open_sync` is feature-gated for tests. The
/// production `open` path stays unchanged. Round-11 review #2 closed
/// the previous public-via-feature replay-admit bypass; the only
/// raw-intent entry point is now `pub(crate)` `#[cfg(test)]`.
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
pub(crate) fn prepare_wal_with_replay(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    inputs: &WalPrepareInputs<'_>,
    now_ms: i64,
) -> Result<(), ReplayError> {
    // Defensive XOR check before any write — see ReplayError::ModeXorViolation.
    if (u8::from(intent.sequence.is_some()) + u8::from(intent.server_challenge.is_some())) != 1 {
        return Err(ReplayError::ModeXorViolation);
    }

    // Round-6 review #1: wrap the admit body in a SAVEPOINT so partial
    // writes (wal_ops PREPARED, wal_op_deps edges) cannot survive a
    // later replay-ledger error if the caller swallows our `Err` and
    // still commits the outer transaction. SQLite SAVEPOINTs are
    // nested transactions: `ROLLBACK TO` undoes only the savepoint's
    // changes; `RELEASE` makes them part of the parent on success.
    tx.execute_batch("SAVEPOINT replay_admit")?;
    match prepare_wal_with_replay_body(tx, intent, inputs, now_ms) {
        Ok(()) => {
            tx.execute_batch("RELEASE replay_admit")?;
            Ok(())
        }
        Err(e) => {
            // Best-effort rollback of the savepoint — even if this
            // fails (e.g., the connection is poisoned), surface the
            // original replay error rather than masking it. The outer
            // tx is left in a state where committing would still drop
            // the savepoint's writes via the parent rollback the
            // caller is expected to perform on Err.
            let _ = tx.execute_batch("ROLLBACK TO replay_admit");
            let _ = tx.execute_batch("RELEASE replay_admit");
            Err(e)
        }
    }
}

fn prepare_wal_with_replay_body(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    inputs: &WalPrepareInputs<'_>,
    now_ms: i64,
) -> Result<(), ReplayError> {
    // Round-4 review #2: derive the signed-payload columns from the
    // intent itself. Caller cannot widen the scope or extend the TTL
    // beyond what the issuer signed.
    let scope_json = canonical_scope_json(&intent.scope);
    let envelope_json =
        serde_json::to_string(intent).map_err(|e| ReplayError::Sqlite(rusqlite_codec_err(&e)))?;
    let expires_at_ms = parse_rfc3339_to_ms(&intent.expires_at)?;

    // Round-10 review #1: verifier-time expiry is not enough. A
    // `VerifiedSignedIntent` token can outlive its signed window
    // (queueing, writer-lock contention, caller holding the proof).
    // Re-check the trusted admission clock against the signed
    // `expires_at` so the §4.2 leaked-key blast-radius bound holds
    // end-to-end. Use exclusive upper-bound semantics matching the
    // verifier's `now >= expires_chrono`.
    if now_ms >= expires_at_ms {
        return Err(ReplayError::IntentExpired {
            operation_id: intent.operation_id.0.clone(),
            expires_at_ms,
            now_ms,
        });
    }

    // Idempotent on operation_id — a retry after a crash that already
    // committed the wal_ops row finds the conflict and continues. The
    // replay-ledger consume below is the source of truth for "this
    // envelope has been admitted", so it is fine for wal_ops to be a
    // no-op here AS LONG AS the existing row was prepared for the same
    // signed intent. Otherwise the replay-ledger consume would happily
    // bind sequence/challenge state to an unrelated wal_ops row whose
    // kind/envelope/target_hash/signature differ from this intent.
    //
    // Round-8 review #1: `issued_seq` is allocated by reading
    // `MAX(issued_seq) + 1` and inserting under a deferred SQLite
    // transaction. Two concurrent admissions on independent
    // connections can read the same MAX before either has acquired
    // the WAL writer lock; the loser's INSERT then trips the
    // `wal_ops_issued_seq_must_advance` trigger as a constraint
    // violation. Retry the read+insert pair a bounded number of
    // times so legitimate mixed-issuer traffic does not surface
    // `ReplayError::Sqlite` to the verb layer for what is really a
    // temporary writer-lock race. Bounded to 16 attempts — enough
    // headroom for high-fan-in workloads, far below any deadlock
    // budget.
    let inserted = insert_wal_ops_with_issued_seq_retry(
        tx,
        intent,
        inputs,
        &envelope_json,
        &scope_json,
        expires_at_ms,
        now_ms,
    )?;

    if inserted == 0 {
        // Conflict — confirm the existing row is for the *same* intent
        // AND in an admissible state before letting consume proceed.
        // Round-7 review #2: also verify state == 'PREPARED' (not
        // committed/aborted/rejected), expires_at + plan_ref match
        // the derived values, and the existing wal_op_deps row set
        // matches `intent.chain_parents` exactly. A retry against a
        // terminal-state row, a divergent plan, or a pruned dependency
        // edge is treated as `OperationMismatch` — replay state stays
        // unconsumed.
        verify_existing_wal_op_matches(
            tx,
            intent,
            inputs.kind,
            &envelope_json,
            &scope_json,
            inputs.plan_ref,
            expires_at_ms,
        )?;
    } else {
        // Fresh row — persist the signed `chain_parents` DAG so the
        // §5.6 WAL recovery / commit scheduler honours the partial
        // ordering the issuer signed. The 0002 schema enforces strict
        // `issued_seq` precedence and acyclicity via triggers; an
        // unknown parent fails the FK fail-closed (round-5 review #1).
        for parent in &intent.chain_parents {
            tx.execute(
                "INSERT INTO wal_op_deps (operation_id, depends_on_op_id) \
                 VALUES (?1, ?2)",
                params![intent.operation_id.0, parent.0],
            )?;
        }
    }

    consume_intent(tx, intent, now_ms)
}

/// On `INSERT … ON CONFLICT DO NOTHING` no-op, ensure the row that
/// blocked us was prepared for *this* envelope. Compares the
/// envelope-immutable columns the IDL/§4.2 defines as part of the
/// signed intent: `kind`, `issuer`, `target_hash`, `signature`,
/// `envelope`, and `scope_json`. Anything else is suspicious — even
/// a same-issuer same-`operation_id` row with a different signature
/// is a different signed message.
/// Read `MAX(issued_seq) + 1` and insert the `wal_ops` row, retrying
/// the pair on the `wal_ops_issued_seq_must_advance` /
/// UNIQUE-issued_seq race that loses to a concurrent writer on a
/// different `SQLite` connection. See `is_issued_seq_race` for the
/// detection logic.
#[allow(clippy::too_many_arguments)]
fn insert_wal_ops_with_issued_seq_retry(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    inputs: &WalPrepareInputs<'_>,
    envelope_json: &str,
    scope_json: &str,
    expires_at_ms: i64,
    now_ms: i64,
) -> Result<usize, ReplayError> {
    const MAX_ISSUED_SEQ_RETRIES: u32 = 16;
    for attempt in 0..=MAX_ISSUED_SEQ_RETRIES {
        let issued_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
            [],
            |r| r.get(0),
        )?;

        let result = tx.execute(
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
                envelope_json,
                intent.issuer.0,
                intent.target_hash,
                scope_json,
                inputs.plan_ref,
                expires_at_ms,
                intent.signature.0,
                now_ms,
            ],
        );

        match result {
            Ok(n) => return Ok(n),
            Err(e) if is_issued_seq_race(&e) && attempt < MAX_ISSUED_SEQ_RETRIES => {
                // Re-read MAX(issued_seq) on next iteration.
            }
            Err(e) => return Err(ReplayError::Sqlite(e)),
        }
    }
    // Loop body always either returns or continues, so this is unreachable.
    unreachable!("issued_seq retry loop returned without resolving");
}

#[allow(clippy::too_many_arguments)]
fn verify_existing_wal_op_matches(
    tx: &Transaction<'_>,
    intent: &SignedIntent,
    expected_kind: &str,
    expected_envelope_json: &str,
    expected_scope_json: &str,
    expected_plan_ref: Option<&str>,
    expected_expires_at_ms: i64,
) -> Result<(), ReplayError> {
    // Tuple form: (kind, issuer, target_hash, signature, envelope, scope_json, state, plan_ref, expires_at)
    type ExistingWalOpRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
    );
    let row: Option<ExistingWalOpRow> = tx
        .query_row(
            "SELECT kind, issuer, target_hash, signature, envelope, scope_json,
                    state, plan_ref, expires_at
               FROM wal_ops
              WHERE operation_id = ?1",
            params![intent.operation_id.0],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        kind,
        issuer,
        target_hash,
        signature,
        envelope,
        scope_json,
        state,
        plan_ref,
        expires_at,
    )) = row
    else {
        // Should not happen — INSERT returned 0 yet the row is gone.
        // Treat as opaque SQL anomaly so the caller's transaction
        // rolls back rather than silently advancing replay state.
        return Err(ReplayError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
    };
    let plan_matches = match (plan_ref.as_deref(), expected_plan_ref) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if state != "PREPARED"
        || kind != expected_kind
        || issuer != intent.issuer.0
        || target_hash != intent.target_hash
        || signature != intent.signature.0
        || envelope != expected_envelope_json
        || scope_json != expected_scope_json
        || expires_at != expected_expires_at_ms
        || !plan_matches
    {
        return Err(ReplayError::OperationMismatch {
            operation_id: intent.operation_id.0.clone(),
        });
    }

    // Verify the dependency edges match the signed `chain_parents`
    // exactly — order-insensitive set comparison since the table has
    // no inherent ordering.
    let mut stored: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut stmt =
        tx.prepare("SELECT depends_on_op_id FROM wal_op_deps WHERE operation_id = ?1")?;
    let mut rows = stmt.query(params![intent.operation_id.0])?;
    while let Some(r) = rows.next()? {
        stored.insert(r.get(0)?);
    }
    let signed: std::collections::BTreeSet<String> =
        intent.chain_parents.iter().map(|p| p.0.clone()).collect();
    if stored != signed {
        return Err(ReplayError::OperationMismatch {
            operation_id: intent.operation_id.0.clone(),
        });
    }

    Ok(())
}

/// Canonical JSON for `intent.scope` — field-ordered, no whitespace.
/// Pinning the field order keeps the `wal_ops.scope_json` column
/// stable across re-serialisations so [`verify_existing_wal_op_matches`]
/// can do a byte-equal comparison on retry.
fn canonical_scope_json(scope: &cairn_core::generated::envelope::SignedIntentScope) -> String {
    use cairn_core::generated::envelope::SignedIntentScopeTier;
    let tier = match scope.tier {
        SignedIntentScopeTier::Private => "private",
        SignedIntentScopeTier::Session => "session",
        SignedIntentScopeTier::Project => "project",
        SignedIntentScopeTier::Team => "team",
        SignedIntentScopeTier::Org => "org",
        SignedIntentScopeTier::Public => "public",
        // The IDL-generated enum is `#[non_exhaustive]`. Future tier
        // additions land via codegen; treat them as unknown until the
        // canonicaliser is updated explicitly.
        _ => "unknown",
    };
    // Hand-rolled to keep field order deterministic — the IDL fixes
    // `entity, tenant, tier, workspace` (alphabetical), and the wire
    // schema is `serde(deny_unknown_fields)` so consumers parse this
    // back without surprises. JSON-escape the strings via serde_json.
    format!(
        r#"{{"entity":{e},"tenant":{tn},"tier":"{ti}","workspace":{w}}}"#,
        e = serde_json::Value::String(scope.entity.clone()),
        tn = serde_json::Value::String(scope.tenant.clone()),
        ti = tier,
        w = serde_json::Value::String(scope.workspace.clone()),
    )
}

/// Wrap a `serde_json::Error` as a [`rusqlite::Error`] so call sites
/// can keep the single `ReplayError::Sqlite` arm.
fn rusqlite_codec_err(err: &serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("serde_json: {err}"),
    )))
}

/// Parse an RFC-3339 timestamp into unix-ms. The IDL guarantees the
/// shape on the wire; in-process callers that bypass IDL validation
/// surface as [`ReplayError::Sqlite`].
fn parse_rfc3339_to_ms(s: &str) -> Result<i64, ReplayError> {
    use chrono::DateTime;
    let dt: DateTime<chrono::FixedOffset> = DateTime::parse_from_rfc3339(s).map_err(|e| {
        ReplayError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("expires_at parse: {e}"),
            ),
        )))
    })?;
    Ok(dt.timestamp_millis())
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
pub(crate) fn consume_intent(
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

/// Is this the per-trigger / per-UNIQUE error that signals a
/// concurrent `issued_seq` allocation lost the race against another
/// connection? Both surfaces converge on the same root cause and the
/// caller's retry loop handles them identically.
fn is_issued_seq_race(err: &rusqlite::Error) -> bool {
    let msg = err.to_string();
    is_constraint_message(err, "wal_ops.issued_seq must strictly advance")
        || (matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        ) && msg.to_lowercase().contains("issued_seq")
            && msg.to_lowercase().contains("unique"))
}

/// Decode a 16-byte base64 nonce into raw bytes. The IDL
/// `Nonce16Base64` admits **both** the 22-char unpadded form and the
/// 24-char padded form — pick the right base64 engine so both wire
/// shapes round-trip to the same 16 bytes. After decoding we assert
/// the length is exactly 16 to defend against in-process callers that
/// bypass IDL validation. A length / decode failure surfaces as a
/// `rusqlite::Error::ToSqlConversionFailure` so the caller's
/// `ReplayError::Sqlite` arm preserves the diagnostic.
fn decode_nonce(b64: &str) -> Result<Vec<u8>, rusqlite::Error> {
    let raw = match b64.len() {
        22 => base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64),
        24 => base64::engine::general_purpose::STANDARD.decode(b64),
        n => {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("nonce base64 decode: unexpected length {n} (want 22 or 24)"),
                ),
            )));
        }
    }
    .map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("nonce base64 decode: {e}"),
        )))
    })?;
    if raw.len() != 16 {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("nonce base64 decode: decoded {} bytes (want 16)", raw.len()),
            ),
        )));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod concurrency_tests;

#[cfg(test)]
mod handshake_roundtrip_tests;
