//! Nexus sandbox sidecar lifecycle and health checks.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cairn_core::config::{CairnConfig, StoreKind};

const MAX_STATUS_LINE_BYTES: usize = 1024;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(25);

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
pub enum ProjectionProbe {
    /// Sidecar answered HTTP 200.
    Healthy,
    /// Sidecar could not be reached or returned a non-200 response.
    Degraded(String),
}

/// Backward-compatible name for direct HTTP probe results.
pub type ProbeResult = ProjectionProbe;

/// Process and health settings for the Nexus sandbox sidecar supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Base HTTP endpoint, formatted as `http://host:port`.
    pub endpoint: String,
    /// Health endpoint path.
    pub health_path: String,
    /// Nexus sidecar data directory.
    pub data_dir: PathBuf,
    /// `SQLite` database path made visible to the child.
    pub sqlite_db: PathBuf,
    /// Maximum time to wait for health to recover.
    pub health_timeout: Duration,
    /// Maximum graceful shutdown window before force-kill.
    pub shutdown_timeout: Duration,
}

/// One-shot status result for the optional Nexus projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionStatus {
    /// Projection status state.
    pub state: ProjectionStatusState,
    /// Resolved projection directory when the Nexus profile is active.
    pub data_dir: Option<PathBuf>,
    /// Configured endpoint when the Nexus profile is active.
    pub endpoint: Option<String>,
    /// Degraded reason when unavailable.
    pub reason: Option<String>,
}

/// Projection state for status assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionStatusState {
    /// Nexus projection profile is not active.
    Disabled,
    /// Nexus sidecar is reachable.
    Healthy,
    /// Nexus sidecar is unavailable.
    Degraded,
}

/// Running Nexus sandbox sidecar process.
#[derive(Debug)]
pub struct NexusSupervisor {
    /// Spawned child process.
    pub child: Child,
    /// Supervisor configuration used to launch the child.
    pub config: SupervisorConfig,
}

impl NexusSupervisor {
    /// Create the sidecar data directory and spawn the configured process.
    pub fn start(config: SupervisorConfig) -> std::io::Result<Self> {
        let vault_dir = derive_vault_dir(&config.sqlite_db)?;
        fs::create_dir_all(&config.data_dir)?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .env("CAIRN_VAULT_DIR", vault_dir)
            .env("CAIRN_NEXUS_DATA_DIR", &config.data_dir)
            .env("CAIRN_NEXUS_ENDPOINT", &config.endpoint)
            .env("CAIRN_NEXUS_HEALTH_PATH", &config.health_path)
            .env("CAIRN_SQLITE_DB", &config.sqlite_db);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            command.process_group(0);
        }
        let child = command.spawn()?;
        Ok(Self { child, config })
    }

    /// Poll the configured HTTP health endpoint until it is healthy or times out.
    pub fn wait_until_healthy(&mut self) -> ProjectionProbe {
        let deadline = Instant::now()
            .checked_add(self.config.health_timeout)
            .unwrap_or_else(Instant::now);
        let mut last_reason = None;

        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    return ProjectionProbe::Degraded(format!(
                        "supervisor process exited before health recovered: {status}"
                    ));
                }
                Ok(None) => {}
                Err(err) => {
                    return ProjectionProbe::Degraded(format!(
                        "checking supervisor process status: {err}"
                    ));
                }
            }

            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return ProjectionProbe::Degraded(format!(
                    "health did not recover before timeout: {}",
                    last_reason
                        .as_deref()
                        .unwrap_or("health endpoint was not probed")
                ));
            };
            if remaining.is_zero() {
                return ProjectionProbe::Degraded(format!(
                    "health did not recover before timeout: {}",
                    last_reason
                        .as_deref()
                        .unwrap_or("health endpoint was not probed")
                ));
            }

            match probe_http_health(&self.config.endpoint, &self.config.health_path, remaining) {
                ProjectionProbe::Healthy if Instant::now() <= deadline => {
                    return ProjectionProbe::Healthy;
                }
                ProjectionProbe::Healthy => {
                    return ProjectionProbe::Degraded(
                        "health did not recover before timeout".to_owned(),
                    );
                }
                ProjectionProbe::Degraded(reason) => last_reason = Some(reason),
            }

            if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                thread::sleep(remaining.min(HEALTH_POLL_INTERVAL));
            }
        }
    }

    /// Stop the sidecar process, escalating to force-kill after the shutdown timeout.
    pub fn stop(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            #[cfg(not(unix))]
            return Ok(());
        }

        self.terminate_gracefully()?;
        let deadline = Instant::now() + self.config.shutdown_timeout;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                #[cfg(not(unix))]
                return Ok(());
                #[cfg(unix)]
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        self.force_kill()?;
        let _ = self.child.wait()?;
        Ok(())
    }

    #[cfg(unix)]
    fn terminate_gracefully(&mut self) -> std::io::Result<()> {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{}", self.child.id()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() || self.child.try_wait()?.is_some() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "kill -TERM exited with status {status}"
            )))
        }
    }

    #[cfg(not(unix))]
    fn terminate_gracefully(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    #[cfg(unix)]
    fn force_kill(&mut self) -> std::io::Result<()> {
        let status = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{}", self.child.id()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() || self.child.try_wait()?.is_some() {
            Ok(())
        } else {
            self.child.kill()
        }
    }

    #[cfg(not(unix))]
    fn force_kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

fn derive_vault_dir(sqlite_db: &Path) -> std::io::Result<&Path> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sqlite_db must be <vault>/.cairn/cairn.db",
        )
    };
    if sqlite_db.file_name().and_then(|name| name.to_str()) != Some("cairn.db") {
        return Err(invalid());
    }
    let cairn_dir = sqlite_db.parent().ok_or_else(invalid)?;
    if cairn_dir.file_name().and_then(|name| name.to_str()) != Some(".cairn") {
        return Err(invalid());
    }
    cairn_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(invalid)
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
    let endpoint = config.store.nexus.endpoint.clone();
    match probe_http_health(
        &endpoint,
        &config.store.nexus.health_path,
        Duration::from_millis(config.store.nexus.health_timeout_ms),
    ) {
        ProbeResult::Healthy => ProjectionStatus {
            state: ProjectionStatusState::Healthy,
            data_dir: Some(data_dir),
            endpoint: Some(endpoint),
            reason: None,
        },
        ProbeResult::Degraded(reason) => ProjectionStatus {
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

impl Drop for NexusSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
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
        let (host, port_raw) = if let Some(ipv6_rest) = rest.strip_prefix('[') {
            let Some((host, after_host)) = ipv6_rest.split_once(']') else {
                return Err("IPv6 endpoint host must close with ]".into());
            };
            let Some(port_raw) = after_host.strip_prefix(':') else {
                return Err("endpoint must include an explicit port".into());
            };
            (host, port_raw)
        } else {
            let Some((host, port_raw)) = rest.rsplit_once(':') else {
                return Err("endpoint must include an explicit port".into());
            };
            (host, port_raw)
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

    fn socket_addrs(&self) -> Result<Vec<SocketAddr>, String> {
        let addrs = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|err| format!("resolving endpoint: {err}"))?
            .collect::<Vec<_>>();
        if addrs.is_empty() {
            Err("endpoint resolved to no socket addresses".to_owned())
        } else {
            Ok(addrs)
        }
    }

    fn host_header_value(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Probe the Nexus sidecar health endpoint using a minimal HTTP/1.1 request.
#[must_use]
pub fn probe_http_health(endpoint: &str, health_path: &str, timeout: Duration) -> ProbeResult {
    if timeout.is_zero() {
        return ProbeResult::Degraded("health probe timed out".into());
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    if let Err(err) = validate_health_path(health_path) {
        return ProbeResult::Degraded(err);
    }
    let endpoint = match HttpEndpoint::parse(endpoint) {
        Ok(endpoint) => endpoint,
        Err(err) => return ProbeResult::Degraded(err),
    };
    let addrs = match endpoint.socket_addrs() {
        Ok(addrs) => addrs,
        Err(err) => return ProbeResult::Degraded(err),
    };

    let mut failures = Vec::new();
    for addr in addrs {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            failures.push("health probe timed out".to_owned());
            break;
        };
        if remaining.is_zero() {
            failures.push("health probe timed out".to_owned());
            break;
        }
        match probe_addr(&endpoint, addr, health_path, remaining) {
            ProbeResult::Healthy => return ProbeResult::Healthy,
            ProbeResult::Degraded(reason) => failures.push(format!("{addr}: {reason}")),
        }
    }

    ProbeResult::Degraded(format!(
        "all health endpoint addresses failed: {}",
        failures.join("; ")
    ))
}

fn validate_health_path(health_path: &str) -> Result<(), String> {
    if health_path.is_empty() {
        return Err("health_path must not be empty".into());
    }
    if health_path
        .bytes()
        .any(|byte| byte == b' ' || byte.is_ascii_control())
    {
        return Err("health_path must not contain spaces or control characters".into());
    }
    Ok(())
}

fn probe_addr(
    endpoint: &HttpEndpoint,
    addr: SocketAddr,
    health_path: &str,
    timeout: Duration,
) -> ProbeResult {
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(stream) => stream,
        Err(err) => return ProbeResult::Degraded(format!("connecting health endpoint: {err}")),
    };
    if let Err(err) = stream.set_read_timeout(Some(timeout)) {
        return ProbeResult::Degraded(format!("setting health read timeout: {err}"));
    }
    if let Err(err) = stream.set_write_timeout(Some(timeout)) {
        return ProbeResult::Degraded(format!("setting health write timeout: {err}"));
    }
    let request = format!(
        "GET {health_path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        endpoint.host_header_value()
    );
    if let Err(err) = stream.write_all(request.as_bytes()) {
        return ProbeResult::Degraded(format!("writing health request: {err}"));
    }

    match read_status_line(&mut stream) {
        Ok(status_line) if is_success_status(&status_line) => ProbeResult::Healthy,
        Ok(_) => ProbeResult::Degraded("health endpoint returned non-200".into()),
        Err(err) => ProbeResult::Degraded(err),
    }
}

fn read_status_line(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut status_line = Vec::with_capacity(64);
    let mut byte = [0_u8; 1];
    while status_line.len() < MAX_STATUS_LINE_BYTES {
        match stream.read(&mut byte) {
            Ok(0) if status_line.is_empty() => {
                return Err("health endpoint closed before status line".into());
            }
            Ok(0) => return Ok(status_line),
            Ok(_) => {
                status_line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(status_line);
                }
            }
            Err(err) => return Err(format!("reading health status line: {err}")),
        }
    }
    Err("health status line exceeded 1024 bytes".into())
}

fn is_success_status(status_line: &[u8]) -> bool {
    status_line.starts_with(b"HTTP/1.1 200") || status_line.starts_with(b"HTTP/1.0 200")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

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

    fn closed_endpoint() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    fn supervisor_config(
        data_dir: &Path,
        endpoint: String,
        health_timeout: Duration,
    ) -> SupervisorConfig {
        let (command, args) = sleeper_command();
        SupervisorConfig {
            command,
            args,
            endpoint,
            health_path: "/health".to_owned(),
            data_dir: data_dir.to_path_buf(),
            sqlite_db: data_dir.parent().unwrap_or(data_dir).join("cairn.db"),
            health_timeout,
            shutdown_timeout: Duration::from_millis(200),
        }
    }

    #[cfg(unix)]
    fn sleeper_command() -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), "sleep 10".to_owned()],
        )
    }

    #[cfg(windows)]
    fn sleeper_command() -> (String, Vec<String>) {
        (
            "cmd".to_owned(),
            vec!["/C".to_owned(), "ping -n 11 127.0.0.1 >NUL".to_owned()],
        )
    }

    #[cfg(unix)]
    fn stubborn_command() -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), "trap '' TERM; exec sleep 30".to_owned()],
        )
    }

    #[cfg(unix)]
    fn graceful_term_command() -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                "trap 'exit 0' TERM; while true; do sleep 1; done".to_owned(),
            ],
        )
    }

    #[cfg(unix)]
    fn descendant_command(pid_file: &Path) -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                format!("sleep 30 & echo $! > {}; wait", shell_quote(pid_file)),
            ],
        )
    }

    #[cfg(unix)]
    fn detached_descendant_command(pid_file: &Path) -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                format!("sleep 30 & echo $! > {}; exit 0", shell_quote(pid_file)),
            ],
        )
    }

    fn test_harness_helper_command() -> (String, Vec<String>) {
        (
            std::env::current_exe()
                .expect("current test executable")
                .display()
                .to_string(),
            vec![
                "nexus_sidecar_helper_process".to_owned(),
                "--nocapture".to_owned(),
            ],
        )
    }

    #[cfg(windows)]
    fn stubborn_command() -> (String, Vec<String>) {
        (
            "powershell".to_owned(),
            vec![
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                "Start-Sleep -Seconds 30".to_owned(),
            ],
        )
    }

    fn env_capture_command(output: &Path) -> (String, Vec<String>) {
        #[cfg(unix)]
        {
            (
                "/bin/sh".to_owned(),
                vec![
                    "-c".to_owned(),
                    format!(
                        "printf '%s\n%s\n%s\n%s\n%s\n' \"$CAIRN_VAULT_DIR\" \"$CAIRN_NEXUS_DATA_DIR\" \"$CAIRN_SQLITE_DB\" \"$CAIRN_NEXUS_ENDPOINT\" \"$CAIRN_NEXUS_HEALTH_PATH\" > {}; sleep 10",
                        shell_quote(output)
                    ),
                ],
            )
        }
        #[cfg(windows)]
        {
            (
                "powershell".to_owned(),
                vec![
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    format!(
                        "[IO.File]::WriteAllLines('{}', @($env:CAIRN_VAULT_DIR, $env:CAIRN_NEXUS_DATA_DIR, $env:CAIRN_SQLITE_DB, $env:CAIRN_NEXUS_ENDPOINT, $env:CAIRN_NEXUS_HEALTH_PATH)); Start-Sleep -Seconds 10",
                        output.display()
                    ),
                ],
            )
        }
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if path.is_file() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {}", path.display());
    }

    fn reserve_loopback_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn nexus_sidecar_helper_process() {
        let Ok(data_dir) = std::env::var("CAIRN_NEXUS_DATA_DIR") else {
            return;
        };
        let data_dir = Path::new(&data_dir);
        let port_file = data_dir.join("health-port");
        if !port_file.is_file() {
            return;
        }

        let port = fs::read_to_string(&port_file)
            .unwrap()
            .trim()
            .parse::<u16>()
            .unwrap();
        let env_capture = [
            std::env::var("CAIRN_VAULT_DIR").unwrap_or_default(),
            std::env::var("CAIRN_NEXUS_DATA_DIR").unwrap_or_default(),
            std::env::var("CAIRN_SQLITE_DB").unwrap_or_default(),
            std::env::var("CAIRN_NEXUS_ENDPOINT").unwrap_or_default(),
            std::env::var("CAIRN_NEXUS_HEALTH_PATH").unwrap_or_default(),
        ]
        .join("\n");
        fs::write(data_dir.join("helper-env.txt"), env_capture).unwrap();

        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        fs::write(data_dir.join("helper-ready"), b"ready").unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0_u8; 512];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn nexus_stop_stderr_helper_process() {
        if std::env::var("CAIRN_RUN_STOP_STDERR_HELPER").as_deref() != Ok("1") {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let (command, args) = graceful_term_command();
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: data_dir.parent().unwrap().join("cairn.db"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();

        supervisor.stop().unwrap();
    }

    fn spawn_delayed_health_server(response_delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                thread::sleep(response_delay);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        format!("http://{addr}")
    }

    fn wait_until_not_running(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if !process_is_running(pid) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        !process_is_running(pid)
    }

    #[test]
    fn supervisor_starts_mock_sidecar_process_serving_real_health_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let vault_dir = tmp.path().join("vault");
        let data_dir = vault_dir.join("nexus-data");
        let sqlite_db = vault_dir.join(".cairn").join("cairn.db");
        fs::create_dir_all(&data_dir).unwrap();
        let port = reserve_loopback_port();
        let endpoint = format!("http://127.0.0.1:{port}");
        fs::write(data_dir.join("health-port"), port.to_string()).unwrap();
        let (command, args) = test_harness_helper_command();
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: endpoint.clone(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: sqlite_db.clone(),
            health_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();

        assert!(matches!(
            supervisor.wait_until_healthy(),
            ProjectionProbe::Healthy
        ));
        wait_for_file(&data_dir.join("helper-ready"));
        wait_for_file(&data_dir.join("helper-env.txt"));
        let env_capture = fs::read_to_string(data_dir.join("helper-env.txt")).unwrap();
        let lines = env_capture.lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                vault_dir.to_str().unwrap(),
                data_dir.to_str().unwrap(),
                sqlite_db.to_str().unwrap(),
                endpoint.as_str(),
                "/health",
            ]
        );
        let pid = supervisor.child.id();

        supervisor.stop().unwrap();

        assert!(!process_is_running(pid));
    }

    #[test]
    fn supervisor_creates_data_dir_and_reaches_healthy_state() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let endpoint = spawn_health_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut supervisor = NexusSupervisor::start(supervisor_config(
            &data_dir,
            endpoint,
            Duration::from_secs(1),
        ))
        .unwrap();

        assert!(data_dir.is_dir());
        assert!(matches!(
            supervisor.wait_until_healthy(),
            ProjectionProbe::Healthy
        ));

        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_passes_exact_environment_to_child() {
        let tmp = tempfile::tempdir().unwrap();
        let vault_dir = tmp.path().join("vault");
        let data_dir = vault_dir.join(".cairn").join("nexus-data");
        let sqlite_db = vault_dir.join(".cairn").join("cairn.db");
        let env_file = tmp.path().join("env.txt");
        let (command, args) = env_capture_command(&env_file);
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: sqlite_db.clone(),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(100),
        })
        .unwrap();

        wait_for_file(&env_file);
        let captured = fs::read_to_string(&env_file).unwrap();
        let lines = captured.lines().collect::<Vec<_>>();

        assert_eq!(
            lines,
            vec![
                vault_dir.to_str().unwrap(),
                data_dir.to_str().unwrap(),
                sqlite_db.to_str().unwrap(),
                "http://127.0.0.1:1",
                "/health",
            ]
        );
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_rejects_sqlite_path_without_vault_root() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("nexus-data");
        let (command, args) = sleeper_command();
        let err = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir,
            sqlite_db: Path::new("cairn.db").to_path_buf(),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(100),
        })
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn supervisor_rejects_sqlite_path_outside_authority_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let (command, args) = sleeper_command();
        let err = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: data_dir.join("nexus.sqlite"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(100),
        })
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn supervisor_reports_degraded_when_health_never_recovers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut supervisor = NexusSupervisor::start(supervisor_config(
            &tmp.path().join(".cairn").join("nexus-data"),
            closed_endpoint(),
            Duration::from_millis(100),
        ))
        .unwrap();

        let probe = supervisor.wait_until_healthy();

        assert!(matches!(probe, ProjectionProbe::Degraded(reason) if reason.contains("health")));
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_does_not_report_healthy_after_health_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let mut supervisor = NexusSupervisor::start(supervisor_config(
            &tmp.path().join(".cairn").join("nexus-data"),
            spawn_delayed_health_server(Duration::from_millis(20)),
            Duration::from_millis(5),
        ))
        .unwrap();

        let probe = supervisor.wait_until_healthy();

        assert!(matches!(probe, ProjectionProbe::Degraded(reason) if reason.contains("timeout")));
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_uses_configured_health_timeout_for_each_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let mut supervisor = NexusSupervisor::start(supervisor_config(
            &tmp.path().join(".cairn").join("nexus-data"),
            spawn_delayed_health_server(Duration::from_millis(75)),
            Duration::from_millis(300),
        ))
        .unwrap();

        let probe = supervisor.wait_until_healthy();

        assert!(matches!(probe, ProjectionProbe::Healthy));
        supervisor.stop().unwrap();
    }

    #[test]
    fn supervisor_force_kills_process_that_ignores_graceful_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let (command, args) = stubborn_command();
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: data_dir.parent().unwrap().join("cairn.db"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(100),
        })
        .unwrap();
        let pid = supervisor.child.id();

        supervisor.stop().unwrap();

        assert!(!process_is_running(pid));
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_stop_does_not_leak_expected_kill_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let fake_bin = tmp.path().join("bin");
        fs::create_dir(&fake_bin).unwrap();
        let fake_kill = fake_bin.join("kill");
        fs::write(
            &fake_kill,
            "#!/bin/sh\necho fake-kill-stderr:$* >&2\nexec /bin/kill \"$@\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_kill).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_kill, permissions).unwrap();

        let mut paths = vec![fake_bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .arg("nexus::tests::nexus_stop_stderr_helper_process")
            .arg("--exact")
            .arg("--nocapture")
            .env("CAIRN_RUN_STOP_STDERR_HELPER", "1")
            .env("PATH", path)
            .output()
            .expect("run helper test process");
        assert!(
            output.status.success(),
            "helper failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("fake-kill-stderr:"),
            "expected supervisor cleanup not to leak kill stderr, got:\n{stderr}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_gracefully_stops_descendant_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let child_pid_file = tmp.path().join("child.pid");
        let (command, args) = descendant_command(&child_pid_file);
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: data_dir.parent().unwrap().join("cairn.db"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();

        wait_for_file(&child_pid_file);
        let child_pid = fs::read_to_string(&child_pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();

        supervisor.stop().unwrap();

        assert!(!process_is_running(child_pid));
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_stop_cleans_descendant_after_wrapper_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join(".cairn").join("nexus-data");
        let child_pid_file = tmp.path().join("child.pid");
        let (command, args) = detached_descendant_command(&child_pid_file);
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command,
            args,
            endpoint: "http://127.0.0.1:1".to_owned(),
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db: data_dir.parent().unwrap().join("cairn.db"),
            health_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_millis(100),
        })
        .unwrap();

        wait_for_file(&child_pid_file);
        let child_pid = fs::read_to_string(&child_pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && supervisor.child.try_wait().unwrap().is_none() {
            thread::sleep(Duration::from_millis(10));
        }

        supervisor.stop().unwrap();

        let cleaned_up = wait_until_not_running(child_pid);
        if !cleaned_up {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(child_pid.to_string())
                .status();
        }
        assert!(cleaned_up, "descendant process {child_pid} still running");
    }

    fn process_is_running(pid: u32) -> bool {
        #[cfg(unix)]
        {
            Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .status()
                .is_ok_and(|status| status.success())
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args([
                    "/C",
                    &format!("tasklist /FI \"PID eq {pid}\" | findstr {pid}"),
                ])
                .status()
                .is_ok_and(|status| status.success())
        }
    }

    #[test]
    fn http_endpoint_parses_host_and_port() {
        let endpoint = HttpEndpoint::parse("http://127.0.0.1:8765").unwrap();
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8765);
    }

    #[test]
    fn http_endpoint_parses_bracketed_ipv6_host_and_port() {
        let endpoint = HttpEndpoint::parse("http://[::1]:8765").unwrap();
        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.port, 8765);
    }

    #[test]
    fn probe_health_brackets_ipv6_literal_in_host_header() {
        let listener = match TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(err) => panic!("bind IPv6 loopback health server: {err}"),
        };
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let n = stream.read(&mut buf).unwrap();
                tx.send(String::from_utf8_lossy(&buf[..n]).into_owned())
                    .unwrap();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let result = probe_http_health(
            &format!("http://[::1]:{port}"),
            "/health",
            Duration::from_secs(1),
        );

        assert!(matches!(result, ProbeResult::Healthy));
        let request = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
        assert!(
            request.contains(&format!("Host: [::1]:{port}\r\n")),
            "IPv6 Host header must be bracketed, got: {request:?}"
        );
    }

    #[test]
    fn probe_health_reports_healthy_on_200() {
        let endpoint = spawn_health_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let result = probe_http_health(&endpoint, "/health", Duration::from_secs(1));
        assert!(matches!(result, ProbeResult::Healthy));
    }

    #[test]
    fn probe_health_reports_healthy_on_ipv6_loopback_when_available() {
        let listener = match TcpListener::bind("[::1]:0") {
            Ok(listener) => listener,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(err) => panic!("bind IPv6 loopback health server: {err}"),
        };
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let result = probe_http_health(
            &format!("http://[::1]:{port}"),
            "/health",
            Duration::from_secs(1),
        );

        assert!(matches!(result, ProbeResult::Healthy));
    }

    #[test]
    fn probe_health_reports_healthy_without_waiting_for_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n");
                thread::sleep(Duration::from_millis(200));
            }
        });

        let result = probe_http_health(
            &format!("http://{addr}"),
            "/health",
            Duration::from_millis(50),
        );

        assert!(matches!(result, ProbeResult::Healthy));
    }

    #[test]
    fn probe_health_reports_degraded_on_connection_failure() {
        let result = probe_http_health(&closed_endpoint(), "/health", Duration::from_millis(25));
        assert!(matches!(result, ProbeResult::Degraded(_)));
    }

    #[test]
    fn probe_health_rejects_invalid_health_path_before_connecting() {
        let result = probe_http_health(&closed_endpoint(), "/bad path", Duration::from_millis(25));
        assert!(matches!(
            result,
            ProbeResult::Degraded(reason) if reason.contains("health_path")
        ));
    }

    #[test]
    fn evaluate_projection_status_honors_configured_health_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CairnConfig::default();
        config.store.kind = StoreKind::NexusSandbox;
        config.store.nexus.endpoint = spawn_delayed_health_server(Duration::from_millis(300));
        config.store.nexus.health_timeout_ms = 1_000;

        let status = evaluate_projection_status(tmp.path(), &config);

        assert_eq!(status.state, ProjectionStatusState::Healthy);
    }
}
