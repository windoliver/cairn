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

fn synth_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_vault(dir.path());
    dir
}

fn seed_vault(root: &std::path::Path) {
    let cairn_dir = root.join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("mkdir .cairn");

    std::fs::write(cairn_dir.join("vault.id"), "01J8WSKJ5T0R6XKYV5T2P4ZQVD")
        .expect("write vault.id");
    std::fs::write(
        cairn_dir.join("config.yaml"),
        "search:\n  local_embeddings: false\nmcp:\n  stdio:\n    single_tenant: false\n",
    )
    .expect("write config.yaml");
}

fn synth_vault_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_vault(dir.path());
    dir
}

fn write_project_mcp_config(project: &std::path::Path, vault: &std::path::Path) {
    let body = serde_json::json!({
        "mcpServers": {
            "cairn": {
                "command": env!("CARGO_BIN_EXE_cairn"),
                "args": ["--vault", vault.display().to_string(), "mcp"]
            }
        }
    });
    std::fs::write(
        project.join(".mcp.json"),
        serde_json::to_vec_pretty(&body).expect("serialize .mcp.json"),
    )
    .expect("write .mcp.json");
}

fn write_custom_mcp_config(project: &std::path::Path, command: &str, args: &[&str]) {
    let body = serde_json::json!({
        "mcpServers": {
            "cairn": {
                "command": command,
                "args": args
            }
        }
    });
    std::fs::write(
        project.join(".mcp.json"),
        serde_json::to_vec_pretty(&body).expect("serialize .mcp.json"),
    )
    .expect("write .mcp.json");
}

fn write_invalid_project_mcp_config(project: &std::path::Path) {
    std::fs::write(project.join(".mcp.json"), b"{ invalid json").expect("write invalid .mcp.json");
}

fn write_user_claude_config_local_project_scope(
    home: &std::path::Path,
    project: &std::path::Path,
    command: &str,
    args: &[&str],
) {
    let body = serde_json::json!({
        "projects": {
            project.display().to_string(): {
                "mcpServers": {
                    "cairn": {
                        "command": command,
                        "args": args
                    }
                }
            }
        }
    });
    std::fs::write(
        home.join(".claude.json"),
        serde_json::to_vec_pretty(&body).expect("serialize .claude.json"),
    )
    .expect("write .claude.json");
}

fn write_user_claude_config_user_scope(
    home: &std::path::Path,
    command: &str,
    args: &[&str],
    server_type: Option<&str>,
) {
    let mut server = serde_json::json!({
        "command": command,
        "args": args
    });
    if let Some(server_type) = server_type {
        server["type"] = serde_json::Value::String(server_type.to_owned());
    }
    let body = serde_json::json!({
        "mcpServers": {
            "cairn": server
        }
    });
    std::fs::write(
        home.join(".claude.json"),
        serde_json::to_vec_pretty(&body).expect("serialize .claude.json"),
    )
    .expect("write .claude.json");
}

fn write_hook_settings(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{"command": "cairn hook SessionStart"}],
            "UserPromptSubmit": [{"command": "cairn hook UserPromptSubmit"}],
            "PreToolUse": [{"command": "cairn hook PreToolUse"}],
            "PostToolUse": [{"command": "cairn hook PostToolUse"}],
            "Stop": [{"command": "cairn hook Stop"}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_hook_settings_nested_matchers(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "cairn hook SessionStart"}]}],
            "UserPromptSubmit": [{"matcher": "*", "hooks": [{"type": "command", "command": "cairn hook UserPromptSubmit"}]}],
            "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "cairn hook PreToolUse"}]}],
            "PostToolUse": [{"matcher": "*", "hooks": [{"type": "command", "command": "cairn hook PostToolUse"}]}],
            "Stop": [{"matcher": "*", "hooks": [{"type": "command", "command": "cairn hook Stop"}]}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_partial_project_hook_settings(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{"command": "cairn hook SessionStart"}],
            "UserPromptSubmit": [{"command": "cairn hook UserPromptSubmit"}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_partial_user_hook_settings(home: &std::path::Path) {
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir ~/.claude");
    let body = serde_json::json!({
        "hooks": {
            "PreToolUse": [{"command": "cairn hook PreToolUse"}],
            "PostToolUse": [{"command": "cairn hook PostToolUse"}],
            "Stop": [{"command": "cairn hook Stop"}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_vec_pretty(&body).expect("serialize ~/.claude/settings.json"),
    )
    .expect("write ~/.claude/settings.json");
}

fn write_hook_settings_empty(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [],
            "UserPromptSubmit": [],
            "PreToolUse": [],
            "PostToolUse": [],
            "Stop": []
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_hook_settings_missing_stop(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [],
            "UserPromptSubmit": [],
            "PreToolUse": [],
            "PostToolUse": []
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_hook_settings_matcher_only(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{"matcher": "*", "hooks": []}],
            "UserPromptSubmit": [{"matcher": "*", "hooks": []}],
            "PreToolUse": [{"matcher": "*", "hooks": []}],
            "PostToolUse": [{"matcher": "*", "hooks": []}],
            "Stop": [{"matcher": "*", "hooks": []}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

fn write_executable_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write script");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, name, body);
        panic!("doctor_cli executable script helper requires unix");
    }
}

#[test]
fn doctor_help_lists_claude_code_subcommand() {
    let out = cli()
        .args(["doctor", "--help"])
        .output()
        .expect("cairn doctor --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("claude-code"),
        "doctor help missing claude-code: {stdout}",
    );
}

#[test]
fn claude_code_doctor_reports_missing_registration() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], false);
    assert_eq!(json["consumer"], "claude-code");
    assert_eq!(json["stages"][0]["name"], "mcp_config");
    assert_eq!(json["stages"][0]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_invalid_json_config() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    write_invalid_project_mcp_config(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][0]["name"], "mcp_config");
    assert_eq!(json["stages"][0]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_missing_binary() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    write_custom_mcp_config(project.path(), "/definitely/missing/cairn", &["mcp"]);

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][0]["name"], "mcp_config");
    assert_eq!(json["stages"][0]["status"], "ok");
    assert_eq!(json["stages"][1]["name"], "binary");
    assert_eq!(json["stages"][1]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_unresolved_environment_variable_binary() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    write_custom_mcp_config(project.path(), "${CAIRN_BIN}", &["mcp"]);

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][1]["name"], "binary");
    assert_eq!(json["stages"][1]["status"], "failed");
}

#[test]
fn claude_code_doctor_rejects_non_stdio_registration() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    write_user_claude_config_user_scope(
        home.path(),
        env!("CARGO_BIN_EXE_cairn"),
        &["mcp"],
        Some("sse"),
    );

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][2]["name"], "mcp_registration");
    assert_eq!(json["stages"][2]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_mcp_startup_failure() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "fake-cairn-startup-fail.sh",
        "#!/bin/bash\nprintf 'not json\\n'\n",
    );
    write_custom_mcp_config(
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][2]["name"], "mcp_registration");
    assert_eq!(json["stages"][2]["status"], "ok");
    assert_eq!(json["stages"][3]["name"], "mcp_startup");
    assert_eq!(json["stages"][3]["status"], "failed");
}

#[test]
fn claude_code_doctor_includes_stderr_for_mcp_startup_failure() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "fake-cairn-startup-stderr.sh",
        "#!/bin/bash\nprintf 'boom on stderr\\n' >&2\nprintf 'not json\\n'\n",
    );
    write_custom_mcp_config(
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][3]["name"], "mcp_startup");
    assert_eq!(json["stages"][3]["status"], "failed");
    let detail = json["stages"][3]["detail"].as_str().expect("detail");
    assert!(
        detail.contains("boom on stderr"),
        "detail should include stderr text: {detail}",
    );
}

#[test]
fn claude_code_doctor_accepts_nested_matcher_hook_shapes() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_hook_settings_nested_matchers(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "ok");
}

#[test]
fn claude_code_doctor_merges_hook_entries_across_project_and_user_settings() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_partial_project_hook_settings(project.path());
    write_partial_user_hook_settings(home.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "ok");
}

#[test]
fn claude_code_doctor_reports_runtime_status_failure() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "fake-cairn-status-fail.sh",
        "#!/bin/bash\nread line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"serverInfo\":{\"name\":\"fake\",\"version\":\"0.1.0\"}}}\\n'\nread line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32603,\"message\":\"status unavailable\"}}\\n'\n",
    );
    write_custom_mcp_config(
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][3]["name"], "mcp_startup");
    assert_eq!(json["stages"][3]["status"], "ok");
    assert_eq!(json["stages"][4]["name"], "mcp_status_call");
    assert_eq!(json["stages"][4]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_missing_hook_entry() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_hook_settings_missing_stop(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_empty_hook_installation() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_hook_settings_empty(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "failed");
}

#[test]
fn claude_code_doctor_reports_matcher_only_hook_installation() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_hook_settings_matcher_only(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(out.status.code(), Some(69), "exit: {:?}", out.status);
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "failed");
}

#[test]
fn claude_code_doctor_proves_status_call_and_hook_presence() {
    let project = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let vault = synth_vault();

    write_project_mcp_config(project.path(), vault.path());
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["consumer"], "claude-code");
    assert_eq!(json["stages"][0]["name"], "mcp_config");
    assert_eq!(json["stages"][0]["status"], "ok");
    assert_eq!(json["stages"][1]["name"], "binary");
    assert_eq!(json["stages"][1]["status"], "ok");
    assert_eq!(json["stages"][2]["name"], "mcp_registration");
    assert_eq!(json["stages"][2]["status"], "ok");
    assert_eq!(json["stages"][3]["name"], "mcp_startup");
    assert_eq!(json["stages"][3]["status"], "ok");
    assert_eq!(json["stages"][4]["name"], "mcp_status_call");
    assert_eq!(json["stages"][4]["status"], "ok");
    assert_eq!(json["stages"][5]["name"], "hooks");
    assert_eq!(json["stages"][5]["status"], "ok");
}

#[test]
fn claude_code_doctor_accepts_wrapper_launch_and_preserves_cwd_vault_resolution() {
    let project = synth_vault_project();
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "wrapper-launch.sh",
        &format!(
            "#!/bin/bash\nexec \"{}\" \"$@\"\n",
            env!("CARGO_BIN_EXE_cairn")
        ),
    );

    write_custom_mcp_config(
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(project.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["stages"][2]["name"], "mcp_registration");
    assert_eq!(json["stages"][2]["status"], "ok");
}

#[test]
fn claude_code_doctor_uses_project_dir_for_cwd_based_wrapper_launches() {
    let project = synth_vault_project();
    let outside = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "wrapper-launch.sh",
        &format!(
            "#!/bin/bash\nexec \"{}\" \"$@\"\n",
            env!("CARGO_BIN_EXE_cairn")
        ),
    );

    write_custom_mcp_config(
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(outside.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], true);
    assert_eq!(json["stages"][3]["name"], "mcp_startup");
    assert_eq!(json["stages"][3]["status"], "ok");
    assert_eq!(json["stages"][4]["name"], "mcp_status_call");
    assert_eq!(json["stages"][4]["status"], "ok");
}

#[test]
fn claude_code_doctor_uses_local_project_scope_from_user_claude_json() {
    let project = synth_vault_project();
    let outside = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let script = write_executable_script(
        project.path(),
        "wrapper-launch.sh",
        &format!(
            "#!/bin/bash\nexec \"{}\" \"$@\"\n",
            env!("CARGO_BIN_EXE_cairn")
        ),
    );

    write_user_claude_config_local_project_scope(
        home.path(),
        project.path(),
        script.to_str().expect("utf-8 path"),
        &["mcp"],
    );
    write_hook_settings(project.path());

    let out = cli()
        .current_dir(outside.path())
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("utf-8 project dir"),
            "--home-dir",
            home.path().to_str().expect("utf-8 home dir"),
            "--json",
        ])
        .output()
        .expect("cairn doctor claude-code --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_stdout_json(out);
    assert_eq!(json["ok"], true);
    assert!(
        json["stages"][0]["detail"]
            .as_str()
            .expect("detail")
            .contains(".claude.json"),
        "expected local-project ~/.claude.json registration detail"
    );
}
