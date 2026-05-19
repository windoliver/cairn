//! Operator-facing Nexus setup and diagnostics.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};

use anyhow::{Context, Result};
use cairn_core::config::{CairnConfig, NexusSandboxConfig, StoreKind};
use clap::ArgMatches;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::nexus::{self, ProjectionStatusState};

/// Build the `cairn nexus` command tree.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("nexus")
        .about("Nexus sandbox setup and diagnostics")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("enable")
                .about("Enable the Nexus sandbox profile for the active vault")
                .arg(
                    clap::Arg::new("install")
                        .long("install")
                        .action(clap::ArgAction::SetTrue)
                        .help("Explicitly install Nexus into ~/nexus/.venv before enabling"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt"),
                ),
        )
        .subcommand(
            clap::Command::new("setup")
                .about("Find a compatible Nexus daemon and print explicit setup guidance")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON guidance"),
                ),
        )
        .subcommand(
            clap::Command::new("doctor")
                .about("Check the configured Nexus sandbox projection")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt"),
                ),
        )
}

/// Run `cairn nexus enable`.
#[must_use]
pub fn run_enable(matches: &ArgMatches, vault_path: &Path, config: &CairnConfig) -> ExitCode {
    let json = matches.get_flag("json");
    let install = matches.get_flag("install");
    let receipt = enable_receipt(vault_path, config, install);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).expect("Nexus enable receipt serializes")
        );
    } else {
        println!("{}", render_enable_human(&receipt));
    }
    match receipt.status {
        "enabled" => ExitCode::SUCCESS,
        "missing" | "install_failed" => ExitCode::from(69), // EX_UNAVAILABLE
        _ => ExitCode::from(1),
    }
}

/// Run `cairn nexus setup`.
#[must_use]
pub fn run_setup(matches: &ArgMatches) -> ExitCode {
    let json = matches.get_flag("json");
    let receipt = setup_receipt();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).expect("Nexus setup receipt serializes")
        );
    } else {
        println!("{}", render_setup_human(&receipt));
    }
    ExitCode::SUCCESS
}

/// Run `cairn nexus doctor`.
#[must_use]
pub fn run_doctor(matches: &ArgMatches, vault_path: &Path, config: &CairnConfig) -> ExitCode {
    let json = matches.get_flag("json");
    let receipt = doctor_receipt(vault_path, config);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).expect("Nexus doctor receipt serializes")
        );
    } else {
        println!("{}", render_doctor_human(&receipt));
    }
    match receipt.status {
        "healthy" | "disabled" => ExitCode::SUCCESS,
        "degraded" => ExitCode::from(69), // EX_UNAVAILABLE
        _ => ExitCode::from(1),
    }
}

#[derive(Debug, Serialize)]
struct NexusSetupReceipt {
    status: &'static str,
    auto_install: bool,
    detected_command: Option<String>,
    recommended: RecommendedNexusConfig,
    install_steps: Vec<String>,
    remediation: Vec<String>,
    message: &'static str,
}

#[derive(Debug, Serialize)]
struct NexusEnableReceipt {
    status: &'static str,
    changed: bool,
    installed: bool,
    detected_command: Option<String>,
    config_path: Option<String>,
    recommended: RecommendedNexusConfig,
    doctor: NexusDoctorSummary,
    install_steps: Vec<String>,
    remediation: Vec<String>,
    install_error: Option<String>,
    message: String,
}

#[derive(Debug, Serialize)]
struct RecommendedNexusConfig {
    data_dir: String,
    command: String,
    args: Vec<String>,
    endpoint: String,
    health_path: String,
    health_timeout_ms: u64,
    shutdown_timeout_ms: u64,
}

#[derive(Debug, Serialize)]
struct NexusDoctorSummary {
    status: &'static str,
    reason: Option<String>,
    endpoint: String,
    health_path: String,
    data_dir: Option<String>,
}

#[derive(Debug, Serialize)]
struct NexusDoctorReceipt {
    status: &'static str,
    command: String,
    args: Vec<String>,
    endpoint: String,
    health_path: String,
    data_dir: Option<String>,
    reason: Option<String>,
    remediation: Vec<String>,
    detected_command: Option<String>,
}

fn enable_receipt(vault_path: &Path, config: &CairnConfig, install: bool) -> NexusEnableReceipt {
    let mut installed = false;
    let mut install_error = None;
    let mut detected_command = detect_nexusd().map(|path| path.display().to_string());

    if detected_command.is_none() && install {
        match install_nexus() {
            Ok(()) => {
                installed = true;
                detected_command = detect_nexusd().map(|path| path.display().to_string());
                if detected_command.is_none() {
                    install_error = Some(
                        "installer completed but ~/nexus/.venv/bin/nexusd was not created"
                            .to_owned(),
                    );
                }
            }
            Err(err) => install_error = Some(format!("{err:#}")),
        }
    }

    let recommended = recommended_config(detected_command.as_deref());

    if let Some(error) = install_error.clone() {
        return NexusEnableReceipt {
            status: "install_failed",
            changed: false,
            installed,
            detected_command,
            config_path: None,
            recommended,
            doctor: doctor_summary(vault_path, config),
            install_steps: install_steps(),
            remediation: enable_remediation(),
            install_error: Some(error),
            message: "Nexus install did not complete; config was not changed.".to_owned(),
        };
    }

    if detected_command.is_none() {
        return NexusEnableReceipt {
            status: "missing",
            changed: false,
            installed: false,
            detected_command,
            config_path: None,
            recommended,
            doctor: doctor_summary(vault_path, config),
            install_steps: install_steps(),
            remediation: enable_remediation(),
            install_error: None,
            message: "No compatible `nexusd` found. Run `cairn nexus enable --install` to install Nexus into ~/nexus/.venv, or put `nexusd` on PATH.".to_owned(),
        };
    }

    match write_enabled_config(vault_path, &recommended) {
        Ok(config_path) => {
            let mut enabled_config = config.clone();
            enabled_config.store.kind = StoreKind::NexusSandbox;
            enabled_config.store.nexus = nexus_config_from_recommended(&recommended);
            NexusEnableReceipt {
                status: "enabled",
                changed: true,
                installed,
                detected_command,
                config_path: Some(config_path.display().to_string()),
                recommended,
                doctor: doctor_summary(vault_path, &enabled_config),
                install_steps: install_steps(),
                remediation: enabled_remediation(),
                install_error: None,
                message: "Nexus sandbox enabled in .cairn/config.yaml.".to_owned(),
            }
        }
        Err(err) => NexusEnableReceipt {
            status: "install_failed",
            changed: false,
            installed,
            detected_command,
            config_path: None,
            recommended,
            doctor: doctor_summary(vault_path, config),
            install_steps: install_steps(),
            remediation: enable_remediation(),
            install_error: Some(format!("{err:#}")),
            message: "Nexus sandbox could not be enabled; config was not changed.".to_owned(),
        },
    }
}

fn setup_receipt() -> NexusSetupReceipt {
    let detected_command = detect_nexusd().map(|path| path.display().to_string());
    let recommended = recommended_config(detected_command.as_deref());
    NexusSetupReceipt {
        status: "guidance",
        auto_install: false,
        detected_command,
        recommended,
        install_steps: install_steps(),
        remediation: setup_remediation(),
        message: "No changes made. Install or select Nexus explicitly, then configure the sandbox profile.",
    }
}

fn doctor_summary(vault_path: &Path, config: &CairnConfig) -> NexusDoctorSummary {
    let status = nexus::evaluate_projection_status(vault_path, config);
    let status_name = match status.state {
        ProjectionStatusState::Disabled => "disabled",
        ProjectionStatusState::Healthy => "healthy",
        ProjectionStatusState::Degraded => "degraded",
    };
    NexusDoctorSummary {
        status: status_name,
        reason: status.reason,
        endpoint: status
            .endpoint
            .unwrap_or_else(|| config.store.nexus.endpoint.clone()),
        health_path: config.store.nexus.health_path.clone(),
        data_dir: status.data_dir.map(|path| path.display().to_string()),
    }
}

fn doctor_receipt(vault_path: &Path, config: &CairnConfig) -> NexusDoctorReceipt {
    let status = nexus::evaluate_projection_status(vault_path, config);
    let detected_command = detect_nexusd().map(|path| path.display().to_string());
    let status_name = match status.state {
        ProjectionStatusState::Disabled => "disabled",
        ProjectionStatusState::Healthy => "healthy",
        ProjectionStatusState::Degraded => "degraded",
    };
    let remediation = if matches!(config.store.kind, StoreKind::NexusSandbox) {
        enabled_remediation()
    } else {
        vec![
            "Set `store.kind: nexus-sandbox` when you want Cairn to use a Nexus projection."
                .to_owned(),
            "Run `cairn nexus enable` to detect Nexus and write the profile automatically."
                .to_owned(),
        ]
    };
    NexusDoctorReceipt {
        status: status_name,
        command: config.store.nexus.command.clone(),
        args: config.store.nexus.args.clone(),
        endpoint: config.store.nexus.endpoint.clone(),
        health_path: config.store.nexus.health_path.clone(),
        data_dir: status.data_dir.map(|path| path.display().to_string()),
        reason: status.reason,
        remediation,
        detected_command,
    }
}

fn nexus_config_from_recommended(recommended: &RecommendedNexusConfig) -> NexusSandboxConfig {
    NexusSandboxConfig {
        data_dir: recommended.data_dir.clone(),
        command: recommended.command.clone(),
        args: recommended.args.clone(),
        endpoint: recommended.endpoint.clone(),
        health_path: recommended.health_path.clone(),
        health_timeout_ms: recommended.health_timeout_ms,
        shutdown_timeout_ms: recommended.shutdown_timeout_ms,
    }
}

fn recommended_config(command: Option<&str>) -> RecommendedNexusConfig {
    let default = NexusSandboxConfig::default();
    RecommendedNexusConfig {
        data_dir: default.data_dir,
        command: command.unwrap_or(&default.command).to_owned(),
        args: default.args,
        endpoint: default.endpoint,
        health_path: default.health_path,
        health_timeout_ms: default.health_timeout_ms,
        shutdown_timeout_ms: default.shutdown_timeout_ms,
    }
}

fn install_steps() -> Vec<String> {
    vec![
        "mkdir -p ~/nexus".to_owned(),
        "python3.14 -m venv ~/nexus/.venv".to_owned(),
        "~/nexus/.venv/bin/python -m pip install --upgrade pip".to_owned(),
        "~/nexus/.venv/bin/python -m pip install 'nexus-ai-fs[sandbox]'".to_owned(),
    ]
}

fn enable_remediation() -> Vec<String> {
    vec![
        "Run `cairn nexus enable --install` to install Nexus into ~/nexus/.venv and enable the sandbox profile.".to_owned(),
        "Or install Nexus yourself and rerun `cairn nexus enable` after `nexusd` is on PATH or at ~/nexus/.venv/bin/nexusd.".to_owned(),
        "Use `cairn nexus setup --json` for a read-only setup receipt.".to_owned(),
    ]
}

fn enabled_remediation() -> Vec<String> {
    vec![
        "Run `cairn nexus doctor` to inspect the configured projection.".to_owned(),
        "Start the configured Nexus daemon when `doctor.status` is degraded because the health endpoint is not reachable.".to_owned(),
        "Delete `nexus-data/` only when you want to rebuild the derived projection; .cairn/cairn.db remains authoritative.".to_owned(),
    ]
}

fn setup_remediation() -> Vec<String> {
    vec![
        "Install Nexus at `~/nexus/.venv/bin/nexusd` with the install steps above, or put a compatible `nexusd` on PATH.".to_owned(),
        "Set `store.kind: nexus-sandbox` and `store.nexus.command` to the detected `nexusd` path when it is not on PATH.".to_owned(),
        "Keep `{vault_dir}` and `{data_dir}` in `store.nexus.args`; Cairn expands them before launching the daemon.".to_owned(),
        "Do not point `store.nexus.command` at a generic `nexus` CLI unless that binary exposes the daemon health protocol.".to_owned(),
    ]
}

fn render_enable_human(receipt: &NexusEnableReceipt) -> String {
    let mut lines = vec![
        format!("cairn nexus enable: {}", receipt.status),
        format!("  changed: {}", receipt.changed),
        format!("  installed: {}", receipt.installed),
        format!(
            "  detected_command: {}",
            receipt.detected_command.as_deref().unwrap_or("(none)")
        ),
    ];
    if let Some(path) = &receipt.config_path {
        lines.push(format!("  config_path: {path}"));
    }
    if let Some(error) = &receipt.install_error {
        lines.push(format!("  install_error: {error}"));
    }
    lines.extend([
        "  recommended config:".to_owned(),
        "    store:".to_owned(),
        "      kind: nexus-sandbox".to_owned(),
        "      nexus:".to_owned(),
        format!("        data_dir: {}", receipt.recommended.data_dir),
        format!("        command: {}", receipt.recommended.command),
        format!("        args: [{}]", shell_words(&receipt.recommended.args)),
        format!("        endpoint: {}", receipt.recommended.endpoint),
        format!("        health_path: {}", receipt.recommended.health_path),
        format!(
            "        health_timeout_ms: {}",
            receipt.recommended.health_timeout_ms
        ),
        format!(
            "        shutdown_timeout_ms: {}",
            receipt.recommended.shutdown_timeout_ms
        ),
        "  doctor:".to_owned(),
        format!("    status: {}", receipt.doctor.status),
        format!("    endpoint: {}", receipt.doctor.endpoint),
        format!("    health_path: {}", receipt.doctor.health_path),
    ]);
    if let Some(data_dir) = &receipt.doctor.data_dir {
        lines.push(format!("    data_dir: {data_dir}"));
    }
    if let Some(reason) = &receipt.doctor.reason {
        lines.push(format!("    reason: {reason}"));
    }
    lines.push(format!("  {}", receipt.message));
    if receipt.status != "enabled" {
        lines.push("  install:".to_owned());
        lines.extend(
            receipt
                .install_steps
                .iter()
                .map(|step| format!("    $ {step}")),
        );
    }
    lines.push("  next:".to_owned());
    lines.extend(
        receipt
            .remediation
            .iter()
            .map(|step| format!("    - {step}")),
    );
    lines.join("\n")
}

fn render_setup_human(receipt: &NexusSetupReceipt) -> String {
    let mut lines = vec![
        "cairn nexus setup: guidance".to_owned(),
        format!("  auto_install: {}", receipt.auto_install),
        format!(
            "  detected_command: {}",
            receipt.detected_command.as_deref().unwrap_or("(none)")
        ),
        "  recommended config:".to_owned(),
        "    store:".to_owned(),
        "      kind: nexus-sandbox".to_owned(),
        "      nexus:".to_owned(),
        format!("        data_dir: {}", receipt.recommended.data_dir),
        format!("        command: {}", receipt.recommended.command),
        format!("        args: [{}]", shell_words(&receipt.recommended.args)),
        format!("        endpoint: {}", receipt.recommended.endpoint),
        format!("        health_path: {}", receipt.recommended.health_path),
        format!(
            "        health_timeout_ms: {}",
            receipt.recommended.health_timeout_ms
        ),
        format!(
            "        shutdown_timeout_ms: {}",
            receipt.recommended.shutdown_timeout_ms
        ),
        format!("  {}", receipt.message),
        "  install:".to_owned(),
    ];
    lines.extend(
        receipt
            .install_steps
            .iter()
            .map(|step| format!("    $ {step}")),
    );
    lines.extend(["  next:".to_owned()]);
    lines.extend(
        receipt
            .remediation
            .iter()
            .map(|step| format!("    - {step}")),
    );
    lines.join("\n")
}

fn render_doctor_human(receipt: &NexusDoctorReceipt) -> String {
    let mut lines = vec![
        format!("cairn nexus doctor: {}", receipt.status),
        format!("  command: {}", receipt.command),
        format!("  args: [{}]", shell_words(&receipt.args)),
        format!("  endpoint: {}", receipt.endpoint),
        format!("  health_path: {}", receipt.health_path),
    ];
    if let Some(data_dir) = &receipt.data_dir {
        lines.push(format!("  data_dir: {data_dir}"));
    }
    if let Some(reason) = &receipt.reason {
        lines.push(format!("  reason: {reason}"));
    }
    if let Some(detected) = &receipt.detected_command {
        lines.push(format!("  detected_command: {detected}"));
    }
    lines.push("  next:".to_owned());
    lines.extend(
        receipt
            .remediation
            .iter()
            .map(|step| format!("    - {step}")),
    );
    lines.join("\n")
}

fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn install_nexus() -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME is required to install Nexus")?;
    let nexus_root = PathBuf::from(home).join("nexus");
    let venv = nexus_root.join(".venv");
    let python = venv.join("bin/python");

    fs::create_dir_all(&nexus_root)
        .with_context(|| format!("creating {}", nexus_root.display()))?;
    run_install_command(
        ProcessCommand::new("python3.14")
            .arg("-m")
            .arg("venv")
            .arg(&venv),
        "creating Nexus virtualenv with python3.14",
    )?;
    run_install_command(
        ProcessCommand::new(&python)
            .arg("-m")
            .arg("pip")
            .arg("install")
            .arg("--upgrade")
            .arg("pip"),
        "upgrading pip in Nexus virtualenv",
    )?;
    run_install_command(
        ProcessCommand::new(&python)
            .arg("-m")
            .arg("pip")
            .arg("install")
            .arg("nexus-ai-fs[sandbox]"),
        "installing nexus-ai-fs[sandbox]",
    )?;
    Ok(())
}

fn run_install_command(command: &mut ProcessCommand, label: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("{label}: spawning installer command"))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
        "{label}: command exited with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    );
}

fn write_enabled_config(
    vault_path: &Path,
    recommended: &RecommendedNexusConfig,
) -> Result<PathBuf> {
    let config_dir = vault_path.join(".cairn");
    let config_path = config_dir.join("config.yaml");
    let tmp_path = config_dir.join("config.yaml.tmp");
    let mut root = if config_path.exists() {
        let raw = fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        let value: Value = yaml_serde::from_str(&raw)
            .with_context(|| format!("parsing {}", config_path.display()))?;
        if value.is_null() {
            Value::Object(Map::new())
        } else {
            value
        }
    } else {
        Value::Object(Map::new())
    };

    let root_map = root.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "{} must contain a YAML mapping before `cairn nexus enable` can update it",
            config_path.display()
        )
    })?;
    let store = root_map
        .entry("store".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !store.is_object() {
        *store = Value::Object(Map::new());
    }
    let store_map = store
        .as_object_mut()
        .expect("store was forced to an object above");
    store_map.insert("kind".to_owned(), Value::String("nexus-sandbox".to_owned()));
    store_map.insert(
        "nexus".to_owned(),
        serde_json::to_value(nexus_config_from_recommended(recommended))
            .context("serializing Nexus config")?,
    );

    fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;
    let yaml = yaml_serde::to_string(&root).context("serializing config to YAML")?;
    fs::write(&tmp_path, yaml).with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &config_path).with_context(|| {
        format!(
            "replacing {} with {}",
            config_path.display(),
            tmp_path.display()
        )
    })?;
    Ok(config_path)
}

fn detect_nexusd() -> Option<PathBuf> {
    candidate_nexusd_paths().into_iter().find(|path| {
        is_executable_file(path)
            && path.file_name().and_then(|name| name.to_str()) == Some("nexusd")
    })
}

fn candidate_nexusd_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        push_candidate(
            &mut candidates,
            PathBuf::from(home).join("nexus/.venv/bin/nexusd"),
        );
    }
    if let Some(path) = find_on_path("nexusd") {
        push_candidate(&mut candidates, path);
    }
    candidates
}

fn push_candidate(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.iter().any(|existing| existing == &path) {
        candidates.push(path);
    }
}

fn find_on_path(command: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(command))
        .find(|path| path.is_file())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
