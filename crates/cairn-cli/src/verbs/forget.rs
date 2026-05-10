//! `cairn forget` handler.
//!
//! # Trust boundary (spec §3.5)
//!
//! `forget` is an issuer-dependent verb: it produces a signed tombstone record
//! once the store is wired.  The guard call below enforces the
//! `VaultDegraded → EX_TEMPFAIL=75` contract even in the stub path.
//!
//! # Dispatch state
//!
//! `forget --record <ulid>` is wired end-to-end through
//! [`cairn_store_sqlite::SqliteMemoryStore::forget_record`] (issue #58 — the
//! engine work landed first; CLI dispatch followed). The actor is taken from
//! `CAIRN_ISSUER` (or [`DEFAULT_FORGET_ISSUER`] when unset) and resolved
//! against the vault's identity registry before any WAL state is written;
//! unregistered issuers are rejected with `Unauthorized` and exit
//! `EX_NOPERM=77`. The local CLI path bypasses signed-intent verification —
//! the operator has direct filesystem access to the vault, so a
//! `SignedIntent` admission would not strengthen the trust boundary; #9 will
//! revisit for cross-host invocations.
//!
//! `forget --session` and `forget --scope` still return `CapabilityUnavailable`
//! until the v0.2 / v0.3 runtime lands.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::identity::{keys::IdentityRevision, provision::ProvisionInput};
use cairn_core::domain::{DomainError, Identity, TargetId};
use cairn_core::generated::envelope::{Response, ResponseData, ResponseVerb};
use cairn_core::generated::verbs::forget::ForgetData;
use clap::ArgMatches;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};
use crate::llm::EX_NOPERM;

use super::envelope::{
    EX_UNAVAILABLE, capability_unavailable_response, emit_json, human_error, invalid_args_response,
    new_operation_id, not_found_response,
};
use super::status;

/// Default issuer used when `CAIRN_ISSUER` is unset. The local CLI path
/// authors the consent journal entry with this principal — the operator has
/// direct filesystem access to the vault, so the value records intent rather
/// than enforcing it.
const DEFAULT_FORGET_ISSUER: &str = "hmn:cairn-cli:v1";

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
        // distinction collapses to "produce a placeholder plan."
        return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
    }

    // §3.5 trust-boundary guard: refuse if the vault is degraded.
    // In this P0 path the report is built against a clean default — full
    // reconciliation would require opening the identity store first.
    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn forget: VaultDegraded — {e}");
        return ExitCode::from(75); // EX_TEMPFAIL
    }

    // --session / --scope still fail-closed: capability is not advertised.
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

    // --record path: real dispatch.
    if let Some(exit) = require_bound_vault(json, vault_root.as_path()) {
        return exit;
    }

    let raw_id = if let Some(s) = sub.get_one::<String>("record_id") {
        s.clone()
    } else {
        // clap's ArgGroup(required=true) should prevent this; treat as
        // a clap-style usage failure if it ever fires.
        let reason = "exactly one of --record / --session / --scope is required";
        let resp = invalid_args_response(ResponseVerb::Forget, "record_id", reason);
        if json {
            emit_json(&resp);
        } else {
            human_error("forget", "InvalidArgs", reason, &resp.operation_id);
        }
        return ExitCode::from(64);
    };

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
                human_error("forget", "Internal", &format!("{e}"), &resp.operation_id);
            }
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(run_async_record(vault_root, config, json, raw_id))
}

#[tracing::instrument(
    skip(vault_root, config, raw_id),
    fields(verb = "forget", target_id = %raw_id),
)]
async fn run_async_record(
    vault_root: PathBuf,
    config: CairnConfig,
    json: bool,
    raw_id: String,
) -> ExitCode {
    let target = match TargetId::parse(raw_id.clone()) {
        Ok(t) => t,
        Err(e) => {
            let reason = e.to_string();
            let resp = invalid_args_response(ResponseVerb::Forget, "record_id", &reason);
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "InvalidArgs", &reason, &resp.operation_id);
            }
            return ExitCode::from(64);
        }
    };

    let ctx = match super::signed::open_context(ResponseVerb::Forget, &vault_root, config).await {
        Ok(ctx) => ctx,
        Err(resp) => {
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "forget",
                    "Internal",
                    "vault open failed",
                    &resp.operation_id,
                );
            }
            return ExitCode::from(78);
        }
    };

    let issuer_wire =
        std::env::var("CAIRN_ISSUER").unwrap_or_else(|_| DEFAULT_FORGET_ISSUER.to_owned());
    let actor = match Identity::parse(issuer_wire) {
        Ok(id) => id,
        Err(e) => {
            let resp = invalid_args_response(ResponseVerb::Forget, "CAIRN_ISSUER", &e.to_string());
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "InvalidArgs", &e.to_string(), &resp.operation_id);
            }
            return ExitCode::from(64);
        }
    };

    // Resolve the actor against the durable identity registry BEFORE
    // writing the destructive WAL op. Without this gate any string a
    // shell happens to set in `CAIRN_ISSUER` would land verbatim as
    // `consent_journal.actor` for an unrecoverable delete. The local
    // CLI path still bypasses signed-intent verification (operator has
    // filesystem access to the vault), but a registered issuer is a
    // strictly stronger gate than "any env-var string."
    if let Err(resp) = ensure_forget_issuer(&ctx, &actor).await {
        if json {
            emit_json(&resp);
        } else {
            let code = super::signed::response_error_code(&resp).unwrap_or("Unauthorized");
            let message = resp
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("issuer not registered");
            human_error("forget", code, message, &resp.operation_id);
        }
        // EX_NOPERM=77 for Rejected/Unauthorized; EX_CONFIG=78 for
        // backend-side aborts (registry lookup error / provision error).
        return match resp.status {
            cairn_core::generated::envelope::ResponseStatus::Aborted => ExitCode::from(78),
            _ => ExitCode::from(EX_NOPERM),
        };
    }

    let receipt = match ctx.store.forget_record(&target, &actor).await {
        Ok(r) => r,
        Err(e) => {
            let resp =
                super::signed::aborted(ResponseVerb::Forget, format!("store forget_record: {e}"));
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "forget",
                    "Internal",
                    &format!("store forget_record: {e}"),
                    &resp.operation_id,
                );
            }
            return ExitCode::FAILURE;
        }
    };

    // ForgetData.tombstones carries the deleted record ULID list. The
    // ForgetReceipt only retains the salted target_id hash (brief §14
    // forget-receipt allowlist), so we cannot leak the original record
    // ULIDs here without changing the contract surface. Set tombstones
    // to None — the deleted_count and committed status already prove
    // the operation took effect.
    //
    // `deleted_count` is the live-version count captured engine-side
    // before locks were taken. Idempotent re-forgets and forgets
    // against never-existing target ids report `0` here (status still
    // `committed` — the WAL op did execute, idempotently, per
    // brief §5.6). Operators reading `deleted_count: 0` get the signal
    // they need to debug stale IDs / typos / repeat forgets without
    // us promoting the no-op into a hard failure.
    let data = ForgetData {
        deleted_count: receipt.deleted_count,
        plan_ref: None,
        tombstones: None,
    };
    let resp = super::signed::committed(
        ResponseVerb::Forget,
        new_operation_id(),
        ResponseData::Forget(data),
        Vec::new(),
    );
    if json {
        emit_json(&resp);
    } else {
        println!("cairn forget: committed deletion of target {raw_id}");
    }
    ExitCode::SUCCESS
}

/// Reject the forget call when the actor is not registered in the
/// vault's identity registry.
///
/// Mirrors `ingest::ensure_ingest_issuer`: when the operator's
/// `CAIRN_ISSUER` is the default ([`DEFAULT_FORGET_ISSUER`]), the helper
/// auto-provisions it on first use — operators with vault filesystem
/// access can already write any record they like, so requiring an
/// explicit handshake step before the very first `forget` would be
/// CLI-unfriendly noise. Custom `CAIRN_ISSUER` values that are not in
/// the registry are rejected with `Unauthorized` and the CLI exits
/// `EX_NOPERM=77`. Brief §14: the registered issuer is what lands in
/// `consent_journal.actor` for the destructive op.
async fn ensure_forget_issuer(
    ctx: &super::signed::OpenedVerbContext,
    issuer: &Identity,
) -> Result<(), Response> {
    let existing = ctx
        .identity
        .registry
        .get_identity(issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::Forget, format!("identity lookup: {e}"))
        })?;
    if existing.is_some() {
        return Ok(());
    }
    if issuer.as_str() != DEFAULT_FORGET_ISSUER {
        return Err(super::signed::rejected_from_domain(
            ResponseVerb::Forget,
            DomainError::Unauthorized {
                message: format!(
                    "issuer {issuer} is not active in this vault — \
                     run `cairn handshake --issuer {issuer}` to register it before forgetting"
                ),
            },
        ));
    }

    // Auto-provision the default forget issuer on first use. Mirrors
    // `ingest::ensure_ingest_issuer`'s default-only auto-provision.
    let mut rng = rand_core::OsRng;
    let input = ProvisionInput {
        vault_id: ctx.identity.vault_id.clone(),
        id: issuer.clone(),
        kind: issuer.kind(),
        revision: IdentityRevision::FIRST,
    };
    ctx.identity
        .provision(issuer.kind(), input, &mut rng)
        .await
        .map_err(|e| {
            super::signed::aborted(
                ResponseVerb::Forget,
                format!("default issuer provision: {e}"),
            )
        })?;
    Ok(())
}

fn require_bound_vault(json: bool, vault_root: &std::path::Path) -> Option<ExitCode> {
    match status::probe_vault_binding(vault_root) {
        status::VaultBinding::Bound => None,
        status::VaultBinding::Unbound => {
            let msg = format!(
                "no Cairn vault at {} — run `cairn bootstrap` first",
                vault_root.display()
            );
            let resp = not_found_response(ResponseVerb::Forget, "vault", &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "NotFound", &msg, &resp.operation_id);
            }
            Some(ExitCode::from(78))
        }
        status::VaultBinding::Invalid(reason) => {
            let msg = format!("vault binding error — {reason}");
            let resp = super::envelope::internal_error_response(ResponseVerb::Forget, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("forget", "Internal", &msg, &resp.operation_id);
            }
            Some(ExitCode::from(78))
        }
    }
}
