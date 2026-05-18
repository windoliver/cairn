use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Claude Code MCP configuration scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeCodeScope {
    /// Register in the user's `~/.claude.json` for one project path.
    Local,
    /// Register in the project's `.mcp.json`.
    Project,
}

/// Options for registering Cairn as a Claude Code MCP server.
#[derive(Debug, Clone)]
pub struct ClaudeCodeSetupOpts {
    /// Target Claude Code configuration scope.
    pub scope: ClaudeCodeScope,
    /// Project directory used for local scope and project config placement.
    pub project_dir: PathBuf,
    /// Home directory used for local Claude Code config placement.
    pub home_dir: PathBuf,
    /// MCP server name to create or update.
    pub server_name: String,
    /// Cairn vault path passed to the MCP server.
    pub vault: PathBuf,
    /// Cairn binary path used as the MCP command.
    pub binary: PathBuf,
}

/// Options for removing a Claude Code MCP server registration.
#[derive(Debug, Clone)]
pub struct ClaudeCodeRemoveOpts {
    /// Target Claude Code configuration scope.
    pub scope: ClaudeCodeScope,
    /// Project directory used for local scope and project config placement.
    pub project_dir: PathBuf,
    /// Home directory used for local Claude Code config placement.
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

/// Machine-readable receipt for Claude Code setup operations.
#[derive(Debug, Serialize)]
pub struct ClaudeCodeSetupReceipt {
    /// Target Claude Code configuration scope.
    pub scope: ClaudeCodeScope,
    /// Configuration file path inspected or written.
    pub config_path: PathBuf,
    /// MCP server name selected by the operation.
    pub server_name: String,
    /// Command registered for setup operations.
    pub command: PathBuf,
    /// Arguments registered for setup operations.
    pub args: Vec<String>,
    /// Operation status.
    pub status: SetupStatus,
}

/// Error returned by Claude Code setup helpers.
#[derive(Debug, thiserror::Error)]
pub enum ClaudeCodeSetupError {
    /// An invalid option was supplied.
    #[error("invalid option: {0}")]
    InvalidOption(String),
    /// Existing configuration could not be parsed as JSON.
    #[error("failed to parse JSON config at {path}")]
    ConfigParse {
        /// Configuration file path.
        path: PathBuf,
        /// JSON parser error.
        source: serde_json::Error,
    },
    /// Existing configuration root was not a JSON object.
    #[error("config root must be a JSON object at {path}")]
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
}

impl ClaudeCodeSetupError {
    /// Process exit code associated with the error.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidOption(_) | Self::ConfigParse { .. } | Self::ConfigRoot { .. } => 78,
            Self::Io { .. } => 74,
        }
    }
}

/// Result alias for Claude Code setup operations.
pub type Result<T> = std::result::Result<T, ClaudeCodeSetupError>;

/// Register Cairn as a Claude Code MCP server.
pub fn setup(opts: &ClaudeCodeSetupOpts) -> Result<ClaudeCodeSetupReceipt> {
    validate_server_name(&opts.server_name)?;

    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let vault = absolute(&opts.vault)?;
    let binary = absolute(&opts.binary)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);
    let mut config = read_config_or_empty(&config_path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| ClaudeCodeSetupError::ConfigRoot {
            path: config_path.clone(),
        })?;
    let entry = registration_entry(&binary, &vault);
    let servers = ensure_mcp_servers_mut(root, opts.scope, &project_dir, &config_path)?;
    let status = match servers.get(&opts.server_name) {
        Some(existing) if existing == &entry => SetupStatus::Unchanged,
        Some(_) => SetupStatus::Updated,
        None => SetupStatus::Created,
    };

    if status != SetupStatus::Unchanged {
        servers.insert(opts.server_name.clone(), entry);
        write_config(&config_path, &config)?;
    }

    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
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

/// Remove a Claude Code MCP server registration.
pub fn remove(opts: &ClaudeCodeRemoveOpts) -> Result<ClaudeCodeSetupReceipt> {
    validate_server_name(&opts.server_name)?;

    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);

    if !config_path.exists() {
        return Ok(ClaudeCodeSetupReceipt {
            scope: opts.scope,
            config_path,
            server_name: opts.server_name.clone(),
            command: PathBuf::new(),
            args: Vec::new(),
            status: SetupStatus::NotFound,
        });
    }

    let mut config = read_config_or_empty(&config_path)?;
    let root = config
        .as_object_mut()
        .ok_or_else(|| ClaudeCodeSetupError::ConfigRoot {
            path: config_path.clone(),
        })?;
    let status = if let Some(servers) =
        mcp_servers_mut_if_present(root, opts.scope, &project_dir, &config_path)?
    {
        if servers.remove(&opts.server_name).is_some() {
            write_config(&config_path, &config)?;
            SetupStatus::Removed
        } else {
            SetupStatus::NotFound
        }
    } else {
        SetupStatus::NotFound
    };

    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
        server_name: opts.server_name.clone(),
        command: PathBuf::new(),
        args: Vec::new(),
        status,
    })
}

/// Render a human-readable setup receipt.
#[must_use]
pub fn render_human(receipt: &ClaudeCodeSetupReceipt) -> String {
    let action = match receipt.status {
        SetupStatus::Created => "registered",
        SetupStatus::Updated => "updated",
        SetupStatus::Unchanged => "already registered",
        SetupStatus::Removed => "removed",
        SetupStatus::NotFound => "not found",
    };
    let mut output = format!(
        "Claude Code MCP server '{}' {} in {}",
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
        output.push_str("\nverify: cairn doctor claude-code");
    }
    output
}

fn config_path(scope: ClaudeCodeScope, project_dir: &Path, home_dir: &Path) -> PathBuf {
    match scope {
        ClaudeCodeScope::Local => home_dir.join(".claude.json"),
        ClaudeCodeScope::Project => project_dir.join(".mcp.json"),
    }
}

fn validate_server_name(server_name: &str) -> Result<()> {
    if server_name.trim().is_empty() {
        return Err(ClaudeCodeSetupError::InvalidOption(
            "server_name must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|source| ClaudeCodeSetupError::Io {
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

fn local_project_keys(project_dir: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    if let Ok(canonical) = project_dir.canonicalize() {
        push_unique_project_key(&mut keys, &normalize_path(&canonical));
    }
    push_unique_project_key(&mut keys, project_dir);
    keys
}

fn push_unique_project_key(keys: &mut Vec<String>, path: &Path) {
    let key = path_string(path);
    if !keys.iter().any(|existing| existing == &key) {
        keys.push(key);
    }
}

fn local_project_key_for_write(projects: &Map<String, Value>, project_dir: &Path) -> String {
    let keys = local_project_keys(project_dir);
    keys.iter()
        .find(|key| projects.contains_key(key.as_str()))
        .cloned()
        .unwrap_or_else(|| keys[0].clone())
}

fn local_project_key_if_present(
    projects: &Map<String, Value>,
    project_dir: &Path,
) -> Option<String> {
    local_project_keys(project_dir)
        .into_iter()
        .find(|key| projects.contains_key(key.as_str()))
}

fn registration_entry(binary: &Path, vault: &Path) -> Value {
    json!({
        "type": "stdio",
        "command": path_string(binary),
        "args": ["--vault", path_string(vault), "mcp"],
        "env": {}
    })
}

fn read_config_or_empty(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|source| ClaudeCodeSetupError::ConfigParse {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Map::new()))
        }
        Err(source) => Err(ClaudeCodeSetupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_config(path: &Path, config: &Value) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| ClaudeCodeSetupError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut contents =
        serde_json::to_string_pretty(config).expect("serializing serde_json::Value cannot fail");
    contents.push('\n');
    let (temp_path, mut temp_file) = create_temp_file(parent, path)?;

    if let Err(source) = temp_file.write_all(contents.as_bytes()) {
        cleanup_temp_file(&temp_path);
        return Err(ClaudeCodeSetupError::Io {
            path: temp_path,
            source,
        });
    }
    if let Err(source) = temp_file.sync_all() {
        cleanup_temp_file(&temp_path);
        return Err(ClaudeCodeSetupError::Io {
            path: temp_path,
            source,
        });
    }
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|source| {
        cleanup_temp_file(&temp_path);
        ClaudeCodeSetupError::Io {
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
                return Err(ClaudeCodeSetupError::Io {
                    path: temp_path,
                    source,
                });
            }
        }
    }
    Err(ClaudeCodeSetupError::Io {
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

fn ensure_object_child<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    config_path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    if let Some(existing) = object.get(key) {
        if !existing.is_object() {
            return Err(ClaudeCodeSetupError::ConfigRoot {
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

fn ensure_mcp_servers_mut<'a>(
    root: &'a mut Map<String, Value>,
    scope: ClaudeCodeScope,
    project_dir: &Path,
    config_path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    match scope {
        ClaudeCodeScope::Local => {
            let projects = ensure_object_child(root, "projects", config_path)?;
            let project_key = local_project_key_for_write(projects, project_dir);
            let project = ensure_object_child(projects, &project_key, config_path)?;
            ensure_object_child(project, "mcpServers", config_path)
        }
        ClaudeCodeScope::Project => ensure_object_child(root, "mcpServers", config_path),
    }
}

fn mcp_servers_mut_if_present<'a>(
    root: &'a mut Map<String, Value>,
    scope: ClaudeCodeScope,
    project_dir: &Path,
    config_path: &Path,
) -> Result<Option<&'a mut Map<String, Value>>> {
    match scope {
        ClaudeCodeScope::Local => {
            let Some(projects) = object_child_mut_if_present(root, "projects", config_path)? else {
                return Ok(None);
            };
            let Some(project_key) = local_project_key_if_present(projects, project_dir) else {
                return Ok(None);
            };
            let Some(project) = object_child_mut_if_present(projects, &project_key, config_path)?
            else {
                return Ok(None);
            };
            object_child_mut_if_present(project, "mcpServers", config_path)
        }
        ClaudeCodeScope::Project => object_child_mut_if_present(root, "mcpServers", config_path),
    }
}

fn object_child_mut_if_present<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    config_path: &Path,
) -> Result<Option<&'a mut Map<String, Value>>> {
    let Some(value) = object.get_mut(key) else {
        return Ok(None);
    };
    value
        .as_object_mut()
        .map(Some)
        .ok_or_else(|| ClaudeCodeSetupError::ConfigRoot {
            path: config_path.to_path_buf(),
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    fn setup_opts(
        root: &TempDir,
        scope: ClaudeCodeScope,
        server_name: &str,
    ) -> ClaudeCodeSetupOpts {
        ClaudeCodeSetupOpts {
            scope,
            project_dir: root.path().join("project"),
            home_dir: root.path().join("home"),
            server_name: server_name.to_string(),
            vault: root.path().join("vault"),
            binary: root.path().join("bin/cairn"),
        }
    }

    fn remove_opts(
        root: &TempDir,
        scope: ClaudeCodeScope,
        server_name: &str,
    ) -> ClaudeCodeRemoveOpts {
        ClaudeCodeRemoveOpts {
            scope,
            project_dir: root.path().join("project"),
            home_dir: root.path().join("home"),
            server_name: server_name.to_string(),
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("config should be readable"))
            .expect("config should be JSON")
    }

    fn server<'a>(config: &'a Value, project_dir: &Path, name: &str) -> &'a Value {
        let projects = config["projects"]
            .as_object()
            .expect("projects should be an object");
        let project_key = local_project_keys(project_dir)
            .into_iter()
            .find(|key| projects.contains_key(key.as_str()))
            .expect("project should exist");
        &projects[&project_key]["mcpServers"][name]
    }

    fn expected_entry(binary: &Path, vault: &Path) -> Value {
        json!({
            "type": "stdio",
            "command": binary.to_string_lossy(),
            "args": ["--vault", vault.to_string_lossy(), "mcp"],
            "env": {}
        })
    }

    #[test]
    fn local_setup_creates_project_mcp_server() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");

        let receipt = setup(&opts).expect("setup should succeed");

        assert_eq!(receipt.status, SetupStatus::Created);
        assert_eq!(receipt.config_path, opts.home_dir.join(".claude.json"));
        let config = read_json(&receipt.config_path);
        assert_eq!(
            server(&config, &opts.project_dir, "cairn"),
            &expected_entry(&opts.binary, &opts.vault)
        );
    }

    #[test]
    fn local_setup_is_idempotent_and_keeps_file_bytes_stable() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        let first = setup(&opts).expect("first setup should succeed");
        let bytes_before = fs::read(&first.config_path).expect("config should be readable");

        let second = setup(&opts).expect("second setup should succeed");

        assert_eq!(second.status, SetupStatus::Unchanged);
        assert_eq!(
            fs::read(&first.config_path).expect("config should be readable"),
            bytes_before
        );
    }

    #[test]
    fn local_setup_replaces_only_stale_selected_server() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        fs::create_dir_all(&opts.home_dir).expect("home dir");
        fs::write(
            opts.home_dir.join(".claude.json"),
            serde_json::to_string_pretty(&json!({
                "theme": "dark",
                "projects": {
                    opts.project_dir.to_string_lossy().as_ref(): {
                        "keep": true,
                        "mcpServers": {
                            "cairn": { "type": "stdio", "command": "/old", "args": [], "env": {} },
                            "other": { "type": "stdio", "command": "/other", "args": ["serve"], "env": { "A": "B" } }
                        }
                    },
                    "/elsewhere": {
                        "mcpServers": {
                            "cairn": { "type": "stdio", "command": "/elsewhere", "args": [], "env": {} }
                        }
                    }
                }
            }))
            .expect("serialize config"),
        )
        .expect("write config");

        let receipt = setup(&opts).expect("setup should succeed");

        assert_eq!(receipt.status, SetupStatus::Updated);
        let config = read_json(&receipt.config_path);
        assert_eq!(
            server(&config, &opts.project_dir, "cairn"),
            &expected_entry(&opts.binary, &opts.vault)
        );
        assert_eq!(
            config["projects"][opts.project_dir.to_string_lossy().as_ref()]["mcpServers"]["other"]
                ["command"],
            "/other"
        );
        assert_eq!(
            config["projects"]["/elsewhere"]["mcpServers"]["cairn"]["command"],
            "/elsewhere"
        );
        assert_eq!(config["theme"], "dark");
        assert_eq!(
            config["projects"][opts.project_dir.to_string_lossy().as_ref()]["keep"],
            true
        );
    }

    #[test]
    fn project_setup_creates_mcp_json_and_preserves_unrelated_content() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Project, "cairn");
        fs::create_dir_all(&opts.project_dir).expect("project dir");
        fs::write(
            opts.project_dir.join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "enabled": true,
                "mcpServers": {
                    "other": { "type": "stdio", "command": "/other", "args": [], "env": {} }
                }
            }))
            .expect("serialize config"),
        )
        .expect("write config");

        let receipt = setup(&opts).expect("setup should succeed");

        assert_eq!(receipt.status, SetupStatus::Created);
        let config = read_json(&receipt.config_path);
        assert_eq!(config["enabled"], true);
        assert_eq!(config["mcpServers"]["other"]["command"], "/other");
        assert_eq!(
            config["mcpServers"]["cairn"],
            expected_entry(&opts.binary, &opts.vault)
        );
    }

    #[test]
    fn local_remove_deletes_only_selected_server() {
        let root = TempDir::new().expect("tempdir");
        let cairn_opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        setup(&cairn_opts).expect("setup should succeed");
        let mut other_opts = setup_opts(&root, ClaudeCodeScope::Local, "other");
        other_opts.binary = PathBuf::from("/tmp/other");
        setup(&other_opts).expect("other setup should succeed");

        let receipt = remove(&remove_opts(&root, ClaudeCodeScope::Local, "cairn"))
            .expect("remove should succeed");

        assert_eq!(receipt.status, SetupStatus::Removed);
        let config = read_json(&receipt.config_path);
        assert!(server(&config, &cairn_opts.project_dir, "cairn").is_null());
        assert_eq!(
            server(&config, &cairn_opts.project_dir, "other")["command"],
            "/tmp/other"
        );
    }

    #[test]
    fn project_remove_absent_returns_not_found_and_does_not_create_file() {
        let root = TempDir::new().expect("tempdir");
        let opts = remove_opts(&root, ClaudeCodeScope::Project, "cairn");

        let receipt = remove(&opts).expect("remove should succeed");

        assert_eq!(receipt.status, SetupStatus::NotFound);
        assert_eq!(receipt.config_path, opts.project_dir.join(".mcp.json"));
        assert!(!receipt.config_path.exists());
    }

    #[test]
    fn project_remove_rejects_non_object_mcp_servers_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = remove_opts(&root, ClaudeCodeScope::Project, "cairn");
        fs::create_dir_all(&opts.project_dir).expect("project dir");
        let config_path = opts.project_dir.join(".mcp.json");
        let original = br#"{"mcpServers":[],"keep":true}
"#;
        fs::write(&config_path, original).expect("write config");

        let err = remove(&opts).expect_err("remove should reject non-object mcpServers");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn local_remove_rejects_non_object_projects_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = remove_opts(&root, ClaudeCodeScope::Local, "cairn");
        fs::create_dir_all(&opts.home_dir).expect("home dir");
        let config_path = opts.home_dir.join(".claude.json");
        let original = br#"{"projects":[],"keep":true}
"#;
        fs::write(&config_path, original).expect("write config");

        let err = remove(&opts).expect_err("remove should reject non-object projects");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn setup_rejects_non_object_config_root_with_exit_code_78() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Project, "cairn");
        fs::create_dir_all(&opts.project_dir).expect("project dir");
        fs::write(opts.project_dir.join(".mcp.json"), "[]\n").expect("write config");

        let err = setup(&opts).expect_err("setup should reject array root");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
    }

    #[test]
    fn project_setup_rejects_non_object_mcp_servers_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Project, "cairn");
        fs::create_dir_all(&opts.project_dir).expect("project dir");
        let config_path = opts.project_dir.join(".mcp.json");
        let original = br#"{"mcpServers":[],"keep":true}
"#;
        fs::write(&config_path, original).expect("write config");

        let err = setup(&opts).expect_err("setup should reject non-object mcpServers");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn local_setup_rejects_non_object_projects_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        fs::create_dir_all(&opts.home_dir).expect("home dir");
        let config_path = opts.home_dir.join(".claude.json");
        let original = br#"{"projects":[],"keep":true}
"#;
        fs::write(&config_path, original).expect("write config");

        let err = setup(&opts).expect_err("setup should reject non-object projects");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn local_setup_rejects_non_object_project_entry_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        fs::create_dir_all(&opts.home_dir).expect("home dir");
        let config_path = opts.home_dir.join(".claude.json");
        let original = format!(
            "{{\"projects\":{{\"{}\":[]}},\"keep\":true}}\n",
            opts.project_dir.to_string_lossy()
        )
        .into_bytes();
        fs::write(&config_path, &original).expect("write config");

        let err = setup(&opts).expect_err("setup should reject non-object project entry");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn local_setup_rejects_non_object_nested_mcp_servers_without_rewriting() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");
        fs::create_dir_all(&opts.home_dir).expect("home dir");
        let config_path = opts.home_dir.join(".claude.json");
        let original = format!(
            "{{\"projects\":{{\"{}\":{{\"mcpServers\":[],\"keep\":true}}}}}}\n",
            opts.project_dir.to_string_lossy()
        )
        .into_bytes();
        fs::write(&config_path, &original).expect("write config");

        let err = setup(&opts).expect_err("setup should reject non-object nested mcpServers");

        assert_eq!(err.exit_code(), 78);
        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(
            fs::read(&config_path).expect("config should remain"),
            original
        );
    }

    #[test]
    fn setup_receipt_serializes_expected_json_fields() {
        let root = TempDir::new().expect("tempdir");
        let opts = setup_opts(&root, ClaudeCodeScope::Local, "cairn");

        let receipt = setup(&opts).expect("setup should succeed");
        let json = serde_json::to_value(&receipt).expect("receipt should serialize");

        assert_eq!(json["scope"], "local");
        assert_eq!(json["status"], "created");
        assert_eq!(json["server_name"], "cairn");
    }

    #[test]
    fn render_human_setup_includes_action_command_and_verify() {
        let receipt = ClaudeCodeSetupReceipt {
            scope: ClaudeCodeScope::Local,
            config_path: PathBuf::from("/home/me/.claude.json"),
            server_name: "cairn".to_string(),
            command: PathBuf::from("/usr/local/bin/cairn"),
            args: vec![
                "--vault".to_string(),
                "/home/me/vault".to_string(),
                "mcp".to_string(),
            ],
            status: SetupStatus::Created,
        };

        let output = render_human(&receipt);

        assert!(output.contains("registered"));
        assert!(output.contains("command: /usr/local/bin/cairn --vault /home/me/vault mcp"));
        assert!(output.contains("verify: cairn doctor claude-code"));
    }

    #[test]
    fn render_human_remove_and_not_found_omit_command_details() {
        for status in [SetupStatus::Removed, SetupStatus::NotFound] {
            let receipt = ClaudeCodeSetupReceipt {
                scope: ClaudeCodeScope::Project,
                config_path: PathBuf::from("/repo/.mcp.json"),
                server_name: "cairn".to_string(),
                command: PathBuf::new(),
                args: Vec::new(),
                status,
            };

            let output = render_human(&receipt);

            assert!(output.contains(match status {
                SetupStatus::Removed => "removed",
                SetupStatus::NotFound => "not found",
                _ => unreachable!("test only covers removal statuses"),
            }));
            assert!(!output.contains("command:"));
            assert!(!output.contains("verify:"));
        }
    }
}
