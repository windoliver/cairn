//! Minimal vault snapshot builder for Skillify gate collision checks.
//!
//! Walks `.cairn/evolution/skillify/*/bundle/skills/skill_*.md` (materialized
//! candidates) and `skills/*.md` (promoted live skills), reading minimal YAML
//! frontmatter to build [`SkillLintSnapshot`] entries for collision detection.
//!
//! Not a replacement for `cairn-cli`'s full lint snapshot — this is a
//! workflow-local view scoped to what gate runners need: lane, triggers,
//! `files_to`, and the `uses` path. The cairn-cli snapshot includes
//! additional validation (gate-report freshness, rollback metadata) that
//! isn't required at gate-run time.

use std::path::Path;

use cairn_core::pipeline::skillify::{
    SkillArtifactKind, SkillLintSkill, SkillLintSnapshot, SkillifyGateReport, SkillifyGateStatus,
};

/// Build a [`SkillLintSnapshot`] from the vault filesystem.
///
/// If `exclude_candidate_id` is `Some`, that candidate's entry is omitted from
/// the snapshot — useful when a candidate is being gated against the rest of
/// the vault (the candidate must not collide with itself).
///
/// # Errors
/// Returns an [`std::io::Error`] when a required directory cannot be read.
/// Individual unparseable skill files are skipped (best-effort snapshot).
pub fn build_vault_snapshot(
    vault_root: &Path,
    exclude_candidate_id: Option<&str>,
) -> std::io::Result<SkillLintSnapshot> {
    let mut skills = Vec::new();

    // Promoted live skills in `skills/`.
    let live_dir = vault_root.join("skills");
    if live_dir.is_dir() {
        for entry in std::fs::read_dir(&live_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                continue;
            }
            // Round 5 hardening: per-skill failures (unreadable file,
            // missing frontmatter, missing required field) propagate as
            // errors instead of being silently dropped. A malformed live
            // skill could otherwise vanish from collision checks and let a
            // duplicate-lane candidate promote.
            skills.push(read_live_skill(vault_root, &path)?);
        }
    }

    // Materialized candidates.
    let candidates_root = vault_root.join(".cairn/evolution/skillify");
    if candidates_root.is_dir() {
        for entry in std::fs::read_dir(&candidates_root)? {
            let entry = entry?;
            let candidate_dir = entry.path();
            if !candidate_dir.is_dir() {
                continue;
            }
            let Some(candidate_id) = candidate_dir.file_name().and_then(std::ffi::OsStr::to_str)
            else {
                continue;
            };
            if exclude_candidate_id == Some(candidate_id) {
                continue;
            }
            // Skip hidden directories (e.g. the `.unpack-tmp-*` scratch
            // directory created by `unpack_archive`, the install `.lock`
            // file's surrounding entries).
            if candidate_id.starts_with('.') {
                continue;
            }
            // Round 20 hardening (supersedes Round 19): a candidate
            // participates in collision detection when it is either
            // in-flight (no gate report yet, or the materialize-time
            // stub marker) OR promotion-ready. Candidates whose gates
            // actually ran and failed are excluded — those will
            // rollback and must not block siblings.
            //
            // Including in-flight candidates closes a concurrent-run
            // race: two pipeline invocations for the same lane would
            // each materialize then build a snapshot; with only
            // promotion-ready siblings in scope, neither would see the
            // other and both would pass collision gates. With in-flight
            // included, the snapshots cross-detect and at least one
            // (typically both, conservatively) fails collision.
            if !candidate_in_collision_scope(&candidate_dir) {
                continue;
            }
            let skills_dir = candidate_dir.join("bundle/skills");
            if !skills_dir.is_dir() {
                continue;
            }
            for skill_entry in std::fs::read_dir(&skills_dir)? {
                let skill_entry = skill_entry?;
                let path = skill_entry.path();
                if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                    continue;
                }
                skills.push(read_candidate_skill(vault_root, candidate_id, &path)?);
            }
        }
    }

    Ok(SkillLintSnapshot { skills })
}

/// Returns true when a candidate should participate in collision
/// detection. Includes promotion-ready candidates, in-flight candidates
/// (no gate report yet, or the materialize-time stub marker), and
/// candidates with malformed gate reports (fail-safe: prefer
/// over-reporting collisions to under-reporting). Excludes only
/// candidates whose gates actually ran and at least one failed.
fn candidate_in_collision_scope(candidate_dir: &Path) -> bool {
    let report_path = candidate_dir.join("gate-report.json");
    let Ok(bytes) = std::fs::read(&report_path) else {
        // No gate report yet — in-flight materialization. Include so
        // a concurrent same-lane sibling's snapshot sees us.
        return true;
    };
    let Ok(report) = serde_json::from_slice::<SkillifyGateReport>(&bytes) else {
        // Malformed report — include for safety (false-positive
        // collisions are recoverable; false negatives let two same-
        // lane candidates both pass and corrupt the vault).
        return true;
    };
    if report.ready_for_promotion() {
        return true;
    }
    // Distinguish the materialize-time stub (all required gates
    // Blocked, no messages, no extra gates) from an actually-failed
    // run. The stub means gates have not yet executed, so the
    // candidate is still in-flight.
    is_materialize_stub_marker(&report)
}

/// True when the report matches the shape `materialize_bundle` writes
/// at candidate creation: every required gate is present, each is
/// `Blocked` with no message, and there are no extra gates.
fn is_materialize_stub_marker(report: &SkillifyGateReport) -> bool {
    let required = SkillArtifactKind::required();
    if report.gates.len() != required.len() {
        return false;
    }
    let required_names: std::collections::HashSet<&str> =
        required.iter().map(|k| k.as_str()).collect();
    let report_names: std::collections::HashSet<&str> =
        report.gates.iter().map(|g| g.name.as_str()).collect();
    if required_names != report_names {
        return false;
    }
    report
        .gates
        .iter()
        .all(|g| matches!(g.status, SkillifyGateStatus::Blocked) && g.message.is_none())
}

fn read_live_skill(vault_root: &Path, path: &Path) -> std::io::Result<SkillLintSkill> {
    let body = std::fs::read_to_string(path)?;
    let fm = frontmatter(&body).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("skill `{}` has no YAML frontmatter", path.display()),
        )
    })?;
    let skill_id = scalar(fm, "name").unwrap_or_else(|| {
        path.file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("unknown")
            .to_owned()
    });
    let lane = scalar(fm, "lane").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("skill `{}` missing required `lane` field", path.display()),
        )
    })?;
    let uses = scalar(fm, "uses");
    let files_to = scalar(fm, "files_to");
    let resolver_triggers = inline_or_list(fm, "triggers");
    if resolver_triggers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "skill `{}` missing or empty `triggers` field",
                path.display()
            ),
        ));
    }
    let rel_path = rel(vault_root, path);
    let existing_paths = vec![rel_path.clone()];

    Ok(SkillLintSkill {
        skill_id,
        lane,
        path: rel_path,
        uses,
        resolver_triggers,
        files_to,
        // Live skills are assumed to have passed gates at promotion time;
        // we don't re-check here. The pipeline only needs this snapshot for
        // collision detection.
        gate_report_passed: true,
        rollback_version_count: 1,
        existing_paths,
    })
}

fn read_candidate_skill(
    vault_root: &Path,
    candidate_id: &str,
    path: &Path,
) -> std::io::Result<SkillLintSkill> {
    let body = std::fs::read_to_string(path)?;
    let fm = frontmatter(&body).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "candidate `{candidate_id}` skill `{}` has no YAML frontmatter",
                path.display()
            ),
        )
    })?;
    let skill_id = candidate_id.to_owned();
    let lane = scalar(fm, "lane").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "candidate `{candidate_id}` skill `{}` missing required `lane` field",
                path.display()
            ),
        )
    })?;
    let uses = scalar(fm, "uses");
    let files_to = scalar(fm, "files_to");
    let resolver_triggers = inline_or_list(fm, "triggers");
    if resolver_triggers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "candidate `{candidate_id}` skill `{}` missing or empty `triggers` field",
                path.display()
            ),
        ));
    }
    let rel_path = rel(vault_root, path);
    let existing_paths = vec![rel_path.clone()];

    Ok(SkillLintSkill {
        skill_id,
        lane,
        path: rel_path,
        uses,
        resolver_triggers,
        files_to,
        gate_report_passed: true,
        rollback_version_count: 1,
        existing_paths,
    })
}

fn frontmatter(body: &str) -> Option<&str> {
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn scalar(fm: &str, key: &str) -> Option<String> {
    for line in fm.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() || value.starts_with('[') {
                return None;
            }
            return Some(value.to_owned());
        }
    }
    None
}

/// Parse a YAML value that may be inline (`triggers: ["a", "b"]`) or a
/// block list:
/// ```text
/// triggers:
///   - a
///   - b
/// ```
fn inline_or_list(fm: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let inline = rest.trim();
            if !inline.is_empty() {
                if let Some(arr) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    for item in arr.split(',') {
                        let v = item.trim().trim_matches('"').trim_matches('\'');
                        if !v.is_empty() {
                            out.push(v.to_owned());
                        }
                    }
                    return out;
                }
                // Inline scalar — single trigger.
                let v = inline.trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    out.push(v.to_owned());
                }
                return out;
            }
            // Block-list form: read subsequent lines that start with `-`.
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j];
                if let Some(item) = next.trim_start().strip_prefix("- ") {
                    let v = item.trim().trim_matches('"').trim_matches('\'');
                    if !v.is_empty() {
                        out.push(v.to_owned());
                    }
                    j += 1;
                } else if next.trim().is_empty() {
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

fn rel(vault_root: &Path, path: &Path) -> String {
    path.strip_prefix(vault_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::pipeline::skillify::{
        SkillArtifactKind, SkillifyGate, SkillifyGateStatus,
    };
    use tempfile::TempDir;

    fn write_md(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn write_gate_report(vault_root: &Path, candidate_id: &str, all_passed: bool) {
        let report = SkillifyGateReport {
            candidate_id: candidate_id.to_owned(),
            gates: SkillArtifactKind::required()
                .iter()
                .map(|kind| SkillifyGate {
                    name: kind.as_str().to_owned(),
                    status: if all_passed {
                        SkillifyGateStatus::Passed
                    } else {
                        SkillifyGateStatus::Blocked
                    },
                    message: None,
                })
                .collect(),
        };
        let path = vault_root
            .join(".cairn/evolution/skillify")
            .join(candidate_id)
            .join("gate-report.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }

    #[test]
    fn empty_vault_returns_empty_snapshot() {
        let temp = TempDir::new().unwrap();
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert!(snap.skills.is_empty());
    }

    #[test]
    fn malformed_live_skill_fails_closed() {
        let temp = TempDir::new().unwrap();
        write_md(&temp.path().join("skills/bad.md"), "no frontmatter here");
        let err = build_vault_snapshot(temp.path(), None).unwrap_err();
        assert!(
            err.to_string().contains("frontmatter"),
            "expected frontmatter error, got: {err}"
        );
    }

    #[test]
    fn live_skill_missing_lane_fails_closed() {
        let temp = TempDir::new().unwrap();
        write_md(
            &temp.path().join("skills/no-lane.md"),
            "---\nname: no-lane\ntriggers:\n  - x\nuses: scripts/x.sh\nfiles_to: wiki/x/\n---\nBody.",
        );
        let err = build_vault_snapshot(temp.path(), None).unwrap_err();
        assert!(
            err.to_string().contains("lane"),
            "expected lane-missing error, got: {err}"
        );
    }

    #[test]
    fn live_skill_is_picked_up() {
        let temp = TempDir::new().unwrap();
        write_md(
            &temp.path().join("skills/foo.md"),
            "---\nname: foo\nlane: ops.foo\ntriggers:\n  - run foo\nuses: scripts/foo.sh\nfiles_to: wiki/foo/\n---\nBody.",
        );
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert_eq!(snap.skills.len(), 1);
        assert_eq!(snap.skills[0].lane, "ops.foo");
        assert_eq!(snap.skills[0].resolver_triggers, vec!["run foo".to_owned()]);
    }

    #[test]
    fn candidate_skill_is_picked_up() {
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_a/bundle/skills/skill_a.md"),
            "---\nname: a\nlane: test.a\ntriggers: [\"trig a\"]\nuses: scripts/a.sh\nfiles_to: wiki/a/\n---\nBody.",
        );
        write_gate_report(temp.path(), "skc_a", true);
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert_eq!(snap.skills.len(), 1);
        assert_eq!(snap.skills[0].lane, "test.a");
        assert_eq!(snap.skills[0].resolver_triggers, vec!["trig a".to_owned()]);
    }

    #[test]
    fn exclude_candidate_omits_self() {
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_self/bundle/skills/skill_self.md"),
            "---\nname: self\nlane: test.self\ntriggers: [\"x\"]\nuses: scripts/s.sh\nfiles_to: wiki/s/\n---\nBody.",
        );
        write_gate_report(temp.path(), "skc_self", true);
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_other/bundle/skills/skill_other.md"),
            "---\nname: other\nlane: test.other\ntriggers: [\"y\"]\nuses: scripts/o.sh\nfiles_to: wiki/o/\n---\nBody.",
        );
        write_gate_report(temp.path(), "skc_other", true);
        let snap = build_vault_snapshot(temp.path(), Some("skc_self")).unwrap();
        assert_eq!(snap.skills.len(), 1);
        assert_eq!(snap.skills[0].lane, "test.other");
    }

    #[test]
    fn candidate_without_gate_report_is_included_as_in_flight() {
        // Round-20 fix: a candidate missing its gate report is treated
        // as in-flight (materializing but not yet gated) and MUST
        // appear in the snapshot so concurrent same-lane siblings
        // detect the collision before both pass gates.
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_ungated/bundle/skills/skill_ungated.md"),
            "---\nname: ungated\nlane: test.u\ntriggers: [\"u\"]\nuses: scripts/u.sh\nfiles_to: wiki/u/\n---\nBody.",
        );
        // Intentionally no gate-report.json.
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert_eq!(snap.skills.len(), 1, "in-flight candidate must be visible");
        assert_eq!(snap.skills[0].skill_id, "skc_ungated");
    }

    #[test]
    fn candidate_with_malformed_gate_report_is_included_for_safety() {
        // Fail-safe: a malformed gate report is treated as in-flight
        // rather than silently dropped — false-positive collisions are
        // recoverable, false-negative collisions corrupt the vault.
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_bad/bundle/skills/skill_bad.md"),
            "---\nname: bad\nlane: test.b\ntriggers: [\"b\"]\nuses: scripts/b.sh\nfiles_to: wiki/b/\n---\nBody.",
        );
        let report_path = temp
            .path()
            .join(".cairn/evolution/skillify/skc_bad/gate-report.json");
        std::fs::write(&report_path, b"not valid json").unwrap();
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert_eq!(snap.skills.len(), 1);
        assert_eq!(snap.skills[0].skill_id, "skc_bad");
    }

    #[test]
    fn candidate_with_materialize_stub_marker_is_included() {
        // Round-20 race regression: the materialize-time stub marker
        // (all-Blocked, no messages) means gates have not yet run.
        // The candidate must remain in the snapshot so concurrent
        // same-lane siblings cross-detect the collision.
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_stub/bundle/skills/skill_stub.md"),
            "---\nname: stub\nlane: test.s\ntriggers: [\"s\"]\nuses: scripts/s.sh\nfiles_to: wiki/s/\n---\nBody.",
        );
        // materialize_bundle writes this exact shape: every required
        // gate present, all Blocked, no messages.
        write_gate_report(temp.path(), "skc_stub", false);
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert_eq!(snap.skills.len(), 1, "in-flight stub marker must be visible");
    }

    #[test]
    fn concurrent_same_lane_candidates_cross_detect() {
        // Round-20 hardening: two pipeline invocations for the same
        // lane each materialize a candidate. Before either runs gates,
        // each builds a snapshot excluding itself. The snapshot MUST
        // surface the sibling so collision gates can fail at least one.
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_race_a/bundle/skills/skill_race_a.md"),
            "---\nname: race-a\nlane: test.shared\ntriggers: [\"go\"]\nuses: scripts/a.sh\nfiles_to: wiki/g/\n---\nBody.",
        );
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_race_b/bundle/skills/skill_race_b.md"),
            "---\nname: race-b\nlane: test.shared\ntriggers: [\"go\"]\nuses: scripts/b.sh\nfiles_to: wiki/g/\n---\nBody.",
        );
        // Both candidates carry only the materialize-time stub marker.
        write_gate_report(temp.path(), "skc_race_a", false);
        write_gate_report(temp.path(), "skc_race_b", false);

        // From A's perspective: B must be visible.
        let snap_a = build_vault_snapshot(temp.path(), Some("skc_race_a")).unwrap();
        assert_eq!(snap_a.skills.len(), 1);
        assert_eq!(snap_a.skills[0].skill_id, "skc_race_b");

        // And vice-versa.
        let snap_b = build_vault_snapshot(temp.path(), Some("skc_race_b")).unwrap();
        assert_eq!(snap_b.skills.len(), 1);
        assert_eq!(snap_b.skills[0].skill_id, "skc_race_a");
    }

    #[test]
    fn gated_failed_candidate_is_excluded_from_snapshot() {
        // Round-19 preserved: a candidate whose gates RAN and at
        // least one returned a real failure (with a message) is
        // excluded so the doomed candidate does not block its
        // promotion-ready sibling.
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/skc_dead/bundle/skills/skill_dead.md"),
            "---\nname: dead\nlane: test.d\ntriggers: [\"d\"]\nuses: scripts/d.sh\nfiles_to: wiki/d/\n---\nBody.",
        );
        // Construct a report that looks like real gate output (has a
        // message), not the all-Blocked stub marker.
        let report = SkillifyGateReport {
            candidate_id: "skc_dead".to_owned(),
            gates: SkillArtifactKind::required()
                .iter()
                .map(|kind| SkillifyGate {
                    name: kind.as_str().to_owned(),
                    status: SkillifyGateStatus::Failed,
                    message: Some("real failure".to_owned()),
                })
                .collect(),
        };
        let path = temp
            .path()
            .join(".cairn/evolution/skillify/skc_dead/gate-report.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert!(snap.skills.is_empty(), "gated-failed candidate must be excluded");
    }

    #[test]
    fn unpack_temp_dirs_are_ignored() {
        let temp = TempDir::new().unwrap();
        write_md(
            &temp
                .path()
                .join(".cairn/evolution/skillify/.unpack-tmp-123/bundle/skills/skill_bad.md"),
            "---\nname: bad\nlane: should.not.appear\ntriggers: [\"x\"]\nuses: scripts/b.sh\nfiles_to: wiki/b/\n---\nBody.",
        );
        let snap = build_vault_snapshot(temp.path(), None).unwrap();
        assert!(snap.skills.is_empty());
    }
}
