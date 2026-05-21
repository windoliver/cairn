//! Consumer acceptance coverage for external memory migration bridges.

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

fn run_json_ok_with_issuer(vault: &Path, args: &[&str], issuer: &str) -> serde_json::Value {
    let out = cli()
        .current_dir(vault)
        .env("CAIRN_ISSUER", issuer)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"));
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

fn upsert_record_id_containing(plan: &PersistedPlan, needle: &str) -> String {
    plan.plan
        .mutations
        .iter()
        .find_map(|mutation| {
            let PlannedMutation::Upsert { record, .. } = mutation else {
                return None;
            };
            record
                .body
                .contains(needle)
                .then(|| record.id.as_str().to_owned())
        })
        .unwrap_or_else(|| panic!("expected an upsert containing {needle:?}: {plan:?}"))
}

#[allow(
    clippy::type_complexity,
    reason = "scope tuple mirrors the persisted record fields asserted by the fixture"
)]
fn upsert_record_scope_containing(
    plan: &PersistedPlan,
    needle: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    plan.plan
        .mutations
        .iter()
        .find_map(|mutation| {
            let PlannedMutation::Upsert { record, .. } = mutation else {
                return None;
            };
            record.body.contains(needle).then(|| {
                (
                    record.scope.tenant.clone(),
                    record.scope.workspace.clone(),
                    record.scope.entity.clone(),
                    record.scope.user.clone(),
                    record.scope.agent.clone(),
                )
            })
        })
        .unwrap_or_else(|| panic!("expected an upsert containing {needle:?}: {plan:?}"))
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

#[test]
fn koi_v1_import_plan_applies_to_search_retrieve_lint_and_forget() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    run_json_ok(vault.path(), &["identity", "init-defaults", "--json"]);
    run_json_ok(
        vault.path(),
        &["identity", "provision", "human", "koi-import", "--json"],
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
    let archive = tempfile::tempdir().expect("koi archive");
    let koi_body = "koi acceptance bridge remembers calico telemetry marker";
    std::fs::write(
        archive.path().join("memory.json"),
        serde_json::json!({
            "id": "koi-legacy-001",
            "kind": "reference",
            "text": koi_body,
            "tags": ["migration", "acceptance"],
            "scope": {
                "tenant": "default",
                "workspace": "my-vault",
                "entity": "ingest",
                "user": "hmn:tafeng"
            }
        })
        .to_string(),
    )
    .expect("write koi fixture");

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "koi-v1",
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

    let hits = hit_record_ids(vault.path(), "calico");
    assert_eq!(hits, vec![record_id.clone()], "search should find import");

    let retrieve = run_json_ok(vault.path(), &["retrieve", &record_id, "--json"]);
    assert_eq!(
        retrieve["data"]["body"], koi_body,
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
        hit_record_ids(vault.path(), "calico").is_empty(),
        "forgotten import should leave keyword search"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "end-to-end import fixture keeps workflow steps together"
)]
fn opencode_import_plan_applies_to_search_retrieve_lint_and_forget() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    run_json_ok(vault.path(), &["identity", "init-defaults", "--json"]);
    run_json_ok(
        vault.path(),
        &[
            "identity",
            "provision",
            "human",
            "opencode-import",
            "--json",
        ],
    );
    run_json_ok(
        vault.path(),
        &["identity", "provision", "human", "tafeng", "--json"],
    );
    run_json_ok(
        vault.path(),
        &[
            "identity",
            "provision",
            "agent",
            "opencode:default:import",
            "--json",
        ],
    );
    run_json_ok(
        vault.path(),
        &[
            "identity",
            "provision",
            "sensor",
            "opencode:import:local",
            "--json",
        ],
    );
    let archive = tempfile::tempdir().expect("opencode archive");
    std::fs::write(
        archive.path().join("AGENTS.md"),
        "# OpenCode Instructions\n\nProject purpose includes migration persistence.",
    )
    .expect("write opencode instructions");
    std::fs::create_dir_all(archive.path().join("sessions")).expect("sessions dir");
    std::fs::write(
        archive.path().join("memory.json"),
        serde_json::json!({
            "id": "opencode-memory-001",
            "kind": "reference",
            "text": "OpenCode retrieve acceptance stores opencoderetrieve marker.",
            "scope": {
                "tenant": "default",
                "workspace": "my-vault",
                "entity": "ingest",
                "user": "hmn:tafeng"
            }
        })
        .to_string(),
    )
    .expect("write opencode memory");
    std::fs::write(
        archive.path().join("sessions").join("session.json"),
        serde_json::json!({
            "id": "ses_issue_156",
            "session_id": "ses_issue_156",
            "created_at": "2026-04-01T10:00:00Z",
            "scope": {
                "tenant": "default",
                "workspace": "my-vault",
                "entity": "ingest",
                "user": "hmn:tafeng"
            },
            "summary": {
                "Goal": "Migrate OpenCode memory through reviewable plans.",
                "Constraints": ["Preserve typed part ordering."],
                "Progress": "Imported zephyrcheckpoint session part.",
                "Decisions": ["Use Cairn FlushPlans."]
            },
            "parts": [
                {"id": "p1", "type": "user", "text": "Please preserve zephyrcheckpoint in imported traces."},
                {"id": "p2", "type": "tool", "tool": "skill", "text": "Loaded migration skill."}
            ]
        })
        .to_string(),
    )
    .expect("write opencode session");

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "opencode",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "64",
            "--json",
        ],
    );
    assert_eq!(import["manifest"]["system"], "opencode", "import: {import}");
    assert_eq!(import["records"], 8, "import summary: {import}");
    assert_eq!(import["plans"], 1, "import summary: {import}");
    assert!(
        import["manifest"]["items"]
            .as_array()
            .expect("manifest items")
            .iter()
            .any(|item| item["kind"] == "skill" && item["legacy_id"] == "skill"),
        "OpenCode skill tool part should emit a skill manifest item: {import}"
    );

    let (operation_id, pending) = single_pending_plan(vault.path());
    let record_id = upsert_record_id_containing(&pending, "Please preserve zephyrcheckpoint");
    let retrieve_record_id = upsert_record_id_containing(&pending, "opencoderetrieve");
    assert_eq!(
        upsert_record_scope_containing(&pending, "Please preserve zephyrcheckpoint"),
        (
            Some("default".to_owned()),
            Some("my-vault".to_owned()),
            Some("ingest".to_owned()),
            Some("hmn:tafeng".to_owned()),
            None
        )
    );

    run_json_ok(vault.path(), &["flush", "apply", &operation_id, "--json"]);

    let hits = hit_record_ids(vault.path(), "zephyrcheckpoint");
    assert!(
        hits.contains(&record_id),
        "search should find imported OpenCode trace {record_id}; hits: {hits:?}"
    );

    let retrieve = run_json_ok_with_issuer(
        vault.path(),
        &["retrieve", &retrieve_record_id, "--json"],
        "hmn:tafeng:v1",
    );
    assert_eq!(
        retrieve["data"]["body"], "OpenCode retrieve acceptance stores opencoderetrieve marker.",
        "retrieve should expose imported OpenCode record body: {retrieve}"
    );

    let lint_out = run_in_vault(vault.path(), &["lint", "--json"]);
    let lint = json_output(&lint_out, &["lint", "--json"]);
    assert_eq!(lint["status"], "committed", "lint should complete: {lint}");
    assert_eq!(
        lint["data"]["summary"]["by_severity"]["error"], 0,
        "OpenCode imported records should not produce lint errors: {lint}"
    );

    run_json_ok(vault.path(), &["forget", "--record", &record_id, "--json"]);
    assert!(
        !hit_record_ids(vault.path(), "zephyrcheckpoint").contains(&record_id),
        "forgotten OpenCode trace should leave keyword search"
    );
}

#[test]
fn koi_v1_import_json_emits_manifest_and_migration_report() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let archive = tempfile::tempdir().expect("koi archive");
    std::fs::write(
        archive.path().join("memory.json"),
        serde_json::json!({
            "id": "koi-legacy-002",
            "kind": "reference",
            "text": "koi migration report fixture",
            "scope": {
                "project": "legacy-project",
                "session_id": "legacy-session"
            },
            "skills": [{"id": "legacy-skill"}],
            "legacy_embedding": [0.1, 0.2],
            "api_token": "redacted-before-review"
        })
        .to_string(),
    )
    .expect("write koi fixture");

    let import = run_json_ok(
        vault.path(),
        &[
            "import",
            "--from",
            "koi-v1",
            archive.path().to_str().expect("utf-8 archive path"),
            "--batch-size",
            "1",
            "--json",
        ],
    );

    assert_eq!(import["records"], 1, "import summary: {import}");
    assert_eq!(import["manifest"]["system"], "koi-v1", "manifest: {import}");
    assert_eq!(
        import["manifest"]["items"].as_array().expect("items").len(),
        3,
        "record, session, and skill manifest items should be emitted: {import}"
    );
    assert_eq!(
        import["migration_report"]["ambiguous_fields"], 1,
        "project scope fallback should be counted: {import}"
    );
    assert_eq!(
        import["migration_report"]["unsupported_fields"], 1,
        "unsupported legacy field should be counted: {import}"
    );
    assert_eq!(
        import["migration_report"]["privacy_sensitive_fields"], 1,
        "privacy-sensitive legacy field should be counted: {import}"
    );
    assert!(
        import["migration_report"]["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(
                |finding| finding["field"] == "api_token" && finding["kind"] == "privacy_sensitive"
            ),
        "privacy-sensitive finding should be emitted: {import}"
    );
}
