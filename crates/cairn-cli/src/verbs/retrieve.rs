//! `cairn retrieve` handler.
#![allow(
    clippy::result_large_err,
    reason = "CLI helpers return complete response envelopes for direct JSON emission"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::identity::keys::SecretHandle;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{
    Identity, MemoryKind, MemoryRecord, RecordId, ScopeTuple, SessionId, TreeReadRecord,
    TreeReadWindow, TreeReadWindowInput, plan_tree_read_window,
};
use cairn_core::generated::common::{Cursor, Ed25519Signature, ScopeFilter, ScopeFilterTier, Ulid};
use cairn_core::generated::envelope::{
    RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace,
    ResponsePolicyTraceResult, ResponseStatus, ResponseVerb, RetrieveData, SignedIntent,
    SignedIntentScope, SignedIntentScopeTier,
};
use cairn_core::generated::verbs::retrieve::{
    RetrieveArgs, RetrieveArgsSessionInclude, RetrieveArgsSessionOrder, RetrieveArgsTurnInclude,
    TurnItem,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;
use sha2::Digest as _;

use super::envelope::{emit_json, human_error, invalid_args_response, new_operation_id};

const DEFAULT_RETRIEVE_ISSUER: &str = "agt:cairn-cli:default:writer:v1";
const DEFAULT_TENANT: &str = "default";
const RETRIEVE_DEFAULT_ENTITY: &str = "ingest";
const SESSION_CURSOR_PREFIX: &str = "session:v1";

#[derive(Clone)]
struct ReadAuthorization {
    operation_id: Ulid,
    scope: ScopeTuple,
    max_visibility: MemoryVisibility,
    rebac: cairn_core::rebac::RebacContext,
}

#[derive(Debug)]
struct SessionTurnGroup {
    turn_id: String,
    sort_time: String,
    records: Vec<MemoryRecord>,
}

struct SessionRetrieveRequest {
    session_id: String,
    limit: Option<i64>,
    order: Option<RetrieveArgsSessionOrder>,
    include: Option<Vec<RetrieveArgsSessionInclude>>,
    cursor: Option<Cursor>,
    rehydrate: bool,
    read_budget_chars: usize,
}

#[derive(Debug, Clone)]
struct BudgetReport {
    budget_chars: usize,
    items_in: usize,
    items_out: usize,
    turns_in: usize,
    turns_out: usize,
    trimmed: bool,
    rehydrate: Option<RehydrateReport>,
}

#[derive(Debug, Clone)]
struct RehydrateReport {
    elapsed_ms: u128,
    source_tier: &'static str,
}

/// Run `cairn retrieve`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let args = match retrieve_args_from_matches(sub) {
        Ok(args) => args,
        Err(resp) => {
            if json {
                emit_json(&resp);
            } else {
                emit_human(&resp);
            }
            return ExitCode::from(64);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp =
                super::signed::aborted(ResponseVerb::Retrieve, format!("runtime build: {e}"));
            if json {
                emit_json(&resp);
            } else {
                emit_human(&resp);
            }
            return ExitCode::FAILURE;
        }
    };

    let resp = rt.block_on(run_async(args, vault_root, config));
    if json {
        emit_json(&resp);
    } else {
        emit_human(&resp);
    }
    response_exit_code(&resp)
}

async fn run_async(args: RetrieveArgs, vault_root: PathBuf, config: CairnConfig) -> Response {
    let ctx = match super::signed::open_context(ResponseVerb::Retrieve, &vault_root, config).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let operation_id = new_operation_id();
    let auth = match signed_read_authorization(&ctx, &args, operation_id).await {
        Ok(auth) => auth,
        Err(resp) => return resp,
    };
    let read_budget_chars = ctx.config.search.max_snippet_chars_per_page;

    match args {
        RetrieveArgs::Record { id } => retrieve_record(&ctx.store, id.0, &auth).await,
        RetrieveArgs::Folder { depth, path } => {
            retrieve_folder(&ctx.store, path, depth, &auth).await
        }
        RetrieveArgs::Scope { cursor, scope } => {
            if cursor.is_some() {
                return invalid_args_response(
                    ResponseVerb::Retrieve,
                    "cursor",
                    "scope pagination is not yet supported",
                );
            }
            retrieve_scope(&ctx.store, scope, &auth).await
        }
        RetrieveArgs::Session {
            cursor,
            include,
            limit,
            order,
            rehydrate,
            session_id,
            ..
        } => {
            retrieve_session(
                &ctx.store,
                &vault_root,
                SessionRetrieveRequest {
                    session_id,
                    limit,
                    order,
                    include,
                    cursor,
                    rehydrate: rehydrate.unwrap_or(false),
                    read_budget_chars,
                },
                &auth,
            )
            .await
        }
        RetrieveArgs::Turn {
            include,
            session_id,
            turn_id,
            ..
        } => {
            retrieve_turn(
                &ctx.store,
                session_id,
                turn_id,
                include,
                read_budget_chars,
                &auth,
            )
            .await
        }
        RetrieveArgs::ToolCall {
            session_id,
            turn_id,
            tool_call_id,
        } => {
            retrieve_tool_call(
                &ctx.store,
                session_id,
                turn_id,
                tool_call_id,
                read_budget_chars,
                &auth,
            )
            .await
        }
        RetrieveArgs::Profile { agent, user } => {
            retrieve_profile(&ctx.store, user, agent, &auth).await
        }
        _ => super::signed::aborted(ResponseVerb::Retrieve, "unsupported retrieve target"),
    }
}

async fn retrieve_record(
    store: &SqliteMemoryStore,
    id: String,
    auth: &ReadAuthorization,
) -> Response {
    let record_id = match RecordId::parse(id.clone()) {
        Ok(record_id) => record_id,
        Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Retrieve, e),
    };
    let mut args = scoped_list_args(auth);
    args.record_ids = vec![record_id];
    args.limit = 1;
    match list_records(store, args).await {
        Ok(mut records) => match records.pop() {
            Some(record) => {
                committed_after_access(
                    store,
                    auth,
                    cairn_core::verbs::retrieve::record_data(&record),
                    std::slice::from_ref(&record),
                    None,
                )
                .await
            }
            None => committed(
                auth,
                cairn_core::verbs::retrieve::missing_record_data(Ulid(id)),
                &[],
                None,
            ),
        },
        Err(resp) => resp,
    }
}

async fn retrieve_folder(
    store: &SqliteMemoryStore,
    path: String,
    depth: Option<u64>,
    auth: &ReadAuthorization,
) -> Response {
    let mut records = match list_records(store, scoped_list_args(auth)).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| folder_matches(record, &path, depth));
    committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::folder_data(path, depth, &records),
        &records,
        None,
    )
    .await
}

async fn retrieve_scope(
    store: &SqliteMemoryStore,
    scope: ScopeFilter,
    auth: &ReadAuthorization,
) -> Response {
    let Some(list_args) = scoped_list_args_for_filter(auth, &scope) else {
        return committed(
            auth,
            cairn_core::verbs::retrieve::scope_data(scope, &[], None),
            &[],
            None,
        );
    };
    let mut records = match list_records(store, list_args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| scope_matches(record, &scope));
    committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::scope_data(scope, &records, None),
        &records,
        None,
    )
    .await
}

async fn retrieve_session(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    request: SessionRetrieveRequest,
    auth: &ReadAuthorization,
) -> Response {
    let SessionRetrieveRequest {
        session_id,
        limit,
        order,
        include,
        cursor,
        rehydrate,
        read_budget_chars,
    } = request;
    let rehydrate_started = rehydrate.then(Instant::now);
    let target_session = match SessionId::parse(session_id.clone()) {
        Ok(session_id) => session_id,
        Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Retrieve, e),
    };
    let order = order.unwrap_or(RetrieveArgsSessionOrder::Asc);
    let start = match parse_session_cursor(cursor.as_ref(), order) {
        Ok(start) => start,
        Err(resp) => return resp,
    };
    let requested_limit = limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(usize::MAX);
    let (hot_records, mut tree_policy_trace) = match session_records_for_retrieve(
        store,
        auth,
        &session_id,
        &target_session,
        read_budget_chars,
    )
    .await
    {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    let (records, source_tier) = if rehydrate {
        match cold_records_for_hot(vault_root, &session_id, &hot_records) {
            Ok(Some(cold_records)) => (cold_records, "cold"),
            Ok(None) => (hot_records, "hot_or_warm"),
            Err(error) => {
                return super::signed::aborted(
                    ResponseVerb::Retrieve,
                    format!("cold rehydrate: {error:#}"),
                );
            }
        }
    } else {
        (hot_records, "hot_or_warm")
    };
    let (include_reasoning, include_tool_calls) = session_include_flags(include.as_deref());
    let groups = session_turn_groups(records, order);
    let total_groups = groups.len();
    let groups = groups
        .into_iter()
        .skip(start)
        .take(requested_limit)
        .collect::<Vec<_>>();
    let (groups, mut budget_report) = trim_groups_to_budget(
        groups,
        read_budget_chars,
        include_reasoning,
        include_tool_calls,
    );
    if let Some(started) = rehydrate_started {
        budget_report.rehydrate = Some(RehydrateReport {
            elapsed_ms: started.elapsed().as_millis(),
            source_tier,
        });
    }
    let next_offset = start.saturating_add(groups.len());
    let next_cursor = (next_offset < total_groups).then(|| session_cursor(order, next_offset));
    let records = groups
        .into_iter()
        .flat_map(|group| group.records)
        .collect::<Vec<_>>();
    let mut response = committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::session_data_with_options(
            session_id,
            &records,
            next_cursor,
            include_reasoning,
            include_tool_calls,
        ),
        &records,
        Some(&budget_report),
    )
    .await;
    if records.is_empty() {
        tree_policy_trace.clear();
    }
    response.policy_trace.append(&mut tree_policy_trace);
    response
}

async fn session_records_for_retrieve(
    store: &SqliteMemoryStore,
    auth: &ReadAuthorization,
    session_id: &str,
    target_session: &SessionId,
    read_budget_chars: usize,
) -> Result<(Vec<MemoryRecord>, Vec<ResponsePolicyTrace>), Response> {
    let tree = match store.get_session_tree(target_session).await {
        Ok(tree) => tree,
        Err(e) if store_error_is_capability_unavailable(&e) => None,
        Err(e) => {
            return Err(super::signed::aborted(
                ResponseVerb::Retrieve,
                format!("session tree: {e}"),
            ));
        }
    };
    if let Some(tree) = tree
        && tree_has_retrieve_context(&tree, target_session)?
    {
        return tree_session_records_for_retrieve(
            store,
            auth,
            target_session,
            read_budget_chars,
            &tree,
        )
        .await;
    }
    flat_session_records_for_retrieve(store, auth, session_id)
        .await
        .map(|records| (records, Vec::new()))
}

fn tree_has_retrieve_context(
    tree: &cairn_core::domain::SessionTree,
    target_session: &SessionId,
) -> Result<bool, Response> {
    let lineage = tree.lineage(target_session).map_err(|e| {
        super::signed::aborted(ResponseVerb::Retrieve, format!("session tree: {e}"))
    })?;
    Ok(lineage.len() > 1 || !tree.merges().is_empty())
}

async fn tree_session_records_for_retrieve(
    store: &SqliteMemoryStore,
    auth: &ReadAuthorization,
    target_session: &SessionId,
    read_budget_chars: usize,
    tree: &cairn_core::domain::SessionTree,
) -> Result<(Vec<MemoryRecord>, Vec<ResponsePolicyTrace>), Response> {
    let lineage = tree.lineage(target_session).map_err(|e| {
        super::signed::aborted(ResponseVerb::Retrieve, format!("session tree: {e}"))
    })?;
    let mut lineage_records = Vec::new();
    for lineage_session in lineage {
        let mut session_records =
            flat_session_records_for_retrieve(store, auth, lineage_session.as_str()).await?;
        lineage_records.append(&mut session_records);
    }
    let tree_records = tree_read_records_from_memory_records(&lineage_records).map_err(|e| {
        super::signed::aborted(ResponseVerb::Retrieve, format!("session tree records: {e}"))
    })?;
    let window = plan_tree_read_window(TreeReadWindowInput {
        tree,
        target_session,
        records: &tree_records,
        budget_bytes: read_budget_chars,
    })
    .map_err(|e| super::signed::aborted(ResponseVerb::Retrieve, format!("session tree: {e}")))?;
    Ok((
        memory_records_in_tree_order(lineage_records, &window),
        tree_read_policy_trace(&window),
    ))
}

async fn flat_session_records_for_retrieve(
    store: &SqliteMemoryStore,
    auth: &ReadAuthorization,
    session_id: &str,
) -> Result<Vec<MemoryRecord>, Response> {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.session_id = Some(session_id.to_owned());
    }
    let mut records = list_records(store, args).await?;
    records.retain(|record| record.scope.session_id.as_deref() == Some(session_id));
    Ok(records)
}

async fn retrieve_turn(
    store: &SqliteMemoryStore,
    session_id: String,
    turn_id: String,
    include: Option<Vec<RetrieveArgsTurnInclude>>,
    read_budget_chars: usize,
    auth: &ReadAuthorization,
) -> Response {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.session_id = Some(session_id.clone());
    }
    let mut records = match list_records(store, args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| {
        record.scope.session_id.as_deref() == Some(session_id.as_str())
            && trace_turn_id(record).as_deref() == Some(turn_id.as_str())
    });
    sort_trace_records(&mut records, Some(RetrieveArgsSessionOrder::Asc));
    if records.is_empty() {
        return committed(
            auth,
            cairn_core::verbs::retrieve::empty_turn_data(session_id, turn_id),
            &[],
            None,
        );
    }
    let (include_reasoning, include_tool_calls) = turn_include_flags(include.as_deref());
    let (records, budget_report) = trim_records_to_budget(
        records,
        read_budget_chars,
        include_reasoning,
        include_tool_calls,
    );
    committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::turn_data_with_options(
            session_id,
            turn_id,
            &records,
            include_reasoning,
            include_tool_calls,
        ),
        &records,
        Some(&budget_report),
    )
    .await
}

async fn retrieve_tool_call(
    store: &SqliteMemoryStore,
    session_id: String,
    turn_id: String,
    tool_call_id: String,
    read_budget_chars: usize,
    auth: &ReadAuthorization,
) -> Response {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.session_id = Some(session_id.clone());
    }
    let mut records = match list_records(store, args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| {
        record.scope.session_id.as_deref() == Some(session_id.as_str())
            && trace_turn_id(record).as_deref() == Some(turn_id.as_str())
            && trace_tool_call_id(record).as_deref() == Some(tool_call_id.as_str())
    });
    sort_trace_records(&mut records, Some(RetrieveArgsSessionOrder::Asc));
    let (records, budget_report) = trim_records_to_budget(records, read_budget_chars, false, true);
    committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::tool_call_data(session_id, turn_id, tool_call_id, &records),
        &records,
        Some(&budget_report),
    )
    .await
}

async fn retrieve_profile(
    store: &SqliteMemoryStore,
    user: Option<String>,
    agent: Option<String>,
    auth: &ReadAuthorization,
) -> Response {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.user.clone_from(&user);
        scope.agent.clone_from(&agent);
    }
    let records = match list_records(store, args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    committed_after_access(
        store,
        auth,
        cairn_core::verbs::retrieve::profile_data(user, agent, &records),
        &records,
        None,
    )
    .await
}

async fn list_records(
    store: &SqliteMemoryStore,
    args: ListArgs,
) -> Result<Vec<MemoryRecord>, Response> {
    store
        .list(&args)
        .await
        .map(|page| page.records)
        .map_err(|e| super::signed::aborted(ResponseVerb::Retrieve, format!("store list: {e}")))
}

fn cold_records_for_hot(
    vault_root: &Path,
    session_id: &str,
    hot_records: &[MemoryRecord],
) -> anyhow::Result<Option<Vec<MemoryRecord>>> {
    let Some(bundle) = super::cold_session::load_bundle(vault_root, session_id)? else {
        return Ok(None);
    };
    let allowed_targets = hot_records
        .iter()
        .map(|record| record.target_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut records = bundle
        .records
        .into_iter()
        .filter(|record| {
            record.scope.session_id.as_deref() == Some(session_id)
                && allowed_targets.contains(record.target_id.as_str())
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        trace_sequence(left)
            .cmp(&trace_sequence(right))
            .then_with(|| trace_capture_event_id(left).cmp(&trace_capture_event_id(right)))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    Ok(Some(records))
}

fn committed(
    auth: &ReadAuthorization,
    data: RetrieveData,
    records: &[MemoryRecord],
    budget: Option<&BudgetReport>,
) -> Response {
    super::signed::committed_retrieve(
        auth.operation_id.clone(),
        data,
        read_policy_trace(auth, records, budget),
    )
}

async fn committed_after_access(
    store: &SqliteMemoryStore,
    auth: &ReadAuthorization,
    data: RetrieveData,
    records: &[MemoryRecord],
    budget: Option<&BudgetReport>,
) -> Response {
    record_access(store, records, "retrieve").await;
    let mut response = committed(auth, data, records, budget);
    response
        .policy_trace
        .extend(trace_step_result_ref_policy_trace(store, records).await);
    response
}

async fn record_access(store: &SqliteMemoryStore, records: &[MemoryRecord], reason: &str) {
    if records.is_empty() {
        return;
    }
    let record_ids = records
        .iter()
        .map(|record| record.id.clone())
        .collect::<Vec<_>>();
    let accessed_at_ms = current_unix_ms_i64();
    if let Err(e) = store
        .record_access(&record_ids, accessed_at_ms, reason)
        .await
    {
        tracing::warn!(error = %e, reason, "record access tracking failed");
    }
}

async fn signed_read_authorization(
    ctx: &super::signed::OpenedVerbContext,
    args: &RetrieveArgs,
    operation_id: Ulid,
) -> Result<ReadAuthorization, Response> {
    let issuer_wire =
        std::env::var("CAIRN_ISSUER").unwrap_or_else(|_| DEFAULT_RETRIEVE_ISSUER.to_owned());
    let issuer = Identity::parse(issuer_wire)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Retrieve, e))?;
    let active = ctx
        .identity
        .registry
        .get_identity(&issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Retrieve, format!("identity lookup: {e}"))
        })?
        .ok_or_else(|| {
            super::signed::rejected_from_domain(
                ResponseVerb::Retrieve,
                cairn_core::domain::DomainError::Unauthorized {
                    message: format!("issuer {issuer} is not active in this vault"),
                },
            )
        })?;
    let handle = SecretHandle::for_identity(
        ctx.identity.vault_id.clone(),
        issuer.clone(),
        active.current_key_version,
    );
    let signing_key = ctx
        .identity
        .keystore
        .load_signing_key(&handle)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Retrieve, format!("issuer key load: {e}"))
        })?;
    let target_hash = retrieve_args_hash(args)?;
    let mut intent = unsigned_retrieve_intent(
        &issuer,
        active.current_key_version.as_u32(),
        operation_id.clone(),
        ctx.config.vault.name.clone(),
        retrieve_entity(args),
        target_hash.clone(),
    );
    let intent_bytes = canonical_bytes_signed_intent(&intent)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Retrieve, e))?;
    intent.signature = Ed25519Signature(format!(
        "ed25519:{}",
        hex_lower(&signing_key.sign(&intent_bytes).to_bytes())
    ));
    let request = super::signed::request(
        RequestVerb::Retrieve,
        RequestArgs::Retrieve(args.clone()),
        intent,
    );
    let verified = super::signed::verify_request(ctx, request).await?;
    if verified.as_inner().target_hash != target_hash {
        return Err(super::signed::rejected_from_domain(
            ResponseVerb::Retrieve,
            cairn_core::domain::DomainError::Unauthorized {
                message: "retrieve args hash mismatch".to_owned(),
            },
        ));
    }
    let scope = ScopeTuple {
        tenant: Some(verified.as_inner().scope.tenant.clone()),
        workspace: Some(verified.as_inner().scope.workspace.clone()),
        entity: Some(verified.as_inner().scope.entity.clone()),
        ..ScopeTuple::default()
    };
    let max_visibility = intent_tier_to_visibility(verified.as_inner().scope.tier);
    Ok(ReadAuthorization {
        operation_id,
        scope: scope.clone(),
        max_visibility,
        rebac: cairn_core::rebac::RebacContext::for_scope(
            issuer,
            &scope,
            cairn_core::rebac::RebacAction::Read,
            max_visibility,
        ),
    })
}

fn unsigned_retrieve_intent(
    issuer: &Identity,
    key_version: u32,
    operation_id: Ulid,
    workspace: String,
    entity: String,
    target_hash: String,
) -> SignedIntent {
    let issued_at = chrono::Utc::now();
    let expires_at = issued_at + chrono::Duration::minutes(5);
    SignedIntent {
        chain_parents: vec![],
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issued_at: issued_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issuer: cairn_core::generated::common::Identity(issuer.as_str().to_owned()),
        key_version: i64::from(key_version),
        nonce: super::envelope::new_nonce(),
        operation_id,
        scope: SignedIntentScope {
            tenant: DEFAULT_TENANT.to_owned(),
            workspace,
            entity,
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(read_sequence()),
        server_challenge: None,
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash,
    }
}

fn retrieve_args_hash(args: &RetrieveArgs) -> Result<String, Response> {
    serde_json::to_vec(args)
        .map(|bytes| sha256_wire(&bytes))
        .map_err(|e| super::signed::aborted(ResponseVerb::Retrieve, format!("args hash: {e}")))
}

fn retrieve_entity(args: &RetrieveArgs) -> String {
    match args {
        RetrieveArgs::Scope { scope, .. } => scope
            .entity
            .clone()
            .unwrap_or_else(|| RETRIEVE_DEFAULT_ENTITY.to_owned()),
        _ => RETRIEVE_DEFAULT_ENTITY.to_owned(),
    }
}

fn scoped_list_args(auth: &ReadAuthorization) -> ListArgs {
    ListArgs {
        scope: Some(auth.scope.clone()),
        visibility_allowlist: read_visibility_allowlist(auth),
        limit: 1000,
        ..ListArgs::default()
    }
}

fn scoped_list_args_for_filter(
    auth: &ReadAuthorization,
    scope_filter: &ScopeFilter,
) -> Option<ListArgs> {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        if !merge_scope_value(&mut scope.tenant, scope_filter.tenant.as_ref()) {
            return None;
        }
        if !merge_scope_value(&mut scope.workspace, scope_filter.workspace.as_ref()) {
            return None;
        }
        if !merge_scope_value(&mut scope.entity, scope_filter.entity.as_ref()) {
            return None;
        }
        scope.session_id.clone_from(&scope_filter.session_id);
        scope.user.clone_from(&scope_filter.user);
        scope.agent.clone_from(&scope_filter.agent);
    }
    if let Some(ids) = &scope_filter.record_ids {
        let parsed = ids
            .iter()
            .map(|id| RecordId::parse(id.0.clone()))
            .collect::<Result<Vec<_>, _>>();
        let Ok(parsed) = parsed else {
            return None;
        };
        args.record_ids = parsed;
    }
    if let Some(kinds) = &scope_filter.kind
        && kinds.len() == 1
    {
        args.kind = cairn_core::domain::MemoryKind::parse(&kinds[0]).ok();
    }
    Some(args)
}

fn merge_scope_value(current: &mut Option<String>, requested: Option<&String>) -> bool {
    match (current.as_deref(), requested.map(String::as_str)) {
        (_, None) => true,
        (Some(current), Some(requested)) if current == requested => true,
        (None, Some(requested)) => {
            *current = Some(requested.to_owned());
            true
        }
        (Some(_), Some(_)) => false,
    }
}

fn read_visibility_allowlist(auth: &ReadAuthorization) -> Vec<MemoryVisibility> {
    auth.rebac
        .allowed_visibilities_up_to(
            cairn_core::rebac::RebacAction::Read,
            &auth.scope,
            auth.max_visibility,
        )
        .0
}

fn intent_tier_to_visibility(tier: SignedIntentScopeTier) -> MemoryVisibility {
    match tier {
        SignedIntentScopeTier::Session => MemoryVisibility::Session,
        SignedIntentScopeTier::Project => MemoryVisibility::Project,
        SignedIntentScopeTier::Team => MemoryVisibility::Team,
        SignedIntentScopeTier::Org => MemoryVisibility::Org,
        SignedIntentScopeTier::Public => MemoryVisibility::Public,
        _ => MemoryVisibility::Private,
    }
}

fn read_policy_trace(
    auth: &ReadAuthorization,
    records: &[MemoryRecord],
    budget: Option<&BudgetReport>,
) -> Vec<ResponsePolicyTrace> {
    let consent_detail = if records.is_empty() {
        "no_records".to_owned()
    } else if records
        .iter()
        .any(|record| matches!(record.consent_model, Some(ConsentModel::ReceiptTimeline)))
    {
        "receipt_timeline".to_owned()
    } else {
        "legacy_event".to_owned()
    };
    let mut trace = vec![
        ResponsePolicyTrace {
            detail: Some("signed_scope_verified".to_owned()),
            gate: "read.scope".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(format!("tier<={}", auth.max_visibility.as_str())),
            gate: "read.visibility".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(consent_detail),
            gate: "read.consent".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
    ];
    let rebac_trace = auth
        .rebac
        .allowed_visibilities_up_to(
            cairn_core::rebac::RebacAction::Read,
            &auth.scope,
            auth.max_visibility,
        )
        .1
        .into_iter()
        .map(cairn_core::rebac::RebacDecision::to_policy_trace_entry)
        .collect::<Vec<_>>();
    trace.extend(cairn_core::policy_trace::to_wire(&rebac_trace));
    if let Some(budget) = budget {
        trace.push(ResponsePolicyTrace {
            detail: Some(format!(
                "chars={} items_in={} items_out={} turns_in={} turns_out={} trimmed={}",
                budget.budget_chars,
                budget.items_in,
                budget.items_out,
                budget.turns_in,
                budget.turns_out,
                budget.trimmed
            )),
            gate: "read.budget".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        });
        if let Some(rehydrate) = &budget.rehydrate {
            trace.push(ResponsePolicyTrace {
                detail: Some(format!(
                    "requested=true source_tier={} elapsed_ms={} budget_chars={} items_in={} items_out={} turns_in={} turns_out={} trimmed={}",
                    rehydrate.source_tier,
                    rehydrate.elapsed_ms,
                    budget.budget_chars,
                    budget.items_in,
                    budget.items_out,
                    budget.turns_in,
                    budget.turns_out,
                    budget.trimmed
                )),
                gate: "read.rehydrate".to_owned(),
                result: ResponsePolicyTraceResult::Pass,
            });
        }
    }
    trace
}

async fn trace_step_result_ref_policy_trace(
    store: &SqliteMemoryStore,
    records: &[MemoryRecord],
) -> Vec<ResponsePolicyTrace> {
    if records.is_empty() {
        return Vec::new();
    }

    let mut records_with_steps = 0_usize;
    let mut steps = 0_usize;
    for record in records {
        match store
            .find_trace_steps_by_result_ref(record.id.as_str())
            .await
        {
            Ok(found) if !found.is_empty() => {
                records_with_steps = records_with_steps.saturating_add(1);
                steps = steps.saturating_add(found.len());
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "trace-step result-ref lookup failed during retrieve"
                );
            }
        }
    }

    if steps == 0 {
        return Vec::new();
    }

    vec![ResponsePolicyTrace {
        detail: Some(format!("records={records_with_steps} steps={steps}")),
        gate: "trace_canvas.result_ref".to_owned(),
        result: ResponsePolicyTraceResult::Pass,
    }]
}

fn tree_read_records_from_memory_records(
    records: &[MemoryRecord],
) -> Result<Vec<TreeReadRecord>, cairn_core::domain::DomainError> {
    records
        .iter()
        .map(|record| {
            let session_id = record.scope.session_id.clone().ok_or(
                cairn_core::domain::DomainError::MalformedScope {
                    message: "tree read record missing session_id".to_owned(),
                },
            )?;
            Ok(TreeReadRecord {
                session_id: SessionId::parse(session_id)?,
                turn_id: trace_turn_id(record),
                record_id: record.id.clone(),
                body: record.body.clone(),
            })
        })
        .collect()
}

fn memory_records_in_tree_order(
    records: Vec<MemoryRecord>,
    window: &TreeReadWindow,
) -> Vec<MemoryRecord> {
    let records_by_id = records
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    window
        .selected_records
        .iter()
        .filter_map(|record| records_by_id.get(&record.record_id).cloned())
        .collect()
}

fn tree_read_policy_trace(window: &TreeReadWindow) -> Vec<ResponsePolicyTrace> {
    if window.selected_records.is_empty() {
        return Vec::new();
    }
    let mut trace = vec![ResponsePolicyTrace {
        detail: Some(format!(
            "path_sessions={} records_in={} records_out={} skipped_for_budget={} trimmed={}",
            window.ancestry_path.len(),
            window.budget.records_in,
            window.budget.records_out,
            window.budget.skipped_for_budget,
            window.budget.trimmed
        )),
        gate: "tree.lineage".to_owned(),
        result: ResponsePolicyTraceResult::Pass,
    }];
    if !window.sibling_sessions.is_empty() {
        trace.push(ResponsePolicyTrace {
            detail: Some(format!(
                "sibling_sessions={}",
                window.sibling_sessions.len()
            )),
            gate: "tree.sibling_context".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        });
    }
    if !window.merge_notes.is_empty() {
        trace.push(ResponsePolicyTrace {
            detail: Some(format!("merge_notes={}", window.merge_notes.len())),
            gate: "tree.merge_context".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        });
    }
    trace
}

fn store_error_is_capability_unavailable(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.to_string().contains("capability unavailable") {
            return true;
        }
        current = error.source();
    }
    false
}

fn session_include_flags(include: Option<&[RetrieveArgsSessionInclude]>) -> (bool, bool) {
    let include_reasoning = include.is_some_and(|items| {
        items
            .iter()
            .any(|item| matches!(item, RetrieveArgsSessionInclude::Reasoning))
    });
    let include_tool_calls = include.is_some_and(|items| {
        items
            .iter()
            .any(|item| matches!(item, RetrieveArgsSessionInclude::ToolCalls))
    });
    (include_reasoning, include_tool_calls)
}

fn turn_include_flags(include: Option<&[RetrieveArgsTurnInclude]>) -> (bool, bool) {
    let include_reasoning = include.is_some_and(|items| {
        items
            .iter()
            .any(|item| matches!(item, RetrieveArgsTurnInclude::Reasoning))
    });
    let include_tool_calls = include.is_some_and(|items| {
        items
            .iter()
            .any(|item| matches!(item, RetrieveArgsTurnInclude::ToolCalls))
    });
    (include_reasoning, include_tool_calls)
}

#[allow(
    clippy::too_many_lines,
    reason = "generated retrieve CLI has five mutually exclusive target branches"
)]
fn retrieve_args_from_matches(sub: &ArgMatches) -> Result<RetrieveArgs, Response> {
    let value = if let Some(id) = sub.get_one::<String>("id") {
        reject_branch_args(
            sub,
            "record",
            &[
                "session_id",
                "limit",
                "order",
                "rehydrate",
                "include",
                "cursor",
                "turn_id",
                "tool_call_id",
                "path",
                "depth",
                "scope",
                "profile",
                "user",
                "agent",
            ],
        )?;
        serde_json::json!({ "target": "record", "id": id })
    } else if let Some(tool_call_id) = sub.get_one::<String>("tool_call_id") {
        reject_branch_args(
            sub,
            "tool_call",
            &[
                "id",
                "limit",
                "order",
                "rehydrate",
                "include",
                "cursor",
                "path",
                "depth",
                "scope",
                "profile",
                "user",
                "agent",
            ],
        )?;
        let session_id = sub.get_one::<String>("session_id").ok_or_else(|| {
            invalid_args_response(
                ResponseVerb::Retrieve,
                "session_id",
                "required for retrieve tool_call",
            )
        })?;
        let turn_id = sub.get_one::<String>("turn_id").ok_or_else(|| {
            invalid_args_response(
                ResponseVerb::Retrieve,
                "turn_id",
                "required for retrieve tool_call",
            )
        })?;
        serde_json::json!({
            "target": "tool_call",
            "session_id": session_id,
            "turn_id": turn_id,
            "tool_call_id": tool_call_id,
        })
    } else if let Some(session_id) = sub.get_one::<String>("session_id") {
        if let Some(turn_id) = sub.get_one::<String>("turn_id") {
            reject_branch_args(
                sub,
                "turn",
                &[
                    "id",
                    "limit",
                    "order",
                    "rehydrate",
                    "cursor",
                    "tool_call_id",
                    "path",
                    "depth",
                    "scope",
                    "profile",
                    "user",
                    "agent",
                ],
            )?;
            json_with_optional(
                serde_json::json!({
                    "target": "turn",
                    "session_id": session_id,
                    "turn_id": turn_id,
                }),
                "include",
                include_values(sub),
            )
        } else {
            reject_branch_args(
                sub,
                "session",
                &[
                    "id",
                    "tool_call_id",
                    "path",
                    "depth",
                    "scope",
                    "profile",
                    "user",
                    "agent",
                ],
            )?;
            let mut value = serde_json::json!({
                "target": "session",
                "session_id": session_id,
            });
            set_optional(
                &mut value,
                "cursor",
                sub.get_one::<String>("cursor").cloned(),
            );
            set_optional(
                &mut value,
                "limit",
                sub.get_one::<u32>("limit").map(|v| i64::from(*v)),
            );
            set_optional(&mut value, "order", sub.get_one::<String>("order").cloned());
            set_optional(&mut value, "include", include_values(sub));
            if sub.get_flag("rehydrate") {
                set_optional(&mut value, "rehydrate", Some(true));
            }
            value
        }
    } else if let Some(path) = sub.get_one::<String>("path") {
        reject_branch_args(
            sub,
            "folder",
            &[
                "id",
                "session_id",
                "limit",
                "order",
                "rehydrate",
                "include",
                "cursor",
                "turn_id",
                "tool_call_id",
                "scope",
                "profile",
                "user",
                "agent",
            ],
        )?;
        let mut value = serde_json::json!({
            "target": "folder",
            "path": path,
        });
        set_optional(
            &mut value,
            "depth",
            sub.get_one::<u8>("depth").map(|v| u64::from(*v)),
        );
        value
    } else if let Some(scope) = sub.get_one::<String>("scope") {
        reject_branch_args(
            sub,
            "scope",
            &[
                "id",
                "session_id",
                "limit",
                "order",
                "rehydrate",
                "include",
                "turn_id",
                "tool_call_id",
                "path",
                "depth",
                "profile",
                "user",
                "agent",
            ],
        )?;
        let scope_value: serde_json::Value = serde_json::from_str(scope).map_err(|e| {
            invalid_args_response(
                ResponseVerb::Retrieve,
                "scope",
                &format!("invalid JSON: {e}"),
            )
        })?;
        let mut value = serde_json::json!({
            "target": "scope",
            "scope": scope_value,
        });
        set_optional(
            &mut value,
            "cursor",
            sub.get_one::<String>("cursor").cloned(),
        );
        value
    } else if sub.get_flag("profile") {
        reject_branch_args(
            sub,
            "profile",
            &[
                "id",
                "session_id",
                "limit",
                "order",
                "rehydrate",
                "include",
                "cursor",
                "turn_id",
                "tool_call_id",
                "path",
                "depth",
                "scope",
            ],
        )?;
        let mut value = serde_json::json!({ "target": "profile" });
        set_optional(&mut value, "user", sub.get_one::<String>("user").cloned());
        set_optional(&mut value, "agent", sub.get_one::<String>("agent").cloned());
        value
    } else {
        return Err(invalid_args_response(
            ResponseVerb::Retrieve,
            "target",
            "one retrieve target is required",
        ));
    };

    serde_json::from_value(value)
        .map_err(|e| invalid_args_response(ResponseVerb::Retrieve, "args", &e.to_string()))
}

fn reject_branch_args(sub: &ArgMatches, target: &str, disallowed: &[&str]) -> Result<(), Response> {
    if let Some(name) = disallowed.iter().find(|name| arg_present(sub, name)) {
        return Err(invalid_args_response(
            ResponseVerb::Retrieve,
            name,
            &format!("not valid for retrieve {target}"),
        ));
    }
    Ok(())
}

fn arg_present(sub: &ArgMatches, name: &str) -> bool {
    match name {
        "id" | "session_id" | "turn_id" | "tool_call_id" | "path" | "scope" | "user" | "agent"
        | "cursor" | "order" => sub.get_one::<String>(name).is_some(),
        "limit" => sub.get_one::<u32>(name).is_some(),
        "depth" => sub.get_one::<u8>(name).is_some(),
        "include" => sub.get_many::<String>(name).is_some(),
        "profile" | "rehydrate" => sub.get_flag(name),
        _ => false,
    }
}

fn include_values(sub: &ArgMatches) -> Option<Vec<String>> {
    let values = sub
        .get_many::<String>("include")?
        .map(String::to_owned)
        .collect::<Vec<_>>();
    Some(values)
}

fn json_with_optional<T: serde::Serialize>(
    mut value: serde_json::Value,
    key: &str,
    item: Option<T>,
) -> serde_json::Value {
    set_optional(&mut value, key, item);
    value
}

fn set_optional<T: serde::Serialize>(value: &mut serde_json::Value, key: &str, item: Option<T>) {
    if let Some(item) = item
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            key.to_owned(),
            serde_json::to_value(item).expect("invariant: retrieve CLI args are JSON-serializable"),
        );
    }
}

fn folder_matches(record: &MemoryRecord, path: &str, depth: Option<u64>) -> bool {
    let Some(source_path) = record_source_path(record) else {
        return false;
    };
    if source_path == path {
        return true;
    }
    let Some(tail) = source_path
        .strip_prefix(path)
        .and_then(|tail| tail.strip_prefix('/'))
    else {
        return false;
    };
    depth.is_none_or(|max_depth| {
        let depth = tail.split('/').filter(|part| !part.is_empty()).count() as u64;
        depth <= max_depth
    })
}

fn record_source_path(record: &MemoryRecord) -> Option<&str> {
    ["folder", "path", "source_path", "file"]
        .iter()
        .find_map(|key| {
            record
                .extra_frontmatter
                .get(*key)
                .and_then(serde_json::Value::as_str)
        })
}

fn scope_matches(record: &MemoryRecord, scope: &ScopeFilter) -> bool {
    option_matches(scope.tenant.as_ref(), record.scope.tenant.as_deref())
        && option_matches(scope.workspace.as_ref(), record.scope.workspace.as_deref())
        && option_matches(scope.entity.as_ref(), record.scope.entity.as_deref())
        && option_matches(scope.user.as_ref(), record.scope.user.as_deref())
        && option_matches(scope.agent.as_ref(), record.scope.agent.as_deref())
        && option_matches(
            scope.session_id.as_ref(),
            record.scope.session_id.as_deref(),
        )
        && scope
            .kind
            .as_ref()
            .is_none_or(|kinds| kinds.iter().any(|kind| kind == record.kind.as_str()))
        && scope
            .record_ids
            .as_ref()
            .is_none_or(|ids| ids.iter().any(|id| id.0.as_str() == record.id.as_str()))
        && scope.tags.as_ref().is_none_or(|tags| {
            tags.iter()
                .all(|tag| record.tags.iter().any(|record_tag| record_tag == tag))
        })
        && scope
            .tier
            .is_none_or(|tier| tier_matches(record.visibility.as_str(), tier))
}

fn option_matches(expected: Option<&String>, actual: Option<&str>) -> bool {
    expected
        .map(String::as_str)
        .is_none_or(|value| actual == Some(value))
}

fn tier_matches(actual: &str, expected: ScopeFilterTier) -> bool {
    let expected = match expected {
        ScopeFilterTier::Private => "private",
        ScopeFilterTier::Session => "session",
        ScopeFilterTier::Project => "project",
        ScopeFilterTier::Team => "team",
        ScopeFilterTier::Org => "org",
        ScopeFilterTier::Public => "public",
        _ => return false,
    };
    actual == expected
}

fn sort_trace_records(records: &mut [MemoryRecord], order: Option<RetrieveArgsSessionOrder>) {
    records.sort_by(|a, b| {
        trace_sequence(a)
            .cmp(&trace_sequence(b))
            .then_with(|| trace_capture_event_id(a).cmp(&trace_capture_event_id(b)))
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    if matches!(order, Some(RetrieveArgsSessionOrder::Desc)) {
        records.reverse();
    }
}

fn session_turn_groups(
    mut records: Vec<MemoryRecord>,
    order: RetrieveArgsSessionOrder,
) -> Vec<SessionTurnGroup> {
    records.retain(|record| trace_turn_id(record).is_some());
    let mut by_turn = std::collections::BTreeMap::<String, Vec<MemoryRecord>>::new();
    for record in records {
        let turn_id = trace_turn_id(&record).expect("retained trace turn id");
        by_turn.entry(turn_id).or_default().push(record);
    }
    let mut groups = by_turn
        .into_iter()
        .map(|(turn_id, mut records)| {
            sort_trace_records(&mut records, Some(RetrieveArgsSessionOrder::Asc));
            let sort_time = records
                .iter()
                .map(|record| record.updated_at.as_str().to_owned())
                .min()
                .unwrap_or_default();
            SessionTurnGroup {
                turn_id,
                sort_time,
                records,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        a.sort_time
            .cmp(&b.sort_time)
            .then_with(|| a.turn_id.cmp(&b.turn_id))
    });
    if matches!(order, RetrieveArgsSessionOrder::Desc) {
        groups.reverse();
    }
    groups
}

fn parse_session_cursor(
    cursor: Option<&Cursor>,
    order: RetrieveArgsSessionOrder,
) -> Result<usize, Response> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let expected_order = session_order_wire(order);
    let parts = cursor.0.split(':').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "session" || parts[1] != "v1" || parts[2] != expected_order {
        return Err(invalid_args_response(
            ResponseVerb::Retrieve,
            "cursor",
            "invalid session cursor for requested order",
        ));
    }
    parts[3].parse::<usize>().map_err(|_| {
        invalid_args_response(
            ResponseVerb::Retrieve,
            "cursor",
            "invalid session cursor offset",
        )
    })
}

fn session_cursor(order: RetrieveArgsSessionOrder, offset: usize) -> Cursor {
    Cursor(format!(
        "{SESSION_CURSOR_PREFIX}:{}:{offset}",
        session_order_wire(order)
    ))
}

fn session_order_wire(order: RetrieveArgsSessionOrder) -> &'static str {
    match order {
        RetrieveArgsSessionOrder::Desc => "desc",
        _ => "asc",
    }
}

fn trim_records_to_budget(
    records: Vec<MemoryRecord>,
    budget_chars: usize,
    include_reasoning: bool,
    include_tool_calls: bool,
) -> (Vec<MemoryRecord>, BudgetReport) {
    let items_in = records.len();
    let mut used = 0usize;
    let mut out = Vec::new();
    for record in records {
        let cost = record_output_chars(&record, include_reasoning, include_tool_calls);
        if !out.is_empty() && used.saturating_add(cost) > budget_chars {
            break;
        }
        used = used.saturating_add(cost);
        out.push(record);
    }
    let report = BudgetReport {
        budget_chars,
        items_in,
        items_out: out.len(),
        turns_in: 0,
        turns_out: 0,
        trimmed: out.len() < items_in,
        rehydrate: None,
    };
    (out, report)
}

fn trim_groups_to_budget(
    groups: Vec<SessionTurnGroup>,
    budget_chars: usize,
    include_reasoning: bool,
    include_tool_calls: bool,
) -> (Vec<SessionTurnGroup>, BudgetReport) {
    let turns_in = groups.len();
    let items_in = groups.iter().map(|group| group.records.len()).sum();
    let mut used = 0usize;
    let mut out = Vec::new();
    for group in groups {
        let cost = group
            .records
            .iter()
            .map(|record| record_output_chars(record, include_reasoning, include_tool_calls))
            .sum::<usize>();
        if !out.is_empty() && used.saturating_add(cost) > budget_chars {
            break;
        }
        used = used.saturating_add(cost);
        out.push(group);
    }
    let items_out = out.iter().map(|group| group.records.len()).sum();
    let report = BudgetReport {
        budget_chars,
        items_in,
        items_out,
        turns_in,
        turns_out: out.len(),
        trimmed: out.len() < turns_in,
        rehydrate: None,
    };
    (out, report)
}

fn record_output_chars(
    record: &MemoryRecord,
    include_reasoning: bool,
    include_tool_calls: bool,
) -> usize {
    let event = trace_event(record);
    let is_tool_event = matches!(
        event.as_deref(),
        Some("pre_tool" | "post_tool" | "tool_output")
    );
    let is_reasoning = record.kind == MemoryKind::Reasoning;
    if (is_tool_event && !include_tool_calls) || (is_reasoning && !include_reasoning) {
        0
    } else {
        record.body.chars().count()
    }
}

fn trace_sequence(record: &MemoryRecord) -> Option<u64> {
    record
        .extra_frontmatter
        .get("trace")
        .and_then(|trace| trace.get("sequence"))
        .and_then(serde_json::Value::as_u64)
}

fn trace_capture_event_id(record: &MemoryRecord) -> Option<String> {
    trace_string(record, "capture_event_id")
}

fn trace_tool_call_id(record: &MemoryRecord) -> Option<String> {
    trace_string(record, "tool_call_id")
}

fn trace_turn_id(record: &MemoryRecord) -> Option<String> {
    trace_string(record, "turn_id")
}

fn trace_event(record: &MemoryRecord) -> Option<String> {
    record
        .extra_frontmatter
        .get("trace_event")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn trace_string(record: &MemoryRecord, key: &str) -> Option<String> {
    record
        .extra_frontmatter
        .get("trace")
        .and_then(|trace| trace.get(key))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => ExitCode::from(64),
        ResponseStatus::Aborted => ExitCode::from(78),
        _ => ExitCode::FAILURE,
    }
}

fn emit_human(resp: &Response) {
    match (&resp.status, resp.data.as_ref()) {
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Record(data)))) => {
            if let Some(body) = data.body.as_deref() {
                println!("{body}");
            }
        }
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Folder(data)))) => {
            emit_refs(&data.items);
        }
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Scope(data)))) => {
            emit_refs(&data.items);
        }
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Session(data)))) => {
            emit_turn_items(&data.items);
        }
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Turn(data)))) => {
            emit_turn_items(&data.turn);
        }
        (ResponseStatus::Committed, Some(ResponseData::Retrieve(RetrieveData::Profile(data)))) => {
            println!(
                "{}",
                serde_json::to_string_pretty(data)
                    .expect("invariant: profile data is JSON-serializable")
            );
        }
        _ => {
            let code = resp
                .error
                .as_ref()
                .and_then(|e| e.get("code"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Internal");
            let message = resp
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("retrieve failed");
            human_error("retrieve", code, message, &resp.operation_id);
        }
    }
}

fn emit_refs(items: &[cairn_core::generated::verbs::retrieve::RecordRef]) {
    for item in items {
        if let Some(snippet) = item.snippet.as_deref() {
            println!("{}\t{}\t{}", item.record_id.0, item.kind, snippet);
        } else {
            println!("{}\t{}", item.record_id.0, item.kind);
        }
    }
}

fn emit_turn_items(items: &[TurnItem]) {
    for item in items {
        if let Some(content) = item.content.as_deref() {
            println!("{content}");
        }
    }
}

fn sha256_wire(payload: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(payload);
    format!("sha256:{}", hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn read_sequence() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn current_unix_ms_i64() -> i64 {
    i64::try_from(read_sequence()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::record::tests_export::sample_record;
    use cairn_core::domain::{RecordId, ScopeTuple};

    use super::*;

    fn record(id: &str, session_id: &str, turn_id: &str, body: &str) -> MemoryRecord {
        let mut record = sample_record();
        record.id = RecordId::parse(id).expect("valid record id");
        record.scope = ScopeTuple {
            session_id: Some(session_id.to_owned()),
            ..ScopeTuple::default()
        };
        record.body = body.to_owned();
        record.extra_frontmatter.insert(
            "trace".to_owned(),
            serde_json::json!({
                "turn_id": turn_id,
            }),
        );
        record
    }

    #[test]
    fn tree_records_from_memory_records_preserve_trace_turn_ids() {
        let records = vec![
            record("01JTS6R4J70000000000000001", "root", "turn-1", "root body"),
            record(
                "01JTS6R4J70000000000000002",
                "branch",
                "turn-2",
                "branch body",
            ),
        ];

        let tree_records = tree_read_records_from_memory_records(&records)
            .expect("memory records should convert to tree read records");

        assert_eq!(tree_records.len(), 2);
        assert_eq!(tree_records[0].session_id.as_str(), "root");
        assert_eq!(tree_records[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            tree_records[0].record_id.as_str(),
            "01JTS6R4J70000000000000001"
        );
        assert_eq!(tree_records[0].body, "root body");
        assert_eq!(tree_records[1].session_id.as_str(), "branch");
        assert_eq!(tree_records[1].turn_id.as_deref(), Some("turn-2"));
        assert_eq!(
            tree_records[1].record_id.as_str(),
            "01JTS6R4J70000000000000002"
        );
        assert_eq!(tree_records[1].body, "branch body");
    }

    #[test]
    fn flat_tree_is_not_treated_as_tree_retrieve_context() {
        let root = SessionId::parse("root").expect("root session");
        let tree = cairn_core::domain::SessionTree::flat(root.clone());

        assert!(
            !tree_has_retrieve_context(&tree, &root).expect("flat context check"),
            "synthesized flat trees must preserve legacy retrieve ordering and budget"
        );
    }

    #[test]
    fn tree_trace_is_non_identifying_and_suppressed_without_selected_records() {
        let root = SessionId::parse("private-root").expect("root session");
        let branch = SessionId::parse("private-branch").expect("branch session");
        let sibling = SessionId::parse("private-peer").expect("sibling session");
        let mut tree = cairn_core::domain::SessionTree::flat(root.clone());
        tree.fork(&root, branch.clone(), "turn-2").expect("fork");
        tree.fork(&root, sibling.clone(), "turn-2")
            .expect("sibling fork");
        let records = vec![TreeReadRecord {
            session_id: sibling,
            turn_id: Some("turn-3".to_owned()),
            record_id: RecordId::parse("01JTS6R4J70000000000000003").expect("record id"),
            body: "sibling secret body".to_owned(),
        }];
        let empty_window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &records,
            budget_bytes: 1024,
        })
        .expect("window");

        assert!(tree_read_policy_trace(&empty_window).is_empty());

        let authorized = vec![TreeReadRecord {
            session_id: branch.clone(),
            turn_id: Some("turn-4".to_owned()),
            record_id: RecordId::parse("01JTS6R4J70000000000000004").expect("record id"),
            body: "branch secret body".to_owned(),
        }];
        let window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &authorized,
            budget_bytes: 1024,
        })
        .expect("authorized window");
        let detail = tree_read_policy_trace(&window)
            .iter()
            .filter_map(|entry| entry.detail.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(detail.contains("path_sessions=2"));
        assert!(!detail.contains("private-root"));
        assert!(!detail.contains("private-branch"));
        assert!(!detail.contains("private-peer"));
        assert!(!detail.contains("secret body"));
    }

    #[tokio::test]
    async fn trace_step_result_ref_policy_trace_reports_counts_only() {
        let store = cairn_store_sqlite::open_in_memory()
            .await
            .expect("open store");
        let record = record(
            "01JTS6R4J70000000000000005",
            "session-1",
            "turn-1",
            "secret retrieved body",
        );
        store
            .upsert_trace_step(cairn_store_sqlite::TraceStepDraft {
                step_id: "step-1".to_owned(),
                trace_id: "trace-1".to_owned(),
                session_id: "session-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                tool_call_id: Some("toolu-1".to_owned()),
                timestamp_ms: 1,
                tool_name: Some("shell".to_owned()),
                call_summary: "call summary should not leak".to_owned(),
                result_summary: "result summary should not leak".to_owned(),
                result_ref: Some(record.id.as_str().to_owned()),
                salience: 0.5,
                replaceability_score: 0.5,
                node_id: None,
                source_hash: "hash-1".to_owned(),
            })
            .await
            .expect("trace step");

        let trace = trace_step_result_ref_policy_trace(&store, std::slice::from_ref(&record)).await;

        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].gate, "trace_canvas.result_ref");
        assert_eq!(trace[0].detail.as_deref(), Some("records=1 steps=1"));
        let detail = trace[0].detail.as_deref().unwrap_or_default();
        assert!(!detail.contains(record.id.as_str()));
        assert!(!detail.contains("secret"));
        assert!(!detail.contains("step-1"));
    }
}
