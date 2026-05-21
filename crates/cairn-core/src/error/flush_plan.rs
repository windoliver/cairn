//! Error surface for [`crate::domain::flush_plan`].
//!
//! Variants map to sysexits-style CLI exit codes (set in `cairn-cli`):
//!
//! | Variant            | Code              |
//! |--------------------|-------------------|
//! | `Serialize`        | EX_SOFTWARE = 70  |
//! | `Deserialize`      | EX_DATAERR = 65   |
//! | `NotFound`         | EX_NOINPUT = 66   |
//! | `AlreadyTerminal`  | EX_DATAERR = 65   |
//! | `Expired`          | EX_DATAERR = 65   |
//! | `TargetDrift`      | EX_TEMPFAIL = 75  |

use thiserror::Error;

use crate::domain::flush_plan::PlanStatus;
use crate::generated::common::Ulid;

/// Errors raised when planning, persisting, or applying a [`crate::domain::flush_plan::FlushPlan`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FlushPlanError {
    /// Plan → JSON failed.
    #[error("serialize plan: {0}")]
    Serialize(#[source] serde_json::Error),

    /// JSON → plan failed.
    #[error("deserialize plan: {0}")]
    Deserialize(#[source] serde_json::Error),

    /// `cairn flush apply <id>` invoked with no matching pending file.
    #[error("plan {id} not found in pending/")]
    NotFound {
        /// Operation id wire form.
        id: String,
    },

    /// Apply or reject invoked on a plan already in `applied/` or `rejected/`.
    #[error("plan {id} already terminal: {status:?}")]
    AlreadyTerminal {
        /// Operation id wire form.
        id: String,
        /// Terminal state observed.
        status: PlanStatus,
    },

    /// Plan applied past its `expires_at` window.
    #[error("plan {id} expired at {expires_at}")]
    Expired {
        /// Operation id wire form.
        id: String,
        /// RFC 3339 expiry timestamp.
        expires_at: String,
    },

    /// Pre-state hash mismatch — the live record changed between plan
    /// and apply.
    #[error("target drift on {target}: expected hash {expected}, live state hash {actual}")]
    TargetDrift {
        /// Wire form of the [`crate::domain::TargetId`] that drifted.
        target: String,
        /// Hash recorded in the plan.
        expected: String,
        /// Hash observed at apply time.
        actual: String,
    },
}

impl FlushPlanError {
    /// CLI exit code per the sysexits-style table in the module docs.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Serialize(_) => 70,
            Self::Deserialize(_) | Self::AlreadyTerminal { .. } | Self::Expired { .. } => 65,
            Self::NotFound { .. } => 66,
            Self::TargetDrift { .. } => 75,
        }
    }

    /// Convenience constructor — copies the typed `Ulid` into the wire-form `id` field.
    #[must_use]
    pub fn not_found(id: &Ulid) -> Self {
        Self::NotFound { id: id.0.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_table_is_stable() {
        let id = Ulid("01HQZK000000000000000000VP".into());
        assert_eq!(FlushPlanError::not_found(&id).exit_code(), 66);
        let drift = FlushPlanError::TargetDrift {
            target: "rec:abc".into(),
            expected: "00".into(),
            actual: "01".into(),
        };
        assert_eq!(drift.exit_code(), 75);
    }
}
