//! `cairn forget` handler.
//!
//! # Trust boundary (spec §3.5)
//!
//! `forget` is an issuer-dependent verb: it produces a signed tombstone record
//! once the store is wired (#9).  The guard call below enforces the
//! `VaultDegraded → EX_TEMPFAIL=75` contract even in the stub path.
//!
//! **Deferred**: full async wiring (calling [`open_for_signed_verb`] against the
//! resolved vault path) is deferred to issue #9 when this verb becomes async.
//!
//! [`open_for_signed_verb`]: crate::identity::guard::open_for_signed_verb

use std::process::ExitCode;

use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Run `cairn forget`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
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

    // §3.5 trust-boundary guard: refuse if the vault is degraded.
    // In this P0 stub the report is always clean (no store is open); full async
    // wiring against the resolved vault path is deferred to issue #9.
    if let Err(e) = refuse_if_degraded(&ReconciliationReport::default(), vec![]) {
        eprintln!("cairn forget: VaultDegraded — {e}");
        return ExitCode::from(75); // EX_TEMPFAIL
    }

    let resp = unimplemented_response(ResponseVerb::Forget);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "forget",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
