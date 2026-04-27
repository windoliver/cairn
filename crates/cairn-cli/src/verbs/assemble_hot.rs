//! `cairn assemble_hot` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Run `cairn assemble_hot`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let resp = unimplemented_response(ResponseVerb::AssembleHot);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "assemble_hot",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
