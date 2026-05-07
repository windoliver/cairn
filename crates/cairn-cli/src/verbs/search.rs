//! `cairn search` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error};

fn capability_for_mode(sub: &ArgMatches) -> &'static str {
    match sub.get_one::<String>("mode").map(String::as_str) {
        Some("semantic") => "cairn.mcp.v1.search.semantic",
        Some("hybrid") => "cairn.mcp.v1.search.hybrid",
        _ => "cairn.mcp.v1.search.keyword",
    }
}

/// Run `cairn search`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let capability = capability_for_mode(sub);
    let resp = capability_unavailable_response(ResponseVerb::Search, capability);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "search",
            "CapabilityUnavailable",
            "capability is not advertised in this build",
            &resp.operation_id,
        );
    }
    ExitCode::from(EX_UNAVAILABLE)
}
