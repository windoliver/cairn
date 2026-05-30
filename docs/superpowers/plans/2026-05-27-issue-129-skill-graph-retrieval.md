# Skill Graph Retrieval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement dependency-aware skill graph retrieval for issue #129 so skill dependencies and conflicts are represented once in core, checked by lint, used by hot memory, and exposed through search explain where the CLI can provide graph metadata.

**Architecture:** Add a pure `SkillGraphResolver` in `cairn-core` over existing Skillify metadata. CLI and workflow snapshot builders feed `requires`, `provides`, and `conflicts` into the shared model. Hot-memory and search explain consume the resolver without changing ranking or adding persistent storage.

**Tech Stack:** Rust 2024, `serde`, existing Cairn IDL/codegen, existing `cairn-core`, `cairn-cli`, and `cairn-workflows` test harnesses.

---

## File Structure

- Create `crates/cairn-core/src/pipeline/skillify/graph.rs`: pure graph resolver, graph diagnostics, and graph explain structs.
- Modify `crates/cairn-core/src/pipeline/skillify/lint.rs`: add graph metadata fields to `SkillLintSkill` and translate resolver diagnostics into existing lint issue kinds.
- Modify `crates/cairn-core/src/pipeline/skillify/mod.rs`: re-export graph resolver types.
- Modify `crates/cairn-core/tests/skillify_model.rs`: add resolver, metadata, and lint tests.
- Modify `crates/cairn-cli/src/verbs/lint.rs`: expose the snapshot builder within the crate and parse graph metadata from skill YAML.
- Modify `crates/cairn-cli/tests/lint_skill.rs`: add CLI-level graph lint tests.
- Modify `crates/cairn-workflows/src/skillify/snapshot.rs`: parse graph metadata in workflow-local snapshots.
- Modify `crates/cairn-workflows/src/skillify/snapshot.rs` tests: prove graph metadata reaches gate-run snapshots.
- Modify `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`: render active playbook prerequisites within the remaining segment budget.
- Modify `crates/cairn-core/src/verbs/assemble_hot/assembler.rs`: pass remaining hot-memory bytes into the playbook source.
- Modify `crates/cairn-core/src/verbs/search.rs`: attach optional skill graph explain data to search outcomes when a snapshot is present.
- Modify `crates/cairn-core/src/search/explain.rs`: carry optional skill graph explain data in core score explanations.
- Modify `crates/cairn-idl/schema/verbs/search.json`: add optional `skill_graph` to `ScoreExplain`.
- Regenerate generated files with `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` first, then with the repo's write path if drift is expected by the codegen output.

## Task 1: Add Skill Graph Metadata To Skill Snapshots

**Files:**
- Modify: `crates/cairn-core/src/pipeline/skillify/lint.rs`
- Modify: `crates/cairn-core/tests/skillify_model.rs`
- Modify: every existing `SkillLintSkill { ... }` literal reported by `rg -n "SkillLintSkill \\{" crates`

- [ ] **Step 1: Write the failing metadata round-trip test**

Add this test to `crates/cairn-core/tests/skillify_model.rs`:

```rust
#[test]
fn skill_lint_skill_graph_metadata_round_trips() {
    let skill = SkillLintSkill {
        skill_id: "deploy-hotfix".to_owned(),
        lane: "deploy.hotfix".to_owned(),
        path: "skills/skill_deploy-hotfix.md".to_owned(),
        uses: Some("skills/scripts/deploy-hotfix.sh".to_owned()),
        resolver_triggers: vec!["deploy hotfix".to_owned()],
        files_to: Some("wiki/summaries/".to_owned()),
        gate_report_passed: true,
        rollback_version_count: 1,
        existing_paths: vec!["skills/scripts/deploy-hotfix.sh".to_owned()],
        requires: vec!["shell.exec".to_owned()],
        provides: vec!["deploy.hotfix".to_owned()],
        conflicts: vec!["deploy.rollback".to_owned()],
    };

    let json = serde_json::to_string(&skill).expect("serialize");
    assert!(json.contains("\"requires\""));
    assert!(json.contains("\"provides\""));
    assert!(json.contains("\"conflicts\""));

    let parsed: SkillLintSkill = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.requires, ["shell.exec"]);
    assert_eq!(parsed.provides, ["deploy.hotfix"]);
    assert_eq!(parsed.conflicts, ["deploy.rollback"]);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-core skill_lint_skill_graph_metadata_round_trips --locked
```

Expected: FAIL with struct field errors for `requires`, `provides`, and `conflicts`.

- [ ] **Step 3: Add graph fields with legacy-safe serde defaults**

In `crates/cairn-core/src/pipeline/skillify/lint.rs`, extend `SkillLintSkill`:

```rust
    /// Capability, lane, or skill ids this skill needs before activation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Capability ids this skill contributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<String>,
    /// Skill ids, lanes, or capability ids incompatible with this skill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
```

For every existing `SkillLintSkill` literal, add:

```rust
requires: vec![],
provides: vec![],
conflicts: vec![],
```

- [ ] **Step 4: Run the metadata test to verify it passes**

Run:

```bash
cargo test -p cairn-core skill_lint_skill_graph_metadata_round_trips --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/lint.rs crates/cairn-core/tests/skillify_model.rs crates/cairn-workflows/src/skillify/snapshot.rs crates/cairn-workflows/tests/skillify_gate_runners.rs crates/cairn-cli/src/verbs/lint.rs crates/cairn-cli/tests/lint_skill.rs
git commit -m "feat(skillify): carry skill graph metadata"
```

## Task 2: Implement The Pure Skill Graph Resolver

**Files:**
- Create: `crates/cairn-core/src/pipeline/skillify/graph.rs`
- Modify: `crates/cairn-core/src/pipeline/skillify/mod.rs`
- Modify: `crates/cairn-core/tests/skillify_model.rs`

- [ ] **Step 1: Write failing resolver tests**

Add these tests to `crates/cairn-core/tests/skillify_model.rs`:

```rust
fn graph_skill(
    skill_id: &str,
    lane: &str,
    requires: &[&str],
    provides: &[&str],
    conflicts: &[&str],
) -> SkillLintSkill {
    SkillLintSkill {
        skill_id: skill_id.to_owned(),
        lane: lane.to_owned(),
        path: format!("skills/skill_{skill_id}.md"),
        uses: None,
        resolver_triggers: vec![format!("run {skill_id}")],
        files_to: Some("wiki/summaries/".to_owned()),
        gate_report_passed: true,
        rollback_version_count: 1,
        existing_paths: vec![],
        requires: requires.iter().map(|s| (*s).to_owned()).collect(),
        provides: provides.iter().map(|s| (*s).to_owned()).collect(),
        conflicts: conflicts.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn skill_graph_resolver_orders_transitive_prereqs() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("run-tests", "test.run", &[], &["cap.test"], &[]),
            graph_skill("lint-diff", "lint.diff", &["cap.test"], &["cap.lint"], &[]),
            graph_skill("ship-pr", "ship.pr", &["cap.lint"], &["cap.ship"], &[]),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let closure = resolver.resolve_prerequisites("ship-pr");

    assert_eq!(closure.prerequisites, ["run-tests", "lint-diff"]);
    assert!(closure.issues.is_empty());
}

#[test]
fn skill_graph_resolver_reports_missing_ambiguous_cycle_and_conflict() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("a", "lane.a", &["cap.missing"], &["cap.a"], &[]),
            graph_skill("b1", "lane.b1", &[], &["cap.shared"], &[]),
            graph_skill("b2", "lane.b2", &[], &["cap.shared"], &[]),
            graph_skill("c", "lane.c", &["cap.shared"], &["cap.c"], &[]),
            graph_skill("cycle-a", "lane.cycle.a", &["cycle-b"], &["cap.cycle.a"], &[]),
            graph_skill("cycle-b", "lane.cycle.b", &["cycle-a"], &["cap.cycle.b"], &[]),
            graph_skill("conflict-a", "lane.conflict.a", &["conflict-b"], &["cap.conflict.a"], &["conflict-b"]),
            graph_skill("conflict-b", "lane.conflict.b", &[], &["cap.conflict.b"], &[]),
        ],
    };

    let resolver = SkillGraphResolver::new(&snapshot);
    let issues = resolver.lint_all();
    let kinds: Vec<_> = issues.iter().map(|issue| issue.kind).collect();

    assert!(kinds.contains(&SkillGraphIssueKind::MissingDependency));
    assert!(kinds.contains(&SkillGraphIssueKind::AmbiguousDependency));
    assert!(kinds.contains(&SkillGraphIssueKind::Cycle));
    assert!(kinds.contains(&SkillGraphIssueKind::Conflict));
}
```

Add the imports at the top:

```rust
use cairn_core::pipeline::skillify::{
    SkillGraphIssueKind, SkillGraphResolver,
};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p cairn-core skill_graph_resolver --locked
```

Expected: FAIL with unresolved imports for `SkillGraphIssueKind` and `SkillGraphResolver`.

- [ ] **Step 3: Create the resolver implementation**

Create `crates/cairn-core/src/pipeline/skillify/graph.rs` with this structure:

```rust
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
    lanes: BTreeMap<&'a str, &'a str>,
    provides: BTreeMap<&'a str, Vec<&'a str>>,
}

impl<'a> SkillGraphResolver<'a> {
    /// Build deterministic indexes for a skill snapshot.
    #[must_use]
    pub fn new(snapshot: &'a SkillLintSnapshot) -> Self {
        let mut skills = BTreeMap::new();
        let mut lanes = BTreeMap::new();
        let mut provides: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for skill in &snapshot.skills {
            skills.insert(skill.skill_id.as_str(), skill);
            if !skill.lane.trim().is_empty() {
                lanes.insert(skill.lane.as_str(), skill.skill_id.as_str());
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
        for providers in provides.values_mut() {
            providers.sort_unstable();
            providers.dedup();
        }
        Self { skills, lanes, provides }
    }

    /// Resolve one skill's ordered prerequisite closure.
    #[must_use]
    pub fn resolve_prerequisites(&self, skill_id: &str) -> SkillGraphClosure {
        let mut ordered = Vec::new();
        let mut issues = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.visit(skill_id, skill_id, &mut visiting, &mut visited, &mut ordered, &mut issues);
        ordered.retain(|id| id != skill_id);
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
                    self.append_conflict_issues(owner, current, next, ordered, issues);
                    if next != owner && !ordered.iter().any(|id| id == next) {
                        ordered.push(next.to_owned());
                    }
                }
                ReferenceResolution::Missing => issues.push(issue(
                    SkillGraphIssueKind::MissingDependency,
                    current,
                    required,
                    format!("skill `{current}` requires `{required}` but no skill, lane, or capability provides it"),
                )),
                ReferenceResolution::Ambiguous(matches) => issues.push(issue(
                    SkillGraphIssueKind::AmbiguousDependency,
                    current,
                    required,
                    format!("skill `{current}` requires `{required}` but multiple skills provide it: {}", matches.join(", ")),
                )),
            }
        }
        visiting.remove(current);
        visited.insert(current.to_owned());
    }

    fn append_conflict_issues(
        &self,
        owner: &str,
        current: &str,
        next: &str,
        ordered: &[String],
        issues: &mut Vec<SkillGraphIssue>,
    ) {
        let Some(current_skill) = self.skills.get(current).copied() else {
            return;
        };
        let mut selected: BTreeSet<&str> = ordered.iter().map(String::as_str).collect();
        selected.insert(owner);
        selected.insert(current);
        selected.insert(next);
        for conflict in &current_skill.conflicts {
            let matches = self.match_conflict(conflict);
            if matches.iter().any(|candidate| selected.contains(candidate.as_str())) {
                issues.push(issue(
                    SkillGraphIssueKind::Conflict,
                    current,
                    conflict,
                    format!("skill `{current}` conflicts with selected dependency `{conflict}`"),
                ));
            }
        }
    }

    fn resolve_reference(&self, reference: &str) -> ReferenceResolution {
        if self.skills.contains_key(reference) {
            return ReferenceResolution::One(reference);
        }
        if let Some(skill_id) = self.lanes.get(reference).copied() {
            return ReferenceResolution::One(skill_id);
        }
        match self.provides.get(reference).map(Vec::as_slice) {
            Some([one]) => ReferenceResolution::One(one),
            Some(matches) if matches.len() > 1 => {
                ReferenceResolution::Ambiguous(matches.iter().map(|s| (*s).to_owned()).collect())
            }
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
```

- [ ] **Step 4: Re-export the resolver types**

In `crates/cairn-core/src/pipeline/skillify/mod.rs`, add:

```rust
mod graph;
```

and:

```rust
pub use graph::{
    SkillGraphClosure, SkillGraphExplain, SkillGraphIssue, SkillGraphIssueKind,
    SkillGraphResolver,
};
```

- [ ] **Step 5: Run the resolver tests**

Run:

```bash
cargo test -p cairn-core skill_graph_resolver --locked
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/graph.rs crates/cairn-core/src/pipeline/skillify/mod.rs crates/cairn-core/tests/skillify_model.rs
git commit -m "feat(skillify): resolve skill dependency graph"
```

## Task 3: Wire Graph Diagnostics Into Skill Lint

**Files:**
- Modify: `crates/cairn-core/src/pipeline/skillify/lint.rs`
- Modify: `crates/cairn-core/tests/skillify_model.rs`

- [ ] **Step 1: Write failing lint tests**

Add these tests to `crates/cairn-core/tests/skillify_model.rs`:

```rust
#[test]
fn skill_lint_reports_missing_graph_reference() {
    let snapshot = SkillLintSnapshot {
        skills: vec![graph_skill(
            "deploy-hotfix",
            "deploy.hotfix",
            &["cap.shell"],
            &["cap.deploy"],
            &[],
        )],
    };

    let findings = lint_skill_snapshot(&snapshot);
    assert!(findings.iter().any(|finding| {
        finding.kind == SkillLintIssueKind::MissingArtifact
            && finding.message.contains("requires `cap.shell`")
    }));
}

#[test]
fn skill_lint_reports_graph_cycle_and_conflict() {
    let snapshot = SkillLintSnapshot {
        skills: vec![
            graph_skill("cycle-a", "lane.cycle.a", &["cycle-b"], &["cap.a"], &[]),
            graph_skill("cycle-b", "lane.cycle.b", &["cycle-a"], &["cap.b"], &[]),
            graph_skill("conflict-a", "lane.conflict.a", &["conflict-b"], &["cap.conflict.a"], &["conflict-b"]),
            graph_skill("conflict-b", "lane.conflict.b", &[], &["cap.conflict.b"], &[]),
        ],
    };

    let findings = lint_skill_snapshot(&snapshot);
    assert!(findings.iter().any(|finding| {
        finding.kind == SkillLintIssueKind::DuplicateLane
            && finding.message.contains("cycles through")
    }));
    assert!(findings.iter().any(|finding| {
        finding.kind == SkillLintIssueKind::DuplicateLane
            && finding.message.contains("conflicts with selected dependency")
    }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
cargo test -p cairn-core "skill_lint_reports_*graph*" --locked
```

Expected: FAIL because `lint_skill_snapshot` does not append graph issues.

- [ ] **Step 3: Append resolver diagnostics in lint**

In `crates/cairn-core/src/pipeline/skillify/lint.rs`, import the graph types:

```rust
use super::graph::{SkillGraphIssue, SkillGraphIssueKind, SkillGraphResolver};
```

In `lint_skill_snapshot`, after duplicate trigger checks and before sorting:

```rust
append_skill_graph_issues(snapshot, &mut out);
```

Add:

```rust
fn append_skill_graph_issues(snapshot: &SkillLintSnapshot, out: &mut Vec<SkillLintIssue>) {
    let resolver = SkillGraphResolver::new(snapshot);
    for graph_issue in resolver.lint_all() {
        out.push(graph_issue_to_lint_issue(snapshot, &graph_issue));
    }
}

fn graph_issue_to_lint_issue(
    snapshot: &SkillLintSnapshot,
    issue: &SkillGraphIssue,
) -> SkillLintIssue {
    let path = snapshot
        .skills
        .iter()
        .find(|skill| skill.skill_id == issue.skill_id)
        .map_or_else(String::new, |skill| skill.path.clone());
    SkillLintIssue {
        kind: match issue.kind {
            SkillGraphIssueKind::MissingDependency | SkillGraphIssueKind::AmbiguousDependency => {
                SkillLintIssueKind::MissingArtifact
            }
            SkillGraphIssueKind::Cycle | SkillGraphIssueKind::Conflict => {
                SkillLintIssueKind::DuplicateLane
            }
        },
        skill_id: issue.skill_id.clone(),
        path,
        message: issue.message.clone(),
    }
}
```

Update `issue_kind_rank` only if the added output order drifts in existing tests. Keep graph missing dependencies grouped with `MissingArtifact` and graph cycles/conflicts grouped with `DuplicateLane`.

- [ ] **Step 4: Run lint tests**

Run:

```bash
cargo test -p cairn-core "skill_lint_reports_*graph*" --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/skillify/lint.rs crates/cairn-core/tests/skillify_model.rs
git commit -m "feat(skillify): lint skill graph references"
```

## Task 4: Parse Graph Metadata From CLI And Workflow Skill Snapshots

**Files:**
- Modify: `crates/cairn-cli/src/verbs/lint.rs`
- Modify: `crates/cairn-cli/tests/lint_skill.rs`
- Modify: `crates/cairn-workflows/src/skillify/snapshot.rs`
- Modify: `crates/cairn-workflows/src/skillify/snapshot.rs` tests

- [ ] **Step 1: Write failing CLI graph lint test**

Add this test to `crates/cairn-cli/tests/lint_skill.rs`:

```rust
#[tokio::test]
async fn lint_skill_reports_missing_requires_reference() {
    let vault = build_hybrid_test_vault(&[]).await;

    std::fs::create_dir_all(vault.root.join("skills")).expect("skills");
    std::fs::create_dir_all(vault.root.join("skills/scripts")).expect("scripts");
    std::fs::create_dir_all(vault.root.join(".cairn/resolver/skills")).expect("resolver");
    std::fs::create_dir_all(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_graph/versions/v1"),
    )
    .expect("versions");
    std::fs::write(vault.root.join("skills/scripts/deploy.sh"), "#!/usr/bin/env bash\nexit 0\n")
        .expect("script");
    std::fs::write(
        vault.root.join("skills/skill_deploy.md"),
        "---\nskill_id: deploy\nversion: 1\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: skills/scripts/deploy.sh\nfiles_to: wiki/summaries/\ncandidate_id: skc_graph\nstatus: live\nrequires: [\"cap.shell\"]\nprovides: [\"cap.deploy\"]\nconflicts: []\n---\nDeploy.\n",
    )
    .expect("skill");
    std::fs::write(
        vault.root.join(".cairn/resolver/skills/deploy.json"),
        r#"{"skill_id":"deploy","triggers":["deploy hotfix"]}"#,
    )
    .expect("resolver");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_graph/gate-report.json"),
        passed_gate_report("skc_graph"),
    )
    .expect("gate");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_graph/versions/v1/manifest.json"),
        "{}",
    )
    .expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&vault.root)
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("skill_missing_artifact"))
        .stdout(predicate::str::contains("requires `cap.shell`"));
}
```

- [ ] **Step 2: Write failing workflow snapshot test**

Add this test to the `#[cfg(test)]` module in `crates/cairn-workflows/src/skillify/snapshot.rs`:

```rust
#[test]
fn snapshot_parses_skill_graph_metadata() {
    let temp = TempDir::new().unwrap();
    let skill_path = temp.path().join("skills/skill_deploy.md");
    write_md(
        &skill_path,
        "---\nname: deploy\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nrequires: [\"cap.shell\"]\nprovides: [\"cap.deploy\"]\nconflicts:\n  - rollback.force\n---\nDeploy.\n",
    );

    let snapshot = build_vault_snapshot(temp.path(), None).expect("snapshot");
    let skill = snapshot
        .skills
        .iter()
        .find(|skill| skill.skill_id == "deploy")
        .expect("skill");

    assert_eq!(skill.requires, ["cap.shell"]);
    assert_eq!(skill.provides, ["cap.deploy"]);
    assert_eq!(skill.conflicts, ["rollback.force"]);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-cli lint_skill_reports_missing_requires_reference --locked
cargo test -p cairn-workflows snapshot_parses_skill_graph_metadata --locked
```

Expected: the CLI test does not report the missing dependency, and the workflow test sees empty graph metadata.

- [ ] **Step 4: Parse graph arrays in CLI lint snapshots**

In `crates/cairn-cli/src/verbs/lint.rs`, add an array parser next to `yaml_scalar`:

```rust
fn yaml_string_list(frontmatter: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let inline = rest.trim();
            if !inline.is_empty() {
                if let Some(arr) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    for item in arr.split(',') {
                        let value = item.trim().trim_matches('"').trim_matches('\'');
                        if !value.is_empty() {
                            out.push(value.to_owned());
                        }
                    }
                } else {
                    let value = inline.trim_matches('"').trim_matches('\'');
                    if !value.is_empty() {
                        out.push(value.to_owned());
                    }
                }
                return out;
            }
            let mut j = i + 1;
            while j < lines.len() {
                if let Some(item) = lines[j].trim_start().strip_prefix("- ") {
                    let value = item.trim().trim_matches('"').trim_matches('\'');
                    if !value.is_empty() {
                        out.push(value.to_owned());
                    }
                    j += 1;
                } else if lines[j].trim().is_empty() {
                    j += 1;
                } else {
                    break;
                }
            }
            return out;
        }
        i += 1;
    }
    out
}
```

In `append_skill_lint_source`, parse:

```rust
let requires = yaml_string_list(frontmatter, "requires");
let provides = yaml_string_list(frontmatter, "provides");
let conflicts = yaml_string_list(frontmatter, "conflicts");
```

Pass these fields into `SkillLintSkill`.

In `push_candidate_lint_placeholder`, set all three graph fields to `vec![]`.

- [ ] **Step 5: Parse graph arrays in workflow snapshots**

In `crates/cairn-workflows/src/skillify/snapshot.rs`, reuse `inline_or_list`:

```rust
let requires = inline_or_list(fm, "requires");
let provides = inline_or_list(fm, "provides");
let conflicts = inline_or_list(fm, "conflicts");
```

Pass the fields into both `read_live_skill` and `read_candidate_skill`.

- [ ] **Step 6: Run parsing tests**

Run:

```bash
cargo test -p cairn-cli lint_skill_reports_missing_requires_reference --locked
cargo test -p cairn-workflows snapshot_parses_skill_graph_metadata --locked
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/lint.rs crates/cairn-cli/tests/lint_skill.rs crates/cairn-workflows/src/skillify/snapshot.rs
git commit -m "feat(skillify): parse skill graph metadata"
```

## Task 5: Include Playbook Prerequisites In Hot Memory Within Budget

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/assembler.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`
- Test: `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`

- [ ] **Step 1: Write failing playbook prerequisite test**

Add this test to the `#[cfg(test)]` module in `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`:

```rust
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
```

Add this helper trait inside the same test module:

```rust
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
        self.extra_frontmatter.insert("skill_id".to_owned(), serde_json::json!(skill_id));
        self.extra_frontmatter.insert("lane".to_owned(), serde_json::json!(lane));
        self.extra_frontmatter.insert("requires".to_owned(), serde_json::json!(requires));
        self.extra_frontmatter.insert("provides".to_owned(), serde_json::json!(provides));
        self.extra_frontmatter.insert("conflicts".to_owned(), serde_json::json!(conflicts));
        self
    }
}
```

- [ ] **Step 2: Write failing budget exclusion test**

Add:

```rust
#[test]
fn playbook_omits_prerequisite_when_remaining_budget_is_too_small() {
    let prereq = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z")
        .with_graph_metadata("large-prereq", "test.large", &[], &["cap.test"], &[]);
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-core playbook_includes_prerequisite_chain_before_active_playbook --locked
cargo test -p cairn-core playbook_omits_prerequisite_when_remaining_budget_is_too_small --locked
```

Expected: FAIL because `select_with_budget` and graph metadata extraction do not exist.

- [ ] **Step 4: Add graph-aware playbook selection**

In `playbook.rs`, keep `select` as the compatibility wrapper:

```rust
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    select_with_budget(inputs, None)
}
```

Add `select_with_budget`:

```rust
pub fn select_with_budget(
    inputs: &HotMemoryInputs<'_>,
    max_body_bytes: Option<u64>,
) -> LoadedSegment {
    let mut admissible = admissible_playbooks(inputs);
    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });
    let Some((active_trace, active_record)) = admissible.first().cloned() else {
        return LoadedSegment::default();
    };

    let snapshot = playbook_snapshot(&admissible);
    let resolver = crate::pipeline::skillify::SkillGraphResolver::new(&snapshot);
    let active_skill_id = playbook_skill_id(active_record).unwrap_or(active_record.id.as_str());
    let closure = resolver.resolve_prerequisites(active_skill_id);
    let mut ordered_records = prerequisite_records(&closure.prerequisites, &admissible);
    ordered_records.push((active_trace, active_record));

    render_budgeted_playbooks(ordered_records, max_body_bytes)
}
```

Implement helpers with deterministic ordering:

```rust
fn admissible_playbooks<'a>(
    inputs: &'a HotMemoryInputs<'a>,
) -> Vec<(InclusionTrace, &'a MemoryRecord)> {
    let mut out = Vec::new();
    for &record in inputs.playbook_candidates {
        if record.kind != MemoryKind::Playbook {
            continue;
        }
        if admit(record, &inputs.scope, inputs.authorized_visibility).is_ok() {
            out.push((
                InclusionTrace {
                    record_id: record.id.clone(),
                    score: 0.0,
                    note: "dependency-aware playbook",
                },
                record,
            ));
        }
    }
    out
}

fn playbook_skill_id(record: &MemoryRecord) -> Option<&str> {
    record
        .extra_frontmatter
        .get("skill_id")
        .and_then(serde_json::Value::as_str)
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

fn playbook_snapshot(records: &[(InclusionTrace, &MemoryRecord)]) -> SkillLintSnapshot {
    SkillLintSnapshot {
        skills: records
            .iter()
            .map(|(_, record)| SkillLintSkill {
                skill_id: playbook_skill_id(record)
                    .unwrap_or(record.id.as_str())
                    .to_owned(),
                lane: record
                    .extra_frontmatter
                    .get("lane")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(record.id.as_str())
                    .to_owned(),
                path: record.id.as_str().to_owned(),
                uses: None,
                resolver_triggers: vec![],
                files_to: Some("wiki/summaries/".to_owned()),
                gate_report_passed: true,
                rollback_version_count: 1,
                existing_paths: vec![],
                requires: playbook_string_list(record, "requires"),
                provides: playbook_string_list(record, "provides"),
                conflicts: playbook_string_list(record, "conflicts"),
            })
            .collect(),
    }
}

fn prerequisite_records<'a>(
    prerequisites: &[String],
    records: &'a [(InclusionTrace, &'a MemoryRecord)],
) -> Vec<(InclusionTrace, &'a MemoryRecord)> {
    prerequisites
        .iter()
        .filter_map(|skill_id| {
            records
                .iter()
                .find(|(_, record)| playbook_skill_id(record) == Some(skill_id.as_str()))
                .cloned()
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
    for (idx, (trace, record)) in ordered_records.into_iter().enumerate() {
        let block = render_record_block(record);
        let would_fit = max_body_bytes.is_none_or(|limit| {
            (body.len() as u64).saturating_add(block.len() as u64) <= limit
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
```

Use `render_record_block(record)` to measure each block. When `max_body_bytes` is `Some(limit)`, include prerequisites only while `body.len() + block.len() <= limit`, but always append the active playbook block.

- [ ] **Step 5: Pass remaining segment budget from the assembler**

In `assembler.rs`, replace the playbook branch in `inputs_run_step` with a budget-aware helper. Change the loop in `assemble_hot_with_inputs` to track bytes already emitted:

```rust
let used_bytes: u64 = bodies.iter().map(|body| body.len() as u64).sum();
let remaining = u64::from(resolved.max_bytes).saturating_sub(used_bytes);
let segment = inputs_run_step(step, inputs, Some(remaining));
```

Change `inputs_run_step` to accept `remaining_budget` and call:

```rust
HotRecipeStep::ActivePlaybook => {
    super::sources::playbook::select_with_budget(inputs, remaining_budget)
}
```

- [ ] **Step 6: Run playbook tests**

Run:

```bash
cargo test -p cairn-core playbook_includes_prerequisite_chain_before_active_playbook --locked
cargo test -p cairn-core playbook_omits_prerequisite_when_remaining_budget_is_too_small --locked
cargo test -p cairn-core assemble_hot --locked
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/assembler.rs crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs
git commit -m "feat(assemble_hot): include playbook prerequisites"
```

## Task 6: Expose Skill Graph Diagnostics Through Search Explain

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/search.json`
- Modify: generated files from `cairn-codegen`
- Modify: `crates/cairn-core/src/search/explain.rs`
- Modify: `crates/cairn-core/src/verbs/search.rs`
- Modify: `crates/cairn-cli/src/verbs/lint.rs`
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/tests/search_explain.rs`
- Modify: `crates/cairn-mcp/src/handler.rs`
- Modify: `crates/cairn-sdk/src/transport.rs`

- [ ] **Step 1: Write failing CLI search explain test**

Add this test to `crates/cairn-cli/tests/search_explain.rs`:

```rust
#[tokio::test]
async fn search_explain_includes_skill_graph_closure() {
    use cairn_core::contract::memory_store::MemoryStore as _;
    use cairn_core::domain::{RecordId, TargetId};
    use cairn_core::domain::record::tests_export::sample_record;
    use cairn_core::domain::taxonomy::MemoryKind;

    let vault = build_hybrid_test_vault(&[]).await;

    std::fs::create_dir_all(vault.root.join("skills")).expect("skills");
    std::fs::write(
        vault.root.join("skills/skill_test.md"),
        "---\nskill_id: run-tests\nlane: test.run\ntriggers: [\"run tests\"]\nfiles_to: wiki/summaries/\nprovides: [\"cap.test\"]\n---\nRun tests.\n",
    )
    .expect("prereq skill");
    std::fs::write(
        vault.root.join("skills/skill_ship.md"),
        "---\nskill_id: ship-pr\nlane: ship.pr\ntriggers: [\"ship pr\"]\nfiles_to: wiki/summaries/\nrequires: [\"cap.test\"]\nprovides: [\"cap.ship\"]\n---\nShip PR.\n",
    )
    .expect("leaf skill");

    let mut playbook = sample_record();
    playbook.id = RecordId::parse("01HQZX9F5N0000000000000001").expect("id");
    playbook.target_id = TargetId::parse("01HQZX9F5N0000000000000001").expect("target");
    playbook.kind = MemoryKind::Playbook;
    playbook.body = "ship pr playbook\nuse this when shipping a pull request".to_owned();
    playbook
        .extra_frontmatter
        .insert("skill_id".to_owned(), serde_json::json!("ship-pr"));
    playbook
        .extra_frontmatter
        .insert("lane".to_owned(), serde_json::json!("ship.pr"));
    vault.store.upsert(&playbook).await.expect("upsert playbook");
    let root = vault.root.clone();
    let dir = vault.dir;
    drop(vault.store);
    drop(vault.embedder);

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&root)
        .arg("search")
        .arg("--mode")
        .arg("keyword")
        .arg("--explain")
        .arg("--json")
        .arg("ship pr");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("\"skill_graph\""))
        .stdout(predicate::str::contains("run-tests"));
    drop(dir);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli search_explain_includes_skill_graph_closure --locked
```

Expected: FAIL because `ScoreExplain` has no `skill_graph` field and CLI search does not pass a skill snapshot.

- [ ] **Step 3: Extend the search schema**

In `crates/cairn-idl/schema/verbs/search.json`, add `skill_graph` to `ScoreExplain.properties`:

```json
"skill_graph": { "$ref": "#/$defs/SkillGraphExplain" }
```

Add a new definition:

```json
"SkillGraphExplain": {
  "type": "object",
  "additionalProperties": false,
  "required": ["skill_id", "prerequisites", "diagnostics"],
  "properties": {
    "skill_id": { "type": "string", "minLength": 1 },
    "prerequisites": {
      "type": "array",
      "items": { "type": "string", "minLength": 1 }
    },
    "diagnostics": {
      "type": "array",
      "items": { "type": "string", "minLength": 1 }
    }
  }
}
```

- [ ] **Step 4: Regenerate IDL output**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: FAIL with generated drift for the new optional field.

Regenerate by running the same codegen binary without `--check`:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: generated search structs include `SkillGraphExplain` and `ScoreExplain.skill_graph`.

- [ ] **Step 5: Extend core search explain structs**

In `crates/cairn-core/src/search/explain.rs`, add:

```rust
use crate::pipeline::skillify::SkillGraphExplain;
```

Add to core `ScoreExplain`:

```rust
    /// Optional skill dependency diagnostics for playbook or skill-shaped hits.
    pub skill_graph: Option<SkillGraphExplain>,
```

Set `skill_graph: None` in all existing `ScoreExplain` constructors.

- [ ] **Step 6: Pass optional graph snapshots through search requests**

In `crates/cairn-core/src/verbs/search.rs`, add to `SearchRequest`:

```rust
    /// Optional skill graph metadata supplied by adapters that can read skill files.
    pub skill_graph_snapshot: Option<crate::pipeline::skillify::SkillLintSnapshot>,
```

Set `skill_graph_snapshot: None` in MCP and SDK request construction. In CLI search, call `build_skill_lint_snapshot(&vault_root).ok()` when `explain` is true and pass that value.

Make `build_skill_lint_snapshot` in `crates/cairn-cli/src/verbs/lint.rs` visible inside the crate:

```rust
pub(crate) fn build_skill_lint_snapshot(
    vault_root: &Path,
) -> anyhow::Result<cairn_core::pipeline::skillify::SkillLintSnapshot> {
```

- [ ] **Step 7: Attach graph explain after candidate trimming**

In `crates/cairn-core/src/verbs/search.rs`, after token-budget trimming and before returning `SearchOutcome`, enrich explain entries:

```rust
let explain = attach_skill_graph_explain(
    explain,
    &candidates,
    request.skill_graph_snapshot.as_ref(),
);
```

Add:

```rust
fn attach_skill_graph_explain(
    explain: Option<Vec<ScoreExplain>>,
    candidates: &[SearchCandidate],
    snapshot: Option<&crate::pipeline::skillify::SkillLintSnapshot>,
) -> Option<Vec<ScoreExplain>> {
    let Some(mut explain) = explain else {
        return None;
    };
    let Some(snapshot) = snapshot else {
        return Some(explain);
    };
    let resolver = crate::pipeline::skillify::SkillGraphResolver::new(snapshot);
    for (entry, candidate) in explain.iter_mut().zip(candidates) {
        if let Some(skill_id) = candidate_skill_id(candidate) {
            let closure = resolver.resolve_prerequisites(&skill_id);
            entry.skill_graph = Some(crate::pipeline::skillify::SkillGraphExplain {
                skill_id,
                prerequisites: closure.prerequisites,
                diagnostics: closure.issues.into_iter().map(|issue| issue.message).collect(),
            });
        }
    }
    Some(explain)
}

fn candidate_skill_id(candidate: &SearchCandidate) -> Option<String> {
    let record = serde_json::from_str::<MemoryRecord>(&candidate.record_json).ok()?;
    record
        .extra_frontmatter
        .get("skill_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            record
                .extra_frontmatter
                .get("lane")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
}
```

- [ ] **Step 8: Convert core graph explain to generated graph explain**

In `crates/cairn-cli/src/verbs/search.rs`, when mapping `ScoreExplain`, add:

```rust
skill_graph: e.skill_graph.as_ref().map(|graph| {
    cairn_core::generated::verbs::search::SkillGraphExplain {
        skill_id: graph.skill_id.clone(),
        prerequisites: graph.prerequisites.clone(),
        diagnostics: graph.diagnostics.clone(),
    }
}),
```

Repeat the same generated conversion in SDK/MCP envelope conversion paths if they construct generated `ScoreExplain` directly.

- [ ] **Step 9: Run search explain tests**

Run:

```bash
cargo test -p cairn-cli search_explain_includes_skill_graph_closure --locked
cargo test -p cairn-cli search_explain --locked
cargo test -p cairn-mcp search_tool --locked
cargo test -p cairn-sdk search_dispatch --locked
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-idl/schema/verbs/search.json crates/cairn-core/src/generated crates/cairn-cli/src/generated crates/cairn-mcp/src/generated crates/cairn-sdk/src/generated crates/cairn-core/src/search/explain.rs crates/cairn-core/src/verbs/search.rs crates/cairn-cli/src/verbs/lint.rs crates/cairn-cli/src/verbs/search.rs crates/cairn-cli/tests/search_explain.rs crates/cairn-mcp/src/handler.rs crates/cairn-sdk/src/transport.rs
git commit -m "feat(search): expose skill graph explain"
```

## Task 7: Final Verification

**Files:**
- No new source files beyond previous tasks.

- [ ] **Step 1: Run focused tests**

Run:

```bash
cargo test -p cairn-core skill_graph --locked
cargo test -p cairn-core skill_lint --locked
cargo test -p cairn-core assemble_hot --locked
cargo test -p cairn-cli lint_skill --locked
cargo test -p cairn-cli search_explain --locked
cargo test -p cairn-workflows skillify_gate_runners --locked
```

Expected: all listed test commands PASS.

- [ ] **Step 2: Run formatting and codegen checks**

Run:

```bash
cargo fmt --all --check
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
./scripts/check-core-boundary.sh
```

Expected: all commands PASS.

- [ ] **Step 3: Run workspace compile check**

Run:

```bash
cargo check --workspace --all-targets --locked
```

Expected: PASS.

- [ ] **Step 4: Inspect final diff**

Run:

```bash
git status --short
git diff --stat origin/main...HEAD
```

Expected: only issue #129 files and generated schema outputs are changed.

- [ ] **Step 5: Commit verification-only fixes**

If formatting or generated output changed after the previous commits, commit those exact changes:

```bash
git add .
git commit -m "chore: finalize skill graph retrieval"
```

Expected: no commit is created when the working tree is already clean.

## Self-Review

- Spec coverage: core resolver, dependencies/conflicts, hot-memory prerequisites, lint broken references, and search explain diagnostics are each assigned to a task.
- Placeholder scan: no deferred implementation markers remain in this plan.
- Type consistency: `SkillLintSkill.requires/provides/conflicts`, `SkillGraphResolver`, `SkillGraphExplain`, `SearchRequest.skill_graph_snapshot`, and `ScoreExplain.skill_graph` are named consistently across tasks.
- Scope control: persistent graph storage and external marketplace behavior remain outside this plan.
