//! Consumer acceptance coverage for the Rowboat migration bridge.

use std::path::Path;
use std::process::{Command, Output};

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

fn upsert_record_id(plan: &PersistedPlan) -> String {
    let Some(PlannedMutation::Upsert { record, .. }) = plan.plan.mutations.first() else {
        panic!(
            "expected first mutation to be an upsert: {:?}",
            plan.plan.mutations
        );
    };
    record.id.as_str().to_owned()
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

fn write_rowboat_fixture(root: &Path, body: &str) {
    let knowledge = root.join("knowledge");
    std::fs::create_dir_all(knowledge.join("People")).expect("people dir");
    std::fs::write(
        knowledge.join("People").join("Ada Lovelace.md"),
        format!(
            r"---
type: People
source: Gmail
rowboat_id: person-ada
unmapped_field: should-be-reviewed
---
# Ada Lovelace

{body}

Works with [[Analytical Engine]] on agent memory planning.
"
        ),
    )
    .expect("write rowboat note");
    std::fs::write(
        root.join("agent_notes_state.json"),
        r#"{
          "last_sync_at": "2026-03-04T05:06:07Z",
          "workflows": [{"id": "wf-gmail", "name": "Gmail Sync"}],
          "oauth_token": "redacted-before-review"
        }"#,
    )
    .expect("write rowboat state");
}

#[test]
fn rowboat_import_plan_applies_to_search_retrieve_lint_and_forget() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    run_json_ok(vault.path(), &["identity", "init-defaults", "--json"]);
    run_json_ok(
        vault.path(),
        &["identity", "provision", "human", "rowboat-import", "--json"],
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
    let archive = tempfile::tempdir().expect("rowboat archive");
    let body = "rowboat bridge remembers lapis relationship marker";
    write_rowboat_fixture(archive.path(), body);

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "rowboat",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "1",
            "--json",
        ],
    );
    assert_eq!(import["records"], 1, "import summary: {import}");
    assert_eq!(import["plans"], 1, "import summary: {import}");

    let (operation_id, pending) = single_pending_plan(vault.path());
    let record_id = upsert_record_id(&pending);

    run_json_ok(vault.path(), &["flush", "apply", &operation_id, "--json"]);

    let hits = hit_record_ids(vault.path(), "lapis");
    assert_eq!(hits, vec![record_id.clone()], "search should find import");

    let retrieve = run_json_ok(vault.path(), &["retrieve", &record_id, "--json"]);
    assert!(
        retrieve["data"]["body"]
            .as_str()
            .expect("retrieve body")
            .contains(body),
        "retrieve should expose imported body: {retrieve}"
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
        hit_record_ids(vault.path(), "lapis").is_empty(),
        "forgotten import should leave keyword search"
    );
}

#[test]
fn rowboat_import_json_emits_manifest_and_migration_report() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let archive = tempfile::tempdir().expect("rowboat archive");
    write_rowboat_fixture(archive.path(), "rowboat migration report fixture");

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "rowboat",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "1",
            "--json",
        ],
    );

    assert_eq!(import["records"], 1, "import summary: {import}");
    assert_eq!(
        import["manifest"]["system"], "rowboat",
        "manifest: {import}"
    );
    assert_eq!(
        import["manifest"]["items"].as_array().expect("items").len(),
        2,
        "record and workflow manifest items should be emitted: {import}"
    );
    assert!(
        import["manifest"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "skill" && item["legacy_id"] == "wf-gmail"),
        "workflow context should be preserved as a skill-like item: {import}"
    );
    assert_eq!(
        import["migration_report"]["unsupported_fields"], 1,
        "unsupported note frontmatter should be counted: {import}"
    );
    assert_eq!(
        import["migration_report"]["privacy_sensitive_fields"], 1,
        "privacy-sensitive state fields should be counted: {import}"
    );
    assert!(
        import["migration_report"]["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| {
                finding["field"] == "oauth_token" && finding["kind"] == "privacy_sensitive"
            }),
        "privacy-sensitive finding should be emitted: {import}"
    );
}
