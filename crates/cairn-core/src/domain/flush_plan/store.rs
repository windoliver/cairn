//! Pure path helpers for the `.cairn/flush/` layout. No I/O — adapter
//! crates do the actual file writes.
//!
//! Layout (brief §5.5):
//!
//! ```text
//! <vault>/.cairn/flush/
//! ├── pending/   <id>.plan.json   <id>.diff.md
//! ├── applied/   <id>.plan.json
//! └── rejected/  <id>.plan.json
//! ```

use std::path::{Path, PathBuf};

use crate::generated::common::Ulid;

/// Bucket inside `.cairn/flush/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Plans that have been created but not yet applied or rejected.
    Pending,
    /// Plans that have been successfully applied.
    Applied,
    /// Plans that have been explicitly rejected.
    Rejected,
}

impl Bucket {
    /// Returns the directory name for this bucket.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }

    /// All buckets, ordered for `flush list --all` display.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Pending, Self::Applied, Self::Rejected]
    }
}

/// Root: `<vault>/.cairn/flush`.
#[must_use]
pub fn root(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("flush")
}

/// `<vault>/.cairn/flush/<bucket>/`.
#[must_use]
pub fn bucket_dir(vault_root: &Path, bucket: Bucket) -> PathBuf {
    root(vault_root).join(bucket.dir_name())
}

/// `<vault>/.cairn/flush/<bucket>/<operation_id>.plan.json`.
#[must_use]
pub fn plan_path(vault_root: &Path, bucket: Bucket, operation_id: &Ulid) -> PathBuf {
    bucket_dir(vault_root, bucket).join(format!("{}.plan.json", operation_id.0))
}

/// `<vault>/.cairn/flush/pending/<operation_id>.diff.md`. Diff sidecar
/// only lives in `pending/`; apply / reject delete it on transition.
#[must_use]
pub fn diff_path(vault_root: &Path, operation_id: &Ulid) -> PathBuf {
    bucket_dir(vault_root, Bucket::Pending).join(format!("{}.diff.md", operation_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ulid(s: &str) -> Ulid {
        Ulid(s.to_owned())
    }

    #[test]
    fn plan_path_layout_is_stable() {
        let root = PathBuf::from("/tmp/v");
        let id = ulid("01HQZK000000000000000000VP");
        assert_eq!(
            plan_path(&root, Bucket::Pending, &id),
            PathBuf::from("/tmp/v/.cairn/flush/pending/01HQZK000000000000000000VP.plan.json")
        );
        assert_eq!(
            plan_path(&root, Bucket::Applied, &id),
            PathBuf::from("/tmp/v/.cairn/flush/applied/01HQZK000000000000000000VP.plan.json")
        );
        assert_eq!(
            plan_path(&root, Bucket::Rejected, &id),
            PathBuf::from("/tmp/v/.cairn/flush/rejected/01HQZK000000000000000000VP.plan.json")
        );
    }

    #[test]
    fn diff_path_lives_in_pending() {
        let root = PathBuf::from("/tmp/v");
        let id = ulid("01HQZK000000000000000000VP");
        assert_eq!(
            diff_path(&root, &id),
            PathBuf::from("/tmp/v/.cairn/flush/pending/01HQZK000000000000000000VP.diff.md")
        );
    }
}
