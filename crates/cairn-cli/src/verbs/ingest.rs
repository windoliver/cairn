//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Returns `Internal aborted` until the store is wired (issue #9).

use std::io::Read;
use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Run `cairn ingest`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // Resolve body: positional `source` wins if set; --body/--file/--url otherwise.
    let _body_resolved: Option<String> = if let Some(src) = sub.get_one::<String>("source") {
        if src == "-" {
            let mut buf = String::new();
            if std::io::stdin().read_to_string(&mut buf).is_err() {
                let r = unimplemented_response(ResponseVerb::Ingest);
                if json {
                    emit_json(&r);
                } else {
                    human_error(
                        "ingest",
                        "Internal",
                        "failed to read stdin",
                        &r.operation_id,
                    );
                }
                return ExitCode::FAILURE;
            }
            Some(buf)
        } else {
            Some(src.clone())
        }
    } else {
        sub.get_one::<String>("body").cloned()
    };

    let resp = unimplemented_response(ResponseVerb::Ingest);
    if json {
        emit_json(&resp);
    } else {
        let op = resp.operation_id.clone();
        human_error(
            "ingest",
            "Internal",
            "store not wired in this P0 build",
            &op,
        );
    }
    ExitCode::FAILURE
}
