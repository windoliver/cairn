//! §6.1-§6.3 — profile / insight / fact taxonomy conventions.

use std::collections::BTreeMap;

use crate::domain::taxonomy::{MemoryClass, MemoryKind};
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_record};

const DOC_LINK: &str = "docs/design/taxonomy-conventions.md";

/// Run taxonomy-convention checks.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(orphan_insights(inputs));
    findings.extend(duplicate_profiles(inputs));
    findings.extend(wrong_class_for_kind(inputs));
    findings
}

fn orphan_insights(inputs: &LintInputs<'_>) -> Vec<Finding> {
    inputs
        .records
        .iter()
        .filter_map(|lint_record| {
            let record = &lint_record.stored.record;
            if record.kind != MemoryKind::Belief || !record.provenance.source_ids.is_empty() {
                return None;
            }

            let mut f = finding(
                Kind::OrphanInsight,
                Severity::Warning,
                format!(
                    "insight record {} is `belief` but has no provenance.source_ids; see {DOC_LINK}",
                    record.id.as_str()
                ),
            );
            f.target = Some(target_record(&record.id));
            f.suggested_fix = Some(
                "attach the source records or summarize operation that produced this insight"
                    .to_owned(),
            );
            Some(f)
        })
        .collect()
}

fn duplicate_profiles(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut by_actor: BTreeMap<String, Vec<&crate::domain::RecordId>> = BTreeMap::new();
    for lint_record in inputs.records {
        let record = &lint_record.stored.record;
        let Some(actor) = profile_actor(record) else {
            continue;
        };
        by_actor
            .entry(actor.to_owned())
            .or_default()
            .push(&record.id);
    }

    by_actor
        .into_iter()
        .filter_map(|(actor, record_ids)| {
            if record_ids.len() < 2 {
                return None;
            }
            let record_list = record_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let mut f = finding(
                Kind::MisclassifiedProfile,
                Severity::Warning,
                format!(
                    "multiple profile records use well_known_id `profile:{actor}`: {record_list}; see {DOC_LINK}"
                ),
            );
            f.target = record_ids.first().map(|id| target_record(id));
            f.entities = Some(record_ids.iter().map(|id| id.as_str().to_owned()).collect());
            f.suggested_fix = Some(
                "keep at most one profile well-known record per actor in the vault".to_owned(),
            );
            Some(f)
        })
        .collect()
}

fn wrong_class_for_kind(inputs: &LintInputs<'_>) -> Vec<Finding> {
    inputs
        .records
        .iter()
        .filter_map(|lint_record| {
            let record = &lint_record.stored.record;
            let expected = canonical_class(record.kind);
            if record.class == expected {
                return None;
            }

            let mut f = finding(
                Kind::WrongClassForKind,
                Severity::Warning,
                format!(
                    "record {} has kind `{}` with class `{}`; canonical class is `{}` per {DOC_LINK}",
                    record.id.as_str(),
                    record.kind.as_str(),
                    record.class.as_str(),
                    expected.as_str(),
                ),
            );
            f.target = Some(target_record(&record.id));
            f.suggested_fix = Some(format!("reclassify this record as `{}`", expected.as_str()));
            Some(f)
        })
        .collect()
}

fn profile_actor(record: &crate::domain::MemoryRecord) -> Option<&str> {
    record
        .extra_frontmatter
        .get("well_known_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.strip_prefix("profile:"))
        .filter(|actor| !actor.is_empty())
}

const fn canonical_class(kind: MemoryKind) -> MemoryClass {
    match kind {
        MemoryKind::Event
        | MemoryKind::Feedback
        | MemoryKind::Reasoning
        | MemoryKind::SensorObservation
        | MemoryKind::Trace
        | MemoryKind::UserSignal => MemoryClass::Episodic,
        MemoryKind::Playbook
        | MemoryKind::Rule
        | MemoryKind::StrategyFailure
        | MemoryKind::StrategySuccess
        | MemoryKind::Workflow => MemoryClass::Procedural,
        MemoryKind::Belief
        | MemoryKind::Entity
        | MemoryKind::Fact
        | MemoryKind::KnowledgeGap
        | MemoryKind::Opinion
        | MemoryKind::Project
        | MemoryKind::Reference
        | MemoryKind::User => MemoryClass::Semantic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use crate::verbs::lint::{ConsentModel, LintInputs, LintRecord, SchemaVersion};

    fn lint_record(record_id: &str, kind: MemoryKind, class: MemoryClass) -> LintRecord {
        let mut record = sample_record();
        record.id = crate::domain::record::RecordId::parse(record_id).expect("valid record id");
        record.target_id = crate::domain::TargetId::parse(record_id).expect("valid target id");
        record.kind = kind;
        record.class = class;
        record.visibility = MemoryVisibility::Private;
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn inputs<'a>(records: &'a [LintRecord], cfg: &'a CairnConfig) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
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
        }
    }

    #[test]
    fn orphan_insight_without_source_ids_emits_warning_snapshot() {
        let cfg = CairnConfig::default();
        let mut insight = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            MemoryKind::Belief,
            MemoryClass::Semantic,
        );
        insight.stored.record.provenance.source_ids.clear();
        let records = [insight];

        let findings = run(&inputs(&records, &cfg));

        assert_eq!(findings.len(), 1);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn duplicate_profile_well_known_ids_emit_warning_snapshot() {
        let cfg = CairnConfig::default();
        let mut first = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        first.stored.record.extra_frontmatter.insert(
            "well_known_id".to_owned(),
            serde_json::json!("profile:hmn:alice"),
        );
        let mut second = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        second.stored.record.extra_frontmatter.insert(
            "well_known_id".to_owned(),
            serde_json::json!("profile:hmn:alice"),
        );
        let records = [first, second];

        let findings = run(&inputs(&records, &cfg));

        assert_eq!(findings.len(), 1);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn fact_with_non_semantic_class_emits_warning_snapshot() {
        let cfg = CairnConfig::default();
        let fact = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            MemoryKind::Fact,
            MemoryClass::Episodic,
        );
        let records = [fact];

        let findings = run(&inputs(&records, &cfg));

        assert_eq!(findings.len(), 1);
        insta::assert_json_snapshot!(findings);
    }

    #[test]
    fn canonical_class_table_accepts_every_kind_without_warning() {
        let cfg = CairnConfig::default();
        let cases = [
            (MemoryKind::User, MemoryClass::Semantic),
            (MemoryKind::Feedback, MemoryClass::Episodic),
            (MemoryKind::Project, MemoryClass::Semantic),
            (MemoryKind::Reference, MemoryClass::Semantic),
            (MemoryKind::Fact, MemoryClass::Semantic),
            (MemoryKind::Belief, MemoryClass::Semantic),
            (MemoryKind::Opinion, MemoryClass::Semantic),
            (MemoryKind::Event, MemoryClass::Episodic),
            (MemoryKind::Entity, MemoryClass::Semantic),
            (MemoryKind::Workflow, MemoryClass::Procedural),
            (MemoryKind::Rule, MemoryClass::Procedural),
            (MemoryKind::StrategySuccess, MemoryClass::Procedural),
            (MemoryKind::StrategyFailure, MemoryClass::Procedural),
            (MemoryKind::Trace, MemoryClass::Episodic),
            (MemoryKind::Reasoning, MemoryClass::Episodic),
            (MemoryKind::Playbook, MemoryClass::Procedural),
            (MemoryKind::SensorObservation, MemoryClass::Episodic),
            (MemoryKind::UserSignal, MemoryClass::Episodic),
            (MemoryKind::KnowledgeGap, MemoryClass::Semantic),
        ];
        let records: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(i, (kind, class))| {
                lint_record(&format!("01ARZ3NDEKTSV4RRFFQ69G5F{i:02}"), *kind, *class)
            })
            .collect();

        let findings = run(&inputs(&records, &cfg));

        assert!(
            !findings
                .iter()
                .any(|f| matches!(f.kind, Kind::WrongClassForKind)),
            "canonical kind/class pairs should not warn: {findings:?}"
        );
    }

    #[test]
    fn profile_well_known_ids_only_duplicate_same_non_empty_actor() {
        let cfg = CairnConfig::default();
        let mut alice_one = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F00",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        alice_one.stored.record.extra_frontmatter.insert(
            "well_known_id".to_owned(),
            serde_json::json!("profile:hmn:alice"),
        );
        let mut alice_two = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F01",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        alice_two.stored.record.extra_frontmatter.insert(
            "well_known_id".to_owned(),
            serde_json::json!("profile:hmn:alice"),
        );
        let mut bob = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F02",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        bob.stored.record.extra_frontmatter.insert(
            "well_known_id".to_owned(),
            serde_json::json!("profile:hmn:bob"),
        );
        let mut empty_actor = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F03",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        empty_actor
            .stored
            .record
            .extra_frontmatter
            .insert("well_known_id".to_owned(), serde_json::json!("profile:"));
        let mut non_string = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F04",
            MemoryKind::User,
            MemoryClass::Semantic,
        );
        non_string
            .stored
            .record
            .extra_frontmatter
            .insert("well_known_id".to_owned(), serde_json::json!(42));
        let records = [alice_one, alice_two, bob, empty_actor, non_string];

        let findings = run(&inputs(&records, &cfg));
        let profile_findings: Vec<_> = findings
            .iter()
            .filter(|f| matches!(f.kind, Kind::MisclassifiedProfile))
            .collect();

        assert_eq!(profile_findings.len(), 1, "{findings:?}");
        assert!(
            profile_findings[0].message.contains("profile:hmn:alice"),
            "{profile_findings:?}"
        );
    }

    #[test]
    fn belief_with_source_ids_is_not_orphan_insight() {
        let cfg = CairnConfig::default();
        let belief = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5F00",
            MemoryKind::Belief,
            MemoryClass::Semantic,
        );
        let records = [belief];

        let findings = run(&inputs(&records, &cfg));

        assert!(
            !findings
                .iter()
                .any(|f| matches!(f.kind, Kind::OrphanInsight)),
            "belief records with provenance.source_ids should not warn: {findings:?}"
        );
    }
}
