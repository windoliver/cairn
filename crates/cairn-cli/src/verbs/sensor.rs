//! Local sensor consent and policy management commands.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, anyhow};
use cairn_core::config::{CairnConfig, SensorCaptureBudget, SensorRetentionConfig};
use cairn_core::domain::{
    BudgetObservation, ConsentEvent, ConsentKind, ConsentPayload, Identity, LocalSensorName,
    Rfc3339Timestamp, SensorGateReason, SensorLabel,
};
use clap::{Arg, ArgAction, ArgMatches};
use serde::Serialize;

use crate::sensor_gate::{self, SensorConsentState};

use super::envelope::{emit_json, new_operation_id};

/// Build the `cairn sensor` subcommand.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("sensor")
        .about("Manage local sensor consent and policy gates")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(toggle_command("enable", "Enable a local sensor"))
        .subcommand(toggle_command("disable", "Disable a local sensor"))
        .subcommand(
            clap::Command::new("status")
                .about("Show local sensor gate status")
                .arg(sensor_arg().required(false))
                .arg(json_arg()),
        )
}

/// Run a `cairn sensor` subcommand.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path, config: CairnConfig) -> ExitCode {
    let outcome = match sub.subcommand() {
        Some(("enable", enable)) => run_toggle(enable, vault_root, config, true),
        Some(("disable", disable)) => run_toggle(disable, vault_root, config, false),
        Some(("status", status)) => run_status(status, vault_root, &config),
        _ => unreachable!("clap subcommand_required(true) ensures a subcommand is present"),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let json = sub
                .subcommand()
                .is_some_and(|(_, matches)| matches.get_flag("json"));
            if json {
                emit_json(&serde_json::json!({
                    "error": {
                        "code": "sensor_command_failed",
                        "message": format!("{error:#}"),
                    },
                }));
            } else {
                eprintln!("cairn sensor: {error:#}");
            }
            ExitCode::from(70)
        }
    }
}

fn toggle_command(name: &'static str, about: &'static str) -> clap::Command {
    clap::Command::new(name)
        .about(about)
        .arg(sensor_arg().required(true))
        .arg(
            Arg::new("reason")
                .long("reason")
                .value_name("CODE")
                .required(true)
                .value_parser(clap::builder::NonEmptyStringValueParser::new())
                .help("Body-free consent reason code, e.g. operator_on or operator_off"),
        )
        .arg(
            Arg::new("actor")
                .long("actor")
                .value_name("IDENTITY")
                .default_value("hmn:local-operator")
                .help("Human or agent identity writing the consent event"),
        )
        .arg(json_arg())
}

fn sensor_arg() -> Arg {
    Arg::new("sensor")
        .value_name("SENSOR")
        .help("Local sensor: hook, ide, terminal, clipboard, voice, screen, or recording")
}

fn json_arg() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Emit JSON output")
}

fn run_toggle(
    sub: &ArgMatches,
    vault_root: &Path,
    mut config: CairnConfig,
    enabled: bool,
) -> anyhow::Result<()> {
    let json = sub.get_flag("json");
    let sensor = parse_sensor(sub)?;
    let reason = sub
        .get_one::<String>("reason")
        .expect("clap requires --reason");
    let actor = sub
        .get_one::<String>("actor")
        .expect("clap supplies default actor");

    set_sensor_enabled(&mut config, sensor, enabled);
    config
        .validate()
        .map_err(anyhow::Error::from)
        .context("validating updated sensor config")?;

    let event = sensor_consent_event(sensor, enabled, reason, actor)?;
    block_on(append_consent_event(vault_root, event))?;
    write_config_atomic(vault_root, &config)?;

    let receipt = ToggleReceipt {
        status: if enabled { "enabled" } else { "disabled" },
        sensor: sensor.as_str(),
        consent: if enabled {
            SensorConsentState::Enabled
        } else {
            SensorConsentState::Disabled
        },
    };
    if json {
        emit_json(&receipt);
    } else {
        println!("{} {}", receipt.sensor, receipt.status);
    }
    Ok(())
}

fn run_status(sub: &ArgMatches, vault_root: &Path, config: &CairnConfig) -> anyhow::Result<()> {
    let json = sub.get_flag("json");
    if let Some(raw) = sub.get_one::<String>("sensor") {
        let sensor = parse_sensor_name(raw)?;
        let row = block_on(sensor_status_row(vault_root, config, sensor))?;
        if json {
            emit_json(&row);
        } else {
            print_status_row(&row);
        }
    } else {
        let rows = block_on(sensor_status_rows(vault_root, config))?;
        if json {
            emit_json(&serde_json::json!({ "sensors": rows }));
        } else {
            for row in &rows {
                print_status_row(row);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ToggleReceipt {
    status: &'static str,
    sensor: &'static str,
    consent: SensorConsentState,
}

#[derive(Debug, Serialize)]
struct SensorStatusRow {
    sensor: &'static str,
    enabled: bool,
    consent: SensorConsentState,
    gate: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<SensorGateReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget: Option<SensorBudgetStatus>,
    retention: SensorRetentionStatus,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SensorBudgetStatus {
    Event {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_items: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },
    Screen {
        max_frames_per_minute: u32,
        max_text_bytes_per_event: u32,
    },
}

#[derive(Debug, Serialize)]
struct SensorRetentionStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_days: Option<u32>,
}

async fn sensor_status_rows(
    vault_root: &Path,
    config: &CairnConfig,
) -> anyhow::Result<Vec<SensorStatusRow>> {
    let store = open_store(vault_root).await?;
    let mut rows = Vec::with_capacity(LocalSensorName::ALL.len());
    for sensor in LocalSensorName::ALL {
        rows.push(sensor_status_row_with_store(&store, config, sensor).await?);
    }
    Ok(rows)
}

async fn sensor_status_row(
    vault_root: &Path,
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> anyhow::Result<SensorStatusRow> {
    let store = open_store(vault_root).await?;
    sensor_status_row_with_store(&store, config, sensor).await
}

async fn sensor_status_row_with_store(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> anyhow::Result<SensorStatusRow> {
    let enabled = sensor_gate::sensor_enabled(config, sensor);
    let consent = sensor_gate::latest_sensor_consent(store, sensor).await?;
    let gate_result = sensor_gate::evaluate_sensor_gate(
        config,
        consent,
        sensor,
        BudgetObservation { items: 0, bytes: 0 },
    );
    let reason = gate_result.err();
    Ok(SensorStatusRow {
        sensor: sensor.as_str(),
        enabled,
        consent,
        gate: if reason.is_some() {
            "denied"
        } else {
            "allowed"
        },
        reason,
        budget: sensor_budget_status(config, sensor),
        retention: SensorRetentionStatus {
            max_days: sensor_retention(config, sensor).max_days,
        },
    })
}

async fn append_consent_event(vault_root: &Path, event: ConsentEvent) -> anyhow::Result<()> {
    let store = open_store(vault_root).await?;
    let conn = store
        .raw_conn_for_admin()
        .cloned()
        .context("sqlite store has no connection")?;
    let cairn_dir = vault_root.join(".cairn");
    conn.call(move |c| {
        cairn_store_sqlite::consent::append(c, &event)
            .map_err(|error| tokio_rusqlite::Error::Other(Box::new(error)))?;
        let mut materializer = cairn_workflows::ConsentLogMaterializer::open(&cairn_dir)
            .map_err(|error| tokio_rusqlite::Error::Other(Box::new(error)))?;
        materializer
            .tick(c)
            .map_err(|error| tokio_rusqlite::Error::Other(Box::new(error)))?;
        Ok(())
    })
    .await
    .context("append sensor consent event")?;
    Ok(())
}

async fn open_store(vault_root: &Path) -> anyhow::Result<cairn_store_sqlite::SqliteMemoryStore> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    cairn_store_sqlite::open(&db_path)
        .await
        .with_context(|| format!("open {}", db_path.display()))
}

fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build async runtime")?
        .block_on(future)
}

fn sensor_consent_event(
    sensor: LocalSensorName,
    enabled: bool,
    reason: &str,
    actor: &str,
) -> anyhow::Result<ConsentEvent> {
    let label = SensorLabel::parse(sensor.family_label().to_owned())
        .with_context(|| format!("parse sensor label {}", sensor.family_label()))?;
    let kind = if enabled {
        ConsentKind::SensorEnable
    } else {
        ConsentKind::SensorDisable
    };
    let event = ConsentEvent {
        consent_id: new_operation_id().0,
        kind,
        actor: Identity::parse(actor.to_owned())
            .with_context(|| format!("parse actor identity {actor}"))?,
        subject: format!("snr:{}", label.as_str()),
        scope: "global".to_owned(),
        op_id: None,
        sensor_id: Some(label.clone()),
        payload: ConsentPayload::SensorToggle {
            sensor_label: label,
            reason_code: reason.to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse(cairn_core::time::now_rfc3339_seconds())
            .context("parse current timestamp")?,
        expires_at: None,
    };
    event
        .validate()
        .context("validating sensor consent event")?;
    Ok(event)
}

fn parse_sensor(sub: &ArgMatches) -> anyhow::Result<LocalSensorName> {
    let raw = sub
        .get_one::<String>("sensor")
        .expect("clap requires sensor argument");
    parse_sensor_name(raw)
}

fn parse_sensor_name(raw: &str) -> anyhow::Result<LocalSensorName> {
    LocalSensorName::parse(raw).ok_or_else(|| anyhow!("unknown local sensor `{raw}`"))
}

fn set_sensor_enabled(config: &mut CairnConfig, sensor: LocalSensorName, enabled: bool) {
    match sensor {
        LocalSensorName::Hook => config.sensors.hooks.enabled = enabled,
        LocalSensorName::Ide => config.sensors.ide.enabled = enabled,
        LocalSensorName::Terminal => config.sensors.terminal.enabled = enabled,
        LocalSensorName::Clipboard => config.sensors.clipboard.enabled = enabled,
        LocalSensorName::Voice => config.sensors.voice.enabled = enabled,
        LocalSensorName::Screen => config.sensors.screen.enabled = enabled,
        LocalSensorName::Recording => config.sensors.recording.enabled = enabled,
    }
}

fn sensor_budget_status(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> Option<SensorBudgetStatus> {
    match sensor {
        LocalSensorName::Screen => Some(SensorBudgetStatus::Screen {
            max_frames_per_minute: config.sensors.screen.budget.max_frames_per_minute,
            max_text_bytes_per_event: config.sensors.screen.budget.max_text_bytes_per_event,
        }),
        _ => sensor_event_budget(config, sensor).map(|budget| SensorBudgetStatus::Event {
            max_items: budget.max_items,
            max_bytes: budget.max_bytes,
        }),
    }
}

fn sensor_event_budget(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> Option<&SensorCaptureBudget> {
    match sensor {
        LocalSensorName::Hook => Some(&config.sensors.hooks.budget),
        LocalSensorName::Ide => Some(&config.sensors.ide.budget),
        LocalSensorName::Terminal => Some(&config.sensors.terminal.budget),
        LocalSensorName::Clipboard => Some(&config.sensors.clipboard.budget),
        LocalSensorName::Voice => Some(&config.sensors.voice.budget),
        LocalSensorName::Screen => None,
        LocalSensorName::Recording => Some(&config.sensors.recording.budget),
    }
}

fn sensor_retention(config: &CairnConfig, sensor: LocalSensorName) -> &SensorRetentionConfig {
    match sensor {
        LocalSensorName::Hook => &config.sensors.hooks.retention,
        LocalSensorName::Ide => &config.sensors.ide.retention,
        LocalSensorName::Terminal => &config.sensors.terminal.retention,
        LocalSensorName::Clipboard => &config.sensors.clipboard.retention,
        LocalSensorName::Voice => &config.sensors.voice.retention,
        LocalSensorName::Screen => &config.sensors.screen.retention,
        LocalSensorName::Recording => &config.sensors.recording.retention,
    }
}

fn write_config_atomic(vault_root: &Path, config: &CairnConfig) -> anyhow::Result<()> {
    let config_dir = vault_root.join(".cairn");
    let config_path = config_dir.join("config.yaml");
    let tmp_path = config_dir.join("config.yaml.tmp");
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("create {}", config_dir.display()))?;
    let yaml = yaml_serde::to_string(config).context("serialize config to YAML")?;
    std::fs::write(&tmp_path, yaml).with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &config_path).with_context(|| {
        format!(
            "replace {} with {}",
            config_path.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}

fn print_status_row(row: &SensorStatusRow) {
    match row.reason {
        Some(reason) => println!(
            "{} enabled={} consent={:?} gate={} reason={:?}",
            row.sensor, row.enabled, row.consent, row.gate, reason
        ),
        None => println!(
            "{} enabled={} consent={:?} gate={}",
            row.sensor, row.enabled, row.consent, row.gate
        ),
    }
}
