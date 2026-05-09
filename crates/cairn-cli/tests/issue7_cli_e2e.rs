//! Issue #7 parent acceptance smoke through the real `cairn` binary.
//!
//! This intentionally stitches the prelude and runtime checks together in one
//! CLI-only flow: bootstrap a vault, ask status what is available, verify the
//! advertised keyword capability can run, verify unadvertised mutating/read
//! capabilities fail closed, and mint two persisted handshake challenges.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

fn run(args: &[&str]) -> Output {
    cli()
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"))
}

fn run_json(args: &[&str]) -> Value {
    let out = run(args);
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

fn bootstrap_vault(vault: &Path) {
    let out = cli()
        .args([
            "bootstrap",
            "--vault-path",
            vault.to_str().expect("temp path must be utf-8"),
            "--json",
        ])
        .output()
        .expect("cairn bootstrap --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "bootstrap failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn capability_set(status: &Value) -> Vec<String> {
    let mut caps: Vec<String> = status["capabilities"]
        .as_array()
        .expect("status capabilities must be an array")
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("capability must be string, got {v}"))
                .to_owned()
        })
        .collect();
    caps.sort();
    caps
}

fn assert_status_capabilities_are_deterministic(vault_path: &str) {
    let status_one = run_json(&["--vault", vault_path, "status", "--json"]);
    let status_two = run_json(&["--vault", vault_path, "status", "--json"]);
    let caps_one = capability_set(&status_one);
    let caps_two = capability_set(&status_two);
    assert_eq!(
        caps_one, caps_two,
        "capability advertisement must be deterministic inside the same bound vault"
    );
    assert!(
        caps_one.contains(&"cairn.mcp.v1.search.keyword".to_owned()),
        "bound vault must advertise keyword search, got {caps_one:?}"
    );
    assert!(
        caps_one.contains(&"cairn.mcp.v1.policy_trace".to_owned()),
        "bound vault must advertise policy_trace, got {caps_one:?}"
    );
    assert!(
        !caps_one.contains(&"cairn.mcp.v1.retrieve.record".to_owned()),
        "retrieve.record must not be advertised until runtime can honor it: {caps_one:?}"
    );
    assert!(
        !caps_one.contains(&"cairn.mcp.v1.forget.record".to_owned()),
        "forget.record must not be advertised until runtime can honor it: {caps_one:?}"
    );
}

fn assert_keyword_search_matches_advertised_capability(vault_path: &str) {
    let search = run_json(&[
        "--vault", vault_path, "search", "--mode", "keyword", "issue7", "--json",
    ]);
    assert_eq!(search["contract"], "cairn.mcp.v1");
    assert_eq!(search["status"], "committed");
    assert_eq!(search["verb"], "search");
    assert!(
        search["data"]["hits"].as_array().is_some(),
        "keyword search must return an IDL hits array: {search}"
    );
}

fn assert_sensitive_verb_rejection(out: &Output, args: &[&str], verb: &str, capability: &str) {
    let rejected: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("rejection envelope must be JSON: {e}"));
    assert_eq!(
        rejected["contract"], "cairn.mcp.v1",
        "cairn {args:?} must reject with an IDL envelope: {rejected}"
    );
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["verb"], verb);
    assert!(
        rejected["data"].is_null(),
        "rejected {verb} must not return record data: {rejected}"
    );

    match rejected["error"]["code"].as_str() {
        Some("CapabilityUnavailable") => {
            assert_eq!(
                out.status.code(),
                Some(69),
                "CapabilityUnavailable must use EX_UNAVAILABLE\nstderr: {}\nstdout: {}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
            assert_eq!(rejected["error"]["data"]["capability"], capability);
        }
        Some("Unauthorized") => {
            assert_eq!(
                out.status.code(),
                Some(77),
                "Unauthorized must use EX_NOPERM\nstderr: {}\nstdout: {}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
            assert_eq!(rejected["error"]["data"]["required"], "authentication");
        }
        other => panic!(
            "cairn {args:?} must fail closed with CapabilityUnavailable or Unauthorized, got {other:?}: {rejected}"
        ),
    }
}

fn assert_unadvertised_sensitive_verbs_fail_closed(vault_path: &str) {
    for (args, verb, capability) in [
        (
            vec![
                "--vault",
                vault_path,
                "retrieve",
                "01JXXXXXXXXXXXXXXXXXXXXXXX",
                "--json",
            ],
            "retrieve",
            "cairn.mcp.v1.retrieve.record",
        ),
        (
            vec![
                "--vault",
                vault_path,
                "forget",
                "--record",
                "01JXXXXXXXXXXXXXXXXXXXXXXX",
                "--json",
            ],
            "forget",
            "cairn.mcp.v1.forget.record",
        ),
    ] {
        let out = run(&args);
        assert_sensitive_verb_rejection(&out, &args, verb, capability);
    }
}

fn assert_persisted_handshakes_are_fresh(vault: &Path, vault_path: &str) {
    let issuer = "hmn:issue7-cli:v1";
    let handshake_one = run_json(&[
        "--vault",
        vault_path,
        "handshake",
        "--issuer",
        issuer,
        "--json",
    ]);
    let handshake_two = run_json(&[
        "--vault",
        vault_path,
        "handshake",
        "--issuer",
        issuer,
        "--json",
    ]);

    let nonce_one = handshake_one["challenge"]["nonce"]
        .as_str()
        .expect("first handshake nonce");
    let nonce_two = handshake_two["challenge"]["nonce"]
        .as_str()
        .expect("second handshake nonce");
    assert_ne!(
        nonce_one, nonce_two,
        "consecutive CLI handshakes must mint distinct challenges"
    );
    assert_eq!(nonce_one.len(), 24, "nonce must be 16-byte base64");
    assert_eq!(nonce_two.len(), 24, "nonce must be 16-byte base64");

    let db_path = vault.join(".cairn/cairn.db");
    let conn = rusqlite::Connection::open(db_path).expect("open cairn.db");
    let persisted: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT challenge) FROM outstanding_challenges WHERE issuer = ?1",
            [issuer],
            |row| row.get(0),
        )
        .expect("count persisted challenges");
    assert_eq!(
        persisted, 2,
        "CLI handshake --issuer must persist both fresh outstanding challenges"
    );
}

#[test]
fn issue7_cli_bound_vault_prelude_and_capabilities_are_end_to_end_consistent() {
    let vault = tempfile::tempdir().expect("temp vault");
    let vault_path = vault.path().to_str().expect("temp path must be utf-8");
    bootstrap_vault(vault.path());

    assert_status_capabilities_are_deterministic(vault_path);
    assert_keyword_search_matches_advertised_capability(vault_path);
    assert_unadvertised_sensitive_verbs_fail_closed(vault_path);
    assert_persisted_handshakes_are_fresh(vault.path(), vault_path);
}
