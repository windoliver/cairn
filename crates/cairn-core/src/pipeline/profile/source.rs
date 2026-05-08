//! Slim projections fed into the [`super::synthesize()`] function.
//!
//! Adapter crates (`cairn-store-sqlite`, etc.) are responsible for
//! producing these from active, non-tombstoned, consent-cleared records
//! — the synthesizer treats whatever it is given as authoritative.

use crate::domain::{RecordId, Rfc3339Timestamp};

/// Seven `key_facts` buckets defined by brief §7.1.
///
/// The bucket assignment is the adapter's responsibility (decided from
/// `kind`, `class`, tags, vault path, etc.); the synthesizer only routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum KeyFactFacet {
    /// Hardware the user works on (laptop, phone, desk monitor).
    Devices,
    /// Software / tools the user has standardized on.
    Software,
    /// Stable preferences (response style, naming, formatting).
    Preferences,
    /// Active blockers in progress (only fed from records observed on
    /// ≥ 2 distinct days per the brief's evidence gate; the gate itself
    /// is the adapter's responsibility).
    CurrentIssues,
    /// Resolved historical issues — kept for context, not as live work.
    AddressedIssues,
    /// Patterns the agent has seen repeat over time.
    RecurringIssues,
    /// Entities the user works with (employer, primary project, key
    /// collaborators).
    KnownEntities,
}

/// Subject of the synthesized profile. At least one of `user` / `agent`
/// must be set — mirrors the IDL `DataProfileSubject` `anyOf` constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSubject {
    /// Stable user identifier (e.g., `hmn:alice`).
    pub user: Option<String>,
    /// Agent identifier when the profile is scoped to an agent variant.
    pub agent: Option<String>,
}

/// One record's contribution to the profile.
///
/// The synthesizer never reads the underlying record body. Adapters
/// derive [`Self::value`] from the body / frontmatter once and hand the
/// canonical short statement here. This keeps the synthesizer free of
/// markdown parsing and the body-reading constraints in §6.5.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSourceRecord {
    /// Stable record id used as the line's evidence pointer. The
    /// `evidence` array on every emitted [`crate::generated::verbs::retrieve::ProfileLine`]
    /// is built from these.
    pub record_id: RecordId,
    /// Static-vs-dynamic split per brief §3.0 / §7.1: records with
    /// `is_static = 1` feed the `static` half of the profile; the rest
    /// feed the `dynamic` half.
    pub is_static: bool,
    /// Confidence scalar in `[0.0, 1.0]`. Records with confidence
    /// `< 0.3` (`ConfidenceBand::Uncertain`) are dropped before the
    /// fact-line merge step — the brief calls this "avoid profile updates
    /// from low-confidence records."
    pub confidence: f32,
    /// Bucket the line lands in. The synthesizer routes by this field;
    /// it never recomputes the facet.
    pub facet: KeyFactFacet,
    /// Canonical short statement (the line's `value`). Lines with the
    /// same `(is_static, facet, value)` triple are merged: confidences
    /// take the max, evidence sets union.
    pub value: String,
    /// Wall-clock instant the source record was last updated. Drives
    /// `DataProfile.updated_at` (latest contributing record).
    pub updated_at: Rfc3339Timestamp,
}
