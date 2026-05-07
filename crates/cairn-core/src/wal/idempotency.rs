//! WAL idempotency primitives — operation IDs, step keys, and the
//! `StepLog` contract the `SQLite` adapter implements.

use std::fmt;

use thiserror::Error;

use crate::wal::fsm::StepState;

/// Errors from [`OperationId::parse`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OperationIdError {
    /// Empty input.
    #[error("operation id must not be empty")]
    Empty,
    /// Input exceeded `OperationId::MAX_LEN` characters.
    #[error("operation id exceeds maximum length ({MAX_LEN})", MAX_LEN = OperationId::MAX_LEN)]
    TooLong,
}

/// Idempotency key for a WAL op. ULID-shaped strings produced by the issuer
/// (e.g. `op-01HQZ...`); we don't enforce ULID format here because the
/// `lint_repair` helper produces UUID-flavoured ids and graph helpers use
/// their own scheme. The only invariants are non-empty and length-bounded.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OperationId(String);

impl OperationId {
    /// Maximum length in characters. Generous; matches `wal_ops.operation_id
    /// TEXT NOT NULL` with no DB-side cap.
    pub const MAX_LEN: usize = 128;

    /// Parse a string as an `OperationId`.
    ///
    /// # Errors
    /// - [`OperationIdError::Empty`] if `raw` is empty.
    /// - [`OperationIdError::TooLong`] if `raw` exceeds [`Self::MAX_LEN`].
    pub fn parse(raw: impl Into<String>) -> Result<Self, OperationIdError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(OperationIdError::Empty);
        }
        if raw.chars().count() > Self::MAX_LEN {
            return Err(OperationIdError::TooLong);
        }
        Ok(Self(raw))
    }

    /// Borrow the wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the underlying `String`.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OperationId").field(&self.0).finish()
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Composite key identifying one row in `wal_steps`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepKey {
    /// FK into `wal_ops.operation_id`.
    pub operation_id: OperationId,
    /// 0-based step ordinal (matches `wal_steps.step_ord`).
    pub step_ord: u32,
}

impl StepKey {
    /// Construct a new `StepKey`.
    #[must_use]
    pub fn new(operation_id: OperationId, step_ord: u32) -> Self {
        Self {
            operation_id,
            step_ord,
        }
    }
}

/// Errors from [`StepLog`] implementors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StepLogError {
    /// Underlying storage failed.
    #[error("step log storage error: {0}")]
    Storage(String),
    /// A transition rejected by the §5.6 step FSM (or the `SQLite` trigger
    /// that enforces it).
    #[error("illegal step transition for {key:?}")]
    IllegalTransition {
        /// The key whose transition was rejected.
        key: StepKey,
    },
}

/// Contract the `SQLite` adapter implements; pure-test impls live in
/// `cairn-test-fixtures` (added in a follow-up).
///
/// The trait deliberately exposes only the four methods the runner needs.
/// Reading the full step set (for recovery) is on the recovery path, not
/// the runner path, and uses a separate adapter API.
pub trait StepLog {
    /// Returns the current state of the step, or `None` if no row exists.
    ///
    /// # Errors
    /// Storage failure.
    fn step_state(&self, key: &StepKey) -> Result<Option<StepState>, StepLogError>;

    /// Mark the step as `PENDING` and increment its `attempts` counter.
    /// Inserts the row if absent.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_attempt(&mut self, key: &StepKey, attempt: u32) -> Result<(), StepLogError>;

    /// Mark the step as `DONE`.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_done(&mut self, key: &StepKey) -> Result<(), StepLogError>;

    /// Mark the step as `FAILED` with the given error message.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_failed(&mut self, key: &StepKey, err: &str) -> Result<(), StepLogError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_empty() {
        assert!(matches!(
            OperationId::parse(""),
            Err(OperationIdError::Empty)
        ));
    }

    #[test]
    fn parse_rejects_too_long() {
        let too_long: String = "x".repeat(OperationId::MAX_LEN + 1);
        assert!(matches!(
            OperationId::parse(too_long),
            Err(OperationIdError::TooLong)
        ));
    }

    #[test]
    fn parse_accepts_typical_op_id() {
        let id = OperationId::parse("op-01HQZ12345").expect("typical id parses");
        assert_eq!(id.as_str(), "op-01HQZ12345");
    }

    #[test]
    fn parse_accepts_max_len() {
        let max: String = "x".repeat(OperationId::MAX_LEN);
        assert!(OperationId::parse(max).is_ok());
    }

    #[test]
    fn step_key_eq_round_trip() {
        let a = StepKey::new(OperationId::parse("op-1").expect("ok"), 3);
        let b = StepKey::new(OperationId::parse("op-1").expect("ok"), 3);
        assert_eq!(a, b);
    }
}
