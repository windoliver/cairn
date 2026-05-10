//! Hot-prefix source classification — see issue #83 / brief §7.
//!
//! The hot-memory prefix is invalidated by a watermark counter per
//! source class. Every record-mutating write classifies the touched
//! record(s) and bumps the matching counter(s) inside the same `SQLite`
//! transaction. Cache rows snapshot all six watermarks at assembly
//! time; a later cache read is a hit only when every field still
//! matches the live counter.

use serde::{Deserialize, Serialize};

/// One bucket of the hot-prefix invalidation tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceClass {
    /// `user`, `feedback`, `entity`, `strategy` records that feed
    /// `AutoUserProfile` (§7.1).
    ProfileEvidence,
    /// Records with `pinned = true`.
    Pinned,
    /// Vault-root `purpose.md` and `index.md`.
    PurposeIndex,
    /// `_summary.md` files at any folder depth.
    Summaries,
    /// `playbook` records.
    Playbooks,
    /// `.cairn/config.yaml` `hot_memory.*` keys.
    Policy,
}

impl SourceClass {
    /// All variants in stable iteration order.
    pub const ALL: [Self; 6] = [
        Self::ProfileEvidence,
        Self::Pinned,
        Self::PurposeIndex,
        Self::Summaries,
        Self::Playbooks,
        Self::Policy,
    ];

    /// Snake-case key used in `SQLite` + JSONL serialisations.
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::ProfileEvidence => "profile_evidence",
            Self::Pinned => "pinned",
            Self::PurposeIndex => "purpose_index",
            Self::Summaries => "summaries",
            Self::Playbooks => "playbooks",
            Self::Policy => "policy",
        }
    }

    /// Inverse of [`Self::as_db_str`]; `None` on unknown input.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "profile_evidence" => Some(Self::ProfileEvidence),
            "pinned" => Some(Self::Pinned),
            "purpose_index" => Some(Self::PurposeIndex),
            "summaries" => Some(Self::Summaries),
            "playbooks" => Some(Self::Playbooks),
            "policy" => Some(Self::Policy),
            _ => None,
        }
    }
}

/// Snapshot of every source-class watermark.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceWatermarks {
    /// Counter for [`SourceClass::ProfileEvidence`] mutations.
    pub profile_evidence: u64,
    /// Counter for [`SourceClass::Pinned`] mutations.
    pub pinned: u64,
    /// Counter for [`SourceClass::PurposeIndex`] mutations.
    pub purpose_index: u64,
    /// Counter for [`SourceClass::Summaries`] mutations.
    pub summaries: u64,
    /// Counter for [`SourceClass::Playbooks`] mutations.
    pub playbooks: u64,
    /// Counter for [`SourceClass::Policy`] mutations.
    pub policy: u64,
}

impl SourceWatermarks {
    /// Field-wise equality.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self == other
    }

    /// Increment the counter for `class`.
    pub fn bump(&mut self, class: SourceClass) {
        match class {
            SourceClass::ProfileEvidence => self.profile_evidence += 1,
            SourceClass::Pinned => self.pinned += 1,
            SourceClass::PurposeIndex => self.purpose_index += 1,
            SourceClass::Summaries => self.summaries += 1,
            SourceClass::Playbooks => self.playbooks += 1,
            SourceClass::Policy => self.policy += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermarks_match_is_reflexive() {
        let w = SourceWatermarks::default();
        assert!(w.matches(&w));
    }

    #[test]
    fn watermarks_match_breaks_when_any_field_diverges() {
        let base = SourceWatermarks::default();
        for class in SourceClass::ALL {
            let mut other = base;
            other.bump(class);
            assert!(
                !base.matches(&other),
                "class {class:?} did not invalidate match"
            );
        }
    }

    #[test]
    fn source_class_all_returns_six_classes() {
        assert_eq!(SourceClass::ALL.len(), 6);
    }

    #[test]
    fn source_class_round_trips_through_db_str() {
        for class in SourceClass::ALL {
            assert_eq!(SourceClass::parse(class.as_db_str()), Some(class));
        }
    }
}
