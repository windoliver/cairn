//! Consumer acceptance coverage for the Hermes Agent migration bridge.

use std::path::Path;
use std::process::{Command, Output};

use cairn_core::domain::MemoryKind;
use cairn_core::domain::flush_plan::{PersistedPlan, PlannedMutation};

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn run_in_vault(vault: &Path, args: &[&str]) -> Output {
    cli()
        .current_dir(vault)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"))
}

fn run_json_ok(vault: &Path, args: &[&str]) -> serde_json::Value {
    let out = run_in_vault(vault, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "cairn {args:?} failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("cairn {args:?} emitted invalid JSON: {e}"))
}

fn json_output(out: &Output, args: &[&str]) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("cairn {args:?} emitted invalid JSON: {e}"))
}

fn single_pending_plan(vault: &Path) -> (String, PersistedPlan) {
    let pending_dir = vault.join(".cairn/flush/pending");
    let entries: Vec<_> = std::fs::read_dir(&pending_dir)
        .unwrap_or_else(|e| panic!("read pending dir {}: {e}", pending_dir.display()))
        .map(|entry| entry.expect("pending dir entry").path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".plan.json"))
        })
        .collect();
    assert_eq!(entries.len(), 1, "expected one pending plan: {entries:?}");
    let path = &entries[0];
    let file_name = path.file_name().expect("file name").to_string_lossy();
    let operation_id = file_name
        .strip_suffix(".plan.json")
        .expect("plan suffix")
        .to_owned();
    let plan: PersistedPlan =
        serde_json::from_slice(&std::fs::read(path).expect("read pending plan"))
            .expect("pending plan json");
    (operation_id, plan)
}

fn record_id_containing(plan: &PersistedPlan, needle: &str) -> String {
    plan.plan
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            PlannedMutation::Upsert { record, .. } if record.body.contains(needle) => {
                Some(record.id.as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected imported record containing `{needle}`: {plan:?}"))
}

fn record_kind_containing(plan: &PersistedPlan, needle: &str) -> MemoryKind {
    plan.plan
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            PlannedMutation::Upsert { record, .. } if record.body.contains(needle) => {
                Some(record.kind)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected imported record containing `{needle}`: {plan:?}"))
}

fn hit_record_ids(vault: &Path, query: &str) -> Vec<String> {
    let search = run_json_ok(vault, &["search", "--mode", "keyword", query, "--json"]);
    search["data"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("search hits must be an array: {search}"))
        .iter()
        .map(|hit| hit["record_id"].as_str().expect("hit record_id").to_owned())
        .collect()
}

fn write_hermes_fixture(archive: &Path) {
    std::fs::create_dir_all(archive.join("memories")).expect("memories dir");
    std::fs::create_dir_all(archive.join("skills")).expect("skills dir");
    std::fs::write(
        archive.join("memories/MEMORY.md"),
        "§ hermes acceptance bridge remembers heliotrope telemetry marker\n\
         § hermes trajectory candidate: when imports are reviewed, search after apply",
    )
    .expect("write MEMORY.md");
    std::fs::write(
        archive.join("memories/USER.md"),
        "§ User prefers compact Hermes migration reports.",
    )
    .expect("write USER.md");
    std::fs::write(
        archive.join("SOUL.md"),
        "§ Always route imported memory through reviewable plans.",
    )
    .expect("write SOUL.md");
    std::fs::write(
        archive.join("skills/review-plan.md"),
        "§ Playbook: review imported Hermes records before applying them.\n\
         § Playbook: validate imported Hermes records after applying them.",
    )
    .expect("write skill");
}

#[test]
fn hermes_agent_import_plan_applies_to_search_retrieve_lint_and_forget() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    run_json_ok(vault.path(), &["identity", "init-defaults", "--json"]);
    run_json_ok(
        vault.path(),
        &["identity", "provision", "human", "hermes-import", "--json"],
    );
    run_json_ok(
        vault.path(),
        &[
            "identity",
            "provision",
            "agent",
            "cairn-cli:default:writer",
            "--json",
        ],
    );
    let archive = tempfile::tempdir().expect("hermes archive");
    write_hermes_fixture(archive.path());

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "hermes-agent",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "16",
            "--json",
        ],
    );
    assert_eq!(import["records"], 6, "import summary: {import}");
    assert_eq!(import["plans"], 1, "import summary: {import}");

    let (operation_id, pending) = single_pending_plan(vault.path());
    let record_id = record_id_containing(&pending, "heliotrope");
    assert_eq!(
        record_kind_containing(&pending, "trajectory candidate"),
        MemoryKind::Trace
    );

    run_json_ok(vault.path(), &["flush", "apply", &operation_id, "--json"]);

    let hits = hit_record_ids(vault.path(), "heliotrope");
    assert_eq!(hits, vec![record_id.clone()], "search should find import");

    let retrieve = run_json_ok(vault.path(), &["retrieve", &record_id, "--json"]);
    assert_eq!(
        retrieve["data"]["body"], "hermes acceptance bridge remembers heliotrope telemetry marker",
        "retrieve should expose imported Hermes body: {retrieve}"
    );

    let lint_out = run_in_vault(vault.path(), &["lint", "--json"]);
    let lint = json_output(&lint_out, &["lint", "--json"]);
    assert_eq!(lint["status"], "committed", "lint should complete: {lint}");
    assert_eq!(
        lint["data"]["summary"]["by_severity"]["error"], 0,
        "imported record should not produce lint errors: {lint}"
    );

    run_json_ok(vault.path(), &["forget", "--record", &record_id, "--json"]);
    assert!(
        hit_record_ids(vault.path(), "heliotrope").is_empty(),
        "forgotten Hermes import should leave keyword search"
    );
}

#[test]
fn hermes_agent_import_json_emits_manifest_sections_and_skill_items() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let archive = tempfile::tempdir().expect("hermes archive");
    write_hermes_fixture(archive.path());

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "hermes-agent",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "16",
            "--json",
        ],
    );

    assert_eq!(import["records"], 6, "import summary: {import}");
    assert_eq!(
        import["manifest"]["system"], "hermes-agent",
        "manifest: {import}"
    );
    assert!(
        import["manifest"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "skill"
                && item["legacy_id"] == "review-plan"
                && item["skill_ids"] == serde_json::json!(["review-plan"])),
        "skill manifest item should be emitted: {import}"
    );
    assert_eq!(
        import["manifest"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter(|item| item["kind"] == "skill" && item["legacy_id"] == "review-plan")
            .count(),
        1,
        "skill manifest item should be emitted once per skill file: {import}"
    );
    assert!(
        import["migration_report"]["findings"]
            .as_array()
            .expect("findings")
            .is_empty(),
        "plain Hermes markdown fixture should not require field review: {import}"
    );
}
