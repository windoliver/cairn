//! Nexus sandbox sidecar lifecycle and health checks.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_STATUS_LINE_BYTES: usize = 1024;

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
}

/// Probe the Nexus sidecar health endpoint using a minimal HTTP/1.1 request.
#[must_use]
pub fn probe_http_health(endpoint: &str, health_path: &str, timeout: Duration) -> ProbeResult {
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
        match probe_addr(&endpoint, addr, health_path, timeout) {
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
        "GET {health_path} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
        endpoint.host, endpoint.port
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

    fn closed_endpoint() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
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
}
