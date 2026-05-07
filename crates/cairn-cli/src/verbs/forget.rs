//! `cairn forget` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error};

fn requested_capability(sub: &ArgMatches) -> &'static str {
    if sub.get_one::<String>("session_id").is_some() {
        "cairn.mcp.v1.forget.session"
    } else if sub.get_one::<String>("scope").is_some() {
        "cairn.mcp.v1.forget.scope"
    } else {
        "cairn.mcp.v1.forget.record"
    }
}

/// Run `cairn forget`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let capability = requested_capability(sub);
    let resp = capability_unavailable_response(ResponseVerb::Forget, capability);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "forget",
            "CapabilityUnavailable",
            "capability is not advertised in this build",
            &resp.operation_id,
        );
    }
    ExitCode::from(EX_UNAVAILABLE)
}
