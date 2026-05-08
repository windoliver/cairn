//! `cairn retrieve` handler.
#![allow(
    clippy::result_large_err,
    reason = "CLI helpers return complete response envelopes for direct JSON emission"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::identity::keys::SecretHandle;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{Identity, MemoryRecord, RecordId, ScopeTuple};
use cairn_core::generated::common::{Ed25519Signature, ScopeFilter, ScopeFilterTier, Ulid};
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

#[derive(Clone)]
struct ReadAuthorization {
    operation_id: Ulid,
    scope: ScopeTuple,
    max_visibility: MemoryVisibility,
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
            session_id,
            ..
        } => {
            if cursor.is_some() {
                return invalid_args_response(
                    ResponseVerb::Retrieve,
                    "cursor",
                    "session pagination is not yet supported",
                );
            }
            retrieve_session(&ctx.store, session_id, limit, order, include, &auth).await
        }
        RetrieveArgs::Turn {
            include,
            session_id,
            turn_id,
            ..
        } => retrieve_turn(&ctx.store, session_id, turn_id, include, &auth).await,
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
            Some(record) => committed(
                auth,
                cairn_core::verbs::retrieve::record_data(&record),
                std::slice::from_ref(&record),
            ),
            None => committed(
                auth,
                cairn_core::verbs::retrieve::missing_record_data(Ulid(id)),
                &[],
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
    committed(
        auth,
        cairn_core::verbs::retrieve::folder_data(path, depth, &records),
        &records,
    )
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
        );
    };
    let mut records = match list_records(store, list_args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| scope_matches(record, &scope));
    committed(
        auth,
        cairn_core::verbs::retrieve::scope_data(scope, &records, None),
        &records,
    )
}

async fn retrieve_session(
    store: &SqliteMemoryStore,
    session_id: String,
    limit: Option<i64>,
    order: Option<RetrieveArgsSessionOrder>,
    include: Option<Vec<RetrieveArgsSessionInclude>>,
    auth: &ReadAuthorization,
) -> Response {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.session_id = Some(session_id.clone());
    }
    let requested_limit = limit.and_then(|v| usize::try_from(v).ok());
    let mut records = match list_records(store, args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    records.retain(|record| record.scope.session_id.as_deref() == Some(session_id.as_str()));
    sort_trace_records(&mut records, order);
    if let Some(limit) = requested_limit {
        records.truncate(limit);
    }
    let (include_reasoning, include_tool_calls) = session_include_flags(include.as_deref());
    committed(
        auth,
        cairn_core::verbs::retrieve::session_data_with_options(
            session_id,
            &records,
            None,
            include_reasoning,
            include_tool_calls,
        ),
        &records,
    )
}

async fn retrieve_turn(
    store: &SqliteMemoryStore,
    session_id: String,
    turn_id: String,
    include: Option<Vec<RetrieveArgsTurnInclude>>,
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
        );
    }
    let (include_reasoning, include_tool_calls) = turn_include_flags(include.as_deref());
    committed(
        auth,
        cairn_core::verbs::retrieve::turn_data_with_options(
            session_id,
            turn_id,
            &records,
            include_reasoning,
            include_tool_calls,
        ),
        &records,
    )
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
    committed(
        auth,
        cairn_core::verbs::retrieve::profile_data(user, agent, &records),
        &records,
    )
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

fn committed(auth: &ReadAuthorization, data: RetrieveData, records: &[MemoryRecord]) -> Response {
    super::signed::committed_retrieve(
        auth.operation_id.clone(),
        data,
        read_policy_trace(auth, records),
    )
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
    Ok(ReadAuthorization {
        operation_id,
        scope: ScopeTuple {
            tenant: Some(verified.as_inner().scope.tenant.clone()),
            workspace: Some(verified.as_inner().scope.workspace.clone()),
            entity: Some(verified.as_inner().scope.entity.clone()),
            ..ScopeTuple::default()
        },
        max_visibility: intent_tier_to_visibility(verified.as_inner().scope.tier),
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
        visibility_allowlist: visibility_allowlist(auth.max_visibility),
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

fn visibility_allowlist(max: MemoryVisibility) -> Vec<MemoryVisibility> {
    [
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ]
    .into_iter()
    .filter(|visibility| *visibility <= max)
    .collect()
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
    vec![
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
    ]
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
                "path",
                "depth",
                "scope",
                "profile",
                "user",
                "agent",
            ],
        )?;
        serde_json::json!({ "target": "record", "id": id })
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
                &["id", "path", "depth", "scope", "profile", "user", "agent"],
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
        "id" | "session_id" | "turn_id" | "path" | "scope" | "user" | "agent" | "cursor"
        | "order" => sub.get_one::<String>(name).is_some(),
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
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    if matches!(order, Some(RetrieveArgsSessionOrder::Desc)) {
        records.reverse();
    }
}

fn trace_sequence(record: &MemoryRecord) -> u64 {
    record
        .extra_frontmatter
        .get("trace")
        .and_then(|trace| trace.get("sequence"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX)
}

fn trace_turn_id(record: &MemoryRecord) -> Option<String> {
    record
        .extra_frontmatter
        .get("trace")
        .and_then(|trace| trace.get("turn_id"))
        .and_then(serde_json::Value::as_str)
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
