//! `cairn assemble_hot` handler.

use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, internal_error_response, new_operation_id};

/// Run `cairn assemble_hot`.
#[must_use]
pub fn run(sub: &ArgMatches, config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");

    match cairn_core::verbs::assemble_hot::assemble_hot(&config.vault.hot_memory) {
        Ok(data) => {
            let resp = Response {
                contract: "cairn.mcp.v1".to_owned(),
                data: Some(ResponseData::AssembleHot(data)),
                error: None,
                operation_id: new_operation_id(),
                policy_trace: Vec::<ResponsePolicyTrace>::new(),
                status: ResponseStatus::Committed,
                target: None,
                verb: ResponseVerb::AssembleHot,
            };
            if json {
                emit_json(&resp);
            } else {
                let segments = match resp.data.as_ref() {
                    Some(ResponseData::AssembleHot(d)) => d.segments.as_ref().map_or(0, Vec::len),
                    _ => 0,
                };
                println!(
                    "assemble_hot: {} bytes, {} segment(s) (operation_id: {})",
                    resp.data
                        .as_ref()
                        .and_then(|d| {
                            if let ResponseData::AssembleHot(inner) = d {
                                Some(inner.bytes)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0),
                    segments,
                    resp.operation_id.0
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let resp =
                internal_error_response(ResponseVerb::AssembleHot, &format!("assemble_hot: {e}"));
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "assemble_hot",
                    "Internal",
                    &format!("{e}"),
                    &resp.operation_id,
                );
            }
            ExitCode::FAILURE
        }
    }
}
