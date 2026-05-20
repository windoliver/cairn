//! HTTP client for Cairn-facing Nexus projection endpoints.

use std::{
    io::{ErrorKind, Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::HttpEndpoint;

/// Batch apply request sent to the Nexus projection endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectionApplyRequest {
    /// Operation id for idempotency.
    pub operation_id: String,
    /// Projection target key.
    pub target: String,
    /// Items to project.
    pub items: Vec<ProjectionRequestItem>,
}

/// One projection request item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectionRequestItem {
    /// Authoritative record id.
    pub record_id: String,
    /// WAL sequence.
    pub wal_sequence: u64,
    /// Authoritative record hash.
    pub record_hash: String,
    /// Optional source hash.
    pub source_hash: Option<String>,
    /// Authoritative body text for lexical projection.
    pub body: String,
}

/// Sidecar response to an apply request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectionApplyResponse {
    /// Per-item response.
    pub items: Vec<ProjectionResponseItem>,
}

/// One projected item response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProjectionResponseItem {
    /// Authoritative record id.
    pub record_id: String,
    /// Record hash the sidecar used.
    pub record_hash: String,
    /// Optional source hash the sidecar used.
    #[serde(default)]
    pub source_hash: Option<String>,
    /// `current`, `failed`, `missing`, or `stale`.
    pub state: String,
    /// Failure reason when state is `failed`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Minimal blocking HTTP projection client.
#[derive(Debug, Clone)]
pub struct ProjectionClient {
    endpoint: String,
    apply_path: String,
    timeout: Duration,
}

impl ProjectionClient {
    /// Construct a projection client.
    #[must_use]
    pub fn new(endpoint: String, apply_path: String, timeout: Duration) -> Self {
        Self {
            endpoint,
            apply_path,
            timeout,
        }
    }

    /// POST one apply request.
    pub fn apply(
        &self,
        request: &ProjectionApplyRequest,
    ) -> Result<ProjectionApplyResponse, String> {
        validate_apply_path(&self.apply_path)?;
        let body = serde_json::to_vec(request)
            .map_err(|err| format!("serialize projection request: {err}"))?;
        let endpoint = HttpEndpoint::parse(&self.endpoint)?;
        self.apply_to_addrs(&endpoint, endpoint.socket_addrs()?, &body)
    }

    fn apply_to_addrs(
        &self,
        endpoint: &HttpEndpoint,
        addrs: Vec<SocketAddr>,
        body: &[u8],
    ) -> Result<ProjectionApplyResponse, String> {
        let mut failures = Vec::new();
        for addr in addrs {
            match self.apply_to_addr(endpoint, addr, body) {
                Ok(response) => return Ok(response),
                Err(err) => failures.push(format!("{addr}: {err}")),
            }
        }
        Err(format!(
            "all projection endpoint addresses failed: {}",
            failures.join("; ")
        ))
    }

    fn apply_to_addr(
        &self,
        endpoint: &HttpEndpoint,
        addr: SocketAddr,
        body: &[u8],
    ) -> Result<ProjectionApplyResponse, String> {
        let mut stream = TcpStream::connect_timeout(&addr, self.timeout)
            .map_err(|err| format!("connect projection endpoint: {err}"))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|err| format!("set projection read timeout: {err}"))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|err| format!("set projection write timeout: {err}"))?;
        let request_head = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.apply_path,
            endpoint.host,
            endpoint.port,
            body.len()
        );
        let mut raw_request = request_head.into_bytes();
        raw_request.extend_from_slice(body);
        stream
            .write_all(&raw_request)
            .map_err(|err| format!("write projection request: {err}"))?;

        let mut raw = Vec::new();
        read_response(&mut stream, &mut raw)?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "projection response missing HTTP body".to_owned())?;
        let status_line = raw
            .split(|byte| *byte == b'\n')
            .next()
            .ok_or_else(|| "projection response missing status".to_owned())?;
        if !is_success_status(status_line) {
            return Err("projection endpoint returned non-200".to_owned());
        }
        serde_json::from_slice(&raw[split + 4..])
            .map_err(|err| format!("parse projection response: {err}"))
    }
}

fn validate_apply_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("apply_path must not be empty".to_owned());
    }
    if !path.starts_with('/') {
        return Err("apply_path must start with /".to_owned());
    }
    if path
        .bytes()
        .any(|byte| byte == b' ' || byte.is_ascii_control())
    {
        return Err("apply_path must not contain spaces or control characters".to_owned());
    }
    Ok(())
}

fn read_response(stream: &mut TcpStream, raw: &mut Vec<u8>) -> Result<(), String> {
    let mut buf = [0_u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == ErrorKind::ConnectionReset && !raw.is_empty() => {
                return Ok(());
            }
            Err(err) => return Err(format!("read projection response: {err}")),
        }
    }
}

fn is_success_status(status_line: &[u8]) -> bool {
    let Ok(status_line) = std::str::from_utf8(status_line) else {
        return false;
    };
    let mut parts = status_line.split_ascii_whitespace();
    matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) && parts.next() == Some("200")
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        thread,
        time::Duration,
    };

    use super::{
        ProjectionApplyRequest, ProjectionApplyResponse, ProjectionClient, ProjectionResponseItem,
    };

    fn empty_request() -> ProjectionApplyRequest {
        ProjectionApplyRequest {
            operation_id: "01ARZ3NDEKTSV4RRFFQ69G5FAA".to_owned(),
            target: "bm25s_lexical".to_owned(),
            items: vec![],
        }
    }

    fn spawn_projection_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        format!("http://{addr}")
    }

    fn spawn_raw_server(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        format!("http://{addr}")
    }

    fn closed_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        addr
    }

    #[test]
    fn projection_client_posts_batch_and_parses_items() {
        let endpoint = spawn_projection_server(
            r#"{"items":[{"record_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","record_hash":"sha256:record-a","state":"current","reason":null}]}"#,
        );
        let client = ProjectionClient::new(
            endpoint,
            "/projection/apply".to_owned(),
            Duration::from_secs(1),
        );

        let response = client.apply(&empty_request()).expect("apply projection");

        assert_eq!(
            response,
            ProjectionApplyResponse {
                items: vec![ProjectionResponseItem {
                    record_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
                    record_hash: "sha256:record-a".to_owned(),
                    source_hash: None,
                    state: "current".to_owned(),
                    reason: None,
                }]
            }
        );
    }

    #[test]
    fn projection_client_tries_later_socket_addresses() {
        let endpoint_raw = spawn_projection_server(r#"{"items":[]}"#);
        let endpoint = crate::nexus::HttpEndpoint::parse(&endpoint_raw).expect("endpoint");
        let good_addr = endpoint
            .socket_addrs()
            .expect("addresses")
            .into_iter()
            .next()
            .expect("good address");
        let client = ProjectionClient::new(
            endpoint_raw,
            "/projection/apply".to_owned(),
            Duration::from_secs(1),
        );
        let request = empty_request();
        let body = serde_json::to_vec(&request).expect("serialize request");

        let response = client
            .apply_to_addrs(&endpoint, vec![closed_addr(), good_addr], &body)
            .expect("projection response");

        assert_eq!(response, ProjectionApplyResponse { items: vec![] });
    }

    #[test]
    fn projection_client_rejects_unsafe_apply_path() {
        for path in ["", "projection/apply", "/bad path", "/bad\r\nX-Test: 1"] {
            let client = ProjectionClient::new(
                "http://127.0.0.1:1".to_owned(),
                path.to_owned(),
                Duration::from_secs(1),
            );

            let err = client.apply(&empty_request()).expect_err("unsafe path");

            assert!(err.contains("apply_path"), "{err}");
        }
    }

    #[test]
    fn projection_client_rejects_non_exact_200_status() {
        let endpoint = spawn_raw_server(
            "HTTP/1.1 2000 Nope\r\nContent-Type: application/json\r\nContent-Length: 12\r\n\r\n{\"items\":[]}",
        );
        let client = ProjectionClient::new(
            endpoint,
            "/projection/apply".to_owned(),
            Duration::from_secs(1),
        );

        let err = client
            .apply(&empty_request())
            .expect_err("invalid status should fail");

        assert!(err.contains("non-200"), "{err}");
    }
}
