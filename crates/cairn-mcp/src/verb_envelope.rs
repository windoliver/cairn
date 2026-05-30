//! Typed envelope helpers for core MCP verb calls.

use cairn_core::generated::common::Capabilities;
use cairn_core::generated::envelope::{
    RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace, ResponseStatus,
    ResponseTarget, ResponseVerb, RetrieveData,
};
use rmcp::model::{CallToolResult, Content};
use serde::de::DeserializeOwned;

const CONTRACT: &str = "cairn.mcp.v1";

/// Return the generated core request verb for an MCP tool name.
///
/// Includes the three federation extension verbs (`propose_share`,
/// `accept_share`, `revoke_share`) so dispatch-stub / parse-args paths
/// can construct typed response envelopes against the codegenerated
/// args types. The federation capability gate in
/// `crate::federation_tools::runtime_ready` is the authoritative
/// admission check — this helper only maps tool names to verb enum
/// variants.
#[must_use]
pub fn core_verb_for_tool(name: &str) -> Option<RequestVerb> {
    match name {
        "ingest" => Some(RequestVerb::Ingest),
        "search" => Some(RequestVerb::Search),
        "retrieve" => Some(RequestVerb::Retrieve),
        "summarize" => Some(RequestVerb::Summarize),
        "assemble_hot" => Some(RequestVerb::AssembleHot),
        "capture_trace" => Some(RequestVerb::CaptureTrace),
        "lint" => Some(RequestVerb::Lint),
        "forget" => Some(RequestVerb::Forget),
        "propose_share" => Some(RequestVerb::ProposeShare),
        "accept_share" => Some(RequestVerb::AcceptShare),
        "revoke_share" => Some(RequestVerb::RevokeShare),
        "admin_snapshot" => Some(RequestVerb::AdminSnapshot),
        "admin_restore" => Some(RequestVerb::AdminRestore),
        "admin_replay_wal" => Some(RequestVerb::AdminReplayWal),
        "admin_connector_enable" => Some(RequestVerb::AdminConnectorEnable),
        "admin_connector_disable" => Some(RequestVerb::AdminConnectorDisable),
        "admin_connector_backfill" => Some(RequestVerb::AdminConnectorBackfill),
        _ => None,
    }
}

/// Map a generated request verb to its generated response envelope verb.
#[must_use]
pub fn response_verb(verb: RequestVerb) -> ResponseVerb {
    match verb {
        RequestVerb::Ingest => ResponseVerb::Ingest,
        RequestVerb::Search => ResponseVerb::Search,
        RequestVerb::Retrieve => ResponseVerb::Retrieve,
        RequestVerb::Summarize => ResponseVerb::Summarize,
        RequestVerb::AssembleHot => ResponseVerb::AssembleHot,
        RequestVerb::CaptureTrace => ResponseVerb::CaptureTrace,
        RequestVerb::Lint => ResponseVerb::Lint,
        RequestVerb::Forget => ResponseVerb::Forget,
        RequestVerb::ProposeShare => ResponseVerb::ProposeShare,
        RequestVerb::AcceptShare => ResponseVerb::AcceptShare,
        RequestVerb::RevokeShare => ResponseVerb::RevokeShare,
        RequestVerb::AdminSnapshot => ResponseVerb::AdminSnapshot,
        RequestVerb::AdminRestore => ResponseVerb::AdminRestore,
        RequestVerb::AdminReplayWal => ResponseVerb::AdminReplayWal,
        RequestVerb::AdminConnectorEnable => ResponseVerb::AdminConnectorEnable,
        RequestVerb::AdminConnectorDisable => ResponseVerb::AdminConnectorDisable,
        RequestVerb::AdminConnectorBackfill => ResponseVerb::AdminConnectorBackfill,
        _ => ResponseVerb::Unknown,
    }
}

/// Parse MCP per-tool args into the generated per-verb request arg enum.
///
/// MCP passes only an args object, not the whole signed request envelope. This
/// adapter dispatches on the already-known tool verb and lets the generated
/// deserializers enforce each verb's shape.
#[allow(
    clippy::result_large_err,
    reason = "the MCP adapter intentionally returns generated response envelopes as typed error payloads at the transport boundary"
)]
pub fn parse_args(
    verb: RequestVerb,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<RequestArgs, Response> {
    let args = match arguments {
        Some(args) => serde_json::Value::Object(args),
        None => serde_json::Value::Object(serde_json::Map::new()),
    };

    match verb {
        RequestVerb::Ingest => parse_arg_payload(verb, args).map(RequestArgs::Ingest),
        RequestVerb::Search => parse_arg_payload(verb, args).map(RequestArgs::Search),
        RequestVerb::Retrieve => parse_arg_payload(verb, args).map(RequestArgs::Retrieve),
        RequestVerb::Summarize => parse_arg_payload(verb, args).map(RequestArgs::Summarize),
        RequestVerb::AssembleHot => parse_arg_payload(verb, args).map(RequestArgs::AssembleHot),
        RequestVerb::CaptureTrace => parse_arg_payload(verb, args).map(RequestArgs::CaptureTrace),
        RequestVerb::Lint => parse_arg_payload(verb, args).map(RequestArgs::Lint),
        RequestVerb::Forget => parse_arg_payload(verb, args).map(RequestArgs::Forget),
        // Federation extension verbs (brief §12.a). The MCP `call_tool`
        // path routes these through `federation_tools` first, so this
        // arm is only reached when an embedder calls `parse_args`
        // directly (e.g., the dispatch stub envelope serialiser).
        RequestVerb::ProposeShare => parse_arg_payload(verb, args).map(RequestArgs::ProposeShare),
        RequestVerb::AcceptShare => parse_arg_payload(verb, args).map(RequestArgs::AcceptShare),
        RequestVerb::RevokeShare => parse_arg_payload(verb, args).map(RequestArgs::RevokeShare),
        _ => Err(unknown_verb_response(format!("{verb:?}"))),
    }
}

/// Build a committed generated response envelope.
#[must_use]
pub fn committed(
    verb: ResponseVerb,
    data: ResponseData,
    policy_trace: Vec<ResponsePolicyTrace>,
) -> Response {
    if !response_data_matches_verb(verb, &data) {
        if matches!(verb, ResponseVerb::Unknown) {
            return unknown_verb_response("unknown");
        }

        let message = if matches!(
            (verb, &data),
            (ResponseVerb::Retrieve, ResponseData::Retrieve(_))
        ) {
            "committed retrieve responses require committed_retrieve so target is set"
        } else {
            "response verb does not match response data"
        };
        return aborted_internal(verb, message);
    }

    Response {
        contract: CONTRACT.to_owned(),
        data: Some(data),
        error: None,
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace,
        status: ResponseStatus::Committed,
        target: None,
        verb,
    }
}

/// Build a committed generated retrieve response envelope.
#[must_use]
pub fn committed_retrieve(data: RetrieveData, policy_trace: Vec<ResponsePolicyTrace>) -> Response {
    let Some(target) = retrieve_target(&data) else {
        return aborted_internal(ResponseVerb::Retrieve, "unsupported retrieve data variant");
    };

    Response {
        contract: CONTRACT.to_owned(),
        data: Some(ResponseData::Retrieve(data)),
        error: None,
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace,
        status: ResponseStatus::Committed,
        target: Some(target),
        verb: ResponseVerb::Retrieve,
    }
}

/// Build a valid `UnknownVerb` rejected response envelope.
#[must_use]
pub fn unknown_verb_response(attempted: impl Into<String>) -> Response {
    let attempted = non_empty_or(attempted.into(), "unknown");
    Response {
        contract: CONTRACT.to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "UnknownVerb",
            "message": format!("unknown verb: {attempted}"),
            "data": { "verb": attempted },
        })),
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb: ResponseVerb::Unknown,
    }
}

/// Build an `InvalidArgs` rejected response envelope.
#[must_use]
pub fn invalid_args_response(verb: ResponseVerb, field: &str, reason: &str) -> Response {
    if matches!(verb, ResponseVerb::Unknown) {
        return unknown_verb_response("unknown");
    }

    let field = non_empty_or(field.to_owned(), "unknown");
    let reason = non_empty_or(reason.to_owned(), "unknown");
    Response {
        contract: CONTRACT.to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "InvalidArgs",
            "message": format!("invalid args: {field}: {reason}"),
            "data": { "field": field, "reason": reason },
        })),
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build an `InvalidFilter` rejected response envelope for `filters`.
#[must_use]
pub fn invalid_filter_response(verb: ResponseVerb, reason: &str) -> Response {
    if matches!(verb, ResponseVerb::Unknown) {
        return unknown_verb_response("unknown");
    }

    let reason = non_empty_or(reason.to_owned(), "unknown");
    Response {
        contract: CONTRACT.to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "InvalidFilter",
            "message": format!("invalid filter: {reason}"),
            "data": { "path": "filters", "reason": reason },
        })),
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build a `CapabilityUnavailable` rejected response envelope.
#[must_use]
pub fn capability_unavailable_response(verb: ResponseVerb, capability: &str) -> Response {
    if matches!(verb, ResponseVerb::Unknown) {
        return unknown_verb_response("unknown");
    }

    let capability = non_empty_or(capability.to_owned(), "unknown");
    if !is_known_capability(&capability) {
        return aborted_internal(verb, &format!("unknown capability: {capability}"));
    }

    let mut data = serde_json::json!({ "capability": capability });
    if let Some(remediation) = cairn_core::status::remediation_for(&capability)
        && let Some(data) = data.as_object_mut()
    {
        data.insert(
            "remediation".to_owned(),
            serde_json::Value::String(remediation.to_owned()),
        );
    }

    Response {
        contract: CONTRACT.to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "CapabilityUnavailable",
            "message": format!(
                "this server does not advertise {capability}; the requested feature requires it"
            ),
            "data": data,
        })),
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build an `Internal` aborted response envelope.
#[must_use]
pub fn aborted_internal(verb: ResponseVerb, message: &str) -> Response {
    if matches!(verb, ResponseVerb::Unknown) {
        return unknown_verb_response("unknown");
    }

    let message = non_empty_or(message.to_owned(), "internal error");
    Response {
        contract: CONTRACT.to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id: cairn_core::time::new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

/// Serialize a generated response envelope into an MCP tool result.
#[must_use]
pub fn call_result_from_response(response: &Response) -> CallToolResult {
    let (text, is_serialization_fallback) = response_json_text(response);
    let is_committed =
        matches!(&response.status, ResponseStatus::Committed) && !is_serialization_fallback;
    let content = vec![Content::text(text)];

    if is_committed {
        CallToolResult::success(content)
    } else {
        CallToolResult::error(content)
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the MCP adapter intentionally returns generated response envelopes as typed error payloads at the transport boundary"
)]
fn parse_arg_payload<T: DeserializeOwned>(
    verb: RequestVerb,
    args: serde_json::Value,
) -> Result<T, Response> {
    serde_json::from_value(args)
        .map_err(|err| invalid_args_response(response_verb(verb), "args", &err.to_string()))
}

fn response_data_matches_verb(verb: ResponseVerb, data: &ResponseData) -> bool {
    matches!(
        (verb, data),
        (ResponseVerb::Ingest, ResponseData::Ingest(_))
            | (ResponseVerb::Search, ResponseData::Search(_))
            | (ResponseVerb::Summarize, ResponseData::Summarize(_))
            | (ResponseVerb::AssembleHot, ResponseData::AssembleHot(_))
            | (ResponseVerb::CaptureTrace, ResponseData::CaptureTrace(_))
            | (ResponseVerb::Lint, ResponseData::Lint(_))
            | (ResponseVerb::Forget, ResponseData::Forget(_))
            | (ResponseVerb::AdminSnapshot, ResponseData::AdminSnapshot(_))
            | (ResponseVerb::AdminRestore, ResponseData::AdminRestore(_))
            | (
                ResponseVerb::AdminReplayWal,
                ResponseData::AdminReplayWal(_)
            )
            | (
                ResponseVerb::AdminConnectorEnable,
                ResponseData::AdminConnectorEnable(_)
            )
            | (
                ResponseVerb::AdminConnectorDisable,
                ResponseData::AdminConnectorDisable(_)
            )
            | (
                ResponseVerb::AdminConnectorBackfill,
                ResponseData::AdminConnectorBackfill(_)
            )
    )
}

fn retrieve_target(data: &RetrieveData) -> Option<ResponseTarget> {
    match data {
        RetrieveData::Record(_) => Some(ResponseTarget::Record),
        RetrieveData::Profile(_) => Some(ResponseTarget::Profile),
        RetrieveData::Session(_) => Some(ResponseTarget::Session),
        RetrieveData::Turn(_) => Some(ResponseTarget::Turn),
        RetrieveData::ToolCall(_) => Some(ResponseTarget::ToolCall),
        RetrieveData::Folder(_) => Some(ResponseTarget::Folder),
        RetrieveData::Scope(_) => Some(ResponseTarget::Scope),
        _ => None,
    }
}

fn is_known_capability(capability: &str) -> bool {
    serde_json::from_value::<Capabilities>(serde_json::Value::String(capability.to_owned())).is_ok()
}

fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn response_json_text(response: &Response) -> (String, bool) {
    match serde_json::to_string(response) {
        Ok(text) => (text, false),
        Err(err) => {
            let fallback = aborted_internal(
                response.verb,
                &format!("response serialization failed: {err}"),
            );
            match serde_json::to_string(&fallback) {
                Ok(text) => (text, true),
                Err(second_err) => (
                    fallback_json_text(response.verb, &second_err.to_string()),
                    true,
                ),
            }
        }
    }
}

fn fallback_json_text(verb: ResponseVerb, reason: &str) -> String {
    if matches!(verb, ResponseVerb::Unknown) {
        return serde_json::json!({
            "contract": CONTRACT,
            "error": {
                "code": "UnknownVerb",
                "message": "unknown verb: unknown",
                "data": { "verb": "unknown" },
            },
            "operation_id": cairn_core::time::new_operation_id(),
            "policy_trace": [],
            "status": "rejected",
            "verb": "unknown",
        })
        .to_string();
    }

    let verb = match serde_json::to_value(verb) {
        Ok(verb) => verb,
        Err(_) => serde_json::Value::String("unknown".to_owned()),
    };
    serde_json::json!({
        "contract": CONTRACT,
        "error": {
            "code": "Internal",
            "message": format!("response serialization failed: {reason}"),
        },
        "operation_id": cairn_core::time::new_operation_id(),
        "policy_trace": [],
        "status": "aborted",
        "verb": verb,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::generated::common::Ulid;
    use cairn_core::generated::envelope::{
        RequestArgs, RequestVerb, Response, ResponseData, ResponseStatus, ResponseTarget,
        ResponseVerb, RetrieveData,
    };
    use cairn_core::generated::verbs::retrieve::DataRecord;
    use cairn_core::generated::verbs::search::SearchData;
    use rmcp::model::RawContent;
    use serde_json::json;

    fn ulid() -> Ulid {
        Ulid("01HQZX9F5N0000000000000000".to_owned())
    }

    fn search_data() -> ResponseData {
        ResponseData::Search(SearchData {
            degraded_legs: None,
            excluded: None,
            hits: Vec::new(),
            next_cursor: None,
            score_explain: None,
            semantic_degraded: None,
        })
    }

    fn retrieve_record_data() -> RetrieveData {
        RetrieveData::Record(DataRecord {
            body: None,
            frontmatter: None,
            kind: "note".to_owned(),
            record_id: ulid(),
        })
    }

    fn response_text(result: &rmcp::model::CallToolResult) -> &str {
        result
            .content
            .iter()
            .find_map(|c| {
                if let RawContent::Text(t) = &**c {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .expect("expected text content")
    }

    #[test]
    fn maps_all_core_tool_names_to_request_verbs() {
        let cases = [
            ("ingest", RequestVerb::Ingest),
            ("search", RequestVerb::Search),
            ("retrieve", RequestVerb::Retrieve),
            ("summarize", RequestVerb::Summarize),
            ("assemble_hot", RequestVerb::AssembleHot),
            ("capture_trace", RequestVerb::CaptureTrace),
            ("lint", RequestVerb::Lint),
            ("forget", RequestVerb::Forget),
        ];
        for (name, expected) in cases {
            assert_eq!(core_verb_for_tool(name), Some(expected), "{name}");
        }
        assert_eq!(core_verb_for_tool("handshake"), None);
        assert_eq!(core_verb_for_tool("graph.neighbours"), None);
    }

    #[test]
    fn parses_search_args_into_typed_request_args() {
        let args = serde_json::Map::from_iter([
            ("query".to_owned(), json!("hello")),
            ("mode".to_owned(), json!("keyword")),
            ("limit".to_owned(), json!(3)),
        ]);
        let parsed = parse_args(RequestVerb::Search, Some(args)).expect("valid search args");
        let RequestArgs::Search(search) = parsed else {
            panic!("expected RequestArgs::Search");
        };
        assert_eq!(search.query, "hello");
        assert_eq!(search.limit, Some(3));
    }

    #[test]
    fn malformed_args_return_invalid_args_envelope() {
        let response = parse_args(RequestVerb::Search, Some(serde_json::Map::new()))
            .expect_err("missing query/mode must reject");
        assert!(matches!(response.status, ResponseStatus::Rejected));
        assert!(matches!(response.verb, ResponseVerb::Search));
        let err = response.error.expect("rejected response must carry error");
        assert_eq!(err["code"], "InvalidArgs");
        assert_eq!(err["data"]["field"], "args");
    }

    #[test]
    fn response_call_result_round_trips_through_generated_deserializer() {
        let response =
            aborted_internal(ResponseVerb::Retrieve, "not yet implemented in MCP adapter");
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(true));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Aborted));
        assert!(matches!(decoded.verb, ResponseVerb::Retrieve));
        assert_eq!(decoded.operation_id.0.len(), 26);
    }

    #[test]
    fn committed_search_response_round_trips_through_generated_deserializer() {
        let response = committed(ResponseVerb::Search, search_data(), Vec::new());
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(false));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Committed));
        assert!(matches!(decoded.verb, ResponseVerb::Search));
        assert!(decoded.target.is_none());
    }

    #[test]
    fn committed_retrieve_sets_target_and_round_trips() {
        let response = committed_retrieve(retrieve_record_data(), Vec::new());
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(false));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Committed));
        assert!(matches!(decoded.verb, ResponseVerb::Retrieve));
        assert!(matches!(decoded.target, Some(ResponseTarget::Record)));
    }

    #[test]
    fn committed_retrieve_through_generic_helper_returns_valid_error_envelope() {
        let response = committed(
            ResponseVerb::Retrieve,
            ResponseData::Retrieve(retrieve_record_data()),
            Vec::new(),
        );
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(true));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Aborted));
        assert!(matches!(decoded.verb, ResponseVerb::Retrieve));
        assert!(decoded.target.is_none());
    }

    #[test]
    fn committed_verb_data_mismatch_returns_valid_error_envelope() {
        let response = committed(
            ResponseVerb::Search,
            ResponseData::Retrieve(retrieve_record_data()),
            Vec::new(),
        );
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(true));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Aborted));
        assert!(matches!(decoded.verb, ResponseVerb::Search));
        let err = decoded.error.expect("mismatch must carry error");
        assert_eq!(err["code"], "Internal");
    }

    #[test]
    fn unknown_capability_falls_back_to_valid_internal_error() {
        let response =
            capability_unavailable_response(ResponseVerb::Search, "not.a.real.capability");
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(true));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Aborted));
        assert!(matches!(decoded.verb, ResponseVerb::Search));
        let err = decoded.error.expect("invalid capability must carry error");
        assert_eq!(err["code"], "Internal");
    }

    #[test]
    fn unknown_verb_response_round_trips_as_unknown_verb_rejection() {
        let response = unknown_verb_response("graph.neighbours");
        let result = call_result_from_response(&response);
        assert_eq!(result.is_error, Some(true));
        let decoded: Response =
            serde_json::from_str(response_text(&result)).expect("content must be Response JSON");
        assert!(matches!(decoded.status, ResponseStatus::Rejected));
        assert!(matches!(decoded.verb, ResponseVerb::Unknown));
        let err = decoded.error.expect("unknown verb must carry error");
        assert_eq!(err["code"], "UnknownVerb");
        assert_eq!(err["data"]["verb"], "graph.neighbours");
    }

    #[test]
    fn unknown_invalid_args_and_internal_helpers_still_round_trip() {
        for response in [
            invalid_args_response(ResponseVerb::Unknown, "args", "bad"),
            invalid_filter_response(ResponseVerb::Unknown, "bad"),
            capability_unavailable_response(ResponseVerb::Unknown, "not.a.real.capability"),
            aborted_internal(ResponseVerb::Unknown, "bad"),
        ] {
            let result = call_result_from_response(&response);
            assert_eq!(result.is_error, Some(true));
            let decoded: Response = serde_json::from_str(response_text(&result))
                .expect("content must be Response JSON");
            assert!(matches!(decoded.status, ResponseStatus::Rejected));
            assert!(matches!(decoded.verb, ResponseVerb::Unknown));
            let err = decoded.error.expect("unknown verb must carry error");
            assert_eq!(err["code"], "UnknownVerb");
        }
    }

    #[test]
    fn empty_public_error_helper_fields_are_sanitized() {
        for response in [
            invalid_args_response(ResponseVerb::Search, "", ""),
            invalid_filter_response(ResponseVerb::Search, ""),
            aborted_internal(ResponseVerb::Search, ""),
        ] {
            let result = call_result_from_response(&response);
            assert_eq!(result.is_error, Some(true));
            let decoded: Response = serde_json::from_str(response_text(&result))
                .expect("content must be Response JSON");
            assert!(matches!(decoded.verb, ResponseVerb::Search));
            assert!(decoded.error.is_some());
        }
    }
}
