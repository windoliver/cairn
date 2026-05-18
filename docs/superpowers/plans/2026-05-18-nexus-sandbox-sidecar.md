# Nexus Sandbox Sidecar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `store.kind: nexus-sandbox` profile that manages a Nexus sandbox sidecar as a derived projection while keeping `.cairn/cairn.db` authoritative.

**Architecture:** Pure config data and validation stay in `cairn-core::config`. Generated status wire types come from `crates/cairn-idl/schema/prelude/status.json` and `cairn-codegen`. Runtime sidecar process management, health probing, and status assembly live in `cairn-cli`, where I/O is already allowed.

**Tech Stack:** Rust 2024, `serde`, `figment`, `serde_yaml`, `rusqlite` for read-only DB health probing, `std::net::TcpStream` for HTTP health checks, `std::process::Command` for the sidecar supervisor, `tempfile` for integration tests, and `cairn-codegen` for generated artifacts.

**Design spec:** `docs/superpowers/specs/2026-05-18-nexus-sandbox-sidecar-design.md`
**Brief sections:** section 3.0 Storage topology; section 5.6 P1 durable messaging; section 19 v0.2 Nexus sandbox

---

## File Map

| Action | Path | Responsibility |
|---|---|---|
| Modify | `crates/cairn-core/src/config/mod.rs` | Add `NexusSandboxConfig`, active-profile validation, and config-level migration tests. |
| Modify | `crates/cairn-cli/tests/config.rs` | Verify v0.1 config loading and v0.2 Nexus profile defaulting through the real loader. |
| Modify | `crates/cairn-idl/schema/prelude/status.json` | Add the generated `health` object to status JSON. |
| Modify | `crates/cairn-idl/tests/codegen_emit_sdk.rs` | Assert generated Rust status types include split health. |
| Modify | `crates/cairn-idl/tests/codegen_emit_mcp.rs` | Assert generated status schema requires `health`. |
| Generated | `crates/cairn-core/src/generated/status.rs` | Regenerated SDK status types. |
| Generated | `crates/cairn-mcp/src/generated/schemas/prelude/status.json` | Regenerated canonical status schema. |
| Modify | `crates/cairn-cli/Cargo.toml` | Add direct `rusqlite` dependency for status DB probing. |
| Modify | `crates/cairn-cli/src/lib.rs` | Export the new `nexus` runtime module. |
| Create | `crates/cairn-cli/src/nexus/mod.rs` | Parse endpoint, probe health, resolve `nexus-data/`, spawn/stop sidecar. |
| Modify | `crates/cairn-cli/src/verbs/status.rs` | Load config, assemble `health`, render human and JSON status. |
| Modify | `crates/cairn-cli/tests/status_snapshot.rs` | Add split-health status tests, including degraded Nexus. |

---

## Task 1: Nexus Config Profile

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Modify: `crates/cairn-cli/tests/config.rs`

- [ ] **Step 1: Write failing core config tests**

In `crates/cairn-core/src/config/mod.rs`, add these tests inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn nexus_sandbox_store_defaults_profile_fields() {
        let config: CairnConfig =
            serde_json::from_str(r#"{"store":{"kind":"nexus-sandbox"}}"#).unwrap();
        assert_eq!(config.store.kind, StoreKind::NexusSandbox);
        assert_eq!(config.store.nexus.data_dir, "nexus-data");
        assert_eq!(config.store.nexus.command, "cairn-nexus-sandbox");
        assert_eq!(
            config.store.nexus.args,
            vec!["sandbox".to_owned(), "serve".to_owned()]
        );
        assert_eq!(config.store.nexus.endpoint, "http://127.0.0.1:8765");
        assert_eq!(config.store.nexus.health_path, "/health");
        assert_eq!(config.store.nexus.health_timeout_ms, 5_000);
        assert_eq!(config.store.nexus.shutdown_timeout_ms, 2_000);
        config.validate().unwrap();
    }

    #[test]
    fn sqlite_store_keeps_nexus_profile_inactive() {
        let config = CairnConfig::default();
        assert_eq!(config.store.kind, StoreKind::Sqlite);
        assert!(!config.store.nexus.is_active_for(&config.store.kind));
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_active_nexus_command() {
        let mut config = CairnConfig::default();
        config.store.kind = StoreKind::NexusSandbox;
        config.store.nexus.command.clear();
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidNexusProfile {
                field: "store.nexus.command",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_nexus_data_dir_under_cairn_authority() {
        let mut config = CairnConfig::default();
        config.store.kind = StoreKind::NexusSandbox;
        config.store.nexus.data_dir = ".cairn/cairn.db".into();
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidNexusProfile {
                field: "store.nexus.data_dir",
                ..
            }
        ));
    }
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo check -p cairn-core --locked
```

Expected: FAIL with errors naming missing `store.nexus`, `InvalidNexusProfile`, and `NexusSandboxConfig`.

- [ ] **Step 3: Add the Nexus config types**

In `crates/cairn-core/src/config/mod.rs`, add a `ConfigError` variant after `InvalidRetentionKey`:

```rust
    /// The active Nexus sandbox profile contains an invalid field.
    #[error("invalid Nexus sandbox profile for {field}: {reason}")]
    InvalidNexusProfile {
        /// The config field name containing the invalid value.
        field: &'static str,
        /// Why the value is invalid.
        reason: String,
    },
```

Replace `StoreConfig` with:

```rust
/// Store adapter selection (§4.1 plugin config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    /// Which memory store adapter is active.
    pub kind: StoreKind,
    /// Nexus sandbox profile. Only active when `kind` is `nexus-sandbox`.
    pub nexus: NexusSandboxConfig,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            kind: StoreKind::Sqlite,
            nexus: NexusSandboxConfig::default(),
        }
    }
}

/// Nexus sandbox sidecar profile (§3.0, §19 v0.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NexusSandboxConfig {
    /// Vault-relative or absolute Nexus projection directory.
    pub data_dir: String,
    /// Sidecar executable name or path.
    pub command: String,
    /// Arguments passed to the sidecar executable.
    pub args: Vec<String>,
    /// Base HTTP endpoint for the sidecar.
    pub endpoint: String,
    /// HTTP path used for health checks.
    pub health_path: String,
    /// Maximum time to wait for health, in milliseconds.
    pub health_timeout_ms: u64,
    /// Maximum graceful shutdown wait, in milliseconds.
    pub shutdown_timeout_ms: u64,
}

impl Default for NexusSandboxConfig {
    fn default() -> Self {
        Self {
            data_dir: "nexus-data".into(),
            command: "cairn-nexus-sandbox".into(),
            args: vec!["sandbox".into(), "serve".into()],
            endpoint: "http://127.0.0.1:8765".into(),
            health_path: "/health".into(),
            health_timeout_ms: 5_000,
            shutdown_timeout_ms: 2_000,
        }
    }
}

impl NexusSandboxConfig {
    /// Whether this profile is active for the selected store kind.
    #[must_use]
    pub fn is_active_for(&self, kind: &StoreKind) -> bool {
        matches!(kind, StoreKind::NexusSandbox)
    }

    fn validate_active(&self) -> Result<(), ConfigError> {
        if self.command.trim().is_empty() {
            return Err(ConfigError::InvalidNexusProfile {
                field: "store.nexus.command",
                reason: "must not be empty".into(),
            });
        }
        if self.health_path.trim().is_empty() || !self.health_path.starts_with('/') {
            return Err(ConfigError::InvalidNexusProfile {
                field: "store.nexus.health_path",
                reason: "must start with /".into(),
            });
        }
        if self.health_timeout_ms == 0 {
            return Err(ConfigError::InvalidBudget {
                field: "store.nexus.health_timeout_ms",
                value: 0,
            });
        }
        if self.shutdown_timeout_ms == 0 {
            return Err(ConfigError::InvalidBudget {
                field: "store.nexus.shutdown_timeout_ms",
                value: 0,
            });
        }
        let data_dir = self.data_dir.trim();
        if data_dir.is_empty() {
            return Err(ConfigError::InvalidNexusProfile {
                field: "store.nexus.data_dir",
                reason: "must not be empty".into(),
            });
        }
        if data_dir == ".cairn" || data_dir == ".cairn/cairn.db" || data_dir.starts_with(".cairn/") {
            return Err(ConfigError::InvalidNexusProfile {
                field: "store.nexus.data_dir",
                reason: "must not point inside .cairn authority state".into(),
            });
        }
        Ok(())
    }
}
```

In `CairnConfig::validate`, after custom store plugin validation, add:

```rust
        if self.store.nexus.is_active_for(&self.store.kind) {
            self.store.nexus.validate_active()?;
        }
```

- [ ] **Step 4: Run core tests**

Run:

```bash
cargo nextest run -p cairn-core nexus_sandbox --locked
```

Expected: PASS for the four new tests.

- [ ] **Step 5: Add loader-level migration tests**

In `crates/cairn-cli/tests/config.rs`, add:

```rust
#[test]
fn v01_config_without_store_stays_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "vault:\n  name: old-vault\n");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config.store.kind, StoreKind::Sqlite);
    assert_eq!(config.store.nexus.data_dir, "nexus-data");
}

#[test]
fn nexus_sandbox_file_defaults_profile() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "store:\n  kind: nexus-sandbox\n");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config.store.kind, StoreKind::NexusSandbox);
    assert_eq!(config.store.nexus.data_dir, "nexus-data");
    assert_eq!(config.store.nexus.command, "cairn-nexus-sandbox");
    assert_eq!(config.store.nexus.health_timeout_ms, 5_000);
}

#[test]
fn disabling_nexus_keeps_sqlite_usable() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(
        dir.path(),
        "store:\n  kind: sqlite\n  nexus:\n    command: \"\"\n",
    );
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config.store.kind, StoreKind::Sqlite);
}
```

- [ ] **Step 6: Run CLI config tests**

Run:

```bash
cargo nextest run -p cairn-cli --test config --locked
```

Expected: PASS.

- [ ] **Step 7: Commit Task 1**

Run:

```bash
git add crates/cairn-core/src/config/mod.rs crates/cairn-cli/tests/config.rs
git commit --only crates/cairn-core/src/config/mod.rs crates/cairn-cli/tests/config.rs -m "feat(config): add nexus sandbox profile"
```

---

## Task 2: Status Health IDL And Generated Types

**Files:**
- Modify: `crates/cairn-idl/schema/prelude/status.json`
- Modify: `crates/cairn-idl/tests/codegen_emit_sdk.rs`
- Modify: `crates/cairn-idl/tests/codegen_emit_mcp.rs`
- Generated: `crates/cairn-core/src/generated/status.rs`
- Generated: `crates/cairn-mcp/src/generated/schemas/prelude/status.json`

- [ ] **Step 1: Write failing codegen tests**

In `crates/cairn-idl/tests/codegen_emit_sdk.rs`, add:

```rust
#[test]
fn status_response_includes_split_health_types() {
    let files = emit_sdk::emit(&doc()).unwrap();
    let status = files
        .iter()
        .find(|f| {
            f.path
                .ends_with("crates/cairn-core/src/generated/status.rs")
        })
        .unwrap();
    let body = std::str::from_utf8(&status.bytes).unwrap();
    assert!(body.contains("pub struct StatusResponseHealth"));
    assert!(body.contains("pub struct StatusResponseHealthAuthorityDb"));
    assert!(body.contains("pub struct StatusResponseHealthNexusProjection"));
    assert!(body.contains("pub health: StatusResponseHealth"));
}
```

In `crates/cairn-idl/tests/codegen_emit_mcp.rs`, add:

```rust
#[test]
fn status_schema_requires_health() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let status = files
        .iter()
        .find(|f| {
            f.path
                .ends_with("crates/cairn-mcp/src/generated/schemas/prelude/status.json")
        })
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&status.bytes).unwrap();
    let required = parsed
        .get("required")
        .and_then(serde_json::Value::as_array)
        .expect("status required array");
    assert!(
        required.iter().any(|v| v.as_str() == Some("health")),
        "status response must require health"
    );
    assert!(parsed.pointer("/properties/health").is_some());
}
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo nextest run -p cairn-idl status_response_includes_split_health_types status_schema_requires_health --locked
```

Expected: FAIL. The first test should report that `StatusResponseHealth` is missing.

- [ ] **Step 3: Add the `health` schema**

In `crates/cairn-idl/schema/prelude/status.json`, change the root `required` array to:

```json
"required": ["contract", "server_info", "capabilities", "extensions", "health"]
```

Add this sibling property after `extensions`:

```json
    "health": {
      "type": "object",
      "additionalProperties": false,
      "required": ["authority_db", "nexus_projection"],
      "properties": {
        "authority_db": {
          "type": "object",
          "additionalProperties": false,
          "required": ["state", "path"],
          "properties": {
            "state": {
              "type": "string",
              "enum": ["healthy", "missing", "unavailable"]
            },
            "path": { "type": "string", "minLength": 1 },
            "reason": { "type": "string", "minLength": 1 }
          }
        },
        "nexus_projection": {
          "type": "object",
          "additionalProperties": false,
          "required": ["state"],
          "properties": {
            "state": {
              "type": "string",
              "enum": ["disabled", "healthy", "degraded"]
            },
            "data_dir": { "type": "string", "minLength": 1 },
            "endpoint": { "type": "string", "minLength": 1 },
            "reason": { "type": "string", "minLength": 1 }
          }
        }
      }
    }
```

- [ ] **Step 4: Regenerate artifacts**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

Expected: `cairn-codegen: wrote ... file(s).`

- [ ] **Step 5: Run codegen tests**

Run:

```bash
cargo nextest run -p cairn-idl status_response_includes_split_health_types status_schema_requires_health --locked
```

Expected: PASS.

- [ ] **Step 6: Run codegen drift check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

Expected: `cairn-codegen: clean`.

- [ ] **Step 7: Commit Task 2**

Run:

```bash
git add crates/cairn-idl/schema/prelude/status.json crates/cairn-idl/tests/codegen_emit_sdk.rs crates/cairn-idl/tests/codegen_emit_mcp.rs crates/cairn-core/src/generated/status.rs crates/cairn-mcp/src/generated/schemas/prelude/status.json
git commit --only crates/cairn-idl/schema/prelude/status.json crates/cairn-idl/tests/codegen_emit_sdk.rs crates/cairn-idl/tests/codegen_emit_mcp.rs crates/cairn-core/src/generated/status.rs crates/cairn-mcp/src/generated/schemas/prelude/status.json -m "feat(status): add split runtime health"
```

---

## Task 3: Status DB Health And Rendering

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml`
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Modify: `crates/cairn-cli/tests/status_snapshot.rs`

- [ ] **Step 1: Add failing status tests**

In `crates/cairn-cli/tests/status_snapshot.rs`, add:

```rust
#[test]
fn status_json_reports_missing_authority_db() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "missing");
    assert!(v["health"]["authority_db"]["path"].as_str().unwrap().ends_with(".cairn/cairn.db"));
    assert_eq!(v["health"]["nexus_projection"]["state"], "disabled");
}

#[test]
fn status_human_prints_split_health() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .current_dir(dir.path())
        .arg("status")
        .output()
        .expect("cairn status");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.contains("authority_db: missing"), "{stdout}");
    assert!(stdout.contains("nexus_projection: disabled"), "{stdout}");
}
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo nextest run -p cairn-cli --test status_snapshot status_json_reports_missing_authority_db status_human_prints_split_health --locked
```

Expected: FAIL because `health` is missing from JSON and human output.

- [ ] **Step 3: Add `rusqlite` as a direct CLI dependency**

In `crates/cairn-cli/Cargo.toml`, add under `[dependencies]`:

```toml
rusqlite = { workspace = true }
```

- [ ] **Step 4: Add status health helpers**

In `crates/cairn-cli/src/verbs/status.rs`, replace the imports with:

```rust
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::generated::common::Capabilities;
use cairn_core::generated::status::{
    StatusResponse, StatusResponseHealth, StatusResponseHealthAuthorityDb,
    StatusResponseHealthAuthorityDbState, StatusResponseHealthNexusProjection,
    StatusResponseHealthNexusProjectionState, StatusResponseServerInfo,
};

use crate::config::{self, CliOverrides};

use super::envelope::{emit_json, new_operation_id};
```

Add these helpers above `run`:

```rust
fn default_vault_path() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("reading current directory: {err}"))
}

fn authority_db_health(vault_path: &Path) -> StatusResponseHealthAuthorityDb {
    let db_path = vault_path.join(".cairn/cairn.db");
    let path = db_path.display().to_string();
    if !db_path.exists() {
        return StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Missing,
            path,
            reason: None,
        };
    }

    match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(_) => StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Healthy,
            path,
            reason: None,
        },
        Err(err) => StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Unavailable,
            path,
            reason: Some(err.to_string()),
        },
    }
}

fn disabled_nexus_projection() -> StatusResponseHealthNexusProjection {
    StatusResponseHealthNexusProjection {
        state: StatusResponseHealthNexusProjectionState::Disabled,
        data_dir: None,
        endpoint: None,
        reason: None,
    }
}

fn render_projection_human(projection: &StatusResponseHealthNexusProjection) -> String {
    match projection.state {
        StatusResponseHealthNexusProjectionState::Disabled => "disabled".to_owned(),
        StatusResponseHealthNexusProjectionState::Healthy => "healthy".to_owned(),
        StatusResponseHealthNexusProjectionState::Degraded => projection
            .reason
            .as_ref()
            .map_or_else(|| "degraded".to_owned(), |reason| format!("degraded ({reason})")),
    }
}

fn render_authority_human(authority: &StatusResponseHealthAuthorityDb) -> String {
    match authority.state {
        StatusResponseHealthAuthorityDbState::Healthy => "healthy".to_owned(),
        StatusResponseHealthAuthorityDbState::Missing => "missing".to_owned(),
        StatusResponseHealthAuthorityDbState::Unavailable => authority
            .reason
            .as_ref()
            .map_or_else(|| "unavailable".to_owned(), |reason| format!("unavailable ({reason})")),
    }
}
```

Replace `run` with:

```rust
#[must_use]
pub fn run(json: bool) -> ExitCode {
    let vault_path = match default_vault_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("cairn status: {err}");
            return ExitCode::from(1);
        }
    };
    run_for_vault(&vault_path, json)
}

#[must_use]
pub fn run_for_vault(vault_path: &Path, json: bool) -> ExitCode {
    let config = match config::load(vault_path, &CliOverrides::default()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("cairn status: {err:#}");
            return ExitCode::from(78);
        }
    };

    let incarnation = new_operation_id();
    let started_at = chrono_like_now();
    let health = StatusResponseHealth {
        authority_db: authority_db_health(vault_path),
        nexus_projection: disabled_nexus_projection(),
    };
    let resp = StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at: started_at.clone(),
            incarnation: incarnation.clone(),
        },
        capabilities: p0_capabilities(&config),
        extensions: vec![],
        health,
    };

    if json {
        emit_json(&resp);
    } else {
        println!("contract:    {}", resp.contract);
        println!("version:     {}", resp.server_info.version);
        println!("build:       {}", resp.server_info.build);
        println!("started_at:  {started_at}");
        println!("incarnation: {}", incarnation.0);
        println!("authority_db: {}", render_authority_human(&resp.health.authority_db));
        println!(
            "nexus_projection: {}",
            render_projection_human(&resp.health.nexus_projection)
        );
        if resp.capabilities.is_empty() {
            println!("capabilities: (none - store not wired in this P0 build)");
        } else {
            for cap in &resp.capabilities {
                println!(
                    "  capability: {}",
                    serde_json::to_string(cap).unwrap_or_default()
                );
            }
        }
    }
    ExitCode::SUCCESS
}
```

Change `p0_capabilities` to accept the loaded config:

```rust
fn p0_capabilities(_config: &cairn_core::config::CairnConfig) -> Vec<Capabilities> {
    vec![]
}
```

In the existing `p0_capabilities_returns_empty` unit test, change the call to:

```rust
        let caps = p0_capabilities(&cairn_core::config::CairnConfig::default());
```

- [ ] **Step 5: Run status tests**

Run:

```bash
cargo nextest run -p cairn-cli --test status_snapshot --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 3**

Run:

```bash
git add crates/cairn-cli/Cargo.toml crates/cairn-cli/src/verbs/status.rs crates/cairn-cli/tests/status_snapshot.rs
git commit --only crates/cairn-cli/Cargo.toml crates/cairn-cli/src/verbs/status.rs crates/cairn-cli/tests/status_snapshot.rs -m "feat(cli): report authoritative status health"
```

---

## Task 4: Nexus Health Client

**Files:**
- Modify: `crates/cairn-cli/src/lib.rs`
- Create: `crates/cairn-cli/src/nexus/mod.rs`

- [ ] **Step 1: Write failing health-client tests**

Create `crates/cairn-cli/src/nexus/mod.rs` with:

```rust
//! Nexus sandbox sidecar lifecycle and health checks.

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn spawn_health_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn http_endpoint_parses_host_and_port() {
        let endpoint = HttpEndpoint::parse("http://127.0.0.1:8765").unwrap();
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8765);
    }

    #[test]
    fn probe_health_reports_healthy_on_200() {
        let endpoint = spawn_health_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let result = probe_http_health(&endpoint, "/health", Duration::from_secs(1));
        assert!(matches!(result, ProbeResult::Healthy));
    }

    #[test]
    fn probe_health_reports_degraded_on_connection_failure() {
        let result = probe_http_health(
            "http://127.0.0.1:9",
            "/health",
            Duration::from_millis(25),
        );
        assert!(matches!(result, ProbeResult::Degraded(_)));
    }
}
```

In `crates/cairn-cli/src/lib.rs`, add:

```rust
pub mod nexus;
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo check -p cairn-cli --locked
```

Expected: FAIL with missing `HttpEndpoint`, `ProbeResult`, and `probe_http_health`.

- [ ] **Step 3: Implement endpoint parsing and health probing**

Add this code above the test module in `crates/cairn-cli/src/nexus/mod.rs`:

```rust
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Parsed HTTP endpoint for the Nexus sidecar health probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEndpoint {
    /// Hostname or IP address.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

/// Result of a Nexus sidecar health probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// Sidecar answered HTTP 200.
    Healthy,
    /// Sidecar could not be reached or returned a non-200 response.
    Degraded(String),
}

impl HttpEndpoint {
    /// Parse `http://host:port`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let Some(rest) = raw.strip_prefix("http://") else {
            return Err("endpoint must start with http://".into());
        };
        if rest.contains('/') {
            return Err("endpoint must not include a path; use health_path instead".into());
        }
        let Some((host, port_raw)) = rest.rsplit_once(':') else {
            return Err("endpoint must include an explicit port".into());
        };
        if host.is_empty() {
            return Err("endpoint host must not be empty".into());
        }
        let port = port_raw
            .parse::<u16>()
            .map_err(|err| format!("endpoint port is invalid: {err}"))?;
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    fn socket_addr(&self) -> Result<SocketAddr, String> {
        (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|err| format!("resolving endpoint: {err}"))?
            .next()
            .ok_or_else(|| "endpoint resolved to no socket addresses".to_owned())
    }
}

/// Probe the Nexus sidecar health endpoint using a minimal HTTP/1.1 request.
#[must_use]
pub fn probe_http_health(endpoint: &str, health_path: &str, timeout: Duration) -> ProbeResult {
    let endpoint = match HttpEndpoint::parse(endpoint) {
        Ok(endpoint) => endpoint,
        Err(err) => return ProbeResult::Degraded(err),
    };
    let addr = match endpoint.socket_addr() {
        Ok(addr) => addr,
        Err(err) => return ProbeResult::Degraded(err),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(stream) => stream,
        Err(err) => return ProbeResult::Degraded(format!("connecting health endpoint: {err}")),
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "GET {health_path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        endpoint.host, endpoint.port
    );
    if let Err(err) = stream.write_all(request.as_bytes()) {
        return ProbeResult::Degraded(format!("writing health request: {err}"));
    }
    let mut response = String::new();
    if let Err(err) = stream.read_to_string(&mut response) {
        return ProbeResult::Degraded(format!("reading health response: {err}"));
    }
    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        ProbeResult::Healthy
    } else {
        ProbeResult::Degraded("health endpoint returned non-200".into())
    }
}
```

- [ ] **Step 4: Run health-client tests**

Run:

```bash
cargo nextest run -p cairn-cli nexus:: --locked
```

Expected: PASS.

- [ ] **Step 5: Commit Task 4**

Run:

```bash
git add crates/cairn-cli/src/lib.rs crates/cairn-cli/src/nexus/mod.rs
git commit --only crates/cairn-cli/src/lib.rs crates/cairn-cli/src/nexus/mod.rs -m "feat(nexus): add sandbox health probe"
```

---

## Task 5: Nexus Sidecar Supervisor

**Files:**
- Modify: `crates/cairn-cli/src/nexus/mod.rs`

- [ ] **Step 1: Add failing supervisor tests**

Add these tests inside `crates/cairn-cli/src/nexus/mod.rs`:

```rust
    fn sleeper_command() -> (String, Vec<String>) {
        (
            "sh".to_owned(),
            vec![
                "-c".to_owned(),
                "trap 'exit 0' TERM; while :; do sleep 1; done".to_owned(),
            ],
        )
    }

    fn stubborn_command() -> (String, Vec<String>) {
        (
            "sh".to_owned(),
            vec![
                "-c".to_owned(),
                "trap '' TERM; while :; do sleep 1; done".to_owned(),
            ],
        )
    }

    #[test]
    fn supervisor_creates_data_dir_and_reaches_healthy_state() {
        let endpoint = spawn_health_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let dir = tempfile::tempdir().unwrap();
        let (command, args) = sleeper_command();
        let profile = SupervisorConfig {
            command,
            args,
            endpoint,
            health_path: "/health".into(),
            data_dir: dir.path().join("nexus-data"),
            sqlite_db: dir.path().join(".cairn/cairn.db"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(200),
        };
        let mut supervisor = NexusSupervisor::start(profile).unwrap();
        assert!(supervisor.data_dir().exists());
        assert!(matches!(supervisor.wait_until_healthy(), ProjectionProbe::Healthy));
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_reports_degraded_when_health_never_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let (command, args) = sleeper_command();
        let profile = SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:9".into(),
            health_path: "/health".into(),
            data_dir: dir.path().join("nexus-data"),
            sqlite_db: dir.path().join(".cairn/cairn.db"),
            health_timeout: Duration::from_millis(30),
            shutdown_timeout: Duration::from_millis(100),
        };
        let mut supervisor = NexusSupervisor::start(profile).unwrap();
        assert!(matches!(supervisor.wait_until_healthy(), ProjectionProbe::Degraded(_)));
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_force_kills_process_that_ignores_graceful_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (command, args) = stubborn_command();
        let profile = SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:9".into(),
            health_path: "/health".into(),
            data_dir: dir.path().join("nexus-data"),
            sqlite_db: dir.path().join(".cairn/cairn.db"),
            health_timeout: Duration::from_millis(30),
            shutdown_timeout: Duration::from_millis(30),
        };
        let mut supervisor = NexusSupervisor::start(profile).unwrap();
        supervisor.stop().unwrap();
    }
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo check -p cairn-cli --locked
```

Expected: FAIL with missing `SupervisorConfig`, `NexusSupervisor`, and `ProjectionProbe`.

- [ ] **Step 3: Implement the supervisor**

Add these imports to `crates/cairn-cli/src/nexus/mod.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};
```

Add these types after `ProbeResult`:

```rust
/// Supervisor runtime configuration after vault-relative paths are resolved.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Sidecar executable.
    pub command: String,
    /// Sidecar arguments.
    pub args: Vec<String>,
    /// Sidecar HTTP endpoint.
    pub endpoint: String,
    /// Sidecar health path.
    pub health_path: String,
    /// Resolved Nexus projection directory.
    pub data_dir: PathBuf,
    /// Resolved authoritative SQLite DB path.
    pub sqlite_db: PathBuf,
    /// Health wait timeout.
    pub health_timeout: Duration,
    /// Graceful shutdown timeout.
    pub shutdown_timeout: Duration,
}

/// Projection health result used by the supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionProbe {
    /// Health endpoint reached HTTP 200.
    Healthy,
    /// Sidecar process or health endpoint is unavailable.
    Degraded(String),
}

/// Running Nexus sidecar process owned by this CLI invocation.
pub struct NexusSupervisor {
    child: Child,
    config: SupervisorConfig,
}

impl NexusSupervisor {
    /// Start the sidecar process and create the projection directory.
    pub fn start(config: SupervisorConfig) -> Result<Self, String> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|err| format!("creating {}: {err}", config.data_dir.display()))?;
        let vault_dir = config
            .sqlite_db
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut command = Command::new(&config.command);
        command.args(&config.args);
        command.env("CAIRN_VAULT_DIR", &vault_dir);
        command.env("CAIRN_NEXUS_DATA_DIR", &config.data_dir);
        command.env("CAIRN_SQLITE_DB", &config.sqlite_db);
        let child = command
            .spawn()
            .map_err(|err| format!("spawning Nexus sidecar `{}`: {err}", config.command))?;
        Ok(Self { child, config })
    }

    /// Return the resolved projection directory.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.config.data_dir
    }

    /// Poll health until healthy or timeout.
    #[must_use]
    pub fn wait_until_healthy(&mut self) -> ProjectionProbe {
        let deadline = Instant::now() + self.config.health_timeout;
        let mut last = "health probe did not run".to_owned();
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return ProjectionProbe::Degraded(format!("sidecar exited with {status}"));
            }
            match probe_http_health(
                &self.config.endpoint,
                &self.config.health_path,
                Duration::from_millis(100),
            ) {
                ProbeResult::Healthy => return ProjectionProbe::Healthy,
                ProbeResult::Degraded(reason) => last = reason,
            }
            thread::sleep(Duration::from_millis(25));
        }
        ProjectionProbe::Degraded(last)
    }

    /// Stop the sidecar process. Escalates to force-kill when needed.
    pub fn stop(&mut self) -> Result<(), String> {
        if self.child.try_wait().map_err(|err| err.to_string())?.is_some() {
            return Ok(());
        }
        terminate_child(&mut self.child)?;
        let deadline = Instant::now() + self.config.shutdown_timeout;
        while Instant::now() < deadline {
            if self.child.try_wait().map_err(|err| err.to_string())?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.child
            .kill()
            .map_err(|err| format!("force-killing Nexus sidecar: {err}"))?;
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for NexusSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn terminate_child(child: &mut Child) -> Result<(), String> {
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status()
            .map_err(|err| format!("sending SIGTERM: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -TERM exited with {status}"))
        }
    }
    #[cfg(not(unix))]
    {
        child.kill().map_err(|err| format!("stopping child: {err}"))
    }
}
```

- [ ] **Step 4: Run supervisor tests**

Run:

```bash
cargo nextest run -p cairn-cli nexus:: --locked
```

Expected: PASS.

- [ ] **Step 5: Commit Task 5**

Run:

```bash
git add crates/cairn-cli/src/nexus/mod.rs
git commit --only crates/cairn-cli/src/nexus/mod.rs -m "feat(nexus): supervise sandbox sidecar"
```

---

## Task 6: Wire Nexus Projection Health Into Status

**Files:**
- Modify: `crates/cairn-cli/src/nexus/mod.rs`
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Modify: `crates/cairn-cli/tests/status_snapshot.rs`

- [ ] **Step 1: Add failing status integration tests**

In `crates/cairn-cli/tests/status_snapshot.rs`, add the test helper and tests:

```rust
fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

#[test]
fn status_json_reports_degraded_nexus_projection_without_failing_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(
        dir.path(),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    command: cairn-missing-nexus-sidecar-for-test\n    endpoint: http://127.0.0.1:9\n    health_timeout_ms: 25\n    shutdown_timeout_ms: 25\n",
    );
    let out = cli()
        .current_dir(dir.path())
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["health"]["authority_db"]["state"], "missing");
    assert_eq!(v["health"]["nexus_projection"]["state"], "degraded");
    assert_eq!(v["health"]["nexus_projection"]["data_dir"], dir.path().join("nexus-data").display().to_string());
    assert!(v["health"]["nexus_projection"]["reason"].as_str().unwrap().contains("spawning"));
}
```

- [ ] **Step 2: Run the red check**

Run:

```bash
cargo nextest run -p cairn-cli --test status_snapshot status_json_reports_degraded_nexus_projection_without_failing_sqlite --locked
```

Expected: FAIL because Nexus projection remains `disabled`.

- [ ] **Step 3: Add config-to-supervisor resolution**

In `crates/cairn-cli/src/nexus/mod.rs`, add:

```rust
use cairn_core::config::{CairnConfig, StoreKind};

/// One-shot status result for a configured Nexus projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionStatus {
    /// Projection state: disabled, healthy, or degraded.
    pub state: ProjectionStatusState,
    /// Resolved projection directory when Nexus is active.
    pub data_dir: Option<PathBuf>,
    /// Configured endpoint when Nexus is active.
    pub endpoint: Option<String>,
    /// Degraded reason when unavailable.
    pub reason: Option<String>,
}

/// Projection state for status assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionStatusState {
    /// Nexus profile is not active.
    Disabled,
    /// Sidecar is reachable.
    Healthy,
    /// Sidecar is unavailable.
    Degraded,
}

/// Resolve and evaluate the optional Nexus projection for one CLI invocation.
#[must_use]
pub fn evaluate_projection_status(vault_path: &Path, config: &CairnConfig) -> ProjectionStatus {
    if !matches!(config.store.kind, StoreKind::NexusSandbox) {
        return ProjectionStatus {
            state: ProjectionStatusState::Disabled,
            data_dir: None,
            endpoint: None,
            reason: None,
        };
    }

    let data_dir = resolve_data_dir(vault_path, &config.store.nexus.data_dir);
    let sqlite_db = vault_path.join(".cairn/cairn.db");
    let endpoint = config.store.nexus.endpoint.clone();
    let existing = probe_http_health(
        &endpoint,
        &config.store.nexus.health_path,
        Duration::from_millis(config.store.nexus.health_timeout_ms.min(250)),
    );
    if matches!(existing, ProbeResult::Healthy) {
        return ProjectionStatus {
            state: ProjectionStatusState::Healthy,
            data_dir: Some(data_dir),
            endpoint: Some(endpoint),
            reason: None,
        };
    }

    let profile = SupervisorConfig {
        command: config.store.nexus.command.clone(),
        args: config.store.nexus.args.clone(),
        endpoint: endpoint.clone(),
        health_path: config.store.nexus.health_path.clone(),
        data_dir: data_dir.clone(),
        sqlite_db,
        health_timeout: Duration::from_millis(config.store.nexus.health_timeout_ms),
        shutdown_timeout: Duration::from_millis(config.store.nexus.shutdown_timeout_ms),
    };

    match NexusSupervisor::start(profile) {
        Ok(mut supervisor) => match supervisor.wait_until_healthy() {
            ProjectionProbe::Healthy => ProjectionStatus {
                state: ProjectionStatusState::Healthy,
                data_dir: Some(data_dir),
                endpoint: Some(endpoint),
                reason: None,
            },
            ProjectionProbe::Degraded(reason) => ProjectionStatus {
                state: ProjectionStatusState::Degraded,
                data_dir: Some(data_dir),
                endpoint: Some(endpoint),
                reason: Some(reason),
            },
        },
        Err(reason) => ProjectionStatus {
            state: ProjectionStatusState::Degraded,
            data_dir: Some(data_dir),
            endpoint: Some(endpoint),
            reason: Some(reason),
        },
    }
}

fn resolve_data_dir(vault_path: &Path, data_dir: &str) -> PathBuf {
    let raw = Path::new(data_dir);
    if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        vault_path.join(raw)
    }
}
```

- [ ] **Step 4: Map Nexus projection status into generated status types**

In `crates/cairn-cli/src/verbs/status.rs`, import the module:

```rust
use crate::nexus::{self, ProjectionStatusState};
```

Add:

```rust
fn nexus_projection_health(
    vault_path: &Path,
    config: &cairn_core::config::CairnConfig,
) -> StatusResponseHealthNexusProjection {
    let projection = nexus::evaluate_projection_status(vault_path, config);
    let state = match projection.state {
        ProjectionStatusState::Disabled => StatusResponseHealthNexusProjectionState::Disabled,
        ProjectionStatusState::Healthy => StatusResponseHealthNexusProjectionState::Healthy,
        ProjectionStatusState::Degraded => StatusResponseHealthNexusProjectionState::Degraded,
    };
    StatusResponseHealthNexusProjection {
        state,
        data_dir: projection.data_dir.map(|p| p.display().to_string()),
        endpoint: projection.endpoint,
        reason: projection.reason,
    }
}
```

In `run_for_vault`, replace `nexus_projection: disabled_nexus_projection(),` with:

```rust
        nexus_projection: nexus_projection_health(vault_path, &config),
```

Keep `disabled_nexus_projection` only if unit tests still use it; otherwise remove it.

- [ ] **Step 5: Run status integration tests**

Run:

```bash
cargo nextest run -p cairn-cli --test status_snapshot --locked
```

Expected: PASS.

- [ ] **Step 6: Commit Task 6**

Run:

```bash
git add crates/cairn-cli/src/nexus/mod.rs crates/cairn-cli/src/verbs/status.rs crates/cairn-cli/tests/status_snapshot.rs
git commit --only crates/cairn-cli/src/nexus/mod.rs crates/cairn-cli/src/verbs/status.rs crates/cairn-cli/tests/status_snapshot.rs -m "feat(status): expose nexus projection health"
```

---

## Task 7: Final Verification And Issue Hygiene

**Files:**
- Modify only files changed by Tasks 1-6.

- [ ] **Step 1: Run targeted tests**

Run:

```bash
cargo nextest run -p cairn-core -p cairn-cli -p cairn-idl --locked
```

Expected: PASS.

- [ ] **Step 2: Run codegen drift check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

Expected: `cairn-codegen: clean`.

- [ ] **Step 3: Run doc tests**

Run:

```bash
cargo test --doc --workspace --locked
```

Expected: PASS.

- [ ] **Step 4: Run core boundary check**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: PASS with no `cairn-core` dependency violations.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git status --short
git diff --stat HEAD
```

Expected: only #104 files are changed, plus generated status artifacts. Pre-existing staged `crates/cairn-core/src/domain/flush_plan/*` files must remain untouched unless the user explicitly brings them into scope.

- [ ] **Step 6: Commit final fixes if verification required any**

Run this only if Step 1-4 required small corrections:

```bash
git add crates/cairn-core/src/config/mod.rs crates/cairn-cli crates/cairn-idl/schema/prelude/status.json crates/cairn-idl/tests crates/cairn-core/src/generated/status.rs crates/cairn-mcp/src/generated/schemas/prelude/status.json
git commit --only crates/cairn-core/src/config/mod.rs crates/cairn-cli crates/cairn-idl/schema/prelude/status.json crates/cairn-idl/tests crates/cairn-core/src/generated/status.rs crates/cairn-mcp/src/generated/schemas/prelude/status.json -m "fix: stabilize nexus sandbox status"
```

---

## Self-Review

Spec coverage:

- Opt-in `store.kind: nexus-sandbox` profile: Task 1.
- `nexus-data/` as derived projection: Tasks 1, 5, 6.
- Start, health-check, stop sidecar with mock process tests: Tasks 4 and 5.
- Status distinguishes DB authority from sidecar projection health: Tasks 2, 3, 6.
- Existing v0.1 vault config migration: Task 1.
- Disabling Nexus leaves SQLite usable: Task 1 and Task 6.
- Generated status JSON backed by IDL instead of hand-edited generated files: Task 2.
- Verification commands from the design spec: Task 7.

Type consistency:

- Config type is `NexusSandboxConfig`.
- Runtime config type is `SupervisorConfig`.
- Runtime status type is `ProjectionStatus`.
- Generated status health fields are `authority_db` and `nexus_projection`.

No intentional changes are planned for Nexus full hub/federation, search/apply protocol, or replacing SQLite authority.
