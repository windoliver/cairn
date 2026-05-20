#![allow(missing_docs)]

use assert_cmd::Command;
use cairn_test_fixtures::build_hybrid_test_vault;
use predicates::prelude::*;

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
