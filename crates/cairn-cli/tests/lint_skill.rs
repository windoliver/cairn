#![allow(missing_docs)]

use assert_cmd::Command;
use cairn_test_fixtures::build_hybrid_test_vault;
use predicates::prelude::*;

fn passed_gate_report(candidate_id: &str) -> String {
    format!(
        r#"{{"candidate_id":"{candidate_id}","gates":[{{"name":"skill_contract","status":"passed","message":null}},{{"name":"deterministic_script","status":"passed","message":null}},{{"name":"unit_tests","status":"passed","message":null}},{{"name":"integration_tests","status":"passed","message":null}},{{"name":"llm_evals","status":"passed","message":null}},{{"name":"resolver_trigger","status":"passed","message":null}},{{"name":"resolver_eval","status":"passed","message":null}},{{"name":"check_resolvable_and_dry","status":"passed","message":null}},{{"name":"e2e_smoke","status":"passed","message":null}},{{"name":"filing_rules","status":"passed","message":null}}]}}"#
    )
}

#[test]
fn lint_skill_requires_existing_vault() {
    let vault = tempfile::tempdir().expect("vault");
    std::fs::create_dir_all(vault.path().join("skills")).expect("skills");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(vault.path())
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("cairn.db is missing"));
}

#[tokio::test]
async fn lint_skill_reports_missing_script() {
    let vault = build_hybrid_test_vault(&[]).await;

    std::fs::create_dir_all(vault.root.join("skills")).expect("skills");
    std::fs::create_dir_all(vault.root.join(".cairn/resolver/skills")).expect("resolver");
    std::fs::create_dir_all(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_fixture/versions/v1"),
    )
    .expect("versions");
    std::fs::write(
        vault.root.join("skills/skill_deploy-hotfix.md"),
        "---\nskill_id: deploy-hotfix\nversion: 1\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: skills/scripts/missing.sh\nfiles_to: wiki/summaries/\ncandidate_id: skc_fixture\nstatus: live\n---\nRun the skill.\n",
    )
    .expect("skill");
    std::fs::write(
        vault.root.join(".cairn/resolver/skills/deploy-hotfix.json"),
        r#"{"skill_id":"deploy-hotfix","triggers":["deploy hotfix"]}"#,
    )
    .expect("resolver");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_fixture/gate-report.json"),
        r#"{"candidate_id":"skc_fixture","gates":[{"name":"skill_contract","status":"passed","message":null},{"name":"deterministic_script","status":"passed","message":null},{"name":"unit_tests","status":"passed","message":null},{"name":"integration_tests","status":"passed","message":null},{"name":"llm_evals","status":"passed","message":null},{"name":"resolver_trigger","status":"passed","message":null},{"name":"resolver_eval","status":"passed","message":null},{"name":"check_resolvable_and_dry","status":"passed","message":null},{"name":"e2e_smoke","status":"passed","message":null},{"name":"filing_rules","status":"passed","message":null}]}"#,
    )
    .expect("gate");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_fixture/versions/v1/manifest.json"),
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
        .stdout(predicate::str::contains("skill_missing_artifact"));
}

#[tokio::test]
async fn lint_skill_scans_candidate_bundles() {
    let vault = build_hybrid_test_vault(&[]).await;
    let candidate_root = vault.root.join(".cairn/evolution/skillify/skc_candidate");

    std::fs::create_dir_all(candidate_root.join("bundle/skills")).expect("skills");
    std::fs::create_dir_all(candidate_root.join("bundle/resolver")).expect("resolver");
    std::fs::create_dir_all(candidate_root.join("versions/v1")).expect("versions");
    std::fs::write(
        candidate_root.join("bundle/skills/skill_deploy-hotfix.md"),
        "---\nskill_id: deploy-hotfix\nversion: 1\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: scripts/missing.sh\nfiles_to: wiki/summaries/\n---\nRun the skill.\n",
    )
    .expect("skill");
    std::fs::write(
        candidate_root.join("bundle/resolver/triggers.json"),
        r#"["deploy hotfix"]"#,
    )
    .expect("resolver");
    std::fs::write(
        candidate_root.join("gate-report.json"),
        r#"{"candidate_id":"skc_candidate","gates":[{"name":"skill_contract","status":"passed","message":null},{"name":"deterministic_script","status":"passed","message":null},{"name":"unit_tests","status":"passed","message":null},{"name":"integration_tests","status":"passed","message":null},{"name":"llm_evals","status":"passed","message":null},{"name":"resolver_trigger","status":"passed","message":null},{"name":"resolver_eval","status":"passed","message":null},{"name":"check_resolvable_and_dry","status":"passed","message":null},{"name":"e2e_smoke","status":"passed","message":null},{"name":"filing_rules","status":"passed","message":null}]}"#,
    )
    .expect("gate");
    std::fs::write(candidate_root.join("versions/v1/manifest.json"), "{}").expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&vault.root)
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("skill_missing_artifact"))
        .stdout(predicate::str::contains(
            ".cairn/evolution/skillify/skc_candidate/bundle/skills/skill_deploy-hotfix.md",
        ));
}

#[cfg(unix)]
#[tokio::test]
async fn lint_skill_rejects_script_symlink_escape() {
    let vault = build_hybrid_test_vault(&[]).await;
    let outside = tempfile::tempdir().expect("outside");
    let outside_script = outside.path().join("escape.sh");

    std::fs::create_dir_all(vault.root.join("skills/scripts")).expect("scripts");
    std::fs::create_dir_all(vault.root.join(".cairn/resolver/skills")).expect("resolver");
    std::fs::create_dir_all(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_symlink/versions/v1"),
    )
    .expect("versions");
    std::fs::write(&outside_script, "#!/usr/bin/env bash\nexit 0\n").expect("outside script");
    std::os::unix::fs::symlink(&outside_script, vault.root.join("skills/scripts/escape.sh"))
        .expect("symlink");
    std::fs::write(
        vault.root.join("skills/skill_symlink.md"),
        "---\nskill_id: symlink-skill\nversion: 1\nlane: deploy.symlink\ntriggers: [\"deploy symlink\"]\nuses: skills/scripts/escape.sh\nfiles_to: wiki/summaries/\ncandidate_id: skc_symlink\nstatus: live\n---\nRun the skill.\n",
    )
    .expect("skill");
    std::fs::write(
        vault.root.join(".cairn/resolver/skills/symlink-skill.json"),
        r#"{"skill_id":"symlink-skill","triggers":["deploy symlink"]}"#,
    )
    .expect("resolver");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_symlink/gate-report.json"),
        r#"{"candidate_id":"skc_symlink","gates":[{"name":"skill_contract","status":"passed","message":null},{"name":"deterministic_script","status":"passed","message":null},{"name":"unit_tests","status":"passed","message":null},{"name":"integration_tests","status":"passed","message":null},{"name":"llm_evals","status":"passed","message":null},{"name":"resolver_trigger","status":"passed","message":null},{"name":"resolver_eval","status":"passed","message":null},{"name":"check_resolvable_and_dry","status":"passed","message":null},{"name":"e2e_smoke","status":"passed","message":null},{"name":"filing_rules","status":"passed","message":null}]}"#,
    )
    .expect("gate");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_symlink/versions/v1/manifest.json"),
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
        .stdout(predicate::str::contains("skill_missing_artifact"));
}

#[tokio::test]
async fn lint_skill_reports_candidate_without_skill_markdown() {
    let vault = build_hybrid_test_vault(&[]).await;
    let candidate_root = vault.root.join(".cairn/evolution/skillify/skc_no_skill");

    std::fs::create_dir_all(candidate_root.join("bundle/scripts")).expect("scripts");
    std::fs::create_dir_all(candidate_root.join("versions/v1")).expect("versions");
    std::fs::write(
        candidate_root.join("gate-report.json"),
        passed_gate_report("skc_no_skill"),
    )
    .expect("gate");
    std::fs::write(candidate_root.join("versions/v1/manifest.json"), "{}").expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&vault.root)
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert().failure().stdout(
        predicate::str::contains("skill_missing_artifact")
            .and(predicate::str::contains("skc_no_skill")),
    );
}

#[tokio::test]
async fn lint_skill_reports_candidate_with_malformed_skill_frontmatter() {
    let vault = build_hybrid_test_vault(&[]).await;
    let candidate_root = vault
        .root
        .join(".cairn/evolution/skillify/skc_malformed_skill");

    std::fs::create_dir_all(candidate_root.join("bundle/skills")).expect("skills");
    std::fs::create_dir_all(candidate_root.join("bundle/resolver")).expect("resolver");
    std::fs::create_dir_all(candidate_root.join("versions/v1")).expect("versions");
    std::fs::write(
        candidate_root.join("bundle/skills/skill_deploy-hotfix.md"),
        "lane: deploy.hotfix\nuses: scripts/deploy-hotfix.sh\n",
    )
    .expect("skill");
    std::fs::write(
        candidate_root.join("bundle/resolver/triggers.json"),
        r#"["deploy hotfix"]"#,
    )
    .expect("resolver");
    std::fs::write(
        candidate_root.join("gate-report.json"),
        passed_gate_report("skc_malformed_skill"),
    )
    .expect("gate");
    std::fs::write(candidate_root.join("versions/v1/manifest.json"), "{}").expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&vault.root)
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("skill_missing_artifact").and(
        predicate::str::contains(
            ".cairn/evolution/skillify/skc_malformed_skill/bundle/skills/skill_deploy-hotfix.md",
        ),
    ));
}

#[tokio::test]
async fn lint_skill_rejects_script_directory_reference() {
    let vault = build_hybrid_test_vault(&[]).await;

    std::fs::create_dir_all(vault.root.join("skills/scripts/deploy-hotfix.sh"))
        .expect("script directory");
    std::fs::create_dir_all(vault.root.join(".cairn/resolver/skills")).expect("resolver");
    std::fs::create_dir_all(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_directory/versions/v1"),
    )
    .expect("versions");
    std::fs::write(
        vault.root.join("skills/skill_deploy-hotfix.md"),
        "---\nskill_id: deploy-hotfix\nversion: 1\nlane: deploy.hotfix\ntriggers: [\"deploy hotfix\"]\nuses: skills/scripts/deploy-hotfix.sh\nfiles_to: wiki/summaries/\ncandidate_id: skc_directory\nstatus: live\n---\nRun the skill.\n",
    )
    .expect("skill");
    std::fs::write(
        vault.root.join(".cairn/resolver/skills/deploy-hotfix.json"),
        r#"{"skill_id":"deploy-hotfix","triggers":["deploy hotfix"]}"#,
    )
    .expect("resolver");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_directory/gate-report.json"),
        passed_gate_report("skc_directory"),
    )
    .expect("gate");
    std::fs::write(
        vault
            .root
            .join(".cairn/evolution/skillify/skc_directory/versions/v1/manifest.json"),
        "{}",
    )
    .expect("manifest");

    let mut cmd = Command::cargo_bin("cairn").expect("bin");
    cmd.arg("--vault")
        .arg(&vault.root)
        .arg("lint")
        .arg("--json")
        .arg("--skill");

    cmd.assert().failure().stdout(
        predicate::str::contains("skill_missing_artifact")
            .and(predicate::str::contains("skills/scripts/deploy-hotfix.sh")),
    );
}
