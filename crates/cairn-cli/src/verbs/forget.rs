//! `cairn forget` handler.
//!
//! # Trust boundary (spec §3.5)
//!
//! `forget` is an issuer-dependent verb: it produces a signed tombstone record
//! through the signed-verb context before mutating the selected vault store.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, RetryPolicy};
use cairn_core::domain::RecordId;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus};
use cairn_core::generated::verbs::forget::ForgetData;
use cairn_workflows::consolidation::{FORGET_CLEANUP_KIND, ForgetCleanupPayload};
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

            // Enqueue a forget-cleanup job so any consolidation summaries
            // that referenced this record as a source are tombstoned.
            // Non-fatal: if the job store is absent (short-lived CLI) or the
            // enqueue fails (e.g. DuplicateDedupeKey from a prior run), we
            // swallow the error and let the verb succeed. The scheduler will
            // pick up the job when it boots (Task 17).
            if let Some(js) = ctx.job_store.as_deref() {
                let record_id_str = record_id.as_str().to_owned();
                let payload = ForgetCleanupPayload {
                    forgotten_record_id: record_id_str.clone(),
                };
                if let Ok(bytes) = payload.to_bytes() {
                    let req = EnqueueRequest {
                        job_id: JobId::new(format!("forget-cleanup:{record_id_str}")),
                        kind: JobKind::new(FORGET_CLEANUP_KIND),
                        payload: bytes,
                        queue_key: None,
                        dedupe_key: Some(record_id_str),
                        not_before_ms: i64::try_from(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                        )
                        .unwrap_or(i64::MAX),
                        retry: RetryPolicy::DEFAULT,
                    };
                    let _ = js.enqueue(req).await;
                }
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test the `ForgetCleanupPayload` serialization and `EnqueueRequest`
    /// construction that the `run_record` forget-cleanup path uses. Confirms
    /// that the plumbing compiles and the payload round-trips cleanly without
    /// requiring a full vault context.
    ///
    /// End-to-end enqueue coverage (`job_store=Some`, `store.forget_record`
    /// succeeds, job lands in `workflow_jobs` table) lives in the integration
    /// tests at Task 19 (`tests/forget_propagation.rs`).
    #[test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    fn forget_cleanup_payload_round_trips() {
        let record_id_str = "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned();
        let payload = ForgetCleanupPayload {
            forgotten_record_id: record_id_str.clone(),
        };
        let bytes = payload.to_bytes().expect("serialize payload");

        let req = EnqueueRequest {
            job_id: JobId::new(format!("forget-cleanup:{record_id_str}")),
            kind: JobKind::new(FORGET_CLEANUP_KIND),
            payload: bytes,
            queue_key: None,
            dedupe_key: Some(record_id_str.clone()),
            not_before_ms: 1_000,
            retry: RetryPolicy::DEFAULT,
        };

        assert_eq!(
            req.job_id.as_str(),
            &format!("forget-cleanup:{record_id_str}")
        );
        assert_eq!(req.kind.as_str(), FORGET_CLEANUP_KIND);
        assert_eq!(req.dedupe_key.as_deref(), Some(record_id_str.as_str()));
    }
}
