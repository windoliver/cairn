//! End-to-end tests for the operator-facing `cairn nexus` UX.

use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

fn write_config(dir: &std::path::Path, content: &str) {
    let config_dir = dir.join(".cairn");
    std::fs::create_dir_all(&config_dir).expect("create .cairn");
    std::fs::write(config_dir.join("vault.id"), "01HQZX9F5N0000000000000000")
        .expect("write vault.id");
    std::fs::write(config_dir.join("config.yaml"), content).expect("write config");
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod executable");
}

#[cfg(unix)]
fn write_fake_nexusd(home: &std::path::Path) -> std::path::PathBuf {
    let path = home.join("nexus/.venv/bin/nexusd");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create fake nexusd dir");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake nexusd");
    make_executable(&path);
    path
}

#[cfg(unix)]
fn write_fake_python314(bin: &std::path::Path) {
    let path = bin.join("python3.14");
    std::fs::create_dir_all(bin).expect("create fake python dir");
    std::fs::write(
        &path,
        r#"#!/bin/sh
if [ "$1" = "-m" ] && [ "$2" = "venv" ]; then
  venv="$3"
  mkdir -p "$venv/bin"
  cat > "$venv/bin/python" <<'PY'
#!/bin/sh
exit 0
PY
  chmod +x "$venv/bin/python"
  cat > "$venv/bin/nexusd" <<'NX'
#!/bin/sh
exit 0
NX
  chmod +x "$venv/bin/nexusd"
  exit 0
fi
exit 0
"#,
    )
    .expect("write fake python3.14");
    make_executable(&path);
}

#[cfg(unix)]
fn test_path_with_system_tools(bin: &std::path::Path) -> String {
    format!("{}:/usr/bin:/bin", bin.display())
}

#[test]
fn nexus_help_lists_setup_and_doctor() {
    let out = cli()
        .args(["nexus", "--help"])
        .output()
        .expect("cairn nexus --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("setup"), "missing setup: {stdout}");
    assert!(stdout.contains("doctor"), "missing doctor: {stdout}");
    assert!(stdout.contains("enable"), "missing enable: {stdout}");
}

#[test]
fn nexus_setup_json_is_guided_and_non_mutating_by_default() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = cli()
        .current_dir(dir.path())
        .args(["nexus", "setup", "--json"])
        .output()
        .expect("cairn nexus setup --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        out.status.success(),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["status"], "guidance");
    assert_eq!(v["auto_install"], false);
    assert_eq!(v["recommended"]["data_dir"], "nexus-data");
    assert!(
        v["recommended"]["command"]
            .as_str()
            .is_some_and(|command| command.ends_with("nexusd")),
        "recommended command should point at nexusd: {stdout}"
    );
    assert_eq!(v["recommended"]["health_timeout_ms"], 120_000);
    assert_eq!(v["recommended"]["shutdown_timeout_ms"], 2_000);
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|message| message.contains("No changes made")),
        "missing no-mutation message: {stdout}"
    );
    let install_steps = v["install_steps"].as_array().expect("install_steps array");
    assert!(
        install_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("nexus-ai-fs[sandbox]"))),
        "missing explicit install guidance: {stdout}"
    );
    assert!(
        install_steps.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("~/nexus/.venv"))),
        "missing ~/nexus venv guidance: {stdout}"
    );
    let remediation = v["remediation"].as_array().expect("remediation array");
    assert!(
        remediation
            .iter()
            .any(|step| step.as_str().is_some_and(|step| step.contains("nexusd"))),
        "missing nexusd remediation: {stdout}"
    );
    assert!(
        remediation.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("store.nexus.command"))),
        "missing config remediation: {stdout}"
    );
    assert!(
        !dir.path().join(".cairn").exists(),
        "setup guidance must not create vault config"
    );
}

#[test]
fn nexus_doctor_json_reports_degraded_with_setup_hint() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(
        dir.path(),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:0\n    health_timeout_ms: 25\n    shutdown_timeout_ms: 25\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .args(["nexus", "doctor", "--json"])
        .output()
        .expect("cairn nexus doctor --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert_eq!(
        out.status.code(),
        Some(69),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["status"], "degraded");
    assert_eq!(v["command"], "nexusd");
    assert_eq!(v["endpoint"], "http://127.0.0.1:0");
    assert!(
        v["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cairn nexus enable")
                && reason.contains("store.nexus.command")
                && reason.contains("nexusd")),
        "missing enable hint: {stdout}"
    );
    let remediation = v["remediation"].as_array().expect("remediation array");
    assert!(
        remediation.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.contains("Start the configured Nexus daemon"))),
        "missing configured-daemon remediation: {stdout}"
    );
    assert!(
        !remediation.iter().any(|step| step
            .as_str()
            .is_some_and(|step| step.starts_with("Install Nexus"))),
        "enabled profile should not lead with install remediation: {stdout}"
    );
    assert!(
        !dir.path().join("nexus-data").exists(),
        "doctor must not create projection directories"
    );
}

#[cfg(unix)]
#[test]
fn nexus_enable_json_writes_config_when_nexusd_is_detected() {
    let dir = tempfile::tempdir().expect("vault dir");
    let home = tempfile::tempdir().expect("home dir");
    let path_bin = tempfile::tempdir().expect("path dir");
    let nexusd = write_fake_nexusd(home.path());
    write_config(
        dir.path(),
        "vault:\n  name: keep-me\nstore:\n  kind: sqlite\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("PATH", path_bin.path())
        .args(["nexus", "enable", "--json"])
        .output()
        .expect("cairn nexus enable --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        out.status.success(),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let expected_config_path = std::fs::canonicalize(dir.path())
        .expect("canonical vault path")
        .join(".cairn/config.yaml");
    assert_eq!(receipt["status"], "enabled");
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["installed"], false);
    assert_eq!(receipt["detected_command"], nexusd.display().to_string());
    assert_eq!(
        receipt["config_path"],
        expected_config_path.display().to_string()
    );

    let config_raw =
        std::fs::read_to_string(dir.path().join(".cairn/config.yaml")).expect("read config");
    let config: serde_json::Value = yaml_serde::from_str(&config_raw).expect("parse config");
    assert_eq!(config["vault"]["name"], "keep-me");
    assert_eq!(config["store"]["kind"], "nexus-sandbox");
    assert_eq!(
        config["store"]["nexus"]["command"],
        nexusd.display().to_string()
    );
    assert_eq!(config["store"]["nexus"]["data_dir"], "nexus-data");
    assert_eq!(
        config["store"]["nexus"]["args"],
        serde_json::json!([
            "--profile",
            "sandbox",
            "--host",
            "127.0.0.1",
            "--port",
            "8765",
            "--workspace",
            "{vault_dir}",
            "--data-dir",
            "{data_dir}"
        ])
    );
}

#[cfg(unix)]
#[test]
fn nexus_enable_without_detected_nexusd_reports_install_hint_and_does_not_mutate() {
    let dir = tempfile::tempdir().expect("vault dir");
    let home = tempfile::tempdir().expect("home dir");
    let path_bin = tempfile::tempdir().expect("path dir");
    let original = "vault:\n  name: keep-me\nstore:\n  kind: sqlite\n";
    write_config(dir.path(), original);

    let out = cli()
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("PATH", path_bin.path())
        .args(["nexus", "enable", "--json"])
        .output()
        .expect("cairn nexus enable --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert_eq!(
        out.status.code(),
        Some(69),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(receipt["status"], "missing");
    assert_eq!(receipt["changed"], false);
    assert_eq!(receipt["installed"], false);
    assert!(
        receipt["message"]
            .as_str()
            .is_some_and(|message| message.contains("cairn nexus enable --install")),
        "missing --install hint: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".cairn/config.yaml")).expect("read config"),
        original
    );
}

#[cfg(unix)]
#[test]
fn nexus_enable_install_runs_explicit_installer_then_writes_config() {
    let dir = tempfile::tempdir().expect("vault dir");
    let home = tempfile::tempdir().expect("home dir");
    let path_bin = tempfile::tempdir().expect("path dir");
    write_fake_python314(path_bin.path());
    write_config(
        dir.path(),
        "vault:\n  name: keep-me\nstore:\n  kind: sqlite\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("PATH", test_path_with_system_tools(path_bin.path()))
        .args(["nexus", "enable", "--install", "--json"])
        .output()
        .expect("cairn nexus enable --install --json");

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        out.status.success(),
        "exit: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let expected_nexusd = home.path().join("nexus/.venv/bin/nexusd");
    assert_eq!(receipt["status"], "enabled");
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["installed"], true);
    assert_eq!(
        receipt["detected_command"],
        expected_nexusd.display().to_string()
    );
    assert!(expected_nexusd.is_file(), "installer did not create nexusd");

    let config_raw =
        std::fs::read_to_string(dir.path().join(".cairn/config.yaml")).expect("read config");
    let config: serde_json::Value = yaml_serde::from_str(&config_raw).expect("parse config");
    assert_eq!(config["store"]["kind"], "nexus-sandbox");
    assert_eq!(
        config["store"]["nexus"]["command"],
        expected_nexusd.display().to_string()
    );
}
