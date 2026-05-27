//! `ActivePlaybook` source — most-recently-updated `playbook` plus prerequisites.

use std::collections::BTreeSet;

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::pipeline::skillify::{
    SkillGraphIssue, SkillGraphIssueKind, SkillGraphResolver, SkillLintSkill, SkillLintSnapshot,
};
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

/// Select active playbook records with no explicit source budget.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    select_with_budget(inputs, None)
}

/// Select the active playbook and prerequisite playbooks that fit in budget.
#[must_use]
pub fn select_with_budget(
    inputs: &HotMemoryInputs<'_>,
    max_body_bytes: Option<u64>,
) -> LoadedSegment {
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.playbook_candidates {
        // NotPinned is reused as the wire-form "wrong kind for this
        // source" reason. Spec §"Selection rules per source" calls this
        // a NotPinned-equivalent. A future WrongKind variant could
        // disambiguate; revisit if downstream tooling needs it.
        if record.kind != MemoryKind::Playbook {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "dependency-aware playbook",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    let Some((active_trace, active_record)) = admissible.first().cloned() else {
        return LoadedSegment {
            body: String::new(),
            included: Vec::new(),
            excluded,
        };
    };

    let graph_snapshot = inputs.skill_graph_snapshot;
    let snapshot = playbook_snapshot(&admissible, graph_snapshot);
    let resolver = SkillGraphResolver::new(&snapshot);
    let active_skill_id = playbook_skill_id(active_record, graph_snapshot);
    let closure = resolver.resolve_prerequisites(&active_skill_id);
    let conflict_excluded = conflicting_prerequisites(
        &closure.issues,
        &closure.prerequisites,
        &admissible,
        graph_snapshot,
    );
    let mut conflict_exclusions = Vec::new();
    let mut ordered_records = Vec::new();
    for (trace, record) in prerequisite_records(&closure.prerequisites, &admissible, graph_snapshot)
    {
        if conflict_excluded.contains(playbook_skill_id(record, graph_snapshot).as_str()) {
            conflict_exclusions.push(ExclusionTrace {
                record_id: trace.record_id,
                reason: ExclusionReason::BeyondTopK,
            });
        } else {
            ordered_records.push((trace, record));
        }
    }
    ordered_records.push((active_trace, active_record));

    let mut segment = render_budgeted_playbooks(ordered_records, max_body_bytes);
    segment.excluded.extend(conflict_exclusions);
    let mut accounted: BTreeSet<String> = segment
        .included
        .iter()
        .map(|trace| trace.record_id.as_str().to_owned())
        .collect();
    accounted.extend(
        segment
            .excluded
            .iter()
            .map(|trace| trace.record_id.as_str().to_owned()),
    );
    for trace in &excluded {
        accounted.insert(trace.record_id.as_str().to_owned());
    }
    for (trace, _) in admissible {
        if accounted.insert(trace.record_id.as_str().to_owned()) {
            segment.excluded.push(ExclusionTrace {
                record_id: trace.record_id,
                reason: ExclusionReason::BeyondTopK,
            });
        }
    }
    excluded.append(&mut segment.excluded);

    LoadedSegment {
        body: segment.body,
        included: segment.included,
        excluded,
    }
}

fn playbook_skill_id(record: &MemoryRecord, graph_snapshot: Option<&SkillLintSnapshot>) -> String {
    if let Some(skill_id) = record
        .extra_frontmatter
        .get("skill_id")
        .and_then(serde_json::Value::as_str)
        .filter(|skill_id| !skill_id.trim().is_empty())
    {
        return skill_id.to_owned();
    }
    if let Some(skill) = snapshot_skill_for_playbook(record, graph_snapshot) {
        return skill.skill_id.clone();
    }
    record.id.as_str().to_owned()
}

fn playbook_lane(record: &MemoryRecord, graph_snapshot: Option<&SkillLintSnapshot>) -> String {
    if let Some(lane) = record
        .extra_frontmatter
        .get("lane")
        .and_then(serde_json::Value::as_str)
        .filter(|lane| !lane.trim().is_empty())
    {
        return lane.to_owned();
    }
    if let Some(skill) = snapshot_skill_for_playbook(record, graph_snapshot) {
        return skill.lane.clone();
    }
    record.id.as_str().to_owned()
}

fn playbook_string_list(record: &MemoryRecord, key: &str) -> Vec<String> {
    record
        .extra_frontmatter
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn playbook_snapshot(
    records: &[(InclusionTrace, &MemoryRecord)],
    graph_snapshot: Option<&SkillLintSnapshot>,
) -> SkillLintSnapshot {
    SkillLintSnapshot {
        skills: records
            .iter()
            .map(|(_, record)| {
                let skill = snapshot_skill_for_playbook(record, graph_snapshot);
                SkillLintSkill {
                    skill_id: playbook_skill_id(record, graph_snapshot),
                    lane: playbook_lane(record, graph_snapshot),
                    path: record.id.as_str().to_owned(),
                    uses: None,
                    resolver_triggers: vec![],
                    files_to: Some("wiki/summaries/".to_owned()),
                    gate_report_passed: true,
                    rollback_version_count: 1,
                    existing_paths: vec![],
                    requires: merged_graph_list(record, "requires", skill.map(|s| &s.requires)),
                    provides: merged_graph_list(record, "provides", skill.map(|s| &s.provides)),
                    conflicts: merged_graph_list(record, "conflicts", skill.map(|s| &s.conflicts)),
                }
            })
            .collect(),
    }
}

fn snapshot_skill_for_playbook<'a>(
    record: &MemoryRecord,
    graph_snapshot: Option<&'a SkillLintSnapshot>,
) -> Option<&'a SkillLintSkill> {
    let snapshot = graph_snapshot?;
    if let Some(skill_id) = record
        .extra_frontmatter
        .get("skill_id")
        .and_then(serde_json::Value::as_str)
        .filter(|skill_id| !skill_id.trim().is_empty())
    {
        return snapshot
            .skills
            .iter()
            .find(|skill| skill.skill_id == skill_id);
    }
    let lane = record
        .extra_frontmatter
        .get("lane")
        .and_then(serde_json::Value::as_str)
        .filter(|lane| !lane.trim().is_empty())?;
    let mut matches = snapshot.skills.iter().filter(|skill| skill.lane == lane);
    let skill = matches.next()?;
    matches.next().is_none().then_some(skill)
}

fn merged_graph_list(
    record: &MemoryRecord,
    key: &str,
    snapshot_values: Option<&Vec<String>>,
) -> Vec<String> {
    let mut out: BTreeSet<String> = snapshot_values
        .into_iter()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect();
    out.extend(
        playbook_string_list(record, key)
            .into_iter()
            .filter(|value| !value.trim().is_empty()),
    );
    out.into_iter().collect()
}

fn prerequisite_records<'a>(
    prerequisites: &[String],
    records: &'a [(InclusionTrace, &'a MemoryRecord)],
    graph_snapshot: Option<&SkillLintSnapshot>,
) -> Vec<(InclusionTrace, &'a MemoryRecord)> {
    prerequisites
        .iter()
        .filter_map(|skill_id| {
            records
                .iter()
                .find(|(_, record)| playbook_skill_id(record, graph_snapshot) == *skill_id)
                .cloned()
        })
        .collect()
}

fn conflicting_prerequisites(
    issues: &[SkillGraphIssue],
    prerequisites: &[String],
    records: &[(InclusionTrace, &MemoryRecord)],
    graph_snapshot: Option<&SkillLintSnapshot>,
) -> BTreeSet<String> {
    let prerequisite_set: BTreeSet<&str> = prerequisites.iter().map(String::as_str).collect();
    let mut excluded = BTreeSet::new();
    for issue in issues {
        if issue.kind != SkillGraphIssueKind::Conflict {
            continue;
        }
        if prerequisite_set.contains(issue.skill_id.as_str()) {
            excluded.insert(issue.skill_id.clone());
        }
        for skill_id in
            playbook_reference_matches(issue.reference.as_str(), records, graph_snapshot)
        {
            if prerequisite_set.contains(skill_id.as_str()) {
                excluded.insert(skill_id);
            }
        }
    }
    excluded
}

fn playbook_reference_matches(
    reference: &str,
    records: &[(InclusionTrace, &MemoryRecord)],
    graph_snapshot: Option<&SkillLintSnapshot>,
) -> Vec<String> {
    records
        .iter()
        .filter_map(|(_, record)| {
            let skill = snapshot_skill_for_playbook(record, graph_snapshot);
            let skill_id = playbook_skill_id(record, graph_snapshot);
            (skill_id == reference
                || playbook_lane(record, graph_snapshot) == reference
                || merged_graph_list(record, "provides", skill.map(|s| &s.provides))
                    .iter()
                    .any(|provided| provided == reference))
            .then_some(skill_id)
        })
        .collect()
}

fn render_budgeted_playbooks(
    ordered_records: Vec<(InclusionTrace, &MemoryRecord)>,
    max_body_bytes: Option<u64>,
) -> LoadedSegment {
    let mut body = String::new();
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    let active_index = ordered_records.len().saturating_sub(1);
    let active_block_len = ordered_records
        .get(active_index)
        .map(|(_, record)| render_record_block(record).len() as u64)
        .unwrap_or(0);
    for (idx, (trace, record)) in ordered_records.into_iter().enumerate() {
        let block = render_record_block(record);
        let reserved_bytes = if idx == active_index {
            0
        } else {
            active_block_len
        };
        let would_fit = max_body_bytes.is_none_or(|limit| {
            (body.len() as u64)
                .saturating_add(block.len() as u64)
                .saturating_add(reserved_bytes)
                <= limit
        });
        if idx != active_index && !would_fit {
            excluded.push(ExclusionTrace {
                record_id: trace.record_id,
                reason: ExclusionReason::BeyondTopK,
            });
            continue;
        }
        body.push_str(&block);
        included.push(trace);
    }
    LoadedSegment {
        body,
        included,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn playbook_record(id: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::Playbook;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: records,
            skill_graph_snapshot: None,
            rolling_summary_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    trait WithGraphMetadata {
        fn with_graph_metadata(
            self,
            skill_id: &str,
            lane: &str,
            requires: &[&str],
            provides: &[&str],
            conflicts: &[&str],
        ) -> Self;
    }

    impl WithGraphMetadata for MemoryRecord {
        fn with_graph_metadata(
            mut self,
            skill_id: &str,
            lane: &str,
            requires: &[&str],
            provides: &[&str],
            conflicts: &[&str],
        ) -> Self {
            self.body = format!("{skill_id}\n{}", self.body);
            self.extra_frontmatter
                .insert("skill_id".to_owned(), serde_json::json!(skill_id));
            self.extra_frontmatter
                .insert("lane".to_owned(), serde_json::json!(lane));
            self.extra_frontmatter
                .insert("requires".to_owned(), serde_json::json!(requires));
            self.extra_frontmatter
                .insert("provides".to_owned(), serde_json::json!(provides));
            self.extra_frontmatter
                .insert("conflicts".to_owned(), serde_json::json!(conflicts));
            self
        }
    }

    #[test]
    fn playbook_includes_prerequisite_chain_before_active_playbook() {
        let prereq = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z")
            .with_graph_metadata("run-tests", "test.run", &[], &["cap.test"], &[]);
        let active = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z")
            .with_graph_metadata("ship-pr", "ship.pr", &["cap.test"], &["cap.ship"], &[]);
        let recs = [&prereq, &active];

        let seg = select_with_budget(&input_with(&recs), Some(4096));

        assert_eq!(
            seg.included
                .iter()
                .map(|trace| trace.record_id.as_str())
                .collect::<Vec<_>>(),
            vec![prereq.id.as_str(), active.id.as_str()]
        );
        assert!(seg.body.find("run-tests").unwrap() < seg.body.find("ship-pr").unwrap());
    }

    #[test]
    fn playbook_omits_prerequisite_when_remaining_budget_is_too_small() {
        let mut prereq = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z")
            .with_graph_metadata("large-prereq", "test.large", &[], &["cap.test"], &[]);
        prereq.body.push_str(&"x".repeat(256));
        let active = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z")
            .with_graph_metadata("ship-pr", "ship.pr", &["cap.test"], &["cap.ship"], &[]);
        let recs = [&prereq, &active];

        let seg = select_with_budget(&input_with(&recs), Some(128));

        assert_eq!(seg.included.len(), 1);
        assert_eq!(seg.included[0].record_id, active.id);
        assert!(seg.excluded.iter().any(|trace| {
            trace.record_id == prereq.id && trace.reason == ExclusionReason::BeyondTopK
        }));
    }

    #[test]
    fn playbook_excludes_prerequisite_that_conflicts_with_active_playbook() {
        let prereq = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z")
            .with_graph_metadata("run-tests", "test.run", &[], &["cap.test"], &["ship-pr"]);
        let active = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z")
            .with_graph_metadata("ship-pr", "ship.pr", &["cap.test"], &["cap.ship"], &[]);
        let recs = [&prereq, &active];

        let seg = select_with_budget(&input_with(&recs), Some(4096));

        assert_eq!(
            seg.included
                .iter()
                .map(|trace| trace.record_id.as_str())
                .collect::<Vec<_>>(),
            vec![active.id.as_str()]
        );
        assert!(!seg.body.contains("run-tests"));
        assert!(seg.excluded.iter().any(|trace| {
            trace.record_id == prereq.id && trace.reason == ExclusionReason::BeyondTopK
        }));
    }

    #[test]
    fn playbook_reserves_budget_for_active_before_admitting_prerequisite() {
        let mut prereq = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z")
            .with_graph_metadata("medium-prereq", "test.medium", &[], &["cap.test"], &[]);
        prereq.body.push_str(&"x".repeat(64));
        let active = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z")
            .with_graph_metadata("ship-pr", "ship.pr", &["cap.test"], &["cap.ship"], &[]);
        let prereq_block_len = render_record_block(&prereq).len() as u64;
        let active_block_len = render_record_block(&active).len() as u64;
        assert!(active_block_len < prereq_block_len);
        let recs = [&prereq, &active];

        let seg = select_with_budget(&input_with(&recs), Some(prereq_block_len));

        assert!(active_block_len <= prereq_block_len);
        assert!(prereq_block_len < prereq_block_len + active_block_len);
        assert_eq!(
            seg.included
                .iter()
                .map(|trace| trace.record_id.as_str())
                .collect::<Vec<_>>(),
            vec![active.id.as_str()]
        );
        assert!(seg.excluded.iter().any(|trace| {
            trace.record_id == prereq.id && trace.reason == ExclusionReason::BeyondTopK
        }));
    }

    #[test]
    fn most_recent_wins() {
        let old = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z");
        let new_p = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z");
        let recs = [&old, &new_p];
        let seg = select(&input_with(&recs));
        assert_eq!(seg.included.len(), 1);
        assert_eq!(seg.included[0].record_id, new_p.id);
        assert_eq!(seg.excluded.len(), 1);
        assert_eq!(seg.excluded[0].reason, ExclusionReason::BeyondTopK);
    }

    #[test]
    fn empty_input_emits_empty_body() {
        let seg = select(&input_with(&[]));
        assert!(seg.body.is_empty());
        assert!(seg.included.is_empty());
        assert!(seg.excluded.is_empty());
    }

    #[test]
    fn rejects_non_playbook_kind() {
        let mut r = playbook_record("01HQZX9F5N0000000000000001", "2026-04-22T14:00:00Z");
        r.kind = MemoryKind::Project;
        let recs = [&r];
        let seg = select(&input_with(&recs));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded[0].reason, ExclusionReason::NotPinned);
    }
}
