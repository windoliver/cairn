//! End-to-end smoke for `cairn forget --record <ulid>` (issue #58).
//!
//! Bootstraps a vault, ingests a fact, then runs `forget --record` against
//! the stored `target_id` and asserts:
//! 1. The committed envelope shape (status=committed, verb=forget,
//!    `deleted_count=1`).
//! 2. The record disappears from `cairn search` afterwards.
//! 3. A second forget against the same target reports `deleted_count=0`
//!    with status=committed (idempotent re-forget; brief §5.6).
//! 4. A forget against a never-existing target id reports
//!    `deleted_count=0` with status=committed.
//! 5. A forget invoked with an unregistered `CAIRN_ISSUER` is rejected
//!    with `Unauthorized` and exit code `EX_NOPERM=77`.
//!
//! Bypasses the binary-level `cairn bootstrap` to avoid the BGE embedding
//! model download that the CLI bootstrap triggers when
//! `search.local_embeddings: true`. The library helper
//! `cairn_cli::vault::bootstrap` performs the same vault layout creation
//! without fetching the model.

use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env_remove("CAIRN_ISSUER");
    cmd
}

#[test]
fn forget_record_commits_and_search_no_longer_returns_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    // 1. Ingest a record.
    let ingest_out = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "hello forget",
            "--json",
        ])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(
        ingest_out.status.code(),
        Some(0),
        "ingest must succeed; stderr: {}",
        String::from_utf8_lossy(&ingest_out.stderr)
    );
    let ingest_stdout = String::from_utf8(ingest_out.stdout).expect("utf-8");
    let ingest_json: serde_json::Value = serde_json::from_str(ingest_stdout.trim())
        .unwrap_or_else(|e| panic!("ingest JSON parse failed: {e}\nstdout: {ingest_stdout:?}"));
    assert_eq!(ingest_json["status"], "committed");

    // Resolve the target_id by reading directly from the SQLite DB. The
    // ingest envelope only carries the version-specific record_id; the
    // forget verb takes the supersession lineage key (target_id) which
    // are independent ULIDs (brief §3 / §3.0).
    let db_path = dir.path().join(".cairn").join("cairn.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open cairn.db");
    let target_id: String = conn
        .query_row("SELECT target_id FROM records LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("read target_id from records table");
    drop(conn);
    assert_eq!(target_id.len(), 26, "target_id must be a 26-char ULID");

    // 2. Forget the record by target_id.
    let forget_out = cli()
        .current_dir(dir.path())
        .args(["forget", "--record", target_id.as_str(), "--json"])
        .output()
        .expect("cairn forget --record --json");
    assert_eq!(
        forget_out.status.code(),
        Some(0),
        "forget must succeed; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&forget_out.stderr),
        String::from_utf8_lossy(&forget_out.stdout)
    );
    let forget_stdout = String::from_utf8(forget_out.stdout).expect("utf-8");
    let forget_json: serde_json::Value = serde_json::from_str(forget_stdout.trim())
        .unwrap_or_else(|e| panic!("forget JSON parse failed: {e}\nstdout: {forget_stdout:?}"));
    assert_eq!(forget_json["contract"], "cairn.mcp.v1");
    assert_eq!(forget_json["status"], "committed");
    assert_eq!(forget_json["verb"], "forget");
    assert!(forget_json["error"].is_null(), "no error on success");
    assert_eq!(
        forget_json["data"]["deleted_count"], 1,
        "deleted_count must be 1 after committing a single forget"
    );
    assert!(forget_json["operation_id"].is_string());
    assert!(forget_json["policy_trace"].is_array());

    // 3. Confirm the record is gone via `cairn search --mode keyword`.
    let search_out = cli()
        .current_dir(dir.path())
        .args(["search", "--mode", "keyword", "hello forget", "--json"])
        .output()
        .expect("cairn search --json");
    assert_eq!(
        search_out.status.code(),
        Some(0),
        "search must exit 0; stderr: {}",
        String::from_utf8_lossy(&search_out.stderr)
    );
    let search_stdout = String::from_utf8(search_out.stdout).expect("utf-8");
    let search_json: serde_json::Value = serde_json::from_str(search_stdout.trim())
        .unwrap_or_else(|e| panic!("search JSON parse failed: {e}\nstdout: {search_stdout:?}"));
    let hits = search_json["data"]["hits"]
        .as_array()
        .expect("search must return data.hits array");
    assert!(
        hits.is_empty(),
        "search after forget must return no hits, got {hits:?}"
    );
}

/// Repeat-forget against the same target reports `deleted_count: 0` and
/// stays `committed`. Brief §5.6: forget is idempotent — the WAL op
/// executes and tombstones zero new rows because the canonical row
/// was already tombstoned + purged on the first call. Operators reading
/// `deleted_count: 0` get the signal they need to debug stale IDs /
/// repeat forgets without us promoting the no-op into a hard failure.
#[test]
fn forget_record_idempotent_re_forget_reports_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let _ = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "second forget",
            "--json",
        ])
        .output()
        .expect("ingest");
    let db_path = dir.path().join(".cairn").join("cairn.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open cairn.db");
    let target_id: String = conn
        .query_row("SELECT target_id FROM records LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("read target_id");
    drop(conn);

    // First forget — should succeed with deleted_count=1.
    let first = cli()
        .current_dir(dir.path())
        .args(["forget", "--record", target_id.as_str(), "--json"])
        .output()
        .expect("first forget");
    assert_eq!(first.status.code(), Some(0), "first forget must succeed");
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first forget JSON");
    assert_eq!(first_json["data"]["deleted_count"], 1);

    // Repeat forget — must commit with deleted_count=0.
    let second = cli()
        .current_dir(dir.path())
        .args(["forget", "--record", target_id.as_str(), "--json"])
        .output()
        .expect("second forget");
    assert_eq!(
        second.status.code(),
        Some(0),
        "idempotent re-forget must exit 0; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&second.stderr),
        String::from_utf8_lossy(&second.stdout)
    );
    let second_json: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("second forget JSON");
    assert_eq!(second_json["status"], "committed");
    assert_eq!(
        second_json["data"]["deleted_count"], 0,
        "repeat forget against an already-tombstoned target must report deleted_count=0"
    );
}

/// Forget against a target that never existed reports `deleted_count: 0`
/// with `status: committed`. The WAL op still executes — that is the
/// brief §5.6 idempotency contract — but the response envelope tells
/// operators no live rows were tombstoned so they can investigate
/// stale IDs / typos without us conflating the no-op with a hard error.
#[test]
fn forget_record_against_never_existing_target_reports_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    // Need at least one ingest so the default issuer auto-provisions
    // before we exercise forget on a never-seen target. Without it the
    // forget would still auto-provision the default issuer, but pinning
    // the order here keeps the test focused on `deleted_count`, not the
    // issuer-resolution side-effect.
    let _ = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "anchor row",
            "--json",
        ])
        .output()
        .expect("ingest");

    let phantom = "01HZZZZZZZZZZZZZZZZZZZZZZZ";
    let out = cli()
        .current_dir(dir.path())
        .args(["forget", "--record", phantom, "--json"])
        .output()
        .expect("forget phantom");
    assert_eq!(
        out.status.code(),
        Some(0),
        "forget against a never-existing target must exit 0; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("forget JSON parse");
    assert_eq!(json["status"], "committed");
    assert_eq!(
        json["data"]["deleted_count"], 0,
        "phantom-target forget must report deleted_count=0"
    );
}

/// `CAIRN_ISSUER=hmn:never-registered:v1` must be rejected with
/// `Unauthorized`. Without this gate any string an environment
/// variable holds would land verbatim as `consent_journal.actor` for
/// the destructive WAL op. Mirrors `ingest`'s issuer-resolution
/// contract — only the default issuer auto-provisions; every custom
/// issuer must be registered first via `cairn handshake --issuer ...`.
#[test]
fn forget_record_rejects_unregistered_custom_issuer() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let _ = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "auth-gate target",
            "--json",
        ])
        .output()
        .expect("ingest");
    // Use a syntactically-valid placeholder ULID. The Unauthorized
    // gate runs BEFORE the engine reads the target row — no live row
    // is required for the assertion. (Pinning a placeholder removes
    // an unrelated SELECT-target_id failure mode from the test.)
    let target_id = "01HZZZZZZZZZZZZZZZZZZZZZZZ";

    let mut forge_cmd = cli();
    forge_cmd
        .current_dir(dir.path())
        .env("CAIRN_ISSUER", "hmn:never-registered:v1")
        .args(["forget", "--record", target_id, "--json"]);
    let out = forge_cmd.output().expect("forget with unregistered issuer");
    assert_eq!(
        out.status.code(),
        Some(77),
        "unregistered custom issuer must exit EX_NOPERM=77; stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("rejected JSON parse");
    assert_eq!(json["status"], "rejected");
    assert_eq!(json["verb"], "forget");
    assert_eq!(
        json["error"]["code"], "Unauthorized",
        "unregistered issuer must surface Unauthorized; got {json:?}"
    );
}
