//! `UserProfileSynthesizer` — pure function, brief §7.1.
//!
//! Walks a slice of [`super::ProfileSourceRecord`] projections and
//! emits the typed [`crate::generated::verbs::retrieve::DataProfile`]
//! used by `retrieve --profile` and the `assemble_hot` hot prefix.
//!
//! Filtering rules (P0):
//! - records with `confidence < 0.3` (`ConfidenceBand::Uncertain`) are
//!   dropped — see brief §6.4 + the issue's "low-confidence exclusion"
//!   acceptance criterion;
//! - the adapter is responsible for excluding `tombstoned` /
//!   privacy-blocked / consent-declined records before calling in.
//!
//! Merge rules:
//! - lines with the same `(is_static, facet, value)` triple coalesce;
//!   the merged line keeps `max(confidence)` and the union of evidence
//!   ULIDs (sorted, deduplicated).
//!
//! Ordering:
//! - within each facet, lines emit in ascending `value` order so the
//!   profile is bytewise-deterministic for fixed inputs (the
//!   forget-propagation tests rely on this).

use crate::domain::Rfc3339Timestamp;
use crate::generated::common::Ulid;
use crate::generated::verbs::retrieve::{
    DataProfile, DataProfileSubject, KeyFacts, ProfileHalf, ProfileLine,
};

use super::source::{KeyFactFacet, ProfileSourceRecord, ProfileSubject};

/// Confidence band threshold below which records are excluded — matches
/// `ConfidenceBand::Uncertain` in `crate::domain::evidence`.
const CONFIDENCE_FLOOR: f32 = 0.3;

/// Errors returned by [`synthesize`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SynthesizeError {
    /// Subject carried neither `user` nor `agent`. The IDL
    /// `DataProfileSubject` requires at least one — failing here keeps
    /// the synthesizer's output validator-symmetric with the schema.
    #[error("profile subject must specify at least one of `user` or `agent`")]
    EmptySubject,
    /// Subject string was set but empty. The IDL constrains both
    /// fields to `minLength: 1`. Whitespace-only strings are not
    /// trimmed at this layer — the caller is expected to normalize
    /// before passing.
    #[error("profile subject `{field}` must not be empty")]
    BlankSubjectField {
        /// Which subject field was blank — `user` or `agent`.
        field: &'static str,
    },
}

/// Pure-function profile materializer (brief §7.1).
///
/// `now` becomes `DataProfile.updated_at` when the input contributes
/// no records; otherwise the synthesizer picks the chronologically
/// latest [`ProfileSourceRecord::updated_at`] across the surviving
/// inputs (post-confidence-filter).
///
/// # Errors
///
/// Returns [`SynthesizeError::EmptySubject`] when neither `user` nor
/// `agent` is set, or [`SynthesizeError::BlankSubjectField`] when a set
/// field is empty.
///
/// # Example
///
/// ```
/// use cairn_core::domain::{RecordId, Rfc3339Timestamp};
/// use cairn_core::pipeline::profile::{
///     synthesize, KeyFactFacet, ProfileSourceRecord, ProfileSubject,
/// };
///
/// let records = vec![ProfileSourceRecord {
///     record_id: RecordId::parse("01HQZX9F5N0000000000000000").unwrap(),
///     is_static: true,
///     confidence: 0.9,
///     facet: KeyFactFacet::Preferences,
///     value: "prefers terse explanations".to_owned(),
///     updated_at: Rfc3339Timestamp::parse("2026-04-22T14:00:00Z").unwrap(),
/// }];
/// let subject = ProfileSubject {
///     user: Some("hmn:alice".to_owned()),
///     agent: None,
/// };
/// let now = Rfc3339Timestamp::parse("2026-04-22T14:00:01Z").unwrap();
///
/// let profile = synthesize(&records, &subject, &now).unwrap();
/// assert_eq!(profile.r#static.key_facts.preferences.len(), 1);
/// assert_eq!(profile.updated_at, "2026-04-22T14:00:00Z");
/// ```
pub fn synthesize(
    records: &[ProfileSourceRecord],
    subject: &ProfileSubject,
    now: &Rfc3339Timestamp,
) -> Result<DataProfile, SynthesizeError> {
    let subject_out = build_subject(subject)?;

    let mut static_records: Vec<&ProfileSourceRecord> = Vec::new();
    let mut dynamic_records: Vec<&ProfileSourceRecord> = Vec::new();
    let mut latest: Option<&Rfc3339Timestamp> = None;

    for r in records {
        if !is_admissible(r) {
            continue;
        }
        if r.is_static {
            static_records.push(r);
        } else {
            dynamic_records.push(r);
        }
        latest = Some(match latest {
            None => &r.updated_at,
            Some(prev) if prev.cmp_chronological(&r.updated_at) == std::cmp::Ordering::Less => {
                &r.updated_at
            }
            Some(prev) => prev,
        });
    }

    let updated_at = latest.unwrap_or(now).as_str().to_owned();

    Ok(DataProfile {
        subject: subject_out,
        r#static: build_half(&static_records),
        dynamic: build_half(&dynamic_records),
        updated_at,
    })
}

fn is_admissible(r: &ProfileSourceRecord) -> bool {
    // NaN, < 0.0, > 1.0 are all out-of-contract: `MemoryRecord.validate`
    // already rejects them upstream, but we re-check at the trust
    // boundary. Without this gate a confidence of `1.5` would slip into
    // the output and the generated `ProfileLine` deserializer (codegen
    // emits `(0.0..=1.0).contains(&raw.confidence)`) would reject the
    // wire round-trip, leaving the synthesizer producing data its own
    // schema rejects.
    if !(0.0..=1.0).contains(&r.confidence) || r.confidence.is_nan() {
        return false;
    }
    if r.confidence < CONFIDENCE_FLOOR {
        return false;
    }
    if r.value.is_empty() {
        return false;
    }
    true
}

fn build_subject(subject: &ProfileSubject) -> Result<DataProfileSubject, SynthesizeError> {
    if subject.user.is_none() && subject.agent.is_none() {
        return Err(SynthesizeError::EmptySubject);
    }
    if matches!(&subject.user, Some(s) if s.is_empty()) {
        return Err(SynthesizeError::BlankSubjectField { field: "user" });
    }
    if matches!(&subject.agent, Some(s) if s.is_empty()) {
        return Err(SynthesizeError::BlankSubjectField { field: "agent" });
    }
    Ok(DataProfileSubject {
        user: subject.user.clone(),
        agent: subject.agent.clone(),
    })
}

fn build_half(records: &[&ProfileSourceRecord]) -> ProfileHalf {
    ProfileHalf {
        // Rolling DreamWorkflow narratives are P1; P0 emits empty bodies.
        summary: String::new(),
        historical_summary: String::new(),
        key_facts: build_key_facts(records),
    }
}

fn build_key_facts(records: &[&ProfileSourceRecord]) -> KeyFacts {
    KeyFacts {
        devices: lines_for(records, KeyFactFacet::Devices),
        software: lines_for(records, KeyFactFacet::Software),
        preferences: lines_for(records, KeyFactFacet::Preferences),
        current_issues: lines_for(records, KeyFactFacet::CurrentIssues),
        addressed_issues: lines_for(records, KeyFactFacet::AddressedIssues),
        recurring_issues: lines_for(records, KeyFactFacet::RecurringIssues),
        known_entities: lines_for(records, KeyFactFacet::KnownEntities),
    }
}

fn lines_for(records: &[&ProfileSourceRecord], facet: KeyFactFacet) -> Vec<ProfileLine> {
    // Stable bucket: BTreeMap keyed by `value` so emission order is
    // bytewise-deterministic and merge of duplicates is implicit.
    let mut by_value: std::collections::BTreeMap<&str, MergedLine<'_>> =
        std::collections::BTreeMap::new();

    for r in records.iter().filter(|r| r.facet == facet) {
        let entry = by_value
            .entry(r.value.as_str())
            .or_insert_with(|| MergedLine {
                value: r.value.as_str(),
                confidence: r.confidence,
                evidence: std::collections::BTreeSet::new(),
            });
        if r.confidence > entry.confidence {
            entry.confidence = r.confidence;
        }
        entry.evidence.insert(r.record_id.as_str());
    }

    by_value
        .into_values()
        .map(|m| ProfileLine {
            value: m.value.to_owned(),
            // f32 → f64 widens losslessly; the IDL is f64 on the wire.
            confidence: f64::from(m.confidence),
            evidence: m.evidence.into_iter().map(|s| Ulid(s.to_owned())).collect(),
        })
        .collect()
}

struct MergedLine<'a> {
    value: &'a str,
    confidence: f32,
    evidence: std::collections::BTreeSet<&'a str>,
}
