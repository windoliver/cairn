//! HTTP client for Cairn-facing Nexus projection endpoints.

use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{HttpEndpoint, ProbeResult};

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
        let body = serde_json::to_vec(request)
            .map_err(|err| format!("serialize projection request: {err}"))?;
        let endpoint = HttpEndpoint::parse(&self.endpoint)?;
        let addr = endpoint
            .socket_addrs()?
            .into_iter()
            .next()
            .ok_or_else(|| "endpoint resolved to no socket addresses".to_owned())?;
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
        stream
            .write_all(request_head.as_bytes())
            .and_then(|()| stream.write_all(&body))
            .map_err(|err| format!("write projection request: {err}"))?;

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|err| format!("read projection response: {err}"))?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "projection response missing HTTP body".to_owned())?;
        let status_line = raw
            .split(|byte| *byte == b'\n')
            .next()
            .ok_or_else(|| "projection response missing status".to_owned())?;
        match ProbeResult::Healthy {
            ProbeResult::Healthy
                if status_line.starts_with(b"HTTP/1.1 200")
                    || status_line.starts_with(b"HTTP/1.0 200") => {}
            _ => return Err("projection endpoint returned non-200".to_owned()),
        }
        serde_json::from_slice(&raw[split + 4..])
            .map_err(|err| format!("parse projection response: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    use super::{
        ProjectionApplyRequest, ProjectionApplyResponse, ProjectionClient, ProjectionResponseItem,
    };

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
                stream.write_all(response.as_bytes()).expect("write response");
            }
        });
        format!("http://{addr}")
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

        let response = client
            .apply(&ProjectionApplyRequest {
                operation_id: "01ARZ3NDEKTSV4RRFFQ69G5FAA".to_owned(),
                target: "bm25s_lexical".to_owned(),
                items: vec![],
            })
            .expect("apply projection");

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
}
