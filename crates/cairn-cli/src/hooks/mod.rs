//! CLI dispatcher and shared types for `cairn hook <name>`.
//!
//! The store-backed per-hook behavior lands in later tasks. This module owns
//! the stable command surface and shared result/error envelope.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::generated::common::Ulid;
use cairn_core::generated::errors::ErrorCode;
use clap::ArgMatches;
use serde::Serialize;
use serde_json::Value;

use crate::verbs::envelope::{emit_json, new_operation_id};

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
    pub hook: HookName,
    /// Operation identifier for retry and support correlation.
    pub operation_id: Ulid,
    /// Artifacts produced by a successful hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<HookArtifacts>,
    /// Typed error details on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<HookErrorBody>,
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
                .help("Hook name")
                .required(true)
                .value_parser(HookName::ALL),
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
            Err(err) => return emit_failure(HookName::Stop, operation_id, err, json, 64),
        },
        None => {
            return emit_failure(
                HookName::Stop,
                operation_id,
                HookError::invalid_args("hook name is required"),
                json,
                64,
            );
        }
    };

    if let Err(err) = load_payload(matches) {
        return emit_failure(hook, operation_id, err, json, 1);
    }

    emit_success(hook, operation_id, json)
}

fn load_payload(matches: &ArgMatches) -> Result<Value, HookError> {
    let value = if let Some(raw) = matches.get_one::<String>("payload") {
        serde_json::from_str(raw)
            .map_err(|err| HookError::invalid_args(format!("payload must be valid JSON: {err}")))?
    } else if let Some(path) = matches.get_one::<PathBuf>("payload-file") {
        let raw = std::fs::read_to_string(path).map_err(|err| {
            HookError::internal(
                format!("failed to read payload file `{}`: {err}", path.display()),
                "restore access to the payload file and retry the same hook command",
            )
        })?;
        serde_json::from_str(&raw).map_err(|err| {
            HookError::invalid_args(format!("payload file must contain valid JSON: {err}"))
        })?
    } else {
        serde_json::json!({})
    };

    if value.is_object() {
        Ok(value)
    } else {
        Err(HookError::invalid_args(
            "hook payload must be a JSON object",
        ))
    }
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

/// Clone the payload object for artifact storage.
#[must_use]
pub fn payload_object(payload: &Value) -> serde_json::Map<String, Value> {
    payload.as_object().cloned().unwrap_or_default()
}

fn emit_success(hook: HookName, operation_id: Ulid, json: bool) -> ExitCode {
    let result = HookResult {
        ok: true,
        hook,
        operation_id,
        artifacts: None,
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

fn emit_failure(
    hook: HookName,
    operation_id: Ulid,
    err: HookError,
    json: bool,
    code: u8,
) -> ExitCode {
    let result = HookResult {
        ok: false,
        hook,
        operation_id,
        artifacts: None,
        error: Some(err.into_body()),
    };
    if json {
        emit_json(&result);
    } else if let Some(error) = &result.error {
        eprintln!(
            "cairn hook {}: {} - {} (operation_id: {}; retry: {})",
            hook.as_str(),
            error.code,
            error.message,
            result.operation_id.0,
            error.retry_guidance
        );
    }
    ExitCode::from(code)
}
