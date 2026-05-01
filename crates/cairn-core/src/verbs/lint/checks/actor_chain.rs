//! §6.2 — `broken_actor_chain` check (issue #256).
//!
//! Wraps the pure `pipeline::lint::author_lifecycle::check_author_lifecycle`
//! function and maps its output into the lint verb's `Finding` shape.
//!
//! Two modes:
//!
//! 1. **Author states plumbed (`LintInputs.author_states.is_some()`):**
//!    run the real per-record check. Chain-shape violations (no
//!    `Author`, duplicate `Author`, role-order violation) and
//!    missing-from-registry authors surface as `Kind::BrokenActorChain`
//!    at `Severity::Error` (real corruption / unknown issuer).
//!    Currently-revoked authors surface at `Severity::Warning` —
//!    historical signatures may still be valid until P1 ships real
//!    Ed25519 + key-version persistence, so blocking on routine
//!    revocation would mis-direct operators toward destructive
//!    remediation.
//! 2. **Author states absent (`None`):** the dispatch layer did not
//!    wire the `IdentityRegistry`. Chain-shape is still cheap — run
//!    `validate_chain` per record and emit `BrokenActorChain` for any
//!    failure. Lifecycle coverage stays deferred and is reported as a
//!    single `DeferredCheck` info finding so the gap stays visible.
//!
//! Real Ed25519 verification of `record.signature` and body-integrity
//! (`target_hash` recompute) remain follow-ups; this leaf is the
//! cheapest defensive slice that ships without schema changes — see
//! `docs/design/2026-04-30-issue-256-signature-lint-design.md`.

use crate::domain::ChainRole;
use crate::domain::actor_chain::validate_chain;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::pipeline::lint::author_lifecycle::{
    AuthorLifecycleFinding, AuthorState, ChainStatus, check_author_lifecycle,
};
use crate::verbs::lint::{LintInputs, finding, target_record};

const TRACKING_ISSUE: i64 = 256;

/// Run the §6.2 broken-actor-chain check.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(states) = inputs.author_states else {
        return deferred_with_chain_shape(inputs);
    };
    inputs
        .records
        .iter()
        .filter_map(|r| {
            let author = r
                .stored
                .record
                .actor_chain
                .iter()
                .find(|e| e.role == ChainRole::Author)
                .map(|e| e.identity.clone());
            let state = match author.as_ref() {
                Some(id) => match states.get(id) {
                    Some(p) => AuthorState::Resolved(*p),
                    None => AuthorState::MissingFromRegistry,
                },
                // No author entry → check_author_lifecycle reaches
                // the chain-shape branch first; author_state is
                // ignored. Pass any value.
                None => AuthorState::MissingFromRegistry,
            };
            check_author_lifecycle(&r.stored.record, state).map(into_finding)
        })
        .collect()
}

/// Fallback when the dispatch layer did not plumb an `IdentityRegistry`.
/// Chain-shape is still pure → run it. Lifecycle stays deferred and
/// surfaces as a single `DeferredCheck` info finding.
fn deferred_with_chain_shape(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    for r in inputs.records {
        if let Err(e) = validate_chain(&r.stored.record.actor_chain) {
            let mut f = finding(
                Kind::BrokenActorChain,
                Severity::Error,
                format!(
                    "actor_chain failed shape validation: {e} — at-rest signature cannot be attributed to a single issuer"
                ),
            );
            f.target = Some(target_record(&r.stored.record.id));
            f.tracking_issue = Some(TRACKING_ISSUE);
            out.push(f);
        }
    }
    let mut deferred = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "author-lifecycle slice of #{TRACKING_ISSUE} skipped — \
             dispatch did not plumb IdentityRegistry into LintInputs"
        ),
    );
    deferred.tracking_issue = Some(TRACKING_ISSUE);
    deferred.suggested_fix = Some(
        "thread an IdentityRegistry into the lint dispatch layer and \
         populate LintInputs.author_states"
            .to_owned(),
    );
    out.push(deferred);
    out
}

fn into_finding(lf: AuthorLifecycleFinding) -> Finding {
    let mut f = finding(Kind::BrokenActorChain, severity_for(lf.status), lf.message);
    f.target = Some(target_record(&lf.record_id));
    f.tracking_issue = Some(TRACKING_ISSUE);
    f.suggested_fix = Some(suggested_fix_for(lf.status).to_owned());
    f
}

fn severity_for(status: ChainStatus) -> Severity {
    match status {
        // Revoked authors don't *prove* a bad record — historical
        // signatures still verify per ProvisioningState::is_operational.
        // Without persisted key_version or real Ed25519 re-verify (P1),
        // this leaf can't tell whether a record was signed pre- or
        // post-revocation, so it must not block on routine revocation.
        // Surface as Warning so operators see the audit trail but don't
        // get destructive remediation advice for valid history.
        ChainStatus::Revoked => Severity::Warning,
        // Chain-shape and unknown-issuer cases are real corruption /
        // missing-truth-source — block.
        ChainStatus::Malformed => Severity::Error,
    }
}

fn suggested_fix_for(status: ChainStatus) -> &'static str {
    match status {
        ChainStatus::Revoked => {
            "audit the affected records — author identity is currently revoked, but historical \
             signatures may still be valid; do NOT auto-tombstone until P1 ships real signature \
             verification + key_version persistence (records signed pre-revocation remain valid)"
        }
        ChainStatus::Malformed => {
            "investigate at-rest tampering, partial migration, or a bypassed write path; \
             re-ingest the record under an Active issuer or tombstone it"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::identity::ProvisioningState;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::{ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp};
    use crate::verbs::lint::{ConsentModel, LintRecord, SchemaVersion};
    use std::collections::HashMap;

    fn lint_record(record: crate::domain::MemoryRecord) -> LintRecord {
        LintRecord {
            stored: StoredRecord { record, version: 1 },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn inputs<'a>(
        records: &'a [LintRecord],
        author_states: Option<&'a HashMap<Identity, ProvisioningState>>,
        cfg: &'a CairnConfig,
    ) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            author_states,
        }
    }

    #[test]
    fn deferred_when_author_states_absent_and_chains_clean() {
        let cfg = CairnConfig::default();
        let r = lint_record(sample_record());
        let recs = [r];
        let inp = inputs(&recs, None, &cfg);
        let f = run(&inp);
        assert_eq!(
            f.len(),
            1,
            "deferred-info finding only when no states + clean chains"
        );
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Info);
        assert_eq!(f[0].tracking_issue, Some(256));
    }

    #[test]
    fn shape_violations_surface_even_without_author_states() {
        // Chain-shape is pure — running it without the registry still
        // catches direct DB tamper.
        let mut record = sample_record();
        record.actor_chain.push(ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("hmn:other").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let cfg = CairnConfig::default();
        let recs = [lint_record(record)];
        let inp = inputs(&recs, None, &cfg);
        let findings = run(&inp);
        assert_eq!(
            findings.len(),
            2,
            "expect one BrokenActorChain (duplicate Author) + one DeferredCheck info"
        );
        let chain_finding = findings
            .iter()
            .find(|f| matches!(f.kind, Kind::BrokenActorChain))
            .expect("BrokenActorChain finding");
        assert_eq!(chain_finding.severity, Severity::Error);
        assert!(chain_finding.target.is_some());
    }

    #[test]
    fn active_author_with_states_yields_no_finding() {
        let cfg = CairnConfig::default();
        let r = sample_record();
        let author_id = r
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone())
            .expect("author");
        let mut states = HashMap::new();
        states.insert(author_id, ProvisioningState::Active);
        let recs = [lint_record(r)];
        let inp = inputs(&recs, Some(&states), &cfg);
        assert!(
            run(&inp).is_empty(),
            "active author + clean chain → no findings"
        );
    }

    #[test]
    fn revoked_author_with_states_emits_warning_not_error() {
        // Routine revocation must not poison historical records with
        // blocking errors — without persisted key_version / real
        // Ed25519 re-verify, this leaf cannot prove the record is
        // post-revocation. Warning surfaces the audit trail without
        // demanding destructive remediation.
        let cfg = CairnConfig::default();
        let r = sample_record();
        let author_id = r
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone())
            .expect("author");
        let mut states = HashMap::new();
        states.insert(author_id, ProvisioningState::Revoked);
        let recs = [lint_record(r)];
        let inp = inputs(&recs, Some(&states), &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenActorChain);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].target.is_some());
        assert!(
            findings[0]
                .suggested_fix
                .as_deref()
                .unwrap_or("")
                .contains("do NOT auto-tombstone"),
            "remediation must not advise destructive cleanup"
        );
    }

    #[test]
    fn missing_from_registry_with_states_emits_error() {
        // Author identity not in map → AuthorState::MissingFromRegistry
        // → ChainStatus::Malformed → Severity::Error.
        let cfg = CairnConfig::default();
        let states: HashMap<Identity, ProvisioningState> = HashMap::new();
        let recs = [lint_record(sample_record())];
        let inp = inputs(&recs, Some(&states), &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenActorChain);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no row in IdentityRegistry"));
    }
}
