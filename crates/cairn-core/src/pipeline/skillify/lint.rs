//! Pure Skillify bundle lint checks.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Snapshot of live skill metadata needed by the pure lint pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintSnapshot {
    /// Skills to lint.
    pub skills: Vec<SkillLintSkill>,
}

/// One skill entry in a lint snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintSkill {
    /// Stable skill id.
    pub skill_id: String,
    /// Resolver lane claimed by this skill.
    pub lane: String,
    /// Path to the skill contract.
    pub path: String,
    /// Script path referenced by the skill contract.
    pub uses: Option<String>,
    /// Resolver triggers that can reach the skill.
    pub resolver_triggers: Vec<String>,
    /// Filing target declared by the skill.
    pub files_to: Option<String>,
    /// Whether the latest gate report passed.
    pub gate_report_passed: bool,
    /// Number of rollback versions available.
    pub rollback_version_count: u32,
    /// Paths present in the snapshot.
    pub existing_paths: Vec<String>,
}

/// Skill lint issue category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillLintIssueKind {
    /// Referenced artifact is absent.
    MissingArtifact,
    /// Skill has no resolver path.
    Unreachable,
    /// More than one live skill claims the same lane.
    DuplicateLane,
    /// Gate report is not passing.
    GateFailed,
    /// Rollback metadata is absent.
    RollbackBroken,
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLintIssue {
    /// Finding kind.
    pub kind: SkillLintIssueKind,
    /// Skill id owning the issue.
    pub skill_id: String,
    /// Skill path owning the issue.
    pub path: String,
    /// Human-readable finding detail.
    pub message: String,
}

/// Run deterministic lint checks against a skill snapshot.
#[must_use]
pub fn lint_skill_snapshot(snapshot: &SkillLintSnapshot) -> Vec<SkillLintIssue> {
    let mut out = Vec::new();
    let mut lanes: BTreeMap<&str, Vec<&SkillLintSkill>> = BTreeMap::new();
    let mut triggers: BTreeMap<String, Vec<&SkillLintSkill>> = BTreeMap::new();

    for skill in &snapshot.skills {
        index_skill(skill, &mut lanes, &mut triggers);
        lint_single_skill(skill, &mut out);
    }

    append_duplicate_lane_issues(lanes, &mut out);
    append_duplicate_trigger_issues(triggers, &mut out);
    sort_issues(&mut out);

    out
}

fn index_skill<'a>(
    skill: &'a SkillLintSkill,
    lanes: &mut BTreeMap<&'a str, Vec<&'a SkillLintSkill>>,
    triggers: &mut BTreeMap<String, Vec<&'a SkillLintSkill>>,
) {
    lanes.entry(skill.lane.as_str()).or_default().push(skill);
    for trigger in &skill.resolver_triggers {
        let normalized = trigger.trim();
        if !normalized.is_empty() {
            triggers
                .entry(normalized.to_owned())
                .or_default()
                .push(skill);
        }
    }
}

fn lint_single_skill(skill: &SkillLintSkill, out: &mut Vec<SkillLintIssue>) {
    let existing: BTreeSet<&str> = skill.existing_paths.iter().map(String::as_str).collect();
    if let Some(uses) = &skill.uses
        && !existing.contains(uses.as_str())
    {
        out.push(issue(
            SkillLintIssueKind::MissingArtifact,
            skill,
            format!(
                "skill `{}` references missing script `{uses}`",
                skill.skill_id
            ),
        ));
    }
    if skill.resolver_triggers.is_empty() {
        out.push(issue(
            SkillLintIssueKind::Unreachable,
            skill,
            format!("skill `{}` has no resolver triggers", skill.skill_id),
        ));
    }
    for trigger in &skill.resolver_triggers {
        if trigger.trim().is_empty() {
            out.push(issue(
                SkillLintIssueKind::Unreachable,
                skill,
                format!("skill `{}` has a blank resolver trigger", skill.skill_id),
            ));
        }
    }
    match skill.files_to.as_deref().map(str::trim) {
        None | Some("") => out.push(issue(
            SkillLintIssueKind::MissingArtifact,
            skill,
            format!(
                "skill `{}` is missing files_to filing rules",
                skill.skill_id
            ),
        )),
        Some(files_to) if !valid_relative_dir(files_to) => out.push(issue(
            SkillLintIssueKind::MissingArtifact,
            skill,
            format!(
                "skill `{}` has invalid files_to `{files_to}`",
                skill.skill_id
            ),
        )),
        Some(_) => {}
    }
    if !skill.gate_report_passed {
        out.push(issue(
            SkillLintIssueKind::GateFailed,
            skill,
            format!(
                "skill `{}` does not have a passing gate report",
                skill.skill_id
            ),
        ));
    }
    if skill.rollback_version_count == 0 {
        out.push(issue(
            SkillLintIssueKind::RollbackBroken,
            skill,
            format!(
                "skill `{}` has no rollback version metadata",
                skill.skill_id
            ),
        ));
    }
}

fn append_duplicate_lane_issues(
    lanes: BTreeMap<&str, Vec<&SkillLintSkill>>,
    out: &mut Vec<SkillLintIssue>,
) {
    for (lane, skills) in lanes {
        if skills.len() > 1 {
            for skill in skills {
                out.push(issue(
                    SkillLintIssueKind::DuplicateLane,
                    skill,
                    format!("lane `{lane}` is used by more than one live skill"),
                ));
            }
        }
    }
}

fn append_duplicate_trigger_issues(
    triggers: BTreeMap<String, Vec<&SkillLintSkill>>,
    out: &mut Vec<SkillLintIssue>,
) {
    for (trigger, mut skills) in triggers {
        skills.sort_by(|a, b| {
            a.skill_id
                .cmp(&b.skill_id)
                .then_with(|| a.path.cmp(&b.path))
        });
        skills.dedup_by(|a, b| a.skill_id == b.skill_id && a.path == b.path);
        if skills.len() > 1 {
            for skill in skills {
                out.push(issue(
                    SkillLintIssueKind::DuplicateLane,
                    skill,
                    format!("resolver trigger `{trigger}` is used by more than one live skill"),
                ));
            }
        }
    }
}

fn sort_issues(out: &mut [SkillLintIssue]) {
    out.sort_by(|a, b| {
        a.skill_id
            .cmp(&b.skill_id)
            .then_with(|| issue_kind_rank(a.kind).cmp(&issue_kind_rank(b.kind)))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.message.cmp(&b.message))
    });
}

pub(crate) fn valid_relative_dir(value: &str) -> bool {
    let path = std::path::Path::new(value);
    !path.is_absolute()
        && value.ends_with('/')
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn issue_kind_rank(kind: SkillLintIssueKind) -> u8 {
    match kind {
        SkillLintIssueKind::MissingArtifact => 0,
        SkillLintIssueKind::Unreachable => 1,
        SkillLintIssueKind::DuplicateLane => 2,
        SkillLintIssueKind::GateFailed => 3,
        SkillLintIssueKind::RollbackBroken => 4,
    }
}

fn issue(kind: SkillLintIssueKind, skill: &SkillLintSkill, message: String) -> SkillLintIssue {
    SkillLintIssue {
        kind,
        skill_id: skill.skill_id.clone(),
        path: skill.path.clone(),
        message,
    }
}
