#![allow(missing_docs)]

use assert_cmd::Command;
use serde_json::Value;

fn cli() -> Command {
    Command::cargo_bin("cairn").expect("cargo bin cairn")
}

fn parse_stdout_json(out: std::process::Output) -> Value {
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout")
}

fn read_json(path: &std::path::Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn bootstrap_vault(vault: &std::path::Path) {
    let out = cli()
        .args([
            "bootstrap",
            "--vault-path",
            vault.to_str().expect("utf-8 vault dir"),
        ])
        .output()
        .expect("cairn bootstrap");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn claude_code_setup_writes_mcp_and_hook_config() {
    let project = tempfile::tempdir().expect("tempdir");
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault dir"),
            "setup",
            "claude-code",
            "--scope",
            "project",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--binary",
            env!("CARGO_BIN_EXE_cairn"),
            "--json",
        ])
        .output()
        .expect("cairn setup claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "project");
    assert_eq!(receipt["server_name"], "cairn");
    assert_eq!(receipt["status"], "created");

    let mcp = read_json(&project.path().join(".mcp.json"));
    assert_eq!(
        mcp["mcpServers"]["cairn"]["command"],
        env!("CARGO_BIN_EXE_cairn")
    );
    assert_eq!(
        mcp["mcpServers"]["cairn"]["args"],
        serde_json::json!(["--vault", vault.path().display().to_string(), "mcp"])
    );
    assert_eq!(mcp["mcpServers"]["cairn"]["type"], "stdio");

    let settings = read_json(&project.path().join(".claude/settings.local.json"));
    for hook in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ] {
        let groups = settings["hooks"][hook]
            .as_array()
            .unwrap_or_else(|| panic!("{hook} hook groups should be an array"));
        assert_eq!(groups.len(), 1, "{hook} should have one matcher group");
        if matches!(hook, "PreToolUse" | "PostToolUse") {
            assert_eq!(groups[0]["matcher"], "*");
        } else {
            assert!(groups[0].get("matcher").is_none());
        }
        let commands = groups[0]["hooks"]
            .as_array()
            .unwrap_or_else(|| panic!("{hook} matcher group should contain hooks"));
        assert_eq!(commands.len(), 1, "{hook} should have one installed action");
        assert_eq!(commands[0]["type"], "command");
        assert_eq!(
            commands[0]["command"],
            format!(
                "{} hook {hook} --vault-path {} --payload-file - --json",
                env!("CARGO_BIN_EXE_cairn"),
                vault.path().display()
            )
        );
    }
}

#[test]
fn claude_code_setup_quotes_hook_paths_for_shell_commands() {
    let project = tempfile::tempdir().expect("tempdir");
    let vault = project.path().join("vault with spaces");
    std::fs::create_dir_all(&vault).expect("mkdir vault with spaces");
    bootstrap_vault(&vault);
    let cairn_bin = project.path().join("bin dir/cairn test");
    std::fs::create_dir_all(cairn_bin.parent().expect("parent")).expect("mkdir bin dir");
    std::fs::write(&cairn_bin, b"").expect("write fake bin");

    let out = cli()
        .current_dir(project.path())
        .args([
            "--vault",
            vault.to_str().expect("utf-8 vault dir"),
            "setup",
            "claude-code",
            "--scope",
            "project",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--binary",
            cairn_bin.to_str().expect("utf-8 cairn bin"),
            "--json",
        ])
        .output()
        .expect("cairn setup claude-code --json");

    assert_eq!(out.status.code(), Some(0));
    let settings = read_json(&project.path().join(".claude/settings.local.json"));
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"],
        format!(
            "'{}' hook Stop --vault-path '{}' --payload-file - --json",
            cairn_bin.display(),
            vault.display()
        )
    );
}

#[test]
fn claude_code_setup_is_idempotent_and_preserves_unrelated_keys() {
    let project = tempfile::tempdir().expect("tempdir");
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    std::fs::create_dir_all(project.path().join(".claude")).expect("mkdir .claude");
    std::fs::write(
        project.path().join(".mcp.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "mcpServers": {
                "other": {
                    "command": "elsewhere",
                    "args": ["serve"]
                }
            },
            "custom": true
        }))
        .expect("serialize .mcp.json"),
    )
    .expect("write .mcp.json");
    std::fs::write(
        project.path().join(".claude/settings.local.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "theme": "dark",
            "hooks": {
                "Stop": [{"type": "command", "command": "notify-send done"}]
            }
        }))
        .expect("serialize settings"),
    )
    .expect("write settings");

    for _ in 0..2 {
        let out = cli()
            .current_dir(project.path())
            .args([
                "--vault",
                vault.path().to_str().expect("utf-8 vault dir"),
                "setup",
                "claude-code",
                "--scope",
                "project",
                "--project-dir",
                project.path().to_str().expect("utf-8 project dir"),
                "--binary",
                env!("CARGO_BIN_EXE_cairn"),
                "--json",
            ])
            .output()
            .expect("cairn setup claude-code --json");
        assert_eq!(out.status.code(), Some(0));
    }

    let mcp = read_json(&project.path().join(".mcp.json"));
    assert_eq!(mcp["custom"], true);
    assert_eq!(mcp["mcpServers"]["other"]["command"], "elsewhere");
    assert_eq!(
        mcp["mcpServers"]["cairn"]["args"],
        serde_json::json!(["--vault", vault.path().display().to_string(), "mcp"])
    );

    let settings = read_json(&project.path().join(".claude/settings.local.json"));
    assert_eq!(settings["theme"], "dark");
    let stop_groups = settings["hooks"]["Stop"].as_array().expect("Stop array");
    assert_eq!(
        stop_groups.len(),
        2,
        "setup should not duplicate the Cairn Stop hook"
    );
    assert_eq!(stop_groups[0]["command"], "notify-send done");
    let stop = stop_groups[1]["hooks"]
        .as_array()
        .expect("new Stop hooks array");
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0]["type"], "command");
}

#[cfg(unix)]
#[test]
fn claude_code_setup_rejects_symlinked_config_targets() {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    let target = outside.path().join("outside.json");
    std::fs::write(&target, b"{\"keep\":true}").expect("write outside target");
    symlink(&target, project.path().join(".mcp.json")).expect("symlink .mcp.json");

    let out = cli()
        .current_dir(project.path())
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault dir"),
            "setup",
            "claude-code",
            "--scope",
            "project",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--binary",
            env!("CARGO_BIN_EXE_cairn"),
            "--json",
        ])
        .output()
        .expect("cairn setup claude-code --json");

    assert_eq!(out.status.code(), Some(78), "exit: {:?}", out.status);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("symlink"),
        "error should mention symlink: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read outside target"),
        "{\"keep\":true}"
    );
}
