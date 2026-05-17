//! Pure pattern extraction for `ReflectionWorkflow` (brief §5.0.b, §10.1).

use std::collections::BTreeMap;

use crate::domain::taxonomy::{MemoryClass, MemoryKind};
use crate::domain::{RecordId, ScopeTuple};

/// One recent, already-admitted observation considered by reflection.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionSignal {
    /// Durable record that backs this signal.
    pub record_id: RecordId,
    /// Coarse signal category.
    pub kind: ReflectionSignalKind,
    /// Text body after upstream redaction/admission.
    pub body: String,
    /// Scope inherited from the source record.
    pub scope: ScopeTuple,
    /// Signal salience in `[0.0, 1.0]`.
    pub salience: f32,
    /// Signal confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Whether upstream privacy, consent, and signature gates permit reuse.
    pub policy: ReflectionPolicy,
}

/// Signal category extracted from recent trace-like records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectionSignalKind {
    /// Repeated tool or command failure.
    ToolError,
    /// User corrected the agent's behavior.
    UserCorrection,
    /// A new named entity surfaced.
    NovelEntity,
    /// The agent lacked needed knowledge.
    KnowledgeGap,
}

/// Policy state inherited from the admission/read boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionPolicy {
    /// Reuse is permitted.
    Allowed,
    /// Reuse must fail closed.
    Rejected,
}

/// Candidate review disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionDisposition {
    /// Eligible for autonomous `FlushPlan` emission.
    ReadyForFlush,
    /// Preserved for human review instead of autonomous write.
    ReviewRequired,
}

/// Candidate record draft produced by reflection.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionCandidate {
    /// Memory kind for the candidate record.
    pub kind: MemoryKind,
    /// Memory class for the candidate record.
    pub class: MemoryClass,
    /// Markdown body for the candidate.
    pub body: String,
    /// Scope shared by the evidence records.
    pub scope: ScopeTuple,
    /// Evidence records supporting the candidate.
    pub evidence_record_ids: Vec<RecordId>,
    /// Aggregate salience.
    pub salience: f32,
    /// Aggregate confidence.
    pub confidence: f32,
    /// Whether this candidate can be written automatically.
    pub disposition: ReflectionDisposition,
}

/// Discarded reflection input or candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionDiscard {
    /// Evidence records that were rejected.
    pub evidence_record_ids: Vec<RecordId>,
    /// Reason the signal or candidate did not become an autonomous record.
    pub reason: ReflectionDiscardReason,
}

/// Reason a signal or candidate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionDiscardReason {
    /// Upstream policy did not permit reuse.
    PolicyRejected,
    /// Confidence fell below the configured floor.
    LowConfidence,
    /// Salience fell below the configured floor.
    LowSalience,
}

/// Tunable gates for reflection extraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflectionConfig {
    /// Required repetitions before repeated-pattern candidates emit.
    pub min_repetitions: usize,
    /// Minimum signal salience considered.
    pub min_salience: f32,
    /// Minimum signal confidence considered.
    pub min_confidence: f32,
    /// Candidates below this confidence require review.
    pub auto_confidence: f32,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            min_repetitions: 3,
            min_salience: 0.5,
            min_confidence: 0.3,
            auto_confidence: 0.7,
        }
    }
}

/// Result of one reflection extraction pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReflectionOutcome {
    /// Candidate records.
    pub candidates: Vec<ReflectionCandidate>,
    /// Signals or groups rejected with auditable reason codes.
    pub discards: Vec<ReflectionDiscard>,
}

/// Extract mid-depth reflection candidates from recent admitted signals.
#[must_use]
pub fn extract_reflection_candidates(
    signals: &[ReflectionSignal],
    config: ReflectionConfig,
) -> ReflectionOutcome {
    let mut outcome = ReflectionOutcome::default();
    let mut groups: BTreeMap<String, Vec<&ReflectionSignal>> = BTreeMap::new();

    for signal in signals {
        match gate_signal(signal, config) {
            Ok(()) => {
                let normalized = normalize_pattern(&signal.body);
                if normalized.is_empty() {
                    continue;
                }
                let key = format!(
                    "{}:{}:{normalized}",
                    signal.kind.as_str(),
                    signal.scope.canonical_wire()
                );
                groups.entry(key).or_default().push(signal);
            }
            Err(reason) => outcome.discards.push(ReflectionDiscard {
                evidence_record_ids: vec![signal.record_id.clone()],
                reason,
            }),
        }
    }

    for group in groups.into_values() {
        let Some(first) = group.first().copied() else {
            continue;
        };
        let required = if first.kind == ReflectionSignalKind::NovelEntity {
            1
        } else {
            config.min_repetitions.max(1)
        };
        if group.len() < required {
            continue;
        }

        let evidence_record_ids = group.iter().map(|s| s.record_id.clone()).collect();
        let salience = average(group.iter().map(|s| s.salience));
        let confidence = average(group.iter().map(|s| s.confidence));
        outcome.candidates.push(ReflectionCandidate {
            kind: first.kind.candidate_kind(),
            class: first.kind.candidate_class(),
            body: render_candidate_body(first.kind, &normalize_pattern(&first.body), group.len()),
            scope: first.scope.clone(),
            evidence_record_ids,
            salience,
            confidence,
            disposition: if confidence >= config.auto_confidence {
                ReflectionDisposition::ReadyForFlush
            } else {
                ReflectionDisposition::ReviewRequired
            },
        });
    }

    outcome
}

impl ReflectionSignalKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ToolError => "tool_error",
            Self::UserCorrection => "user_correction",
            Self::NovelEntity => "novel_entity",
            Self::KnowledgeGap => "knowledge_gap",
        }
    }

    const fn candidate_kind(self) -> MemoryKind {
        match self {
            Self::ToolError | Self::KnowledgeGap => MemoryKind::KnowledgeGap,
            Self::UserCorrection => MemoryKind::Rule,
            Self::NovelEntity => MemoryKind::Entity,
        }
    }

    const fn candidate_class(self) -> MemoryClass {
        match self {
            Self::ToolError | Self::KnowledgeGap | Self::NovelEntity => MemoryClass::Semantic,
            Self::UserCorrection => MemoryClass::Procedural,
        }
    }
}

fn gate_signal(
    signal: &ReflectionSignal,
    config: ReflectionConfig,
) -> Result<(), ReflectionDiscardReason> {
    if signal.policy != ReflectionPolicy::Allowed {
        return Err(ReflectionDiscardReason::PolicyRejected);
    }
    if !signal.confidence.is_finite() || signal.confidence < config.min_confidence {
        return Err(ReflectionDiscardReason::LowConfidence);
    }
    if !signal.salience.is_finite() || signal.salience < config.min_salience {
        return Err(ReflectionDiscardReason::LowSalience);
    }
    Ok(())
}

fn normalize_pattern(body: &str) -> String {
    body.split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn average(values: impl Iterator<Item = f32>) -> f32 {
    let mut avg = 0.0_f32;
    let mut count = 0.0_f32;
    for value in values {
        count += 1.0;
        avg += (value - avg) / count;
    }
    avg
}

fn render_candidate_body(kind: ReflectionSignalKind, pattern: &str, count: usize) -> String {
    match kind {
        ReflectionSignalKind::ToolError => {
            format!("Repeated tool error observed {count} times: {pattern}")
        }
        ReflectionSignalKind::UserCorrection => {
            format!(
                "Candidate rule from repeated user correction ({count} observations): {pattern}"
            )
        }
        ReflectionSignalKind::NovelEntity => {
            format!("Candidate entity observed by reflection: {pattern}")
        }
        ReflectionSignalKind::KnowledgeGap => {
            format!("Repeated knowledge gap observed {count} times: {pattern}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u8) -> RecordId {
        RecordId::parse(format!("000000000000000000000000{n:02}")).expect("valid test ULID")
    }

    fn signal(n: u8, kind: ReflectionSignalKind, body: &str) -> ReflectionSignal {
        ReflectionSignal {
            record_id: rid(n),
            kind,
            body: body.to_owned(),
            scope: ScopeTuple {
                user: Some("hmn:test-user".to_owned()),
                ..ScopeTuple::default()
            },
            salience: 0.8,
            confidence: 0.8,
            policy: ReflectionPolicy::Allowed,
        }
    }

    #[test]
    fn repeated_tool_errors_emit_knowledge_gap_with_evidence_links() {
        let signals = vec![
            signal(
                1,
                ReflectionSignalKind::ToolError,
                "sqlite vec index missing",
            ),
            signal(
                2,
                ReflectionSignalKind::ToolError,
                "SQLite vec index missing",
            ),
            signal(
                3,
                ReflectionSignalKind::ToolError,
                " sqlite   vec index missing ",
            ),
        ];

        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());

        assert_eq!(outcome.candidates.len(), 1);
        let candidate = &outcome.candidates[0];
        assert_eq!(candidate.kind, MemoryKind::KnowledgeGap);
        assert_eq!(candidate.class, MemoryClass::Semantic);
        assert_eq!(candidate.evidence_record_ids, vec![rid(1), rid(2), rid(3)]);
        assert_eq!(candidate.disposition, ReflectionDisposition::ReadyForFlush);
        assert!(candidate.body.contains("sqlite vec index missing"));
    }

    #[test]
    fn policy_rejected_signals_never_emit_candidates() {
        let mut signals = vec![
            signal(
                1,
                ReflectionSignalKind::UserCorrection,
                "always run privacy tests",
            ),
            signal(
                2,
                ReflectionSignalKind::UserCorrection,
                "always run privacy tests",
            ),
            signal(
                3,
                ReflectionSignalKind::UserCorrection,
                "always run privacy tests",
            ),
        ];
        for signal in &mut signals {
            signal.policy = ReflectionPolicy::Rejected;
        }

        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());

        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.discards.len(), 3);
        assert!(
            outcome
                .discards
                .iter()
                .all(|discard| discard.reason == ReflectionDiscardReason::PolicyRejected)
        );
    }

    #[test]
    fn low_confidence_group_is_reviewable_or_discarded_with_reason() {
        let mut signals = vec![
            signal(
                1,
                ReflectionSignalKind::UserCorrection,
                "prefer cargo nextest",
            ),
            signal(
                2,
                ReflectionSignalKind::UserCorrection,
                "prefer cargo nextest",
            ),
            signal(
                3,
                ReflectionSignalKind::UserCorrection,
                "prefer cargo nextest",
            ),
        ];
        for signal in &mut signals {
            signal.confidence = 0.45;
        }

        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());

        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(
            outcome.candidates[0].disposition,
            ReflectionDisposition::ReviewRequired
        );

        signals[0].confidence = 0.1;
        signals[1].confidence = 0.1;
        signals[2].confidence = 0.1;
        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());
        assert!(outcome.candidates.is_empty());
        assert!(
            outcome
                .discards
                .iter()
                .all(|discard| discard.reason == ReflectionDiscardReason::LowConfidence)
        );
    }

    #[test]
    fn novel_entity_emits_single_candidate() {
        let signals = vec![signal(
            1,
            ReflectionSignalKind::NovelEntity,
            "Cairn ReflectionWorkflow",
        )];

        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());

        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].kind, MemoryKind::Entity);
        assert_eq!(outcome.candidates[0].class, MemoryClass::Semantic);
        assert_eq!(outcome.candidates[0].evidence_record_ids, vec![rid(1)]);
    }

    #[test]
    fn repeated_patterns_do_not_merge_across_scopes() {
        let mut signals = vec![
            signal(1, ReflectionSignalKind::UserCorrection, "prefer nextest"),
            signal(2, ReflectionSignalKind::UserCorrection, "prefer nextest"),
            signal(3, ReflectionSignalKind::UserCorrection, "prefer nextest"),
        ];
        signals[2].scope = ScopeTuple {
            user: Some("hmn:other-user".to_owned()),
            ..ScopeTuple::default()
        };

        let outcome = extract_reflection_candidates(&signals, ReflectionConfig::default());

        assert!(outcome.candidates.is_empty());
    }
}
