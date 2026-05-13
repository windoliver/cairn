//! `cairn forget` handler.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryRecord, Rfc3339Timestamp, SourceId,
    TargetId,
};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::forget::ForgetData;
use clap::ArgMatches;
use sha2::{Digest, Sha256};

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{
    EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error, invalid_args_response,
    new_operation_id, not_found_response,
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

#[derive(Debug)]
struct ForgetReceipt {
    deleted_count: u64,
    tombstones: Vec<Ulid>,
}

#[derive(Debug)]
struct SourceGroup {
    source_hash: String,
    source_ids: BTreeSet<SourceId>,
}

#[derive(Debug, thiserror::Error)]
enum ForgetRunError {
    #[error("target `{0}` not found")]
    NotFound(String),
    #[error("source artifact write failed at `{path}`: {message}")]
    SourceRewrite { path: String, message: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Run `cairn forget`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
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
        return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
    }

    if sub.get_one::<String>("session_id").is_some() || sub.get_one::<String>("scope").is_some() {
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
        return ExitCode::from(EX_UNAVAILABLE);
    }

    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn forget: VaultDegraded — {e}");
        return ExitCode::from(75);
    }

    let Some(raw_target) = sub.get_one::<String>("record_id").cloned() else {
        let resp = invalid_args_response(ResponseVerb::Forget, "record_id", "must be provided");
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "forget",
                "InvalidArgs",
                "record_id must be provided",
                &resp.operation_id,
            );
        }
        return ExitCode::from(64);
    };
    let target = match TargetId::parse(raw_target.clone()) {
        Ok(target) => target,
        Err(e) => {
            let resp = invalid_args_response(ResponseVerb::Forget, "record_id", &e.to_string());
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "InvalidArgs", &e.to_string(), &resp.operation_id);
            }
            return ExitCode::from(64);
        }
    };

    let config = match crate::config::load(vault_root, &crate::config::CliOverrides::default()) {
        Ok(config) => config,
        Err(e) => {
            let resp = super::envelope::internal_error_response(
                ResponseVerb::Forget,
                &format!("config load failed: {e:#}"),
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "forget",
                    "Internal",
                    &format!("config load failed: {e:#}"),
                    &resp.operation_id,
                );
            }
            return ExitCode::from(78);
        }
    };

    let operation_id = new_operation_id();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp = super::envelope::internal_error_response(
                ResponseVerb::Forget,
                &format!("runtime build: {e}"),
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "forget",
                    "Internal",
                    &format!("runtime build: {e}"),
                    &resp.operation_id,
                );
            }
            return ExitCode::from(1);
        }
    };

    match rt.block_on(forget_record(
        vault_root.to_path_buf(),
        target,
        &operation_id.0,
        config.vault.source.redact_on_forget,
    )) {
        Ok(receipt) => {
            let resp = Response {
                contract: "cairn.mcp.v1".to_owned(),
                data: Some(ResponseData::Forget(ForgetData {
                    deleted_count: receipt.deleted_count,
                    plan_ref: None,
                    tombstones: Some(receipt.tombstones),
                })),
                error: None,
                operation_id,
                policy_trace: Vec::<ResponsePolicyTrace>::new(),
                status: ResponseStatus::Committed,
                target: None,
                verb: ResponseVerb::Forget,
            };
            if json {
                emit_json(&resp);
            } else {
                println!(
                    "cairn forget: deleted {} record versions",
                    receipt.deleted_count
                );
            }
            ExitCode::SUCCESS
        }
        Err(ForgetRunError::NotFound(target)) => {
            let resp = not_found_response(
                ResponseVerb::Forget,
                &target,
                &format!("target `{target}` was not found"),
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "forget",
                    "NotFound",
                    &format!("target `{target}` was not found"),
                    &resp.operation_id,
                );
            }
            ExitCode::from(1)
        }
        Err(err) => {
            let resp =
                super::envelope::internal_error_response(ResponseVerb::Forget, &err.to_string());
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "Internal", &err.to_string(), &resp.operation_id);
            }
            ExitCode::from(1)
        }
    }
}

async fn forget_record(
    vault_root: PathBuf,
    target: TargetId,
    operation_id: &str,
    redact_on_forget: bool,
) -> Result<ForgetReceipt, ForgetRunError> {
    let db_path = vault_root.join(".cairn/cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .map_err(|e| ForgetRunError::Other(anyhow::anyhow!("open store: {e}")))?;

    let versions = store
        .versions(&target)
        .await
        .map_err(|e| ForgetRunError::Other(anyhow::anyhow!("load versions: {e}")))?;
    if versions.is_empty() {
        return Err(ForgetRunError::NotFound(target.as_str().to_owned()));
    }

    let mut records = Vec::with_capacity(versions.len());
    for version in &versions {
        let record = store
            .get(&version.record_id)
            .await
            .map_err(|e| ForgetRunError::Other(anyhow::anyhow!("load record: {e}")))?;
        let Some(record) = record else {
            return Err(ForgetRunError::Other(anyhow::anyhow!(
                "version `{}` for target `{}` disappeared during forget",
                version.record_id.as_str(),
                target.as_str()
            )));
        };
        records.push(record);
    }

    let actor = records
        .first()
        .and_then(signing_author)
        .unwrap_or_else(|| Identity::parse("agt:cairn-cli:forget:v0").expect("static identity"));
    let scope = records
        .first()
        .map(|record| record.scope.canonical_wire().to_ascii_lowercase())
        .unwrap_or_default();
    let decided_at = Rfc3339Timestamp::parse(cairn_core::time::now_rfc3339_seconds())
        .map_err(|e| ForgetRunError::Other(anyhow::anyhow!("clock: {e}")))?;
    let target_hash = target_id_hash(target.as_str());
    let source_groups = group_sources(&records);

    let record_event = ConsentEvent {
        consent_id: new_operation_id().0,
        kind: ConsentKind::ForgetIntent,
        actor: actor.clone(),
        subject: target_hash.clone(),
        scope: scope.clone(),
        op_id: Some(operation_id.to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: target_hash.clone(),
            scope_tier: records[0].visibility,
            reason_code: "record_forget".to_owned(),
        },
        decided_at: decided_at.clone(),
        expires_at: None,
    };
    let source_events: Vec<ConsentEvent> = source_groups
        .values()
        .map(|group| ConsentEvent {
            consent_id: new_operation_id().0,
            kind: ConsentKind::ForgetIntent,
            actor: actor.clone(),
            subject: group.source_hash.clone(),
            scope: scope.clone(),
            op_id: Some(operation_id.to_owned()),
            sensor_id: None,
            payload: ConsentPayload::IntentReceipt {
                target_id_hash: target_hash.clone(),
                scope_tier: records[0].visibility,
                reason_code: if redact_on_forget {
                    "source_forget_redacted".to_owned()
                } else {
                    "source_forget".to_owned()
                },
            },
            decided_at: decided_at.clone(),
            expires_at: None,
        })
        .collect();

    let target_for_tx = target.clone();
    let tombstones: Vec<Ulid> = versions
        .iter()
        .map(|version| Ulid(version.record_id.as_str().to_owned()))
        .collect();
    let deleted_count = store
        .with_tx(move |tx| {
            tx.append_consent_event(&record_event)?;
            for event in &source_events {
                tx.append_consent_event(event)?;
            }
            tx.purge_target(&target_for_tx)
        })
        .await
        .map_err(|e| ForgetRunError::Other(anyhow::anyhow!("forget transaction: {e:?}")))?;

    if redact_on_forget {
        for group in source_groups.values() {
            for source_id in &group.source_ids {
                rewrite_source_redaction_marker(
                    &vault_root,
                    source_id,
                    &group.source_hash,
                    operation_id,
                )?;
            }
        }
    }

    Ok(ForgetReceipt {
        deleted_count,
        tombstones,
    })
}

fn group_sources(records: &[MemoryRecord]) -> HashMap<String, SourceGroup> {
    let mut groups = HashMap::new();
    for record in records {
        let entry = groups
            .entry(record.provenance.source_hash.clone())
            .or_insert_with(|| SourceGroup {
                source_hash: record.provenance.source_hash.clone(),
                source_ids: BTreeSet::new(),
            });
        entry
            .source_ids
            .extend(record.provenance.source_ids.iter().cloned());
    }
    groups
}

fn signing_author(record: &MemoryRecord) -> Option<Identity> {
    record
        .actor_chain
        .iter()
        .find(|entry| matches!(entry.role, cairn_core::domain::ChainRole::Author))
        .map(|entry| entry.identity.clone())
}

fn target_id_hash(target_id: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

fn rewrite_source_redaction_marker(
    vault_root: &Path,
    source_id: &SourceId,
    source_hash: &str,
    operation_id: &str,
) -> Result<(), ForgetRunError> {
    let path = vault_root.join(source_id.as_str());
    let parent = path.parent().ok_or_else(|| ForgetRunError::SourceRewrite {
        path: source_id.as_str().to_owned(),
        message: "missing parent directory".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|error| ForgetRunError::SourceRewrite {
        path: source_id.as_str().to_owned(),
        message: error.to_string(),
    })?;
    let marker = format!(
        "cairn:redacted-source:v1\nsource_hash={source_hash}\noperation_id={operation_id}\n"
    );
    fs::write(&path, marker).map_err(|error| ForgetRunError::SourceRewrite {
        path: source_id.as_str().to_owned(),
        message: error.to_string(),
    })
}
