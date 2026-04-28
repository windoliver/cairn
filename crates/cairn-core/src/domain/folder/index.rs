//! Folder aggregation + `_index.md` projection.

use std::path::PathBuf;

use crate::contract::memory_store::StoredRecord;
use crate::domain::folder::links::Backlink;
use crate::domain::folder::policy::EffectivePolicy;
use crate::domain::record::RecordId;
use crate::domain::{MemoryKind, Rfc3339Timestamp};

/// Per-record summary line for a folder index.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordEntry {
    /// Vault-relative path of the record file.
    pub path: PathBuf,
    /// Record id.
    pub id: RecordId,
    /// Memory kind.
    pub kind: MemoryKind,
    /// Last-update timestamp from the stored record.
    pub updated_at: Rfc3339Timestamp,
    /// Backlinks pointing at this record.
    pub backlink_count: u32,
}

/// Per-subfolder aggregate row.
#[derive(Debug, Clone, PartialEq)]
pub struct SubfolderEntry {
    /// Subfolder name (basename, no trailing slash).
    pub name: String,
    /// Number of records inside the subtree.
    pub record_count: u32,
    /// Latest `updated_at` across the subtree.
    pub last_updated: Option<Rfc3339Timestamp>,
}

/// Aggregated state for one folder, ready to project as `_index.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderState {
    /// Vault-relative folder path.
    pub path: PathBuf,
    /// Records living directly in this folder, sorted by kind then id.
    pub records: Vec<StoredRecord>,
    /// Subfolders, sorted by name.
    pub subfolders: Vec<SubfolderEntry>,
    /// Backlinks targeting any record in this folder, sorted by source path.
    pub backlinks: Vec<Backlink>,
    /// Resolved effective policy at this folder.
    pub effective_policy: EffectivePolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_state_compiles_with_default_policy() {
        let _ = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
    }
}
