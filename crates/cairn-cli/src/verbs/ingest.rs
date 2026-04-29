//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Returns `Internal aborted` until the store is wired (issue #9).
//!
//! # Trust boundary (spec §3.5)
//!
//! `ingest` is an issuer-dependent verb: it will sign records on behalf of an
//! identity once the store is wired (#9).  The guard call below invokes
//! [`cairn_cli::identity::guard::refuse_if_degraded`] to enforce the
//! `VaultDegraded → EX_TEMPFAIL=75` contract even in the stub path.
//!
//! **Deferred**: full async wiring (calling [`open_for_signed_verb`] against the
//! resolved vault path) is deferred to issue #9 when this verb becomes async.
//! Until then the guard runs against a clean default report and always passes,
//! but the exit-code path is exercised by the unit tests in `guard.rs`.
//!
//! [`open_for_signed_verb`]: crate::identity::guard::open_for_signed_verb

use std::io::Read;
use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Run `cairn ingest`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // Enforce IDL exactly-one-of: body/file/url (positional `source` counts as one).
    let has_source = sub.get_one::<String>("source").is_some();
    let has_body = sub.get_one::<String>("body").is_some();
    let has_file = sub.get_one::<std::path::PathBuf>("file").is_some();
    let has_url = sub.get_one::<String>("url").is_some();
    let source_count =
        u8::from(has_source) + u8::from(has_body) + u8::from(has_file) + u8::from(has_url);
    if source_count != 1 {
        eprintln!(
            "cairn ingest: exactly one of [source, --body, --file, --url] is required (got {source_count})"
        );
        return ExitCode::from(64);
    }

    // Resolve body: positional `source` wins if set; --body/--file/--url otherwise.
    let _body_resolved: Option<String> = if let Some(src) = sub.get_one::<String>("source") {
        if src == "-" {
            let mut buf = String::new();
            // Cap at 4 MiB to avoid unbounded allocation in the stubbed path.
            if std::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_string(&mut buf)
                .is_err()
            {
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

    // §3.5 trust-boundary guard: refuse if the vault is degraded.
    // In this P0 stub the report is always clean (no store is open); full async
    // wiring against the resolved vault path is deferred to issue #9.
    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn ingest: VaultDegraded — {e}");
        return ExitCode::from(75); // EX_TEMPFAIL
    }

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
