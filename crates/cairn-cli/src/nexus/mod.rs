//! Nexus sandbox sidecar lifecycle and health checks.

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
