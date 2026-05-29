//! `AdminError` for `cairn.admin.v1` verbs. Wire envelope matches design brief §8.0.b.

use crate::contract::memory_store::StoreError;
use crate::domain::Identity;
use crate::domain::admin::context::AdminRole;
use crate::wal::StepLogError;

/// Errors raised by `cairn.admin.v1` verbs.
///
/// Wire envelope codes match design brief §8.0.b. Exit codes follow brief §7.3
/// (sysexits-style: 64 auth, 69 capability, 70 data, 75 temp-failure, 1 I/O).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdminError {
    /// Admin capability not negotiated in the current `status` advertisement.
    #[error("admin capability not negotiated: {capability}")]
    CapabilityUnavailable {
        /// The capability code that was not advertised.
        capability: String,
        /// Operator remediation hint surfaced in the wire envelope.
        remediation: String,
    },

    /// Caller is not authorized for the requested role.
    #[error("caller {actor} is not authorized for {needed:?}")]
    NotAuthorized {
        /// Verified identity of the caller.
        actor: Identity,
        /// The role the caller asserted but does not hold.
        needed: AdminRole,
    },

    /// Snapshot artifact integrity check failed (hash mismatch).
    #[error("snapshot artifact integrity check failed: expected {expected}, got {actual}")]
    IntegrityMismatch {
        /// Expected digest.
        expected: String,
        /// Actual digest computed during restore.
        actual: String,
    },

    /// Cross-machine restore is not supported in v0.2.
    #[error(
        "snapshot is from machine {snapshot} but local is {local} — cross-machine restore not supported in v0.2"
    )]
    CrossMachineRestore {
        /// Machine fingerprint embedded in the snapshot.
        snapshot: String,
        /// Machine fingerprint of the local host.
        local: String,
    },

    /// Snapshot vault ID does not match the local vault.
    #[error("snapshot vault id {snapshot} != local vault {local}")]
    VaultIdMismatch {
        /// Vault ID embedded in the snapshot.
        snapshot: String,
        /// Vault ID of the local vault.
        local: String,
    },

    /// Snapshot schema version is newer than the local head; refuse forward
    /// restore to prevent data loss.
    #[error("snapshot schema_version {snapshot} > local head {local} — refuse forward restore")]
    SchemaTooNew {
        /// Schema version embedded in the snapshot.
        snapshot: u32,
        /// Highest schema version known locally.
        local: u32,
    },

    /// A connector name was not found in the registry.
    #[error("connector {name} not found in registry")]
    UnknownConnector {
        /// The connector name that was not found.
        name: String,
    },

    /// A WAL step marker was not found in the ledger.
    #[error("WAL step {marker} not found in ledger")]
    UnknownStepMarker {
        /// The step marker that was not found.
        marker: String,
    },

    /// WAL replay halted at a non-idempotent step and was escalated to
    /// `PURGE_PENDING`.
    #[error("WAL replay halted at non-idempotent step {step}; escalated to PURGE_PENDING")]
    ReplayEscalated {
        /// The step identifier at which replay halted.
        step: String,
    },

    /// The live vault changed (a write advanced the WAL frontier) between the
    /// restore's forget-purge and the swap, so the purge may not reflect the
    /// latest forgets. Restore aborts before swapping rather than risk
    /// resurrecting a concurrently-forgotten record (round-6 review #2).
    #[error(
        "live vault changed during restore (frontier {before} -> {after}); \
         aborted before swap — retry with writers quiesced"
    )]
    RestoreConflict {
        /// WAL frontier captured before the purge.
        before: String,
        /// WAL frontier observed just before the swap.
        after: String,
    },

    /// Underlying store error (propagated from a `MemoryStore` call).
    ///
    /// `StoreError` is `Box<dyn Error + Send + Sync + 'static>` at the
    /// `cairn-core` trait boundary; the concrete adapter type is erased here
    /// so `cairn-core` stays adapter-free.
    #[error("store error: {0}")]
    Store(StoreError),

    /// WAL step-log error (propagated from a `StepLog` call).
    #[error(transparent)]
    Wal(#[from] StepLogError),
}

impl AdminError {
    /// Map to brief §8.0.b wire envelope code string.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::CapabilityUnavailable { .. } => "CapabilityUnavailable",
            Self::NotAuthorized { .. } => "NotAuthorized",
            Self::IntegrityMismatch { .. } => "IntegrityMismatch",
            Self::CrossMachineRestore { .. } => "CrossMachineRestore",
            Self::VaultIdMismatch { .. } => "VaultIdMismatch",
            Self::SchemaTooNew { .. } => "SchemaTooNew",
            Self::UnknownConnector { .. } => "UnknownConnector",
            Self::UnknownStepMarker { .. } => "UnknownStepMarker",
            Self::ReplayEscalated { .. } => "ReplayEscalated",
            Self::RestoreConflict { .. } => "RestoreConflict",
            Self::Store(_) => "StoreError",
            Self::Wal(_) => "WalError",
        }
    }

    /// CLI exit code per spec §7.3.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::CapabilityUnavailable { .. } => 69,
            Self::NotAuthorized { .. } => 64,
            Self::IntegrityMismatch { .. }
            | Self::CrossMachineRestore { .. }
            | Self::VaultIdMismatch { .. }
            | Self::SchemaTooNew { .. }
            | Self::UnknownStepMarker { .. }
            | Self::UnknownConnector { .. } => 70,
            // 75 == EX_TEMPFAIL: replay escalation and a raced restore are both
            // transient — the operator may retry.
            Self::ReplayEscalated { .. } | Self::RestoreConflict { .. } => 75,
            Self::Store(_) | Self::Wal(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        #[allow(clippy::expect_used)]
        let actor = Identity::parse("hmn:x").expect("test fixture: known-valid identity");

        assert_eq!(
            64,
            AdminError::NotAuthorized {
                actor,
                needed: AdminRole::Operator,
            }
            .exit_code()
        );
        assert_eq!(
            69,
            AdminError::CapabilityUnavailable {
                capability: "x".into(),
                remediation: "y".into(),
            }
            .exit_code()
        );
        assert_eq!(
            75,
            AdminError::ReplayEscalated {
                step: "step:1".into(),
            }
            .exit_code()
        );
        assert_eq!(
            70,
            AdminError::IntegrityMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }
            .exit_code(),
        );
        assert_eq!(
            1,
            AdminError::Store(Box::new(std::io::Error::other("x"))).exit_code(),
        );
    }

    #[test]
    fn codes_are_pascal_case() {
        let cases = [
            AdminError::CapabilityUnavailable {
                capability: "x".into(),
                remediation: "y".into(),
            },
            AdminError::IntegrityMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            AdminError::CrossMachineRestore {
                snapshot: "a".into(),
                local: "b".into(),
            },
        ];
        for e in cases {
            let c = e.code();
            #[allow(clippy::expect_used)]
            let first = c
                .chars()
                .next()
                .expect("test fixture: capability codes are non-empty");
            assert!(first.is_ascii_uppercase(), "{c}");
        }
    }
}
