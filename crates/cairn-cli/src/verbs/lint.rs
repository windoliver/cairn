//! `cairn lint` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Run `cairn lint`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let resp = unimplemented_response(ResponseVerb::Lint);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "lint",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
