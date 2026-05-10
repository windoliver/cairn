//! `cairn forget` handler.
//!
//! # Trust boundary (spec §3.5)
//!
//! `forget` is an issuer-dependent verb: it produces a signed tombstone record
//! through the signed-verb context before mutating the selected vault store.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::domain::RecordId;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus};
use cairn_core::generated::verbs::forget::ForgetData;
use clap::ArgMatches;

use super::envelope::{
    EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error, not_found_response,
};

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
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
    if !requires_vault_context(sub) {
        return run_without_context(sub);
    }

    let json = sub.get_flag("json");

    let Some(record_id) = sub.get_one::<String>("record_id") else {
        return run_without_context(sub);
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp = super::signed::aborted(ResponseVerb::Forget, format!("runtime build: {e}"));
            emit_response(&resp, json, record_id);
            return ExitCode::FAILURE;
        }
    };
    let resp = rt.block_on(run_record(record_id.clone(), vault_root, config));
    emit_response(&resp, json, record_id);
    response_exit_code(&resp)
}

/// Whether this invocation needs the resolved vault path and config.
#[must_use]
pub fn requires_vault_context(sub: &ArgMatches) -> bool {
    !sub.get_flag("dry-run")
        && !sub.get_flag("human-review")
        && sub.get_one::<String>("record_id").is_some()
}

/// Run `cairn forget` modes that do not open the vault store.
#[must_use]
pub fn run_without_context(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    let dry_run = sub.get_flag("dry-run");
    let human_review = sub.get_flag("human-review");
    let no_diff = sub.get_flag("no-diff");
    if dry_run || human_review {
        let mode = if dry_run {
            cairn_core::domain::flush_plan::FlushMode::DryRun
        } else {
            cairn_core::domain::flush_plan::FlushMode::HumanReview
        };
        // Reuse the same stub planner — for the stub, the ingest/forget
        // distinction collapses to "produce a placeholder plan." #9 will
        // split them into real builders.
        return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
    }

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

async fn run_record(record_id_raw: String, vault_root: PathBuf, config: CairnConfig) -> Response {
    let record_id = match RecordId::parse(record_id_raw.clone()) {
        Ok(record_id) => record_id,
        Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Forget, e),
    };
    let ctx = match super::signed::open_context(ResponseVerb::Forget, &vault_root, config).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    match ctx.store.forget_record(&record_id).await {
        Ok(outcome) => {
            let operation_id = match response_operation_id(&outcome.operation_id) {
                Ok(operation_id) => operation_id,
                Err(message) => return super::signed::aborted(ResponseVerb::Forget, message),
            };
            let data = ForgetData {
                deleted_count: outcome.deleted_count,
                plan_ref: None,
                tombstones: Some(
                    outcome
                        .tombstones
                        .into_iter()
                        .map(|id| Ulid(id.as_str().to_owned()))
                        .collect(),
                ),
            };
            super::signed::committed(
                ResponseVerb::Forget,
                operation_id,
                ResponseData::Forget(data),
                Vec::new(),
            )
        }
        Err(cairn_store_sqlite::StoreError::NotFound { id }) => not_found_response(
            ResponseVerb::Forget,
            "record",
            &format!("record not found: {id}"),
        ),
        Err(e) => super::signed::aborted(ResponseVerb::Forget, format!("store forget: {e}")),
    }
}

fn response_operation_id(operation_id: &cairn_core::wal::OperationId) -> Result<Ulid, String> {
    const PREFIX: &str = "forget_record-";
    let raw = operation_id.as_str();
    let Some(ulid) = raw.strip_prefix(PREFIX) else {
        return Err(format!("unexpected forget wal operation id: {raw}"));
    };
    RecordId::parse(ulid.to_owned())
        .map_err(|e| format!("invalid forget wal operation id `{raw}`: {e}"))?;
    Ok(Ulid(ulid.to_owned()))
}

fn emit_response(resp: &Response, json: bool, requested_record_id: &str) {
    if json {
        emit_json(resp);
        return;
    }
    match resp.status {
        ResponseStatus::Committed => {
            if let Some(ResponseData::Forget(data)) = resp.data.as_ref() {
                println!(
                    "cairn forget: committed record {requested_record_id} (deleted {})",
                    data.deleted_count
                );
            } else {
                println!("cairn forget: committed record {requested_record_id}");
            }
        }
        ResponseStatus::Rejected | ResponseStatus::Aborted => {
            let code = super::signed::response_error_code(resp).unwrap_or("Internal");
            let message = resp
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("forget failed");
            human_error("forget", code, message, &resp.operation_id);
        }
        _ => human_error(
            "forget",
            "Internal",
            "unknown response status",
            &resp.operation_id,
        ),
    }
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => ExitCode::from(64),
        _ => ExitCode::FAILURE,
    }
}
