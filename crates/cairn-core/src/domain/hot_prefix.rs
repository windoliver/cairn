//! Hot-prefix source classification — see issue #83 / brief §7.
//!
//! The hot-memory prefix is invalidated by a watermark counter per
//! source class. Every record-mutating write classifies the touched
//! record(s) and bumps the matching counter(s) inside the same `SQLite`
//! transaction. Cache rows snapshot every watermark at assembly time;
//! a later cache read is a hit only when every field still matches the
//! live counter.

use smallvec::SmallVec;

use crate::domain::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use serde::{Deserialize, Serialize};

/// One bucket of the hot-prefix invalidation tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceClass {
    /// `user`, `feedback`, `entity`, `strategy_*` records that feed
    /// `AutoUserProfile` (§7.1).
    ProfileEvidence,
    /// Records with `pinned = true` (any kind). Read by the
    /// `pinned_feedback` recipe step.
    Pinned,
    /// `project` records. Read by the `top_salience_project` recipe step
    /// regardless of pin status — see §7 of the brief.
    Projects,
    /// `user_signal` records. Read by the `recent_user_signal` recipe
    /// step.
    UserSignals,
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
    pub const ALL: [Self; 8] = [
        Self::ProfileEvidence,
        Self::Pinned,
        Self::Projects,
        Self::UserSignals,
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
            Self::Projects => "projects",
            Self::UserSignals => "user_signals",
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
            "projects" => Some(Self::Projects),
            "user_signals" => Some(Self::UserSignals),
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
    /// Counter for [`SourceClass::Projects`] mutations.
    #[serde(default)]
    pub projects: u64,
    /// Counter for [`SourceClass::UserSignals`] mutations.
    #[serde(default)]
    pub user_signals: u64,
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
            SourceClass::Projects => self.projects += 1,
            SourceClass::UserSignals => self.user_signals += 1,
            SourceClass::PurposeIndex => self.purpose_index += 1,
            SourceClass::Summaries => self.summaries += 1,
            SourceClass::Playbooks => self.playbooks += 1,
            SourceClass::Policy => self.policy += 1,
        }
    }
}

/// Classify a record into the source classes whose watermarks must be
/// bumped when this record is upserted, updated, or tombstoned.
///
/// The returned `SmallVec` is stack-allocated for the common case of ≤2
/// classes; callers are expected to iterate it and call
/// [`SourceWatermarks::bump`] once per class inside the same transaction.
#[must_use]
pub fn classify_record(r: &MemoryRecord) -> SmallVec<[SourceClass; 2]> {
    let mut out = SmallVec::new();
    let pinned = is_pinned(r);
    match r.kind {
        MemoryKind::User
        | MemoryKind::Feedback
        | MemoryKind::Entity
        | MemoryKind::StrategySuccess
        | MemoryKind::StrategyFailure => {
            out.push(SourceClass::ProfileEvidence);
            if pinned {
                out.push(SourceClass::Pinned);
            }
        }
        MemoryKind::Project => {
            // Read by the `top_salience_project` recipe step regardless
            // of pin status. Pin status only matters for `pinned_feedback`
            // (which selects user/feedback), but a pinned Project still
            // bumps `Pinned` defensively in case a future recipe step
            // selects pinned-anything.
            out.push(SourceClass::Projects);
            if pinned {
                out.push(SourceClass::Pinned);
            }
        }
        MemoryKind::Playbook => out.push(SourceClass::Playbooks),
        MemoryKind::UserSignal => out.push(SourceClass::UserSignals),
        _ => {}
    }
    out
}

/// Mirror of `cairn-cli/src/verbs/assemble_hot.rs:457 is_pinned_record`.
/// A record is pinned when it carries the `"pinned"` tag **or** when
/// `extra_frontmatter["pinned"]` is the JSON boolean `true`.
fn is_pinned(r: &MemoryRecord) -> bool {
    r.tags.iter().any(|tag| tag == "pinned")
        || r.extra_frontmatter
            .get("pinned")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::taxonomy::MemoryKind;

    /// Build a test record of the given kind, optionally setting the canonical
    /// "pinned" tag so that `is_pinned` returns true.
    fn rec(kind: MemoryKind, pinned: bool) -> crate::domain::MemoryRecord {
        let mut r = sample_record();
        r.kind = kind;
        if pinned {
            // Canonical pin predicate: tag "pinned" — mirrors
            // cairn-cli/src/verbs/assemble_hot.rs:457 `is_pinned_record`.
            r.tags.push("pinned".to_owned());
        }
        r
    }

    #[test]
    fn classify_user_pinned_emits_profile_and_pinned() {
        let classes = classify_record(&rec(MemoryKind::User, true));
        assert_eq!(
            classes.into_vec(),
            vec![SourceClass::ProfileEvidence, SourceClass::Pinned]
        );
    }

    #[test]
    fn classify_user_unpinned_emits_only_profile_evidence() {
        let classes = classify_record(&rec(MemoryKind::User, false));
        assert_eq!(classes.into_vec(), vec![SourceClass::ProfileEvidence]);
    }

    #[test]
    fn classify_project_pinned_emits_projects_and_pinned() {
        let classes = classify_record(&rec(MemoryKind::Project, true));
        assert_eq!(
            classes.into_vec(),
            vec![SourceClass::Projects, SourceClass::Pinned]
        );
    }

    #[test]
    fn classify_project_unpinned_emits_projects() {
        let classes = classify_record(&rec(MemoryKind::Project, false));
        assert_eq!(classes.into_vec(), vec![SourceClass::Projects]);
    }

    #[test]
    fn classify_playbook_emits_playbooks() {
        let classes = classify_record(&rec(MemoryKind::Playbook, false));
        assert_eq!(classes.into_vec(), vec![SourceClass::Playbooks]);
    }

    #[test]
    fn classify_user_signal_emits_user_signals() {
        let classes = classify_record(&rec(MemoryKind::UserSignal, false));
        assert_eq!(classes.into_vec(), vec![SourceClass::UserSignals]);
    }

    #[test]
    fn classify_unrelated_kind_emits_nothing() {
        // `Reference` is not classified by any hot-memory recipe step.
        let classes = classify_record(&rec(MemoryKind::Reference, false));
        assert!(classes.is_empty());
    }

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
    fn source_class_all_returns_eight_classes() {
        assert_eq!(SourceClass::ALL.len(), 8);
    }

    #[test]
    fn source_class_round_trips_through_db_str() {
        for class in SourceClass::ALL {
            assert_eq!(SourceClass::parse(class.as_db_str()), Some(class));
        }
    }
}
