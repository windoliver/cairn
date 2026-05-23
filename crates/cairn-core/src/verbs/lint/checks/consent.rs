//! §6.5 — sensor-consent enforcement (Issue #253).
//!
//! Sub-check matrix per record tagged `ConsentModel::ReceiptTimeline`:
//!   1. Covering-grant resolution: does any issued event match
//!      `(sensor, scope)` and cover `created_at`?
//!   2. Sensor binding: timeline has events for the record's sensor.
//!   3. Scope binding: timeline has events for the record's scope tuple.
//!   4. Window: `created_at` falls inside `[issued_at, expires_at)` of an
//!      `Issued` event with no preceding `Revoked` event.
//!   5. State-at-issue: an `Issued` event exists with `decided_at <=
//!      created_at` (otherwise the record was written before any grant).
//!
//! Records tagged `ConsentModel::LegacyEvent` are skipped per Phase-A
//! sequencing — Phase-B (#255) flips the per-row default and wires the
//! ingest writers that populate `consent_timeline`. While every record
//! is still on `LegacyEvent` (no writer has stamped any rows yet), this
//! check emits a single info-severity `DeferredCheck` finding pointing
//! at #255 so operators see that §6.5 enforcement is wired but quiescent.

use crate::contract::consent_lookup::ConsentLookupError;
use crate::domain::SensorLabel;
use crate::domain::consent_timeline::{
    ConsentModel, ConsentTimelineEvent, ConsentTimelineEventKind, CoveringGrant,
};
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_record};
use std::collections::HashMap;

/// Tracking issue for the Phase-B ingest writer that flips records onto
/// `receipt_timeline` and populates the timeline table.
const PHASE_B_TRACKING_ISSUE: i64 = 255;

/// Run the §6.5 sub-check matrix over `ReceiptTimeline` records, plus a
/// Phase-A signal when no records are on `receipt_timeline` yet.
///
/// Fail-closed: when any record carries `ConsentModel::ReceiptTimeline`
/// but the caller did not wire a `ConsentLookup`, emit an error-severity
/// finding rather than silently skipping enforcement. A missing adapter
/// is an operational misconfiguration once Phase-B has stamped any rows;
/// returning empty would let unverified records pass `lint`.
#[must_use]
pub async fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let any_receipt_timeline = inputs
        .records
        .iter()
        .any(|r| r.consent_model == ConsentModel::ReceiptTimeline);

    let Some(lookup) = inputs.consent_lookup else {
        if any_receipt_timeline {
            return vec![lookup_unwired_finding()];
        }
        // Legacy-only with no lookup wired: nothing to enforce, nothing
        // to defer — adapter wiring is a CLI concern, not a vault state.
        return Vec::new();
    };

    // Memoize timelines by consent_ref. Repeated records on the same grant
    // share one fetch — without this, a vault with N records on a single
    // consent issues N round-trips to the store on every lint run.
    let mut cache: HashMap<String, Result<Vec<ConsentTimelineEvent>, String>> = HashMap::new();
    let mut out = Vec::new();
    for r in inputs.records {
        if r.consent_model == ConsentModel::LegacyEvent {
            continue;
        }
        let consent_ref = &r.stored.record.provenance.consent_ref;
        if !cache.contains_key(consent_ref) {
            let res = lookup
                .timeline(consent_ref)
                .await
                .map_err(|e: ConsentLookupError| e.to_string());
            cache.insert(consent_ref.clone(), res);
        }
        let cached = match cache.get(consent_ref) {
            Some(c) => c.as_ref(),
            None => continue,
        };
        out.extend(check_record(r, cached));
    }

    // Phase-A signal: lookup is wired but no records ride the new path.
    // Goes silent on its own once #255 starts stamping `receipt_timeline`.
    if !any_receipt_timeline {
        out.push(phase_a_pending_finding());
    }
    out
}

fn lookup_unwired_finding() -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        "§6.5 consent enforcement skipped: records carry \
         `consent_model='receipt_timeline'` but no `ConsentLookup` adapter \
         is wired into `lint`. Refusing to pass unverified records."
            .to_owned(),
    );
    f.suggested_fix = Some(
        "wire a ConsentLookup adapter (e.g., the SqliteMemoryStore impl) \
         when constructing LintInputs in the calling surface (CLI/MCP/SDK)"
            .to_owned(),
    );
    f
}

fn phase_a_pending_finding() -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "§6.5 sensor-consent enforcement is wired but no records carry \
             `consent_model='receipt_timeline'` yet; ingest writers are tracked under #{PHASE_B_TRACKING_ISSUE}"
        ),
    );
    f.tracking_issue = Some(PHASE_B_TRACKING_ISSUE);
    f.suggested_fix = Some(format!(
        "ship #{PHASE_B_TRACKING_ISSUE} to enable Phase-B (default flip + ingest \
         writes to consent_timeline)"
    ));
    f
}

fn check_record(
    r: &LintRecord,
    cached: Result<&Vec<ConsentTimelineEvent>, &String>,
) -> Vec<Finding> {
    let consent_ref = &r.stored.record.provenance.consent_ref;
    let created_at = &r.stored.record.provenance.created_at;
    // Consent grants are joined to records on the canonical scope tuple
    // wire form (brief §14, #253). `MemoryVisibility` is the coarser
    // tier — narrowing on that alone would let a `team:` grant cover a
    // record with a narrower scope tuple.
    let scope = r.stored.record.scope.canonical_wire();
    let scope = scope.as_str();

    let Ok(sensor) = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor) else {
        return vec![finding_no_grant_with(
            r,
            "provenance.source_sensor is not a sensor identity",
        )];
    };

    let timeline = match cached {
        Ok(t) => t,
        Err(e) => {
            let mut f = finding(
                Kind::MissingProvenance,
                Severity::Error,
                format!("consent timeline lookup failed for {consent_ref}: {e}"),
            );
            f.target = Some(target_record(&r.stored.record.id));
            return vec![f];
        }
    };

    let grant = CoveringGrant::resolve(timeline, &sensor, scope, created_at);

    if grant.is_none() {
        let any_for_sensor = timeline.iter().any(|e| e.sensor_id == sensor);
        let any_for_scope = timeline.iter().any(|e| e.scope == scope);
        let detail = match (any_for_sensor, any_for_scope) {
            (false, true) => "sensor mismatch (timeline has no event for this sensor)",
            (true, false) => "scope mismatch (timeline has no event for this scope)",
            (false, false) => "no events match either sensor or scope",
            (true, true) => {
                use std::cmp::Ordering;
                let any_revoke_at_or_before_t = timeline.iter().any(|e| {
                    e.kind == ConsentTimelineEventKind::Revoked
                        && e.sensor_id == sensor
                        && e.scope == scope
                        && !matches!(
                            e.decided_at.cmp_chronological(created_at),
                            Ordering::Greater
                        )
                });
                let any_issue_at_or_before_t = timeline.iter().any(|e| {
                    e.kind == ConsentTimelineEventKind::Issued
                        && e.sensor_id == sensor
                        && e.scope == scope
                        && !matches!(
                            e.decided_at.cmp_chronological(created_at),
                            Ordering::Greater
                        )
                });
                if !any_issue_at_or_before_t {
                    "record written before any issue event for this consent_ref \
                     (state-at-issue: not yet issued)"
                } else if any_revoke_at_or_before_t {
                    "consent was revoked before record was written"
                } else {
                    "record written after issued grant expired (window mismatch)"
                }
            }
        };
        return vec![finding_no_grant_with(
            r,
            &format!(
                "no covering grant for {consent_ref} at {created}: {detail} \
                 (record sensor={sensor_s}, scope={scope})",
                created = created_at.as_str(),
                sensor_s = sensor.as_str(),
            ),
        )];
    }

    Vec::new() // sub-checks 2-5 will append here in Tasks 9-10.
}

fn finding_no_grant_with(r: &LintRecord, message: &str) -> Finding {
    let mut f = finding(Kind::MissingProvenance, Severity::Error, message.to_owned());
    f.target = Some(target_record(&r.stored.record.id));
    f.suggested_fix = Some(
        "ensure ingest writes a `consent_timeline` issued event matching \
         (sensor, scope, time) before the record is stored, or set \
         `records.consent_model='legacy_event'` for legacy ingests"
            .to_owned(),
    );
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::consent_lookup::{ConsentLookup, ConsentLookupError};
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::consent_timeline::ConsentTimelineEvent;
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{LintInputs, LintRecord, SchemaVersion};
    use async_trait::async_trait;
    use std::collections::HashMap;

    #[derive(Default)]
    struct StaticLookup {
        by_ref: HashMap<String, Vec<ConsentTimelineEvent>>,
    }

    impl StaticLookup {
        fn with(mut self, k: &str, v: Vec<ConsentTimelineEvent>) -> Self {
            self.by_ref.insert(k.to_owned(), v);
            self
        }
    }

    #[async_trait]
    impl ConsentLookup for StaticLookup {
        async fn timeline(
            &self,
            consent_ref: &str,
        ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
            Ok(self.by_ref.get(consent_ref).cloned().unwrap_or_default())
        }
    }

    fn lint_record_with(consent_ref: &str, model: ConsentModel) -> LintRecord {
        let mut r = sample_record();
        r.provenance.consent_ref = consent_ref.to_owned();
        LintRecord {
            stored: StoredRecord {
                record: r,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: model,
        }
    }

    #[tokio::test]
    async fn flags_receipt_timeline_record_without_covering_grant() {
        let r = lint_record_with("consent:missing", ConsentModel::ReceiptTimeline);
        let cfg = CairnConfig::default();
        let lookup = StaticLookup::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let findings = run(&inputs).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no covering grant"));
        assert!(findings[0].target.is_some());
    }

    #[tokio::test]
    async fn legacy_only_records_emit_phase_a_deferred_signal() {
        let r = lint_record_with("consent:legacy", ConsentModel::LegacyEvent);
        let cfg = CairnConfig::default();
        let lookup = StaticLookup::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        // No per-record finding (LegacyEvent skipped) but exactly one
        // info-severity Phase-A signal pointing at #255.
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Info);
        assert_eq!(f[0].tracking_issue, Some(PHASE_B_TRACKING_ISSUE));
    }

    #[tokio::test]
    async fn no_records_with_lookup_emits_phase_a_deferred_signal() {
        // Empty input + lookup wired = Phase-A signal still fires.
        let cfg = CairnConfig::default();
        let lookup = StaticLookup::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].tracking_issue, Some(PHASE_B_TRACKING_ISSUE));
    }

    #[tokio::test]
    async fn flags_sensor_mismatch_with_specific_message() {
        use crate::domain::Rfc3339Timestamp;
        use crate::domain::SensorLabel;
        use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};

        let r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
        let scope = r.stored.record.scope.canonical_wire();
        let other_sensor = ConsentTimelineEvent {
            consent_ref: "consent:c1".to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: SensorLabel::parse("local:terminal:host:v1")
                .expect("invariant: valid sensor label"),
            scope,
            decided_at: Rfc3339Timestamp::parse("2020-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        };
        let lookup = StaticLookup::default().with("consent:c1", vec![other_sensor]);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::MissingProvenance);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(
            f[0].message.contains("sensor"),
            "expected sensor in message, got: {}",
            f[0].message
        );
        assert!(
            f[0].message.contains("mismatch") || f[0].message.contains("does not match"),
            "expected mismatch wording in message, got: {}",
            f[0].message
        );
    }

    #[tokio::test]
    async fn flags_scope_mismatch_with_specific_message() {
        use crate::domain::Rfc3339Timestamp;
        use crate::domain::SensorLabel;
        use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};

        let r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
        let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor)
            .expect("invariant: source_sensor parses as a sensor label");
        let other_scope = ConsentTimelineEvent {
            consent_ref: "consent:c1".to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: sensor,
            scope: "team:other".to_owned(),
            decided_at: Rfc3339Timestamp::parse("2020-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        };
        let lookup = StaticLookup::default().with("consent:c1", vec![other_scope]);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1);
        assert!(
            f[0].message.contains("scope"),
            "expected 'scope' in message, got: {}",
            f[0].message
        );
    }

    #[tokio::test]
    async fn flags_record_written_after_revoke() {
        use crate::domain::Rfc3339Timestamp;
        use crate::domain::SensorLabel;
        use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};

        let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
        r.stored.record.provenance.created_at =
            Rfc3339Timestamp::parse("2026-04-01T00:00:00Z").expect("invariant: valid timestamp");
        let scope = r.stored.record.scope.canonical_wire();
        let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor)
            .expect("invariant: source_sensor parses");
        let issued = ConsentTimelineEvent {
            consent_ref: "consent:c1".to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: sensor.clone(),
            scope: scope.clone(),
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        };
        let revoked = ConsentTimelineEvent {
            seq: 2,
            kind: ConsentTimelineEventKind::Revoked,
            decided_at: Rfc3339Timestamp::parse("2026-03-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            ..issued.clone()
        };
        let lookup = StaticLookup::default().with("consent:c1", vec![issued, revoked]);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1);
        assert!(
            f[0].message.contains("revoke") || f[0].message.contains("revoked"),
            "expected revoke wording, got: {}",
            f[0].message
        );
    }

    #[tokio::test]
    async fn flags_record_written_before_any_issue() {
        use crate::domain::Rfc3339Timestamp;
        use crate::domain::SensorLabel;
        use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};

        let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
        r.stored.record.provenance.created_at =
            Rfc3339Timestamp::parse("2025-12-31T00:00:00Z").expect("invariant: valid timestamp");
        let scope = r.stored.record.scope.canonical_wire();
        let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor)
            .expect("invariant: source_sensor parses");
        let issued = ConsentTimelineEvent {
            consent_ref: "consent:c1".to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: sensor,
            scope,
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        };
        let lookup = StaticLookup::default().with("consent:c1", vec![issued]);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1);
        assert!(
            f[0].message.contains("before") || f[0].message.contains("not yet issued"),
            "expected before/not-yet-issued wording, got: {}",
            f[0].message
        );
    }

    #[tokio::test]
    async fn passes_record_strictly_inside_window() {
        use crate::domain::Rfc3339Timestamp;
        use crate::domain::SensorLabel;
        use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};

        let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
        r.stored.record.provenance.created_at =
            Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").expect("invariant: valid timestamp");
        let scope = r.stored.record.scope.canonical_wire();
        let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor)
            .expect("invariant: source_sensor parses");
        let issued = ConsentTimelineEvent {
            consent_ref: "consent:c1".to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: sensor,
            scope,
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: Some(
                Rfc3339Timestamp::parse("2026-12-31T00:00:00Z")
                    .expect("invariant: valid timestamp"),
            ),
        };
        let lookup = StaticLookup::default().with("consent:c1", vec![issued]);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: Some(&lookup),
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        assert!(run(&inputs).await.is_empty());
    }

    #[tokio::test]
    async fn fails_closed_when_receipt_timeline_records_present_without_lookup() {
        let r = lint_record_with("consent:any", ConsentModel::ReceiptTimeline);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        let f = run(&inputs).await;
        assert_eq!(f.len(), 1, "expected one error finding, got {f:?}");
        assert_eq!(f[0].kind, Kind::MissingProvenance);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(
            f[0].message.contains("no `ConsentLookup` adapter"),
            "expected unwired-adapter wording, got: {}",
            f[0].message
        );
    }

    #[tokio::test]
    async fn legacy_only_records_without_lookup_emit_no_findings() {
        // A purely-legacy vault with no lookup wired is the pre-#253
        // baseline: nothing to enforce, no signal to surface.
        let r = lint_record_with("consent:legacy", ConsentModel::LegacyEvent);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            agent_worker_audit: None,
            agent_canary_state: None,
            now_ms: 0,
        };
        assert!(run(&inputs).await.is_empty());
    }
}
