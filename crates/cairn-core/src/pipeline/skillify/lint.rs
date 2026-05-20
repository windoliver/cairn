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

    for skill in &snapshot.skills {
        lanes.entry(&skill.lane).or_default().push(skill);
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

    out
}

fn issue(kind: SkillLintIssueKind, skill: &SkillLintSkill, message: String) -> SkillLintIssue {
    SkillLintIssue {
        kind,
        skill_id: skill.skill_id.clone(),
        path: skill.path.clone(),
        message,
    }
}
