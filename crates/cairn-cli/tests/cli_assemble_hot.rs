//! End-to-end CLI snapshot for `cairn assemble_hot --json`. Pins the
//! JSON wire shape against a real binary invocation in a bootstrapped
//! tempdir vault.
//!
//! The CLI fails closed on a non-vault working directory (`CwdFallback`),
//! so the test bootstraps a real vault with `cairn_cli::vault::bootstrap`
//! before invoking the binary.

use std::path::Path;
use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};
use cairn_core::config::{CairnConfig, HotMemoryRecipeStep};
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::domain::{RecordId, Rfc3339Timestamp, ScopeTuple, TargetId};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

/// Replace the per-call `operation_id` ULID with a fixed placeholder so the
/// golden snapshot stays deterministic across runs.
fn redact_operation_id(json: &str) -> String {
    const KEY: &str = "\"operation_id\":\"";
    const ULID_LEN: usize = 26;
    const PLACEHOLDER: &str = "01XXXXXXXXXXXXXXXXXXXXXXXX";
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(idx) = rest.find(KEY) {
        out.push_str(&rest[..idx + KEY.len()]);
        let value_start = idx + KEY.len();
        if rest[value_start..].len() >= ULID_LEN {
            out.push_str(PLACEHOLDER);
            rest = &rest[value_start + ULID_LEN..];
        } else {
            rest = &rest[value_start..];
        }
    }
    out.push_str(rest);
    out
}

fn seed_default_identity(vault: &Path) {
    let output = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "identity seed",
            "--json",
        ])
        .output()
        .expect("seed default identity");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn project_playbook_record(
    id: &str,
    body: &str,
    updated_at: &str,
    skill_id: &str,
    lane: &str,
    requires: &[&str],
    provides: &[&str],
) -> MemoryRecord {
    let mut record = sample_record();
    record.id = RecordId::parse(id).expect("record id");
    record.target_id = TargetId::parse(id).expect("target id");
    record.kind = MemoryKind::Playbook;
    record.visibility = MemoryVisibility::Project;
    record.scope = ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("my-vault".to_owned()),
        entity: Some("ingest".to_owned()),
        ..ScopeTuple::default()
    };
    record.body = body.to_owned();
    record.updated_at = Rfc3339Timestamp::parse(updated_at).expect("updated_at");
    record
        .extra_frontmatter
        .insert("skill_id".to_owned(), serde_json::json!(skill_id));
    record
        .extra_frontmatter
        .insert("lane".to_owned(), serde_json::json!(lane));
    record
        .extra_frontmatter
        .insert("requires".to_owned(), serde_json::json!(requires));
    record
        .extra_frontmatter
        .insert("provides".to_owned(), serde_json::json!(provides));
    record
}

fn seed_records(vault: &Path, records: &[MemoryRecord]) {
    let db_path = vault.join(".cairn/cairn.db");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path)
            .await
            .expect("open store");
        for record in records {
            store.upsert(record).await.expect("upsert record");
        }
    });
}

fn write_skill_graph_files(vault: &Path) {
    std::fs::create_dir_all(vault.join("skills")).expect("skills dir");
    std::fs::write(
        vault.join("skills/skill_test.md"),
        "---\nskill_id: run-tests\nlane: test.run\ntriggers: [\"run tests\"]\nfiles_to: wiki/summaries/\nprovides: [\"cap.test\"]\n---\nRun tests.\n",
    )
    .expect("prereq skill");
    std::fs::write(
        vault.join("skills/skill_ship.md"),
        "---\nskill_id: ship-pr\nlane: ship.pr\ntriggers: [\"ship pr\"]\nfiles_to: wiki/summaries/\nrequires: [\"cap.test\"]\nprovides: [\"cap.ship\"]\n---\nShip PR.\n",
    )
    .expect("active skill");
}

fn assemble_hot_prefix(vault: &Path) -> String {
    let output = cli()
        .current_dir(vault)
        .args(["assemble_hot", "--budget", "4096", "--json"])
        .output()
        .expect("run assemble_hot");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix")
        .to_owned()
}

#[test]
fn cairn_assemble_hot_json_emits_segments() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run cairn");

    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");

    // Sanity: segments present and length 6.
    let segments = value.pointer("/data/segments").expect("segments present");
    assert!(
        segments.is_array(),
        "segments should be array, got {segments}"
    );
    assert_eq!(
        segments.as_array().unwrap().len(),
        6,
        "default recipe has 6 steps"
    );

    // Redact the volatile operation_id before snapshotting.
    let redacted = redact_operation_id(stdout.trim());
    insta::assert_snapshot!(redacted);
}

#[test]
fn cairn_assemble_hot_includes_active_playbook_prerequisites() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let prereq = project_playbook_record(
        "01HQZX9F5N0000000000000001",
        "run-tests prerequisite playbook",
        "2026-04-22T14:03:00Z",
        "run-tests",
        "test.run",
        &[],
        &["cap.test"],
    );
    let active = project_playbook_record(
        "01HQZX9F5N0000000000000002",
        "ship-pr active playbook",
        "2026-04-22T14:05:00Z",
        "ship-pr",
        "ship.pr",
        &["cap.test"],
        &["cap.ship"],
    );
    seed_records(vault.path(), &[prereq, active]);

    let prefix = assemble_hot_prefix(vault.path());

    let prereq_idx = prefix
        .find("run-tests prerequisite playbook")
        .expect("prerequisite playbook in prefix");
    let active_idx = prefix
        .find("ship-pr active playbook")
        .expect("active playbook in prefix");
    assert!(
        prereq_idx < active_idx,
        "prerequisite should precede active playbook: {prefix}"
    );
}

#[test]
fn cairn_assemble_hot_uses_skill_files_for_playbook_graph_metadata() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());
    write_skill_graph_files(vault.path());

    let prereq = project_playbook_record(
        "01HQZX9F5N0000000000000001",
        "run-tests prerequisite playbook",
        "2026-04-22T14:03:00Z",
        "run-tests",
        "test.run",
        &[],
        &[],
    );
    let active = project_playbook_record(
        "01HQZX9F5N0000000000000002",
        "ship-pr active playbook",
        "2026-04-22T14:05:00Z",
        "ship-pr",
        "ship.pr",
        &[],
        &[],
    );
    seed_records(vault.path(), &[prereq, active]);

    let prefix = assemble_hot_prefix(vault.path());

    let prereq_idx = prefix
        .find("run-tests prerequisite playbook")
        .expect("prerequisite playbook in prefix");
    let active_idx = prefix
        .find("ship-pr active playbook")
        .expect("active playbook in prefix");
    assert!(
        prereq_idx < active_idx,
        "skill-file graph metadata should place prerequisite first: {prefix}"
    );
}

#[test]
fn cairn_assemble_hot_backfills_playbook_prerequisites_beyond_first_page() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let mut records = Vec::new();
    records.push(project_playbook_record(
        "01HQZX9F5N0000000000000001",
        "run-tests prerequisite playbook",
        "2026-04-22T14:03:00Z",
        "run-tests",
        "test.run",
        &[],
        &["cap.test"],
    ));
    records.push(project_playbook_record(
        "01HQZX9F5N0000000000000002",
        "ship-pr active playbook",
        "2026-04-22T15:00:00Z",
        "ship-pr",
        "ship.pr",
        &["cap.test"],
        &["cap.ship"],
    ));
    for idx in 0..16 {
        records.push(project_playbook_record(
            &format!("01HQZX9F5N0000000000000{:03}", idx + 3),
            &format!("filler playbook {idx}"),
            &format!("2026-04-22T14:{:02}:00Z", idx + 4),
            &format!("filler-{idx}"),
            &format!("filler.{idx}"),
            &[],
            &[],
        ));
    }
    seed_records(vault.path(), &records);

    let prefix = assemble_hot_prefix(vault.path());

    let prereq_idx = prefix
        .find("run-tests prerequisite playbook")
        .expect("older prerequisite playbook in prefix");
    let active_idx = prefix
        .find("ship-pr active playbook")
        .expect("active playbook in prefix");
    assert!(
        prereq_idx < active_idx,
        "older prerequisite should be backfilled before active playbook: {prefix}"
    );
}

#[test]
fn cairn_assemble_hot_honors_budget_and_session_args() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("16")
        .arg("--session")
        .arg("01H00000000000000000000000")
        .arg("--json")
        .output()
        .expect("run cairn");

    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let bytes = value
        .pointer("/data/bytes")
        .and_then(serde_json::Value::as_u64)
        .expect("bytes");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");

    assert!(bytes <= 16, "bytes should respect budget, got {bytes}");
    assert!(
        prefix.len() <= 16,
        "prefix should respect budget, got {}",
        prefix.len()
    );
    assert!(value["policy_trace"].is_array());
}

#[test]
fn cairn_assemble_hot_rejects_budget_above_wire_max() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("4194305")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["error"]["code"], "InvalidArgs");
}

#[test]
fn cairn_assemble_hot_rejects_empty_session_id() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--session")
        .arg("")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["error"]["code"], "InvalidArgs");
}

#[test]
fn cairn_assemble_hot_loads_project_memory_from_store() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let ingest = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "project",
            "--body",
            "project hot body",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert!(
        ingest.status.success(),
        "exit={:?} stdout={} stderr={}",
        ingest.status.code(),
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");
    let trace = value
        .pointer("/policy_trace")
        .and_then(serde_json::Value::as_array)
        .expect("policy_trace");

    assert!(
        prefix.contains("project hot body"),
        "prefix missing ingested project memory: {prefix:?}"
    );
    assert!(trace.iter().any(|entry| {
        entry.get("gate").and_then(serde_json::Value::as_str) == Some("read.scope")
            && entry
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|detail| detail.contains("records=1"))
    }));
}

#[test]
fn cairn_assemble_hot_caps_store_backed_sections_to_budget() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let body = "project hot body ".repeat(64);
    let ingest = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "project", "--body", &body, "--json"])
        .output()
        .expect("run ingest");
    assert!(
        ingest.status.success(),
        "exit={:?} stdout={} stderr={}",
        ingest.status.code(),
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );

    let output = cli()
        .current_dir(vault.path())
        .args(["assemble_hot", "--budget", "80", "--json"])
        .output()
        .expect("run assemble_hot");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let bytes = value
        .pointer("/data/bytes")
        .and_then(serde_json::Value::as_u64)
        .expect("bytes");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");

    assert!(bytes <= 80, "bytes should respect budget, got {bytes}");
    assert!(
        prefix.len() <= 80,
        "prefix should respect budget, got {}",
        prefix.len()
    );
}

#[test]
fn cairn_assemble_hot_rejects_oversized_recipe_before_loading_sources() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let config_path = vault.path().join(".cairn/config.yaml");
    let raw_config = std::fs::read_to_string(&config_path).expect("read config");
    let mut config: CairnConfig = yaml_serde::from_str(&raw_config).expect("parse config");
    config.vault.hot_memory.recipe =
        vec![HotMemoryRecipeStep::Purpose; cairn_core::verbs::assemble_hot::MAX_SEGMENTS + 1];
    std::fs::write(
        &config_path,
        yaml_serde::to_string(&config).expect("serialize config"),
    )
    .expect("write config");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(78),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "aborted");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("recipe exceeds"))
    );
}

#[test]
fn cairn_assemble_hot_rejects_unknown_issuer_before_loading_sources() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .env("CAIRN_ISSUER", "agt:cairn-cli:missing:writer:v1")
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert!(value["data"].is_null());
    assert_eq!(value["error"]["code"], "Unauthorized");
}

#[test]
fn cairn_assemble_hot_validates_later_markdown_sources_after_budget_exhaustion() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());
    std::fs::remove_file(vault.path().join("index.md")).expect("remove index.md");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("1")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(78),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "aborted");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("read index.md"))
    );
}
