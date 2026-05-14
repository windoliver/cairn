//! Backup registry domain types for snapshot provenance and rewrite planning.
//!
//! Brief source:
//! - §3 backup registry entries and forget-across-backups rewrite flow

use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Rfc3339Timestamp, TargetId};

/// Immutable registry entry describing one backup artifact and the targets it
/// included at creation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRegistryEntry {
    /// Unique backup identifier.
    pub backup_id: String,
    /// Backup creation instant.
    pub created_at: Rfc3339Timestamp,
    /// Filesystem path to the backup artifact.
    pub artifact_path: String,
    /// Target lineages present in the backup.
    pub target_ids_included: Vec<TargetId>,
}

impl BackupRegistryEntry {
    /// Validate required backup-registry invariants before persistence.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.backup_id.trim().is_empty() {
            return Err(DomainError::EmptyField { field: "backup_id" });
        }
        if self.artifact_path.trim().is_empty() {
            return Err(DomainError::EmptyField {
                field: "artifact_path",
            });
        }
        if self.target_ids_included.is_empty() {
            return Err(DomainError::EmptyField {
                field: "target_ids_included",
            });
        }
        Ok(())
    }
}

/// Deterministic plan for rewriting a backup while dropping forgotten targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewritePlan {
    /// Backup identifier to rewrite.
    pub backup_id: String,
    /// Deterministically sorted, deduplicated target ids to drop.
    pub target_ids: Vec<TargetId>,
}

impl RewritePlan {
    /// Build a deterministic rewrite plan from an arbitrary target list.
    #[must_use]
    pub fn for_targets(backup_id: impl Into<String>, mut target_ids: Vec<TargetId>) -> Self {
        target_ids.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
        target_ids.dedup_by(|left, right| left.as_str() == right.as_str());

        Self {
            backup_id: backup_id.into(),
            target_ids,
        }
    }

    /// Validate required rewrite-plan invariants before execution.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.backup_id.trim().is_empty() {
            return Err(DomainError::EmptyField { field: "backup_id" });
        }
        if self.target_ids.is_empty() {
            return Err(DomainError::EmptyField {
                field: "target_ids",
            });
        }
        Ok(())
    }
}

/// Append-only log entry recording a superseded backup artifact that was
/// shredded after a forget-driven rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShreddedBackupEntry {
    /// Original backup identifier.
    pub backup_id: String,
    /// Filesystem path to the shredded artifact.
    pub artifact_path: String,
    /// Forget operation that caused the shred.
    pub forget_operation_id: String,
    /// When the shred completed.
    pub shredded_at: Rfc3339Timestamp,
}

impl ShreddedBackupEntry {
    /// Build a shredded-backup log entry with the approved task shape.
    #[must_use]
    pub fn new(
        backup_id: impl Into<String>,
        artifact_path: impl Into<String>,
        forget_operation_id: impl Into<String>,
        shredded_at: Rfc3339Timestamp,
    ) -> Self {
        Self {
            backup_id: backup_id.into(),
            artifact_path: artifact_path.into(),
            forget_operation_id: forget_operation_id.into(),
            shredded_at,
        }
    }

    /// Validate required shredded-backup log invariants before persistence.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.backup_id.trim().is_empty() {
            return Err(DomainError::EmptyField { field: "backup_id" });
        }
        if self.artifact_path.trim().is_empty() {
            return Err(DomainError::EmptyField {
                field: "artifact_path",
            });
        }
        if self.forget_operation_id.trim().is_empty() {
            return Err(DomainError::EmptyField {
                field: "forget_operation_id",
            });
        }
        Ok(())
    }
}
