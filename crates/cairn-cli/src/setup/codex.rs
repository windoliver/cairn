use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value, json};
use toml::value::Table;

const HOOK_NAMES: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

/// Codex configuration scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum CodexScope {
    /// Register in the user's `~/.codex/config.toml`.
    Local,
    /// Register in the project's `.codex/config.toml`.
    Project,
}

/// Options for registering Cairn as a Codex MCP server.
#[derive(Debug, Clone)]
pub struct CodexSetupOpts {
    /// Target Codex configuration scope.
    pub scope: CodexScope,
    /// Project directory used for project files and hook placement.
    pub project_dir: PathBuf,
    /// Home directory used for local Codex config placement.
    pub home_dir: PathBuf,
    /// MCP server name to create or update.
    pub server_name: String,
    /// Cairn vault path passed to the MCP server.
    pub vault: PathBuf,
    /// Cairn binary path used as the MCP command.
    pub binary: PathBuf,
}

/// Options for removing a Codex MCP server registration.
#[derive(Debug, Clone)]
pub struct CodexRemoveOpts {
    /// Target Codex configuration scope.
    pub scope: CodexScope,
    /// Project directory used for project files and hook placement.
    pub project_dir: PathBuf,
    /// Home directory used for local Codex config placement.
    pub home_dir: PathBuf,
    /// MCP server name to remove.
    pub server_name: String,
}

/// Status for a setup or removal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupStatus {
    /// The selected server registration was created.
    Created,
    /// The selected server registration existed and was replaced.
    Updated,
    /// The selected server registration already matched the desired value.
    Unchanged,
    /// The selected server registration was removed.
    Removed,
    /// The selected server registration was not present.
    NotFound,
}

/// Machine-readable receipt for Codex setup operations.
#[derive(Debug, Serialize)]
pub struct CodexSetupReceipt {
    /// Target Codex configuration scope.
    pub scope: CodexScope,
    /// Configuration file path inspected or written.
    pub config_path: PathBuf,
    /// Hook file path written for the project.
    pub hooks_path: PathBuf,
    /// MCP server name selected by the operation.
    pub server_name: String,
    /// Command registered for setup operations.
    pub command: PathBuf,
    /// Arguments registered for setup operations.
    pub args: Vec<String>,
    /// Operation status.
    pub status: SetupStatus,
}

/// Error returned by Codex setup helpers.
#[derive(Debug, thiserror::Error)]
pub enum CodexSetupError {
    /// An invalid option was supplied.
    #[error("invalid option: {0}")]
    InvalidOption(String),
    /// Existing TOML configuration could not be parsed.
    #[error("failed to parse TOML config at {path}")]
    ConfigParse {
        /// Configuration file path.
        path: PathBuf,
        /// TOML parser error.
        source: toml::de::Error,
    },
    /// Existing JSON hook configuration could not be parsed.
    #[error("failed to parse JSON hooks at {path}")]
    HooksParse {
        /// Hook file path.
        path: PathBuf,
        /// JSON parser error.
        source: serde_json::Error,
    },
    /// Existing configuration root had the wrong shape.
    #[error("config root must be an object at {path}")]
    ConfigRoot {
        /// Configuration file path.
        path: PathBuf,
    },
    /// File system operation failed.
    #[error("I/O error at {path}")]
    Io {
        /// Path involved in the failing operation.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The selected path would write through a symlink.
    #[error("{path} is a symlink — cairn will not write through it")]
    UnsafePath {
        /// Symlink path that blocked the operation.
        path: PathBuf,
    },
}

impl CodexSetupError {
    /// Process exit code associated with the error.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidOption(_)
            | Self::ConfigParse { .. }
            | Self::HooksParse { .. }
            | Self::ConfigRoot { .. }
            | Self::UnsafePath { .. } => 78,
            Self::Io { .. } => 74,
        }
    }
}

/// Result alias for Codex setup operations.
pub type Result<T> = std::result::Result<T, CodexSetupError>;

/// Register Cairn as a Codex MCP server.
pub fn setup(opts: &CodexSetupOpts) -> Result<CodexSetupReceipt> {
    validate_server_name(&opts.server_name)?;

    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let vault = absolute(&opts.vault)?;
    let binary = absolute(&opts.binary)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);
    let hooks_path = hook_settings_path(&project_dir);
    let entry = registration_entry(&binary, &vault);

    let mut config = read_toml_or_empty(&config_path)?;
    let root = config
        .as_table_mut()
        .ok_or_else(|| CodexSetupError::ConfigRoot {
            path: config_path.clone(),
        })?;
    root.insert(
        "hooks".to_string(),
        toml::Value::String(path_string(&hooks_path)),
    );
    let servers = ensure_toml_table_child(root, "mcp_servers", &config_path)?;
    let status = match servers.get(&opts.server_name) {
        Some(existing) if existing == &entry => SetupStatus::Unchanged,
        Some(_) => SetupStatus::Updated,
        None => SetupStatus::Created,
    };
    servers.insert(opts.server_name.clone(), entry);

    let config_changed = config_changed_toml(&config_path, &config)?;
    if config_changed {
        write_toml_config(&config_path, &config)?;
    }
    let hooks_changed = write_project_hook_settings(&hooks_path, &binary, &vault)?;
    let status = if !config_changed && !hooks_changed {
        SetupStatus::Unchanged
    } else if status == SetupStatus::Unchanged {
        SetupStatus::Updated
    } else {
        status
    };

    Ok(CodexSetupReceipt {
        scope: opts.scope,
        config_path,
        hooks_path,
        server_name: opts.server_name.clone(),
        command: binary,
        args: vec![
            "--vault".to_string(),
            path_string(&vault),
            "mcp".to_string(),
        ],
        status,
    })
}

/// Remove a Codex MCP server registration.
pub fn remove(opts: &CodexRemoveOpts) -> Result<CodexSetupReceipt> {
    validate_server_name(&opts.server_name)?;

    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);
    let hooks_path = hook_settings_path(&project_dir);

    if !config_path.exists() {
        return Ok(CodexSetupReceipt {
            scope: opts.scope,
            config_path,
            hooks_path,
            server_name: opts.server_name.clone(),
            command: PathBuf::new(),
            args: Vec::new(),
            status: SetupStatus::NotFound,
        });
    }

    let mut config = read_toml_or_empty(&config_path)?;
    let root = config
        .as_table_mut()
        .ok_or_else(|| CodexSetupError::ConfigRoot {
            path: config_path.clone(),
        })?;
    let removed = root
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
        .is_some_and(|servers| servers.remove(&opts.server_name).is_some());
    if removed {
        write_toml_config(&config_path, &config)?;
    }

    Ok(CodexSetupReceipt {
        scope: opts.scope,
        config_path,
        hooks_path,
        server_name: opts.server_name.clone(),
        command: PathBuf::new(),
        args: Vec::new(),
        status: if removed {
            SetupStatus::Removed
        } else {
            SetupStatus::NotFound
        },
    })
}

/// Render a human-readable setup receipt.
#[must_use]
pub fn render_human(receipt: &CodexSetupReceipt) -> String {
    let action = match receipt.status {
        SetupStatus::Created => "registered",
        SetupStatus::Updated => "updated",
        SetupStatus::Unchanged => "already registered",
        SetupStatus::Removed => "removed",
        SetupStatus::NotFound => "not found",
    };
    let mut output = format!(
        "Codex MCP server '{}' {} in {}",
        receipt.server_name,
        action,
        receipt.config_path.display()
    );
    if !receipt.command.as_os_str().is_empty() {
        if receipt.args.is_empty() {
            write!(output, "\ncommand: {}", receipt.command.display())
                .expect("writing to String cannot fail");
        } else {
            write!(
                output,
                "\ncommand: {} {}",
                receipt.command.display(),
                receipt.args.join(" ")
            )
            .expect("writing to String cannot fail");
        }
        write!(
            output,
            "\nhooks: {}\nverify: inspect `codex mcp list` and run `cairn status --json`",
            receipt.hooks_path.display()
        )
        .expect("writing to String cannot fail");
    }
    output
}

fn config_path(scope: CodexScope, project_dir: &Path, home_dir: &Path) -> PathBuf {
    match scope {
        CodexScope::Local => home_dir.join(".codex").join("config.toml"),
        CodexScope::Project => project_dir.join(".codex").join("config.toml"),
    }
}

fn validate_server_name(server_name: &str) -> Result<()> {
    if server_name.trim().is_empty() {
        return Err(CodexSetupError::InvalidOption(
            "server_name must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|source| CodexSetupError::Io {
            path: PathBuf::from("."),
            source,
        })?
    };
    Ok(normalize_path(&base.join(path)))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn canonical_existing_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(mut canonical) = cursor.canonicalize() {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        missing.push(cursor.file_name()?.to_owned());
        cursor = cursor.parent()?;
    }
}

fn hook_settings_path(project_dir: &Path) -> PathBuf {
    canonical_existing_path(project_dir)
        .unwrap_or_else(|| project_dir.to_path_buf())
        .join(".codex")
        .join("hooks.json")
}

fn registration_entry(binary: &Path, vault: &Path) -> toml::Value {
    let mut entry = Table::new();
    entry.insert("type".to_string(), toml::Value::String("stdio".to_string()));
    entry.insert(
        "command".to_string(),
        toml::Value::String(path_string(binary)),
    );
    entry.insert(
        "args".to_string(),
        toml::Value::Array(vec![
            toml::Value::String("--vault".to_string()),
            toml::Value::String(path_string(vault)),
            toml::Value::String("mcp".to_string()),
        ]),
    );
    entry.insert("env".to_string(), toml::Value::Table(Table::new()));
    toml::Value::Table(entry)
}

fn write_project_hook_settings(path: &Path, binary: &Path, vault: &Path) -> Result<bool> {
    let mut config = read_json_or_empty(path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| CodexSetupError::ConfigRoot {
            path: path.to_path_buf(),
        })?;
    let hooks = ensure_json_object_child(root, "hooks", path)?;

    for hook in HOOK_NAMES {
        let entry = json!({
            "type": "command",
            "command": hook_command(binary, vault, hook),
        });
        let slot = hooks
            .entry((*hook).to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !slot.is_array() {
            *slot = Value::Array(Vec::new());
        }
        let items = slot
            .as_array_mut()
            .expect("slot was just normalized to an array");
        if !items.iter().any(|item| item == &entry) {
            items.push(entry);
        }
    }

    let changed = config_changed_json(path, &config)?;
    if changed {
        write_json_config(path, &config)?;
    }
    Ok(changed)
}

fn hook_command(binary: &Path, vault: &Path, hook: &str) -> String {
    let mut command = shell_word(&path_string(binary));
    let _ = write!(
        command,
        " hook {hook} --vault-path {} --payload-file - --json",
        shell_word(&path_string(vault))
    );
    command
}

fn shell_word(raw: &str) -> String {
    if raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b':'))
    {
        return raw.to_string();
    }
    format!("'{}'", raw.replace('\'', "'\\''"))
}

fn read_toml_or_empty(path: &Path) -> Result<toml::Value> {
    reject_symlink_ancestors(path)?;
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).map_err(|source| CodexSetupError::ConfigParse {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(Table::new()))
        }
        Err(source) => Err(CodexSetupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_json_or_empty(path: &Path) -> Result<Value> {
    reject_symlink_ancestors(path)?;
    match fs::read_to_string(path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| CodexSetupError::HooksParse {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Map::new()))
        }
        Err(source) => Err(CodexSetupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn ensure_toml_table_child<'a>(
    object: &'a mut Table,
    key: &str,
    config_path: &Path,
) -> Result<&'a mut Table> {
    if let Some(existing) = object.get(key) {
        if !existing.is_table() {
            return Err(CodexSetupError::ConfigRoot {
                path: config_path.to_path_buf(),
            });
        }
    } else {
        object.insert(key.to_string(), toml::Value::Table(Table::new()));
    }
    Ok(object
        .get_mut(key)
        .and_then(toml::Value::as_table_mut)
        .expect("table child was just inserted or validated"))
}

fn ensure_json_object_child<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    config_path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    if let Some(existing) = object.get(key) {
        if !existing.is_object() {
            return Err(CodexSetupError::ConfigRoot {
                path: config_path.to_path_buf(),
            });
        }
    } else {
        object.insert(key.to_string(), Value::Object(Map::new()));
    }
    Ok(object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object child was just inserted or validated"))
}

fn config_changed_toml(path: &Path, desired: &toml::Value) -> Result<bool> {
    reject_symlink_ancestors(path)?;
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(true);
    };
    let current: toml::Value =
        toml::from_str(&contents).map_err(|source| CodexSetupError::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(current != *desired)
}

fn config_changed_json(path: &Path, desired: &Value) -> Result<bool> {
    reject_symlink_ancestors(path)?;
    let Ok(contents) = fs::read(path) else {
        return Ok(true);
    };
    let current: Value =
        serde_json::from_slice(&contents).map_err(|source| CodexSetupError::HooksParse {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(current != *desired)
}

fn write_toml_config(path: &Path, config: &toml::Value) -> Result<()> {
    let parent = parent_dir(path);
    create_dir_checked(parent)?;
    reject_symlink_ancestors(path)?;
    let mut contents = toml::to_string_pretty(config).expect("serializing TOML cannot fail");
    contents.push('\n');
    write_bytes_atomic(path, contents.as_bytes())
}

fn write_json_config(path: &Path, config: &Value) -> Result<()> {
    let parent = parent_dir(path);
    create_dir_checked(parent)?;
    reject_symlink_ancestors(path)?;
    let mut contents =
        serde_json::to_string_pretty(config).expect("serializing serde_json::Value cannot fail");
    contents.push('\n');
    write_bytes_atomic(path, contents.as_bytes())
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn write_bytes_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = parent_dir(path);
    let (temp_path, mut temp_file) = create_temp_file(parent, path)?;
    if let Err(source) = temp_file.write_all(contents) {
        cleanup_temp_file(&temp_path);
        return Err(CodexSetupError::Io {
            path: temp_path,
            source,
        });
    }
    if let Err(source) = temp_file.sync_all() {
        cleanup_temp_file(&temp_path);
        return Err(CodexSetupError::Io {
            path: temp_path,
            source,
        });
    }
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|source| {
        cleanup_temp_file(&temp_path);
        CodexSetupError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

fn create_temp_file(parent: &Path, target: &Path) -> Result<(PathBuf, File)> {
    let file_name = target
        .file_name()
        .map_or_else(|| "config".into(), |name| name.to_string_lossy());
    for attempt in 0..100 {
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(CodexSetupError::Io {
                    path: temp_path,
                    source,
                });
            }
        }
    }
    Err(CodexSetupError::Io {
        path: parent.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create unique temporary config file",
        ),
    })
}

fn cleanup_temp_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn reject_symlink_ancestors(path: &Path) -> Result<()> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| CodexSetupError::Io {
                path: PathBuf::from("."),
                source,
            })?
            .join(path)
    };
    let mut check = PathBuf::new();
    let mut depth = 0usize;
    for component in abs.components() {
        check.push(component);
        depth += 1;
        let is_final = check == abs;
        if (depth > 2 || is_final)
            && fs::symlink_metadata(&check)
                .ok()
                .is_some_and(|meta| meta.file_type().is_symlink())
        {
            return Err(CodexSetupError::UnsafePath { path: check });
        }
    }
    Ok(())
}

fn create_dir_checked(path: &Path) -> Result<()> {
    let mut check = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        check.push(component);
        depth += 1;
        match fs::create_dir(&check) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::metadata(&check).is_ok_and(|meta| meta.is_dir()) {
                    return Err(CodexSetupError::Io {
                        path: check,
                        source: std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "path exists but is not a directory",
                        ),
                    });
                }
            }
            Err(source) => {
                return Err(CodexSetupError::Io {
                    path: check,
                    source,
                });
            }
        }
        let is_final = check == path;
        if (depth > 2 || is_final)
            && fs::symlink_metadata(&check)
                .ok()
                .is_some_and(|meta| meta.file_type().is_symlink())
        {
            return Err(CodexSetupError::UnsafePath { path: check });
        }
    }
    Ok(())
}
