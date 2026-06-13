//! MCP wire tests for mutating-verb dispatch through a wired
//! [`cairn_mcp::MutatingVerbHost`].
//!
//! The MCP adapter must not grow a parallel implementation of the signed
//! write path (CLAUDE.md §4 invariant 3) — instead the embedding process
//! (`cairn mcp` in `cairn-cli`) injects a host that routes `ingest`,
//! `capture_trace`, and `forget` through the same domain logic the CLI
//! verbs use. These tests pin the routing contract with a fake host:
//!
//! - wired host + valid args → the host is called and its `Response`
//!   envelope is returned verbatim;
//! - wired host + malformed args → the generated arg parser rejects
//!   before the host is consulted (fail-closed, no partial writes);
//! - no host → the pre-existing typed aborted stub envelope.
#![allow(missing_docs)]

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::capture_trace::{CaptureTraceArgs, CaptureTraceData};
use cairn_core::generated::verbs::forget::{ForgetArgs, ForgetData};
use cairn_core::generated::verbs::ingest::{IngestArgs, IngestData};
use cairn_mcp::{CairnMcpHandler, MutatingVerbHost};
use rmcp::ServiceExt as _;
use tokio::io::BufReader;

use common::{do_initialize, recv_frame, send_frame};

const RECORD_ULID: &str = "01HQZX9F5N0000000000000000";
const TRACE_ULID: &str = "01HQZX9F5N0000000000000001";

fn committed(verb: ResponseVerb, data: ResponseData) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(data),
        error: None,
        operation_id: Ulid("01HQZX9F5N0000000000000002".to_owned()),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb,
    }
}

/// Fake host that records every call and answers canned committed envelopes.
#[derive(Default)]
struct RecordingHost {
    calls: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MutatingVerbHost for RecordingHost {
    async fn ingest(&self, args: IngestArgs) -> Response {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("ingest:{}", args.body.as_deref().unwrap_or("")));
        committed(
            ResponseVerb::Ingest,
            ResponseData::Ingest(IngestData {
                cache_hits: None,
                cache_misses: None,
                cache_writes: None,
                files_processed: None,
                jsonl_summary: None,
                plan_ref: None,
                record_id: Ulid(RECORD_ULID.to_owned()),
                recording_summary: None,
                session_id: "default".to_owned(),
            }),
        )
    }

    async fn capture_trace(&self, args: CaptureTraceArgs) -> Response {
        self.calls.lock().expect("calls lock").push(format!(
            "capture_trace:{}",
            args.from.as_deref().unwrap_or("")
        ));
        committed(
            ResponseVerb::CaptureTrace,
            ResponseData::CaptureTrace(CaptureTraceData {
                failed_turns: Vec::new(),
                trace_id: Ulid(TRACE_ULID.to_owned()),
            }),
        )
    }

    async fn forget(&self, args: ForgetArgs) -> Response {
        let label = match &args {
            ForgetArgs::Record { record_id, .. } => format!("forget:record:{}", record_id.0),
            ForgetArgs::Session { session_id, .. } => format!("forget:session:{session_id}"),
            ForgetArgs::Scope { .. } => "forget:scope".to_owned(),
            _ => "forget:unknown".to_owned(),
        };
        self.calls.lock().expect("calls lock").push(label);
        committed(
            ResponseVerb::Forget,
            ResponseData::Forget(ForgetData {
                deleted_count: 1,
                plan_ref: None,
                tombstones: Some(vec![Ulid(RECORD_ULID.to_owned())]),
            }),
        )
    }
}

struct Wire {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
}

/// Serve `handler` over an in-process duplex pipe and complete the MCP
/// initialize handshake.
async fn start(handler: CairnMcpHandler) -> Wire {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });
    let (client_read, client_write) = tokio::io::split(client_half);
    let mut wire = Wire {
        writer: client_write,
        reader: BufReader::new(client_read),
    };
    do_initialize(&mut wire.writer, &mut wire.reader).await;
    wire
}

async fn call_tool(wire: &mut Wire, id: u32, name: &str, arguments: &str) -> serde_json::Value {
    let frame = format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
    );
    send_frame(&mut wire.writer, &frame).await;
    recv_frame(&mut wire.reader).await
}

fn envelope_from(call_resp: &serde_json::Value) -> Response {
    let text = call_resp
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected text content; got: {call_resp}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("text must be valid Response JSON: {e}; text={text}"))
}

fn is_error(call_resp: &serde_json::Value) -> bool {
    call_resp
        .pointer("/result/isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn host_handler(host: Arc<RecordingHost>) -> CairnMcpHandler {
    CairnMcpHandler::new().with_mutation_host(host)
}

#[tokio::test]
async fn ingest_routes_through_wired_host() {
    let host = Arc::new(RecordingHost::default());
    let mut wire = start(host_handler(host.clone())).await;

    let resp = call_tool(
        &mut wire,
        2,
        "ingest",
        r#"{"kind":"reference","body":"acme renewal terms"}"#,
    )
    .await;

    assert!(!is_error(&resp), "wired ingest must not error: {resp}");
    let envelope = envelope_from(&resp);
    assert!(matches!(envelope.status, ResponseStatus::Committed));
    assert!(matches!(envelope.verb, ResponseVerb::Ingest));
    let Some(ResponseData::Ingest(data)) = envelope.data else {
        panic!("committed ingest envelope must carry IngestData");
    };
    assert_eq!(data.record_id.0, RECORD_ULID);
    assert_eq!(
        host.calls.lock().expect("calls lock").as_slice(),
        ["ingest:acme renewal terms"]
    );
}

#[tokio::test]
async fn capture_trace_routes_through_wired_host() {
    let host = Arc::new(RecordingHost::default());
    let mut wire = start(host_handler(host.clone())).await;

    let resp = call_tool(
        &mut wire,
        2,
        "capture_trace",
        r#"{"from":"/tmp/trace.jsonl"}"#,
    )
    .await;

    assert!(
        !is_error(&resp),
        "wired capture_trace must not error: {resp}"
    );
    let envelope = envelope_from(&resp);
    assert!(matches!(envelope.status, ResponseStatus::Committed));
    assert!(matches!(envelope.verb, ResponseVerb::CaptureTrace));
    let Some(ResponseData::CaptureTrace(data)) = envelope.data else {
        panic!("committed capture_trace envelope must carry CaptureTraceData");
    };
    assert_eq!(data.trace_id.0, TRACE_ULID);
    assert_eq!(
        host.calls.lock().expect("calls lock").as_slice(),
        ["capture_trace:/tmp/trace.jsonl"]
    );
}

#[tokio::test]
async fn forget_record_routes_through_wired_host() {
    let host = Arc::new(RecordingHost::default());
    let mut wire = start(host_handler(host.clone())).await;

    let resp = call_tool(
        &mut wire,
        2,
        "forget",
        &format!(r#"{{"mode":"record","record_id":"{RECORD_ULID}"}}"#),
    )
    .await;

    assert!(!is_error(&resp), "wired forget must not error: {resp}");
    let envelope = envelope_from(&resp);
    assert!(matches!(envelope.status, ResponseStatus::Committed));
    assert!(matches!(envelope.verb, ResponseVerb::Forget));
    let Some(ResponseData::Forget(data)) = envelope.data else {
        panic!("committed forget envelope must carry ForgetData");
    };
    assert_eq!(data.deleted_count, 1);
    assert_eq!(
        host.calls.lock().expect("calls lock").as_slice(),
        [format!("forget:record:{RECORD_ULID}")]
    );
}

/// Malformed args must be rejected by the generated parser BEFORE the host
/// runs — a host call on unvalidated args could mutate the vault.
#[tokio::test]
async fn malformed_args_reject_before_host_is_called() {
    let host = Arc::new(RecordingHost::default());
    let mut wire = start(host_handler(host.clone())).await;

    // `kind` is required by the generated IngestArgs parser.
    let resp = call_tool(&mut wire, 2, "ingest", r#"{"body":"no kind"}"#).await;

    assert!(is_error(&resp), "malformed ingest args must error: {resp}");
    let envelope = envelope_from(&resp);
    assert!(matches!(envelope.status, ResponseStatus::Rejected));
    assert!(matches!(envelope.verb, ResponseVerb::Ingest));
    assert!(
        host.calls.lock().expect("calls lock").is_empty(),
        "host must not be consulted for unparseable args"
    );
}

/// Without a wired host the pre-existing typed aborted stub is preserved —
/// the adapter fails closed rather than guessing at a write path.
#[tokio::test]
async fn ingest_without_host_returns_typed_aborted_stub() {
    let mut wire = start(CairnMcpHandler::new()).await;

    let resp = call_tool(
        &mut wire,
        2,
        "ingest",
        r#"{"kind":"reference","body":"acme"}"#,
    )
    .await;

    assert!(is_error(&resp), "stubbed ingest must error: {resp}");
    let envelope = envelope_from(&resp);
    assert!(matches!(envelope.status, ResponseStatus::Aborted));
    assert!(matches!(envelope.verb, ResponseVerb::Ingest));
    let message = envelope
        .error
        .as_ref()
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("not yet implemented"),
        "stub message must say not implemented; got: {message}"
    );
}
