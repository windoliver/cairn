//! Operator-facing Nexus setup and diagnostics.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::config::{CairnConfig, NexusSandboxConfig, StoreKind};
use clap::ArgMatches;
use serde::Serialize;

use crate::nexus::{self, ProjectionStatusState};

/// Build the `cairn nexus` command tree.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("nexus")
        .about("Nexus sandbox setup and diagnostics")
        .subcommand_required(true)
        .arg_required_else_help(true)
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
    remediation: Vec<String>,
    message: &'static str,
}

#[derive(Debug, Serialize)]
struct RecommendedNexusConfig {
    command: String,
    args: Vec<String>,
    endpoint: String,
    health_path: String,
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

fn setup_receipt() -> NexusSetupReceipt {
    let detected_command = detect_nexusd().map(|path| path.display().to_string());
    let recommended = recommended_config(detected_command.as_deref());
    NexusSetupReceipt {
        status: "guidance",
        auto_install: false,
        detected_command,
        recommended,
        remediation: setup_remediation(),
        message: "No changes made. Install or select Nexus explicitly, then configure the sandbox profile.",
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
        setup_remediation()
    } else {
        vec![
            "Set `store.kind: nexus-sandbox` when you want Cairn to use a Nexus projection."
                .to_owned(),
            "Run `cairn nexus setup` before enabling the profile on a new machine.".to_owned(),
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

fn recommended_config(command: Option<&str>) -> RecommendedNexusConfig {
    let default = NexusSandboxConfig::default();
    RecommendedNexusConfig {
        command: command.unwrap_or(&default.command).to_owned(),
        args: default.args,
        endpoint: default.endpoint,
        health_path: default.health_path,
    }
}

fn setup_remediation() -> Vec<String> {
    vec![
        "Install Nexus so the `nexusd` daemon is available on PATH, or keep it at `~/nexus/.venv/bin/nexusd`.".to_owned(),
        "Set `store.kind: nexus-sandbox` and `store.nexus.command` to the detected `nexusd` path when it is not on PATH.".to_owned(),
        "Keep `{vault_dir}` and `{data_dir}` in `store.nexus.args`; Cairn expands them before launching the daemon.".to_owned(),
        "Do not point `store.nexus.command` at a generic `nexus` CLI unless that binary exposes the daemon health protocol.".to_owned(),
    ]
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
        format!("        command: {}", receipt.recommended.command),
        format!("        args: [{}]", shell_words(&receipt.recommended.args)),
        format!("        endpoint: {}", receipt.recommended.endpoint),
        format!("        health_path: {}", receipt.recommended.health_path),
        format!("  {}", receipt.message),
        "  next:".to_owned(),
    ];
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
