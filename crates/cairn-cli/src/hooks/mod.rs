//! CLI dispatcher and shared types for `cairn hook <name>`.
//!
//! The store-backed per-hook behavior lands in later tasks. This module owns
//! the stable command surface and shared result/error envelope.

use std::io::{IsTerminal as _, Read as _};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::domain::{BudgetObservation, LocalSensorName, SensorGateReason, SourceFamily};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::errors::ErrorCode;
use clap::ArgMatches;
use serde::Serialize;
use serde_json::Value;

use crate::sensor_gate::{
    SensorDropBudgetMetric, SensorDropMetric, SensorGateStage, append_sensor_drop_metric,
    latest_sensor_consent_for_vault, safe_metric_ref,
};
use crate::verbs::envelope::{emit_json, new_operation_id};

mod artifact;
mod post_tool_use;
mod pre_tool_use;
mod queue;
mod session_start;
mod stop;
mod user_prompt_submit;

/// Canonical v0.1 harness lifecycle hook names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookName {
    /// Startup or resume hook that assembles hot memory.
    SessionStart,
    /// User prompt hook that captures the submitted message.
    UserPromptSubmit,
    /// Tool preflight hook that records planned tool execution.
    PreToolUse,
    /// Tool completion hook that records tool results.
    PostToolUse,
    /// Session or turn boundary hook that schedules post-turn work.
    Stop,
}

impl HookName {
    /// Canonical wire names accepted by the CLI.
    pub const ALL: [&'static str; 5] = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ];

    /// Return the canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }

    fn parse(value: &str) -> Result<Self, HookError> {
        match value {
            "SessionStart" => Ok(Self::SessionStart),
            "UserPromptSubmit" => Ok(Self::UserPromptSubmit),
            "PreToolUse" => Ok(Self::PreToolUse),
            "PostToolUse" => Ok(Self::PostToolUse),
            "Stop" => Ok(Self::Stop),
            other => Err(HookError::invalid_args(format!(
                "unknown hook `{other}`; expected one of {}",
                Self::ALL.join(", ")
            ))),
        }
    }
}

/// Machine-readable result emitted by `cairn hook --json`.
#[derive(Debug, Serialize)]
pub struct HookResult {
    /// Whether the hook completed its synchronous boundary.
    pub ok: bool,
    /// Hook that was executed.
    pub hook: String,
    /// Operation identifier for retry and support correlation.
    pub operation_id: Ulid,
    /// Artifacts produced by a successful hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<HookArtifacts>,
    /// Routing hints produced by prompt-submission hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_hints: Option<HookRoutingHints>,
    /// Typed error details on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<HookErrorBody>,
}

/// Lightweight synchronous routing hints for harness integrations.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize)]
pub struct HookRoutingHints {
    /// Prompt was durably captured as a trace event.
    pub capture_prompt: bool,
    /// Prompt appears to ask Cairn to write or retain memory.
    pub memory_write_suggested: bool,
    /// Prompt appears to ask Cairn to forget existing memory.
    pub forget_suggested: bool,
    /// Prompt appears to benefit from memory search or recall.
    pub search_suggested: bool,
}

/// Artifact identifiers produced by hook execution.
#[derive(Debug, Default, Serialize)]
pub struct HookArtifacts {
    /// Trace artifact id, when the hook records a trace event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<Ulid>,
    /// Vault-relative hot-memory artifact path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hot_path: Option<String>,
    /// Post-turn work request ids enqueued by the hook.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queued_jobs: Vec<Ulid>,
}

impl HookArtifacts {
    fn is_empty(&self) -> bool {
        self.trace_id.is_none() && self.hot_path.is_none() && self.queued_jobs.is_empty()
    }
}

/// Typed hook error body emitted in JSON and human output.
#[derive(Debug, Serialize)]
pub struct HookErrorBody {
    /// Stable Cairn error code.
    pub code: &'static str,
    /// Human-readable error message.
    pub message: String,
    /// Concrete retry guidance for harness integrations.
    pub retry_guidance: String,
}

/// Internal hook error before it is attached to a result envelope.
#[derive(Debug)]
pub struct HookError {
    code: ErrorCode,
    message: String,
    retry_guidance: String,
}

impl HookError {
    /// Build an invalid-arguments hook error.
    #[must_use]
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidArgs,
            message: message.into(),
            retry_guidance: "fix the hook payload or hook name and retry the same command"
                .to_owned(),
        }
    }

    /// Build an internal hook error with explicit retry guidance.
    #[must_use]
    pub fn internal(message: impl Into<String>, retry_guidance: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
            retry_guidance: retry_guidance.into(),
        }
    }

    /// Build an unauthorized hook error.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Unauthorized,
            message: message.into(),
            retry_guidance: "enable the hook sensor with `cairn sensor enable hook --reason <code>` and retry the same command"
                .to_owned(),
        }
    }

    fn with_retry_guidance(self, retry_guidance: impl Into<String>) -> Self {
        Self {
            retry_guidance: retry_guidance.into(),
            ..self
        }
    }

    fn into_body(self) -> HookErrorBody {
        HookErrorBody {
            code: self.code.as_str(),
            message: self.message,
            retry_guidance: self.retry_guidance,
        }
    }
}

/// Build the `cairn hook` clap command.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("hook")
        .about("Run a Cairn harness lifecycle hook")
        .arg(
            clap::Arg::new("name")
                .help("Hook name: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop")
                .required(true)
                .value_name("NAME")
                .value_parser(clap::builder::NonEmptyStringValueParser::new()),
        )
        .arg(
            clap::Arg::new("payload")
                .long("payload")
                .value_name("JSON")
                .help("Hook payload JSON object"),
        )
        .arg(
            clap::Arg::new("payload-file")
                .long("payload-file")
                .value_name("PATH")
                .value_parser(clap::builder::PathBufValueParser::new())
                .conflicts_with("payload")
                .help("Read hook payload JSON object from a file"),
        )
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .value_parser(clap::builder::PathBufValueParser::new())
                .help("Vault root directory used for hook artifacts"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON instead of human-readable output"),
        )
}

/// Run a parsed `cairn hook` invocation.
#[must_use]
pub fn run(matches: &ArgMatches) -> ExitCode {
    let json = matches.get_flag("json");
    let operation_id = new_operation_id();
    let hook = match matches.get_one::<String>("name").map(String::as_str) {
        Some(name) => match HookName::parse(name) {
            Ok(hook) => hook,
            Err(err) => return emit_failure(name, operation_id, err, json, 64),
        },
        None => {
            return emit_failure(
                "unknown",
                operation_id,
                HookError::invalid_args("hook name is required"),
                json,
                64,
            );
        }
    };

    let payload = match load_payload(matches) {
        Ok(payload) => payload,
        Err(err) => return emit_failure(hook.as_str(), operation_id, err, json, 1),
    };
    let vault_path = matches
        .get_one::<PathBuf>("vault-path")
        .map_or_else(|| PathBuf::from("."), Clone::clone);
    match enforce_hook_sensor_gate(&vault_path, &operation_id, &payload) {
        Ok(()) => {}
        Err(HookGateOutcome::Denied(reason)) => {
            return emit_failure(
                hook.as_str(),
                operation_id,
                HookError::unauthorized(format!("hook sensor denied capture: {}", reason.as_str())),
                json,
                77,
            );
        }
        Err(HookGateOutcome::Error(err)) => {
            return emit_failure(hook.as_str(), operation_id, err, json, 1);
        }
    }
    let routing_hints = if hook == HookName::UserPromptSubmit {
        Some(user_prompt_submit::routing_hints(&payload))
    } else {
        None
    };

    let outcome = match hook {
        HookName::SessionStart => session_start::run(&vault_path, operation_id.clone(), &payload),
        HookName::UserPromptSubmit => {
            user_prompt_submit::run(&vault_path, operation_id.clone(), &payload)
        }
        HookName::PreToolUse => pre_tool_use::run(&vault_path, operation_id.clone(), &payload),
        HookName::PostToolUse => post_tool_use::run(&vault_path, operation_id.clone(), &payload),
        HookName::Stop => stop::run(&vault_path, operation_id.clone(), &payload),
    };

    match outcome {
        Ok(artifacts) => emit_success(hook, operation_id, artifacts, routing_hints, json),
        Err(err) => emit_failure(hook.as_str(), operation_id, err, json, 1),
    }
}

fn load_payload(matches: &ArgMatches) -> Result<Value, HookError> {
    let value = if let Some(raw) = matches.get_one::<String>("payload") {
        serde_json::from_str(raw)
            .map_err(|err| HookError::invalid_args(format!("payload must be valid JSON: {err}")))?
    } else if let Some(path) = matches.get_one::<PathBuf>("payload-file") {
        let raw = if path == std::path::Path::new("-") {
            std::io::read_to_string(std::io::stdin()).map_err(|err| {
                HookError::internal(
                    format!("failed to read hook payload from stdin: {err}"),
                    "retry the same hook command with a JSON object on stdin",
                )
            })?
        } else {
            std::fs::read_to_string(path).map_err(|err| {
                HookError::internal(
                    format!("failed to read payload file `{}`: {err}", path.display()),
                    "restore access to the payload file and retry the same hook command",
                )
            })?
        };
        serde_json::from_str(&raw).map_err(|err| {
            HookError::invalid_args(format!("payload file must contain valid JSON: {err}"))
        })?
    } else {
        read_stdin_payload()?
    };

    if value.is_object() {
        Ok(value)
    } else {
        Err(HookError::invalid_args(
            "hook payload must be a JSON object",
        ))
    }
}

fn read_stdin_payload() -> Result<Value, HookError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(serde_json::json!({}));
    }

    let mut raw = String::new();
    stdin.lock().read_to_string(&mut raw).map_err(|err| {
        HookError::internal(
            format!("failed to read hook payload from stdin: {err}"),
            "retry the same hook command with a readable JSON payload on stdin",
        )
    })?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&raw)
        .map_err(|err| HookError::invalid_args(format!("stdin payload must be valid JSON: {err}")))
}

enum HookGateOutcome {
    Denied(SensorGateReason),
    Error(HookError),
}

fn enforce_hook_sensor_gate(
    vault_path: &std::path::Path,
    operation_id: &Ulid,
    payload: &Value,
) -> Result<(), HookGateOutcome> {
    if !vault_path.join(".cairn").join("vault.id").exists() {
        return Ok(());
    }
    let config = crate::config::load(vault_path, &crate::config::CliOverrides::default()).map_err(
        |error| {
            HookGateOutcome::Error(HookError::internal(
                format!("failed to load hook sensor config: {error:#}"),
                "repair .cairn/config.yaml and retry the same hook command",
            ))
        },
    )?;
    let consent = block_on(latest_sensor_consent_for_vault(
        vault_path,
        LocalSensorName::Hook,
    ))
    .map_err(|error| {
        HookGateOutcome::Error(HookError::internal(
            format!("failed to load hook sensor consent: {error:#}"),
            "repair the consent journal and retry the same hook command",
        ))
    })?;
    let bytes =
        serde_json::to_vec(payload).map_or(0, |body| u64::try_from(body.len()).unwrap_or(u64::MAX));
    let observation = BudgetObservation { items: 1, bytes };
    match crate::sensor_gate::evaluate_sensor_gate(
        &config,
        consent,
        LocalSensorName::Hook,
        observation,
    ) {
        Ok(()) => Ok(()),
        Err(reason) => {
            let budget = (reason == SensorGateReason::BudgetExceeded)
                .then(|| {
                    crate::sensor_gate::sensor_budget(&config, LocalSensorName::Hook).map(
                        |budget| SensorDropBudgetMetric {
                            max_items: budget.max_items,
                            max_bytes: budget.max_bytes,
                            observed_items: observation.items,
                            observed_bytes: observation.bytes,
                        },
                    )
                })
                .flatten();
            let metric = SensorDropMetric {
                event: crate::sensor_gate::SENSOR_DROP_EVENT,
                sensor: LocalSensorName::Hook,
                source_family: Some(SourceFamily::Hook),
                reason,
                stage: SensorGateStage::PreArtifact,
                operation_id: Some(operation_id.0.clone()),
                session_id: safe_metric_ref(payload.get("session_id").and_then(Value::as_str)),
                turn_id: None,
                budget,
            };
            append_sensor_drop_metric(vault_path, &metric).map_err(|error| {
                HookGateOutcome::Error(HookError::internal(
                    format!("failed to write hook sensor drop metric: {error:#}"),
                    "repair .cairn/metrics.jsonl and retry the same hook command",
                ))
            })?;
            Err(HookGateOutcome::Denied(reason))
        }
    }
}

fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build async runtime")?
        .block_on(future)
}

/// Read a required non-empty string field from a hook payload.
pub fn require_string(payload: &Value, field: &'static str) -> Result<String, HookError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            HookError::invalid_args(format!("payload.{field} must be a non-empty string"))
        })
}

/// Read the first present non-empty string field from a hook payload.
pub fn optional_string(payload: &Value, field: &'static str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// Read the first available required non-empty string field from a hook payload.
pub fn require_any_string(
    payload: &Value,
    fields: &[&'static str],
    canonical_field: &'static str,
) -> Result<String, HookError> {
    fields
        .iter()
        .find_map(|field| {
            payload
                .get(*field)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| {
            HookError::invalid_args(format!(
                "payload.{canonical_field} must be a non-empty string"
            ))
        })
}

/// Clone the payload object for artifact storage.
#[must_use]
pub fn payload_object(payload: &Value) -> serde_json::Map<String, Value> {
    payload.as_object().cloned().unwrap_or_default()
}

fn emit_success(
    hook: HookName,
    operation_id: Ulid,
    artifacts: HookArtifacts,
    routing_hints: Option<HookRoutingHints>,
    json: bool,
) -> ExitCode {
    let artifacts = if artifacts.is_empty() {
        None
    } else {
        Some(artifacts)
    };
    let result = HookResult {
        ok: true,
        hook: hook.as_str().to_owned(),
        operation_id,
        artifacts,
        routing_hints,
        error: None,
    };
    if json {
        emit_json(&result);
    } else {
        println!(
            "cairn hook {}: ok (operation_id: {})",
            hook.as_str(),
            result.operation_id.0
        );
    }
    ExitCode::SUCCESS
}

fn emit_failure(hook: &str, operation_id: Ulid, err: HookError, json: bool, code: u8) -> ExitCode {
    let result = HookResult {
        ok: false,
        hook: hook.to_owned(),
        operation_id,
        artifacts: None,
        routing_hints: None,
        error: Some(err.into_body()),
    };
    if json {
        emit_json(&result);
    } else if let Some(error) = &result.error {
        eprintln!(
            "cairn hook {}: {} - {} (operation_id: {}; retry: {})",
            hook, error.code, error.message, result.operation_id.0, error.retry_guidance
        );
    }
    ExitCode::from(code)
}
