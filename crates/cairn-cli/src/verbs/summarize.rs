//! `cairn summarize` handler.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::canonical::{
    canonical_bytes_signed_intent, canonical_bytes_signed_payload,
};
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::identity::keys::SecretHandle;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{
    Identity, MemoryRecord, RecordId, ScopeTuple, SignedAdmission, WalActionKind,
};
use cairn_core::generated::common::{Ed25519Signature, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{
    RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace,
    ResponsePolicyTraceResult, ResponseStatus, ResponseVerb, SignedIntent, SignedIntentScope,
    SignedIntentScopeTier,
};
use cairn_core::generated::verbs::ingest::IngestArgs;
use cairn_core::generated::verbs::summarize::{
    SummarizeArgs, SummarizeArgsCitations, SummarizeData,
};
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, invalid_args_response, new_operation_id};

const DEFAULT_SUMMARIZE_ISSUER: &str = "agt:cairn-cli:default:writer:v1";
const DEFAULT_TENANT: &str = "default";
const SUMMARY_SOURCE_ENTITY: &str = "ingest";
const SUMMARY_CHALLENGE_TTL_MS: i64 = 5 * 60 * 1_000;

#[derive(Clone)]
struct ReadAuthorization {
    operation_id: Ulid,
    scope: ScopeTuple,
    max_visibility: MemoryVisibility,
    issuer: Identity,
}

/// Run `cairn summarize`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let args = match summarize_args_from_matches(sub) {
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
                super::signed::aborted(ResponseVerb::Summarize, format!("runtime build: {e}"));
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

async fn run_async(args: SummarizeArgs, vault_root: PathBuf, config: CairnConfig) -> Response {
    let ctx = match super::signed::open_context(ResponseVerb::Summarize, &vault_root, config).await
    {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let operation_id = new_operation_id();
    let auth = match signed_read_authorization(&ctx, &args, operation_id).await {
        Ok(auth) => auth,
        Err(resp) => return resp,
    };

    let records = match load_source_records(&ctx.store, &args, &auth).await {
        Ok(records) => records,
        Err(resp) => return merge_policy_trace(read_policy_trace(&auth, &[]), resp),
    };
    let citations = !matches!(args.citations, Some(SummarizeArgsCitations::Off));
    let summary = cairn_core::verbs::summarize::render_summary(&records, citations);
    let mut data = SummarizeData {
        persisted_record_id: None,
        summary,
    };
    let mut policy_trace = read_policy_trace(&auth, &records);

    if args.persist == Some(true) {
        match persist_summary(&ctx, &args, &auth, &mut data).await {
            Ok(write_trace) => policy_trace.extend(write_trace),
            Err(resp) => return merge_policy_trace(policy_trace, resp),
        }
    }

    super::signed::committed(
        ResponseVerb::Summarize,
        auth.operation_id,
        ResponseData::Summarize(data),
        policy_trace,
    )
}

async fn load_source_records(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    args: &SummarizeArgs,
    auth: &ReadAuthorization,
) -> Result<Vec<MemoryRecord>, Response> {
    let record_ids = args
        .record_ids
        .iter()
        .map(|id| RecordId::parse(id.0.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    if record_ids.len() > 1000 {
        return Err(invalid_args_response(
            ResponseVerb::Summarize,
            "record_ids",
            "summarize accepts at most 1000 source records",
        ));
    }
    let requested: BTreeSet<String> = record_ids.iter().map(|id| id.as_str().to_owned()).collect();
    let limit = record_ids.len().max(1);
    let page = store
        .list(&ListArgs {
            record_ids,
            scope: Some(auth.scope.clone()),
            visibility_allowlist: visibility_allowlist(auth.max_visibility),
            limit,
            ..ListArgs::default()
        })
        .await
        .map_err(|e| super::signed::aborted(ResponseVerb::Summarize, format!("store list: {e}")))?;
    let mut records = page.records;
    records.retain(|record| record_visible_to_issuer(record, &auth.issuer));
    let returned: BTreeSet<String> = records
        .iter()
        .map(|record| record.id.as_str().to_owned())
        .collect();
    let missing = requested
        .difference(&returned)
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(invalid_args_response(
            ResponseVerb::Summarize,
            "record_ids",
            &format!(
                "records not found or not authorized in the signed read scope: {}",
                missing.join(",")
            ),
        ));
    }
    Ok(records)
}

fn record_visible_to_issuer(record: &MemoryRecord, issuer: &Identity) -> bool {
    match record.visibility {
        MemoryVisibility::Private => record_principal_matches_issuer(record, issuer),
        MemoryVisibility::Session => false,
        _ => true,
    }
}

fn record_principal_matches_issuer(record: &MemoryRecord, issuer: &Identity) -> bool {
    match issuer.kind() {
        cairn_core::domain::IdentityKind::Human => {
            record.scope.user.as_deref() == Some(issuer.as_str())
        }
        cairn_core::domain::IdentityKind::Agent => {
            record.scope.agent.as_deref() == Some(issuer.as_str())
        }
        cairn_core::domain::IdentityKind::Sensor => {
            record.provenance.source_sensor.as_str() == issuer.as_str()
        }
    }
}

async fn persist_summary(
    ctx: &super::signed::OpenedVerbContext,
    args: &SummarizeArgs,
    auth: &ReadAuthorization,
    data: &mut SummarizeData,
) -> Result<Vec<ResponsePolicyTrace>, Response> {
    let ingest_args = IngestArgs {
        body: Some(data.summary.clone()),
        dry_run: None,
        file: None,
        folder: None,
        frontmatter: Some(serde_json::json!({
            "summary_sources": args.record_ids.iter().map(|id| id.0.clone()).collect::<Vec<_>>()
        })),
        human_review: None,
        kind: args.kind.clone().unwrap_or_else(|| "reference".to_owned()),
        no_cache: None,
        no_diff: None,
        session_id: None,
        tags: Some(vec!["summary".to_owned()]),
        url: None,
    };
    let prepared =
        cairn_core::verbs::ingest::prepare_ingest_body(&ingest_args, auth.issuer.as_str());
    let (mut record, mut policy_trace) = match prepared {
        Ok(cairn_core::verbs::ingest::PreparedIngest::Proceed {
            record,
            policy_trace,
            ..
        }) => (record, policy_trace),
        Ok(cairn_core::verbs::ingest::PreparedIngest::Rejected { policy_trace, .. }) => {
            let mut resp = super::signed::rejected_from_domain(
                ResponseVerb::Summarize,
                cairn_core::domain::DomainError::Unauthorized {
                    message: "summary record rejected by filter".to_owned(),
                },
            );
            resp.policy_trace = policy_trace;
            return Err(resp);
        }
        Err(e) => {
            return Err(super::signed::rejected_from_domain(
                ResponseVerb::Summarize,
                e,
            ));
        }
    };

    bind_record_scope_to_context(&mut record, ctx);
    let consent_ref = record.provenance.consent_ref.clone();
    let persisted = Ulid(record.id.as_str().to_owned());
    let payload = canonical_bytes_signed_payload(&record)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    let admission = signed_summary_admission(
        ctx,
        args,
        &auth.issuer,
        auth.operation_id.clone(),
        &payload,
        &record,
    )
    .await?;
    let result = ctx
        .store
        .with_tx(move |tx| {
            tx.prepare_wal_with_replay(&admission).map_err(|e| {
                cairn_store_sqlite::StoreError::Invariant {
                    what: format!("wal admission: {e}"),
                }
            })?;
            tx.upsert(&record)?;
            tx.commit_prepared_wal(&admission)?;
            Ok::<_, cairn_store_sqlite::StoreError>(())
        })
        .await;
    if let Err(e) = result {
        let mut resp =
            super::signed::aborted(ResponseVerb::Summarize, format!("summary upsert: {e}"));
        resp.policy_trace = vec![ResponsePolicyTrace {
            detail: Some("summary_upsert_failed".to_owned()),
            gate: "write.wal".to_owned(),
            result: ResponsePolicyTraceResult::Error,
        }];
        return Err(resp);
    }
    data.persisted_record_id = Some(persisted);
    policy_trace.extend([
        ResponsePolicyTrace {
            detail: Some("signed_scope_verified".to_owned()),
            gate: "write.auth".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some("summary_upsert_committed".to_owned()),
            gate: "write.wal".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(consent_ref),
            gate: "write.consent".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
    ]);
    Ok(policy_trace)
}

fn merge_policy_trace(mut prefix: Vec<ResponsePolicyTrace>, mut resp: Response) -> Response {
    prefix.append(&mut resp.policy_trace);
    resp.policy_trace = prefix;
    resp
}

fn bind_record_scope_to_context(
    record: &mut cairn_core::domain::record::MemoryRecord,
    ctx: &super::signed::OpenedVerbContext,
) {
    record.scope.tenant = Some(DEFAULT_TENANT.to_owned());
    record.scope.workspace = Some(ctx.config.vault.name.clone());
    record.scope.entity = Some(SUMMARY_SOURCE_ENTITY.to_owned());
}

async fn signed_read_authorization(
    ctx: &super::signed::OpenedVerbContext,
    args: &SummarizeArgs,
    operation_id: Ulid,
) -> Result<ReadAuthorization, Response> {
    let issuer_wire =
        std::env::var("CAIRN_ISSUER").unwrap_or_else(|_| DEFAULT_SUMMARIZE_ISSUER.to_owned());
    let issuer = Identity::parse(issuer_wire)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    let active = ctx
        .identity
        .registry
        .get_identity(&issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Summarize, format!("identity lookup: {e}"))
        })?
        .ok_or_else(|| {
            super::signed::rejected_from_domain(
                ResponseVerb::Summarize,
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
            super::signed::aborted(ResponseVerb::Summarize, format!("issuer key load: {e}"))
        })?;
    let target_hash = summarize_args_hash(args)?;
    let mut intent = unsigned_summary_intent(
        &issuer,
        active.current_key_version.as_u32(),
        operation_id.clone(),
        ctx.config.vault.name.clone(),
        target_hash.clone(),
        None,
        Some(read_sequence()),
    );
    let intent_bytes = canonical_bytes_signed_intent(&intent)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    intent.signature = Ed25519Signature(format!(
        "ed25519:{}",
        hex_lower(&signing_key.sign(&intent_bytes).to_bytes())
    ));
    let request = super::signed::request(
        RequestVerb::Summarize,
        RequestArgs::Summarize(args.clone()),
        intent,
    );
    let verified = super::signed::verify_request(ctx, request).await?;
    if verified.as_inner().target_hash != target_hash {
        return Err(super::signed::rejected_from_domain(
            ResponseVerb::Summarize,
            cairn_core::domain::DomainError::Unauthorized {
                message: "summarize args hash mismatch".to_owned(),
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
        issuer,
    })
}

async fn signed_summary_admission(
    ctx: &super::signed::OpenedVerbContext,
    args: &SummarizeArgs,
    issuer: &Identity,
    operation_id: Ulid,
    payload: &[u8],
    record: &MemoryRecord,
) -> Result<SignedAdmission, Response> {
    let active = ctx
        .identity
        .registry
        .get_identity(issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Summarize, format!("identity lookup: {e}"))
        })?
        .ok_or_else(|| {
            super::signed::rejected_from_domain(
                ResponseVerb::Summarize,
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
            super::signed::aborted(ResponseVerb::Summarize, format!("issuer key load: {e}"))
        })?;
    let challenge = mint_summary_challenge(ctx, issuer).await?;
    let mut intent = unsigned_summary_intent(
        issuer,
        active.current_key_version.as_u32(),
        operation_id,
        ctx.config.vault.name.clone(),
        sha256_wire(payload),
        Some(challenge),
        None,
    );
    let intent_bytes = canonical_bytes_signed_intent(&intent)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    intent.signature = Ed25519Signature(format!(
        "ed25519:{}",
        hex_lower(&signing_key.sign(&intent_bytes).to_bytes())
    ));
    let request = super::signed::request(
        RequestVerb::Summarize,
        RequestArgs::Summarize(args.clone()),
        intent,
    );
    let verified = super::signed::verify_request(ctx, request).await?;
    record
        .validate_against_intent(&verified)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::Summarize, e))?;
    SignedAdmission::new(verified, WalActionKind::Upsert, None, payload).map_err(|e| {
        super::signed::aborted(ResponseVerb::Summarize, format!("signed admission: {e}"))
    })
}

async fn mint_summary_challenge(
    ctx: &super::signed::OpenedVerbContext,
    issuer: &Identity,
) -> Result<Nonce16Base64, Response> {
    let issuer = issuer.as_str().to_owned();
    let now_ms = unix_time_millis_i64();
    let minted = ctx
        .store
        .with_tx(move |tx| tx.mint_challenge(&issuer, now_ms, SUMMARY_CHALLENGE_TTL_MS))
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Summarize, format!("challenge mint: {e}"))
        })?;
    Ok(Nonce16Base64(minted.nonce_b64))
}

fn unsigned_summary_intent(
    issuer: &Identity,
    key_version: u32,
    operation_id: Ulid,
    workspace: String,
    target_hash: String,
    server_challenge: Option<Nonce16Base64>,
    sequence: Option<u64>,
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
            entity: SUMMARY_SOURCE_ENTITY.to_owned(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence,
        server_challenge,
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash,
    }
}

fn summarize_args_hash(args: &SummarizeArgs) -> Result<String, Response> {
    serde_json::to_vec(args)
        .map(|bytes| sha256_wire(&bytes))
        .map_err(|e| super::signed::aborted(ResponseVerb::Summarize, format!("args hash: {e}")))
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
        SignedIntentScopeTier::Private => MemoryVisibility::Private,
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

fn summarize_args_from_matches(sub: &ArgMatches) -> Result<SummarizeArgs, Response> {
    let record_ids = sub
        .get_many::<String>("record_ids")
        .into_iter()
        .flatten()
        .map(String::to_owned)
        .collect::<Vec<_>>();
    let mut value = serde_json::json!({ "record_ids": record_ids });
    set_optional(&mut value, "kind", sub.get_one::<String>("kind").cloned());
    set_optional(
        &mut value,
        "citations",
        sub.get_one::<String>("citations").cloned(),
    );
    if sub.get_flag("persist") {
        set_optional(&mut value, "persist", Some(true));
    }
    serde_json::from_value(value)
        .map_err(|e| invalid_args_response(ResponseVerb::Summarize, "args", &e.to_string()))
}

fn set_optional<T: serde::Serialize>(value: &mut serde_json::Value, key: &str, item: Option<T>) {
    if let Some(item) = item
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            key.to_owned(),
            serde_json::to_value(item)
                .expect("invariant: summarize CLI args are JSON-serializable"),
        );
    }
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
        (ResponseStatus::Committed, Some(ResponseData::Summarize(data))) => {
            println!("{}", data.summary);
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
                .unwrap_or("summarize failed");
            human_error("summarize", code, message, &resp.operation_id);
        }
    }
}

fn sha256_wire(payload: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest as _;
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

fn unix_time_millis_i64() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
