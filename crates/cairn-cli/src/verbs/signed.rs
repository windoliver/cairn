//! Shared signed-verb utilities for issue #61.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::job_store::JobStore;
use cairn_core::domain::{DomainError, Identity};
use cairn_core::error::wire::envelope_error_for;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Request, RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace, ResponseStatus,
    ResponseTarget, ResponseVerb, RetrieveData,
};
use cairn_core::verifier::{EnvelopeVerifier, ScopePolicy, resolve_issuer};
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_workflows::SqliteJobStore;

use crate::identity::{IdentityService, guard::refuse_if_degraded};

use super::envelope::new_operation_id;

/// Shared resources opened once for a signed verb invocation.
pub struct OpenedVerbContext {
    /// Resolved vault root used for identity and store access.
    pub vault_root: PathBuf,
    /// Parsed Cairn configuration for the selected vault.
    pub config: CairnConfig,
    /// SQLite-backed memory store for the selected vault.
    pub store: SqliteMemoryStore,
    /// Identity service with access to the durable identity registry.
    pub identity: IdentityService,
    /// Optional job store for enqueuing background workflow jobs.
    ///
    /// `None` in short-lived CLI verb invocations (the scheduler loop is
    /// not running). Set to `Some` by long-lived paths (MCP serve) that
    /// boot the `Scheduler` — Task 17. Verb handlers that call
    /// [`enqueue_if_due`] or enqueue forget-cleanup jobs gate on
    /// `is_some()` so they degrade gracefully when the scheduler is absent.
    ///
    /// [`enqueue_if_due`]: cairn_workflows::consolidation::enqueue_if_due
    pub job_store: Option<Arc<dyn JobStore>>,
}

/// Return the string error code from a response error body.
pub fn response_error_code(resp: &Response) -> Option<&str> {
    resp.error
        .as_ref()
        .and_then(|e| e.get("code"))
        .and_then(serde_json::Value::as_str)
}

/// Convert a domain verification error into a rejected response envelope.
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers often construct DomainError inline for immediate envelope conversion"
)]
pub fn rejected_from_domain(verb: ResponseVerb, err: DomainError) -> Response {
    let body = envelope_error_for(&err);
    let error = serde_json::to_value(body).unwrap_or_else(|serialize_err| {
        serde_json::json!({
            "code": "Internal",
            "message": format!("error serialization failed: {serialize_err}")
        })
    });
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(error),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build an aborted response envelope for infrastructure or execution failures.
#[must_use]
pub fn aborted(verb: ResponseVerb, message: impl Into<String>) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message.into(),
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

/// Build a committed response envelope with operation data and policy traces.
#[must_use]
pub fn committed(
    verb: ResponseVerb,
    operation_id: Ulid,
    data: ResponseData,
    policy_trace: Vec<ResponsePolicyTrace>,
) -> Response {
    assert!(
        !matches!(verb, ResponseVerb::Retrieve),
        "retrieve responses require committed_retrieve so target is set"
    );
    assert!(
        response_data_matches_verb(verb, &data),
        "response verb must match response data"
    );
    committed_response(verb, operation_id, data, policy_trace, None)
}

/// Build a committed retrieve response envelope with the required target.
#[must_use]
pub fn committed_retrieve(
    operation_id: Ulid,
    data: RetrieveData,
    policy_trace: Vec<ResponsePolicyTrace>,
) -> Response {
    let target = retrieve_target(&data);
    committed_response(
        ResponseVerb::Retrieve,
        operation_id,
        ResponseData::Retrieve(data),
        policy_trace,
        Some(target),
    )
}

fn committed_response(
    verb: ResponseVerb,
    operation_id: Ulid,
    data: ResponseData,
    policy_trace: Vec<ResponsePolicyTrace>,
    target: Option<ResponseTarget>,
) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(data),
        error: None,
        operation_id,
        policy_trace,
        status: ResponseStatus::Committed,
        target,
        verb,
    }
}

fn response_data_matches_verb(verb: ResponseVerb, data: &ResponseData) -> bool {
    matches!(
        (verb, data),
        (ResponseVerb::Ingest, ResponseData::Ingest(_))
            | (ResponseVerb::Search, ResponseData::Search(_))
            | (ResponseVerb::Summarize, ResponseData::Summarize(_))
            | (ResponseVerb::AssembleHot, ResponseData::AssembleHot(_))
            | (ResponseVerb::CaptureTrace, ResponseData::CaptureTrace(_))
            | (ResponseVerb::Lint, ResponseData::Lint(_))
            | (ResponseVerb::Forget, ResponseData::Forget(_))
    )
}

fn retrieve_target(data: &RetrieveData) -> ResponseTarget {
    match data {
        RetrieveData::Record(_) => ResponseTarget::Record,
        RetrieveData::Profile(_) => ResponseTarget::Profile,
        RetrieveData::Session(_) => ResponseTarget::Session,
        RetrieveData::Turn(_) => ResponseTarget::Turn,
        RetrieveData::ToolCall(_) => ResponseTarget::ToolCall,
        RetrieveData::Folder(_) => ResponseTarget::Folder,
        RetrieveData::Scope(_) => ResponseTarget::Scope,
        _ => panic!("unsupported retrieve data variant"),
    }
}

/// Open the shared vault, store, and identity context required by signed verbs.
///
/// Opens a `SqliteJobStore` against the same `.cairn/cairn.db` as the memory
/// store, using a dedicated `rusqlite::Connection`. This lets short-lived CLI
/// verbs (`capture_trace`, `forget`) enqueue `workflow_jobs` rows that the
/// scheduler (running inside `cairn mcp serve`) will pick up on next boot.
/// `SQLite` WAL mode allows the two connections to co-exist without blocking.
///
/// If the job-store connection fails (e.g. schema not yet migrated), we
/// degrade gracefully: `job_store` is set to `None` and the verb continues
/// without enqueueing. Consolidation jobs will be created on the next
/// successful `mcp serve` boot.
pub async fn open_context(
    verb: ResponseVerb,
    vault_root: &Path,
    config: CairnConfig,
) -> Result<OpenedVerbContext, Response> {
    let (identity, report) = IdentityService::open(vault_root.to_path_buf())
        .await
        .map_err(|e| aborted(verb, format!("identity open: {e}")))?;
    refuse_if_degraded(&report, report.mismatched_ids.clone())
        .map_err(|e| aborted(verb, format!("vault degraded: {e}")))?;
    let db_path = vault_root.join(".cairn/cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .map_err(|e| aborted(verb, format!("store open: {e}")))?;

    // Open a second connection for the job store. The async `open` above ran
    // all migrations (including 0020 which provisions `workflow_jobs`), so
    // `SqliteJobStore::new` finds the required schema objects. WAL mode allows
    // both connections to co-exist. Failure is non-fatal: short-lived verbs
    // degrade to no-enqueue rather than refusing to run.
    let job_store: Option<Arc<dyn JobStore>> = match rusqlite::Connection::open(&db_path) {
        Ok(conn) => match SqliteJobStore::new(conn) {
            Ok(js) => Some(Arc::new(js)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "job-store init failed — workflow enqueue disabled for this invocation"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "job-store connection failed — workflow enqueue disabled for this invocation"
            );
            None
        }
    };

    Ok(OpenedVerbContext {
        vault_root: vault_root.to_path_buf(),
        config,
        store,
        identity,
        job_store,
    })
}

/// Verify a signed request and return its domain-level verified intent token.
pub async fn verify_request(
    ctx: &OpenedVerbContext,
    request: Request,
) -> Result<cairn_core::domain::VerifiedSignedIntent, Response> {
    let issuer = Identity::parse(request.signed_intent.issuer.0.clone())
        .map_err(|e| rejected_from_domain(response_verb(request.verb), e))?;
    let key_version = u32::try_from(request.signed_intent.key_version)
        .ok()
        .and_then(std::num::NonZeroU32::new)
        .map(cairn_core::domain::identity::keys::KeyVersion::new)
        .ok_or_else(|| {
            rejected_from_domain(
                response_verb(request.verb),
                DomainError::Unauthorized {
                    message: "invalid key_version".to_owned(),
                },
            )
        })?;
    let resolved = resolve_issuer(&*ctx.identity.registry, &issuer, key_version)
        .await
        .map_err(|e| rejected_from_domain(response_verb(request.verb), e))?;
    let workspace = ctx.config.vault.name.clone();
    let policy = ScopePolicy::new("default", workspace, ScopePolicy::all_tiers())
        .map_err(|e| rejected_from_domain(response_verb(request.verb), e))?;
    let clock = cairn_core::domain::time::SystemClock;
    let verifier = EnvelopeVerifier::new(&policy, &clock);
    verifier
        .verify(request.signed_intent, resolved)
        .map_err(|e| rejected_from_domain(response_verb(request.verb), e))
}

/// Build a request envelope for a generated verb payload and signed intent.
#[must_use]
pub fn request(
    verb: RequestVerb,
    args: RequestArgs,
    signed_intent: cairn_core::generated::envelope::SignedIntent,
) -> Request {
    Request {
        contract: "cairn.mcp.v1".to_owned(),
        verb,
        args,
        signed_intent,
    }
}

/// Map request verbs to their response echo verb.
#[must_use]
pub const fn response_verb(verb: RequestVerb) -> ResponseVerb {
    match verb {
        RequestVerb::Ingest => ResponseVerb::Ingest,
        RequestVerb::Search => ResponseVerb::Search,
        RequestVerb::Retrieve => ResponseVerb::Retrieve,
        RequestVerb::Summarize => ResponseVerb::Summarize,
        RequestVerb::AssembleHot => ResponseVerb::AssembleHot,
        RequestVerb::CaptureTrace => ResponseVerb::CaptureTrace,
        RequestVerb::Lint => ResponseVerb::Lint,
        RequestVerb::Forget => ResponseVerb::Forget,
        _ => ResponseVerb::Unknown,
    }
}
