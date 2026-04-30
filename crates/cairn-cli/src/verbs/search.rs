//! `cairn search` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{
    capability_unavailable_response, emit_json, human_error, unimplemented_response,
};
use super::status;

/// Capability that gates `--explain`. Mirrors
/// `crates/cairn-idl/schema/verbs/search.json`'s
/// `args.explain.x-cairn-capability-when-true` annotation; tests in
/// `crates/cairn-idl/tests/schema_files.rs` lock the contract.
const EXPLAIN_CAPABILITY: &str = "cairn.mcp.v1.policy_trace";

/// Run `cairn search`. `--explain` requires the
/// `cairn.mcp.v1.policy_trace` capability to be advertised by `status`;
/// otherwise we fail-closed with `CapabilityUnavailable` (sysexit 69)
/// before any verb dispatch (CLAUDE.md §6.5, §4.6).
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let explain = sub.get_flag("explain");

    if explain && !status::p0_capabilities_advertises(EXPLAIN_CAPABILITY) {
        let resp = capability_unavailable_response(ResponseVerb::Search, EXPLAIN_CAPABILITY);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "search",
                "CapabilityUnavailable",
                &format!("--explain requires {EXPLAIN_CAPABILITY}, which is not advertised"),
                &resp.operation_id,
            );
        }
        return ExitCode::from(69); // EX_UNAVAILABLE
    }

    let resp = unimplemented_response(ResponseVerb::Search);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "search",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
