//! Integration: verify an external cairn-pack/v1 directory through the CLI.

use std::process::Command;

fn cairn_binary() -> std::path::PathBuf {
    let raw = env!("CARGO_BIN_EXE_cairn");
    std::path::PathBuf::from(raw)
}

fn write_sample_pack(root: &std::path::Path) {
    std::fs::create_dir(root.join("agents")).expect("create agents dir");
    std::fs::create_dir(root.join("commands")).expect("create commands dir");
    std::fs::create_dir(root.join("hooks")).expect("create hooks dir");

    std::fs::write(
        root.join("pack.json"),
        r#"{
  "schema": "cairn-pack/v1",
  "pack_id": "sample-pack",
  "name": "sample-pack",
  "version": "1.0.0",
  "harness": "codex",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Sample Codex harness pack for external verification.",
  "requires_capabilities": [],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["status"]
    }
  ],
  "commands": [
    {
      "id": "cairn-context",
      "path": "commands/cairn-context.md",
      "kind": "verb-direct",
      "verb": "status"
    }
  ],
  "hooks": {
    "SessionStart": {
      "command": "cairn status --json"
    }
  },
  "manual_fragment": "AGENTS.md"
}
"#,
    )
    .expect("write pack.json");
    std::fs::write(
        root.join("agents/context-loader.md"),
        "# Context Loader\n\nLoads Cairn context.\n",
    )
    .expect("write subagent");
    std::fs::write(
        root.join("commands/cairn-context.md"),
        "# Cairn Context\n\nRun Cairn context retrieval.\n",
    )
    .expect("write command");
    std::fs::write(
        root.join("AGENTS.md"),
        "<!-- BEGIN CAIRN PACK sample-pack -->\nSample pack instructions.\n<!-- END CAIRN PACK sample-pack -->\n",
    )
    .expect("write AGENTS.md");
    std::fs::write(
        root.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"command":"cairn status --json"}]}}"#,
    )
    .expect("write hooks.json");
}

#[test]
fn plugins_verify_pack_path_json_reports_pack_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_sample_pack(root);

    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--pack-path"])
        .arg(root)
        .arg("--json")
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins verify --pack-path must exit 0; status={:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(v["summary"]["failed"], 0, "no pack failures expected");
    let first = &v["plugins"].as_array().expect("plugins array")[0];
    assert_eq!(first["name"], "sample-pack");
    assert_eq!(first["contract"], "pack");
}

#[cfg(unix)]
#[test]
fn plugins_verify_pack_path_json_preserves_symlink_diagnostic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_sample_pack(root);
    let outside = tempfile::tempdir().expect("outside tempdir");
    std::fs::write(outside.path().join("context-loader.md"), "# Outside\n")
        .expect("write outside subagent");
    std::fs::remove_file(root.join("agents/context-loader.md")).expect("remove subagent");
    std::os::unix::fs::symlink(
        outside.path().join("context-loader.md"),
        root.join("agents/context-loader.md"),
    )
    .expect("create symlinked subagent");

    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--pack-path"])
        .arg(root)
        .arg("--json")
        .output()
        .expect("spawn cairn binary");

    assert!(
        !output.status.success(),
        "symlinked referenced path must fail verification"
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let cases = v["plugins"][0]["cases"].as_array().expect("cases array");
    let paths_present = cases
        .iter()
        .find(|case| case["id"] == "pack_paths_present")
        .expect("pack_paths_present case");
    let message = paths_present["message"].as_str().expect("failure message");
    assert!(
        message.contains("symlink"),
        "expected symlink diagnostic, got: {message}"
    );
}
