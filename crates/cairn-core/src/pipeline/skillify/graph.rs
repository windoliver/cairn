//! Pure dependency-aware skill graph resolver.

use std::collections::{BTreeMap, BTreeSet};

use super::lint::{SkillLintSkill, SkillLintSnapshot};

/// Skill graph diagnostic category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillGraphIssueKind {
    /// A dependency token did not resolve to any known skill, lane, or capability.
    MissingDependency,
    /// A dependency token resolved to more than one provider.
    AmbiguousDependency,
    /// Dependency traversal found a cycle.
    Cycle,
    /// A resolved prerequisite conflicts with another selected skill.
    Conflict,
}

/// One graph diagnostic emitted by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillGraphIssue {
    /// Diagnostic category.
    pub kind: SkillGraphIssueKind,
    /// Skill that owns the problematic declaration.
    pub skill_id: String,
    /// Reference token that triggered the diagnostic.
    pub reference: String,
    /// Human-readable detail for lint and explain output.
    pub message: String,
}

/// Ordered dependency closure for one requested skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillGraphClosure {
    /// Requested skill id.
    pub root_skill_id: String,
    /// Prerequisite skill ids ordered from deepest prerequisite to direct parent.
    pub prerequisites: Vec<String>,
    /// Diagnostics found while resolving the closure.
    pub issues: Vec<SkillGraphIssue>,
}

/// Compact explain payload for search and hot-memory debug callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillGraphExplain {
    /// Skill id for the hit or active playbook.
    pub skill_id: String,
    /// Ordered prerequisite skill ids.
    pub prerequisites: Vec<String>,
    /// Diagnostics rendered as stable text.
    pub diagnostics: Vec<String>,
}

/// Pure resolver over one skill lint snapshot.
pub struct SkillGraphResolver<'a> {
    skills: BTreeMap<&'a str, &'a SkillLintSkill>,
    lanes: BTreeMap<&'a str, Vec<&'a str>>,
    provides: BTreeMap<&'a str, Vec<&'a str>>,
}

impl<'a> SkillGraphResolver<'a> {
    /// Build deterministic indexes for a skill snapshot.
    #[must_use]
    pub fn new(snapshot: &'a SkillLintSnapshot) -> Self {
        let mut skills = BTreeMap::new();
        let mut lanes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut provides: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

        for skill in &snapshot.skills {
            skills.insert(skill.skill_id.as_str(), skill);
            if !skill.lane.trim().is_empty() {
                lanes
                    .entry(skill.lane.as_str())
                    .or_default()
                    .push(skill.skill_id.as_str());
            }
            for provided in &skill.provides {
                if !provided.trim().is_empty() {
                    provides
                        .entry(provided.as_str())
                        .or_default()
                        .push(skill.skill_id.as_str());
                }
            }
        }
        for lane_skills in lanes.values_mut() {
            lane_skills.sort_unstable();
            lane_skills.dedup();
        }
        for providers in provides.values_mut() {
            providers.sort_unstable();
            providers.dedup();
        }

        Self {
            skills,
            lanes,
            provides,
        }
    }

    /// Resolve one skill's ordered prerequisite closure.
    #[must_use]
    pub fn resolve_prerequisites(&self, skill_id: &str) -> SkillGraphClosure {
        let mut ordered = Vec::new();
        let mut issues = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();

        self.visit(
            skill_id,
            skill_id,
            &mut visiting,
            &mut visited,
            &mut ordered,
            &mut issues,
        );
        ordered.retain(|id| id != skill_id);
        self.append_selected_conflict_issues(skill_id, &ordered, &mut issues);

        SkillGraphClosure {
            root_skill_id: skill_id.to_owned(),
            prerequisites: ordered,
            issues,
        }
    }

    /// Emit graph diagnostics for every skill in the snapshot.
    #[must_use]
    pub fn lint_all(&self) -> Vec<SkillGraphIssue> {
        let mut issues = Vec::new();
        for skill_id in self.skills.keys() {
            issues.extend(self.resolve_prerequisites(skill_id).issues);
        }
        issues.sort_by(|a, b| {
            a.skill_id
                .cmp(&b.skill_id)
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| a.reference.cmp(&b.reference))
                .then_with(|| a.message.cmp(&b.message))
        });
        issues.dedup_by(|a, b| {
            a.kind == b.kind && a.skill_id == b.skill_id && a.reference == b.reference
        });
        issues
    }

    fn visit(
        &self,
        owner: &str,
        current: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
        issues: &mut Vec<SkillGraphIssue>,
    ) {
        if visited.contains(current) {
            return;
        }
        if !visiting.insert(current.to_owned()) {
            issues.push(issue(
                SkillGraphIssueKind::Cycle,
                owner,
                current,
                format!("skill `{owner}` dependency graph cycles through `{current}`"),
            ));
            return;
        }

        let Some(skill) = self.skills.get(current).copied() else {
            return;
        };

        for required in &skill.requires {
            match self.resolve_reference(required) {
                ReferenceResolution::One(next) => {
                    self.visit(owner, next, visiting, visited, ordered, issues);
                    if next != owner && !ordered.iter().any(|id| id == next) {
                        ordered.push(next.to_owned());
                    }
                }
                ReferenceResolution::Missing => issues.push(issue(
                    SkillGraphIssueKind::MissingDependency,
                    current,
                    required,
                    format!(
                        "skill `{current}` requires `{required}` but no skill, lane, or capability provides it"
                    ),
                )),
                ReferenceResolution::Ambiguous(matches) => issues.push(issue(
                    SkillGraphIssueKind::AmbiguousDependency,
                    current,
                    required,
                    format!(
                        "skill `{current}` requires `{required}` but multiple skills provide it: {}",
                        matches.join(", ")
                    ),
                )),
            }
        }

        visiting.remove(current);
        visited.insert(current.to_owned());
    }

    fn resolve_reference(&self, reference: &str) -> ReferenceResolution<'a> {
        if let Some((skill_id, _)) = self.skills.get_key_value(reference) {
            return ReferenceResolution::One(skill_id);
        }
        match self.lanes.get(reference).map(Vec::as_slice) {
            Some([one]) => return ReferenceResolution::One(one),
            Some(matches) if matches.len() > 1 => {
                return ReferenceResolution::Ambiguous(
                    matches
                        .iter()
                        .map(|skill_id| (*skill_id).to_owned())
                        .collect(),
                );
            }
            _ => {}
        }
        match self.provides.get(reference).map(Vec::as_slice) {
            Some([one]) => ReferenceResolution::One(one),
            Some(matches) if matches.len() > 1 => ReferenceResolution::Ambiguous(
                matches
                    .iter()
                    .map(|skill_id| (*skill_id).to_owned())
                    .collect(),
            ),
            _ => ReferenceResolution::Missing,
        }
    }

    fn match_conflict(&self, reference: &str) -> Vec<String> {
        match self.resolve_reference(reference) {
            ReferenceResolution::One(skill_id) => vec![skill_id.to_owned()],
            ReferenceResolution::Ambiguous(matches) => matches,
            ReferenceResolution::Missing => vec![reference.to_owned()],
        }
    }

    fn append_selected_conflict_issues(
        &self,
        root_skill_id: &str,
        prerequisites: &[String],
        issues: &mut Vec<SkillGraphIssue>,
    ) {
        let mut selected = BTreeSet::new();
        selected.insert(root_skill_id.to_owned());
        selected.extend(prerequisites.iter().cloned());

        for skill_id in &selected {
            let Some(skill) = self.skills.get(skill_id.as_str()).copied() else {
                continue;
            };
            for conflict in &skill.conflicts {
                let matches = self.match_conflict(conflict);
                if matches.iter().any(|candidate| selected.contains(candidate)) {
                    issues.push(issue(
                        SkillGraphIssueKind::Conflict,
                        skill_id,
                        conflict,
                        format!(
                            "skill `{skill_id}` conflicts with selected dependency `{conflict}`"
                        ),
                    ));
                }
            }
        }
    }
}

enum ReferenceResolution<'a> {
    One(&'a str),
    Missing,
    Ambiguous(Vec<String>),
}

fn issue(
    kind: SkillGraphIssueKind,
    skill_id: &str,
    reference: &str,
    message: String,
) -> SkillGraphIssue {
    SkillGraphIssue {
        kind,
        skill_id: skill_id.to_owned(),
        reference: reference.to_owned(),
        message,
    }
}
