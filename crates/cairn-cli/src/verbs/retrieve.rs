//! `cairn retrieve` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error};

fn requested_capability(sub: &ArgMatches) -> &'static str {
    if sub.get_one::<String>("turn_id").is_some() {
        "cairn.mcp.v1.retrieve.turn"
    } else if sub.get_one::<String>("session_id").is_some() {
        "cairn.mcp.v1.retrieve.session"
    } else if sub.get_one::<String>("path").is_some() {
        "cairn.mcp.v1.retrieve.folder"
    } else if sub.get_one::<String>("scope").is_some() {
        "cairn.mcp.v1.retrieve.scope"
    } else if sub.get_flag("profile") {
        "cairn.mcp.v1.retrieve.profile"
    } else {
        "cairn.mcp.v1.retrieve.record"
    }
}

/// Run `cairn retrieve`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let capability = requested_capability(sub);
    let resp = capability_unavailable_response(ResponseVerb::Retrieve, capability);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "retrieve",
            "CapabilityUnavailable",
            "capability is not advertised in this build",
            &resp.operation_id,
        );
    }
    ExitCode::from(EX_UNAVAILABLE)
}
