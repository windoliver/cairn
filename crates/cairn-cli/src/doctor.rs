//! Reference-consumer diagnostics.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::ArgMatches;
use serde::Serialize;
use serde_json::Value;

/// Default Claude Code MCP server name expected by the doctor flow.
pub const DEFAULT_SERVER_NAME: &str = "cairn";

const HOOK_NAMES: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

/// Machine-readable doctor result emitted by `cairn doctor ... --json`.
#[derive(Debug, Serialize)]
pub struct DoctorReceipt {
    /// Whether every verification stage succeeded.
    pub ok: bool,
    /// Reference consumer that was diagnosed.
    pub consumer: &'static str,
    /// MCP server entry expected in the consumer config.
    pub server_name: String,
    /// Project directory used for config and hook lookups.
    pub project_dir: PathBuf,
    /// Ordered verification stages and their outcomes.
    pub stages: Vec<DoctorStage>,
}

/// One verification stage inside a doctor run.
#[derive(Debug, Serialize)]
pub struct DoctorStage {
    /// Stable stage identifier.
    pub name: &'static str,
    /// Outcome status (`ok` or `failed`).
    pub status: &'static str,
    /// Human-readable outcome detail.
    pub detail: String,
    /// Next step the operator should take when the stage fails.
    pub remediation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Config or file path that produced the stage result, when applicable.
    pub source: Option<String>,
}

#[derive(Debug)]
struct DoctorError {
    detail: String,
    remediation: String,
    source: Option<String>,
}

impl DoctorError {
    fn new(detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            remediation: remediation.into(),
            source: None,
        }
    }

    fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

#[derive(Debug)]
struct McpRegistration {
    source: String,
    command: String,
    args: Vec<String>,
    server_type: Option<String>,
}

/// Run the `cairn doctor` subcommand tree.
#[must_use]
pub fn run(matches: &ArgMatches) -> ExitCode {
    match matches.subcommand() {
        Some(("claude-code", sub)) => run_claude_code(sub),
        _ => unreachable!("clap subcommand_required(true) on doctor ensures a subcommand"),
    }
}

fn run_claude_code(matches: &ArgMatches) -> ExitCode {
    let json = matches.get_flag("json");
    let project_dir = matches.get_one::<String>("project-dir").map_or_else(
        || std::env::current_dir().expect("cwd available"),
        PathBuf::from,
    );
    let home_dir = matches
        .get_one::<String>("home-dir")
        .map(PathBuf::from)
        .or_else(default_home_dir);
    let server_name = matches
        .get_one::<String>("server-name")
        .expect("server-name defaulted")
        .clone();

    let mut stages = Vec::new();

    let Some(registration) = push_stage(
        &mut stages,
        "mcp_config",
        locate_registration(&project_dir, home_dir.as_deref(), &server_name),
        |reg| format!("found Claude Code MCP registration in {}", reg.source),
        |reg| Some(reg.source.clone()),
    ) else {
        return finish(json, server_name, project_dir, stages);
    };

    let Some(resolved_command) = push_stage(
        &mut stages,
        "binary",
        resolve_command_path(&registration.command),
        |path| format!("resolved MCP command to {}", path.display()),
        |path| Some(path.display().to_string()),
    ) else {
        return finish(json, server_name, project_dir, stages);
    };

    let Some(()) = push_stage(
        &mut stages,
        "mcp_registration",
        verify_registration_shape(&registration),
        |()| {
            format!(
                "registration targets stdio Cairn MCP: {} {}",
                registration.command,
                registration.args.join(" ")
            )
        },
        |()| Some(registration.source.clone()),
    ) else {
        return finish(json, server_name, project_dir, stages);
    };
    let spawn = SpawnConfig {
        command: resolved_command,
        args: registration.args.clone(),
        project_dir: project_dir.clone(),
    };

    if push_stage(
        &mut stages,
        "mcp_startup",
        probe_mcp_startup(&spawn),
        |()| "initialized the configured MCP server successfully".to_owned(),
        |()| None,
    )
    .is_none()
    {
        return finish(json, server_name, project_dir, stages);
    }

    if push_stage(
        &mut stages,
        "mcp_status_call",
        probe_status_call(&spawn),
        |()| "called status successfully through the configured MCP server".to_owned(),
        |()| None,
    )
    .is_none()
    {
        return finish(json, server_name, project_dir, stages);
    }

    if push_stage(
        &mut stages,
        "hooks",
        verify_hooks(&project_dir, home_dir.as_deref()),
        |source| format!("found all five hook entries across {source}"),
        |source| Some(source.clone()),
    )
    .is_none()
    {
        return finish(json, server_name, project_dir, stages);
    }

    finish(json, server_name, project_dir, stages)
}

fn finish(
    json: bool,
    server_name: String,
    project_dir: PathBuf,
    stages: Vec<DoctorStage>,
) -> ExitCode {
    let ok = stages.iter().all(|stage| stage.status == "ok");
    let receipt = DoctorReceipt {
        ok,
        consumer: "claude-code",
        server_name,
        project_dir,
        stages,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receipt).expect("doctor receipt serializes")
        );
    } else {
        println!("{}", render_human(&receipt));
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(69)
    }
}

fn render_human(receipt: &DoctorReceipt) -> String {
    let mut out = String::new();
    let summary = if receipt.ok { "ok" } else { "failed" };
    let _ = write!(
        out,
        "cairn doctor claude-code: {summary}\n  project  {}\n  server   {}\n",
        receipt.project_dir.display(),
        receipt.server_name
    );
    for stage in &receipt.stages {
        let _ = write!(
            out,
            "  [{}] {}\n    {}\n    remediation: {}\n",
            stage.status, stage.name, stage.detail, stage.remediation
        );
    }
    out.trim_end().to_owned()
}

fn push_stage<T, FDetail, FSource>(
    stages: &mut Vec<DoctorStage>,
    name: &'static str,
    result: Result<T, DoctorError>,
    detail_ok: FDetail,
    source_ok: FSource,
) -> Option<T>
where
    FDetail: FnOnce(&T) -> String,
    FSource: FnOnce(&T) -> Option<String>,
{
    match result {
        Ok(value) => {
            stages.push(DoctorStage {
                name,
                status: "ok",
                detail: detail_ok(&value),
                remediation: "none".to_owned(),
                source: source_ok(&value),
            });
            Some(value)
        }
        Err(err) => {
            stages.push(DoctorStage {
                name,
                status: "failed",
                detail: err.detail,
                remediation: err.remediation,
                source: err.source,
            });
            None
        }
    }
}

fn default_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn locate_registration(
    project_dir: &Path,
    home_dir: Option<&Path>,
    server_name: &str,
) -> Result<McpRegistration, DoctorError> {
    let mut candidates = Vec::new();

    if let Some(home_dir) = home_dir {
        candidates.push(RegistrationCandidate {
            path: home_dir.join(".claude.json"),
            kind: RegistrationSource::LocalProject {
                project_dir: project_dir.to_path_buf(),
            },
        });
    }
    candidates.push(RegistrationCandidate {
        path: project_dir.join(".mcp.json"),
        kind: RegistrationSource::Project,
    });
    if let Some(home_dir) = home_dir {
        candidates.push(RegistrationCandidate {
            path: home_dir.join(".claude.json"),
            kind: RegistrationSource::User,
        });
    }

    for candidate in candidates {
        let Some(root) = read_optional_json(&candidate.path)? else {
            continue;
        };
        if let Some(reg) = candidate.kind.extract(&root, server_name) {
            return Ok(McpRegistration {
                source: candidate.describe(),
                command: reg.command,
                args: reg.args,
                server_type: reg.server_type,
            });
        }
    }

    Err(DoctorError::new(
        format!(
            "no Claude Code MCP registration named `{server_name}` was found in local, project, or user scope"
        ),
        "run `cairn setup claude-code --vault <name-or-path>` from the project, then rerun `cairn doctor claude-code`",
    ))
}

#[derive(Debug)]
struct RegistrationCandidate {
    path: PathBuf,
    kind: RegistrationSource,
}

impl RegistrationCandidate {
    fn describe(&self) -> String {
        match &self.kind {
            RegistrationSource::LocalProject { .. } => {
                format!("{} (local scope)", self.path.display())
            }
            RegistrationSource::Project => format!("{} (project scope)", self.path.display()),
            RegistrationSource::User => format!("{} (user scope)", self.path.display()),
        }
    }
}

#[derive(Debug)]
enum RegistrationSource {
    LocalProject { project_dir: PathBuf },
    Project,
    User,
}

impl RegistrationSource {
    fn extract(&self, root: &Value, server_name: &str) -> Option<RawRegistration> {
        match self {
            Self::LocalProject { project_dir } => root
                .get("projects")?
                .get(project_dir.to_string_lossy().as_ref())?
                .get("mcpServers")?
                .get(server_name)
                .and_then(parse_registration),
            Self::Project | Self::User => root
                .get("mcpServers")?
                .get(server_name)
                .and_then(parse_registration),
        }
    }
}

#[derive(Debug)]
struct RawRegistration {
    command: String,
    args: Vec<String>,
    server_type: Option<String>,
}

fn parse_registration(value: &Value) -> Option<RawRegistration> {
    let command = value.get("command")?.as_str()?.to_owned();
    let args = value
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let server_type = value
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(RawRegistration {
        command,
        args,
        server_type,
    })
}

fn read_optional_json(path: &Path) -> Result<Option<Value>, DoctorError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|err| {
            DoctorError::new(
                format!("failed to parse JSON config {}: {err}", path.display()),
                "fix the JSON syntax in the reported Claude Code config file and rerun doctor",
            )
            .with_source(path.display().to_string())
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(DoctorError::new(
            format!("failed to read config {}: {err}", path.display()),
            "restore read access to the reported Claude Code config file and rerun doctor",
        )
        .with_source(path.display().to_string())),
    }
}

fn resolve_command_path(command: &str) -> Result<PathBuf, DoctorError> {
    if command.contains("${") {
        return Err(DoctorError::new(
            format!(
                "configured MCP command `{command}` still contains unresolved environment variables"
            ),
            "replace unresolved `${VAR}` placeholders with concrete values or defaults that resolve on this machine",
        ));
    }

    let candidate = PathBuf::from(command);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(DoctorError::new(
            format!(
                "configured MCP command path {} does not exist",
                candidate.display()
            ),
            "install Cairn at the configured path or update the Claude Code MCP registration to point at the correct binary",
        ));
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(DoctorError::new(
        format!("configured MCP command `{command}` was not found on PATH"),
        "install Cairn or update the Claude Code MCP registration to point at an executable `cairn` binary",
    ))
}

fn verify_registration_shape(registration: &McpRegistration) -> Result<(), DoctorError> {
    if registration
        .server_type
        .as_deref()
        .is_some_and(|server_type| server_type != "stdio")
    {
        return Err(
            DoctorError::new(
                format!(
                    "registration uses transport {:?}, but the reference consumer expects a stdio MCP server",
                    registration.server_type
                ),
                "run `cairn setup claude-code --vault <name-or-path>` to replace the entry with a stdio Cairn MCP registration, then rerun doctor",
            )
            .with_source(registration.source.clone()),
        );
    }

    let args_contain_mcp = registration.args.iter().any(|arg| arg == "mcp");
    if !args_contain_mcp {
        return Err(
            DoctorError::new(
                format!(
                    "registration does not look like a Cairn MCP launch: command=`{}` args={:?}",
                    registration.command, registration.args
                ),
                "run `cairn setup claude-code --vault <name-or-path>` so the Claude Code registration launches the Cairn binary with the `mcp` argument",
            )
            .with_source(registration.source.clone()),
        );
    }

    Ok(())
}

#[derive(Debug)]
struct SpawnConfig {
    command: PathBuf,
    args: Vec<String>,
    project_dir: PathBuf,
}

fn probe_mcp_startup(spawn: &SpawnConfig) -> Result<(), DoctorError> {
    let mut child = Command::new(&spawn.command)
        .args(&spawn.args)
        .current_dir(&spawn.project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            DoctorError::new(
                format!(
                    "failed to spawn configured MCP command {}: {err}",
                    spawn.command.display()
                ),
                "fix the configured command path or permissions, then rerun doctor",
            )
        })?;

    let result = (|| -> Result<(), DoctorError> {
        let stdin = child.stdin.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stdin".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stdout".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stderr".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;

        let mut client = ProtocolClient::new(stdin, stdout, stderr);
        client.initialize()?;
        client.close_stdin();
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    result
}

fn probe_status_call(spawn: &SpawnConfig) -> Result<(), DoctorError> {
    let mut child = Command::new(&spawn.command)
        .args(&spawn.args)
        .current_dir(&spawn.project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            DoctorError::new(
                format!(
                    "failed to spawn configured MCP command {}: {err}",
                    spawn.command.display()
                ),
                "fix the configured command path or permissions, then rerun doctor",
            )
        })?;

    let result = (|| -> Result<(), DoctorError> {
        let stdin = child.stdin.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stdin".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stdout".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            DoctorError::new(
                "spawned MCP process did not expose stderr".to_owned(),
                "verify the configured command starts a stdio MCP server, then rerun doctor",
            )
        })?;

        let mut client = ProtocolClient::new(stdin, stdout, stderr);
        client.initialize()?;
        client.call_status()?;
        client.close_stdin();
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    result
}

struct ProtocolClient {
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    stderr: Arc<Mutex<String>>,
}

impl ProtocolClient {
    fn new(
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
        stderr: std::process::ChildStderr,
    ) -> Self {
        let captured_stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured_stderr);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Ok(mut buf) = sink.lock() {
                            buf.push_str(&line);
                        }
                    }
                }
            }
        });
        Self {
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr: captured_stderr,
        }
    }

    fn initialize(&mut self) -> Result<(), DoctorError> {
        self.send(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"cairn-doctor","version":"0.1.0"}}}"#,
        )?;
        let response = self.recv()?;
        if response.get("result").is_none() {
            return Err(DoctorError::new(
                format!("MCP initialize failed: {response}"),
                format!(
                    "fix the configured MCP command so it starts cleanly; stderr so far: {}",
                    self.stderr_so_far()
                ),
            ));
        }
        Ok(())
    }

    fn call_status(&mut self) -> Result<(), DoctorError> {
        self.send(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
        )?;
        let response = self.recv()?;
        if response.get("result").is_some() {
            return Ok(());
        }
        Err(DoctorError::new(
            format!("MCP status call failed: {response}"),
            format!(
                "fix the configured MCP server so `status` is callable through Claude Code; stderr so far: {}",
                self.stderr_so_far()
            ),
        ))
    }

    fn send(&mut self, line: &str) -> Result<(), DoctorError> {
        {
            let stdin = self.stdin.as_mut().ok_or_else(|| {
                DoctorError::new(
                    "MCP stdin closed unexpectedly".to_owned(),
                    "rerun doctor after fixing the configured server command",
                )
            })?;
            if let Err(err) = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.flush())
            {
                return Err(DoctorError::new(
                    format!(
                        "failed to write JSON-RPC request to MCP server: {err}; stderr so far: {}",
                        self.stderr_so_far()
                    ),
                    "verify the configured command still speaks stdio MCP and rerun doctor",
                ));
            }
        }
        Ok(())
    }

    fn recv(&mut self) -> Result<Value, DoctorError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut line = String::new();
        loop {
            if Instant::now() >= deadline {
                return Err(DoctorError::new(
                    "timed out waiting for MCP response".to_owned(),
                    format!(
                        "fix the configured MCP server startup so it responds within 15s; stderr so far: {}",
                        self.stderr_so_far()
                    ),
                ));
            }
            line.clear();
            let bytes = self.stdout.read_line(&mut line).map_err(|err| {
                DoctorError::new(
                    format!("failed reading MCP stdout: {err}"),
                    "verify the configured server starts and keeps stdout open",
                )
            })?;
            if bytes == 0 {
                return Err(DoctorError::new(
                    "configured MCP process exited before replying".to_owned(),
                    format!(
                        "fix the configured command so it stays up as a stdio MCP server; stderr so far: {}",
                        self.stderr_so_far()
                    ),
                ));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed).map_err(|err| {
                DoctorError::new(
                    format!(
                        "failed to parse MCP JSON-RPC response `{trimmed}`: {err}; stderr so far: {}",
                        self.stderr_so_far()
                    ),
                    "verify the configured server writes valid newline-delimited JSON-RPC frames",
                )
            });
        }
    }

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn stderr_so_far(&mut self) -> String {
        for _ in 0..5 {
            match self.stderr.lock() {
                Ok(buf) => {
                    let text = buf.trim().to_owned();
                    if !text.is_empty() {
                        return text;
                    }
                }
                Err(_) => return "stderr unavailable".to_owned(),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        "stderr empty".to_owned()
    }
}

fn verify_hooks(project_dir: &Path, home_dir: Option<&Path>) -> Result<String, DoctorError> {
    let candidates = [
        project_dir.join(".claude/settings.local.json"),
        project_dir.join(".claude/settings.json"),
        home_dir
            .map(|dir| dir.join(".claude/settings.json"))
            .unwrap_or_default(),
    ];

    let mut seen = std::collections::BTreeSet::new();
    let mut found_in = Vec::new();
    for path in candidates {
        if path.as_os_str().is_empty() {
            continue;
        }
        let Some(root) = read_optional_json(&path)? else {
            continue;
        };
        let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
            continue;
        };
        found_in.push(path.display().to_string());
        for hook in HOOK_NAMES {
            if hooks.get(*hook).is_some_and(hook_has_installed_action) {
                seen.insert(*hook);
            }
        }
    }

    if found_in.is_empty() {
        return Err(DoctorError::new(
            "no Claude Code hook settings file was found".to_owned(),
            "add hook configuration to `.claude/settings.local.json`, `.claude/settings.json`, or `~/.claude/settings.json`, then rerun doctor",
        ));
    }

    let missing: Vec<&str> = HOOK_NAMES
        .iter()
        .copied()
        .filter(|hook| !seen.contains(hook))
        .collect();
    if !missing.is_empty() {
        return Err(DoctorError::new(
            format!(
                "hook configuration is missing entries for {}",
                missing.join(", ")
            ),
            "add the missing Claude Code hook entries and rerun doctor",
        ));
    }

    Ok(found_in.join(", "))
}

fn hook_has_installed_action(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(hook_has_installed_action),
        Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(Value::as_str) {
                return !command.trim().is_empty();
            }
            if let Some(hooks) = map.get("hooks") {
                return hook_has_installed_action(hooks);
            }
            false
        }
        Value::String(text) => !text.trim().is_empty(),
        _ => false,
    }
}
