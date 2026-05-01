//! §6.2 — `broken_actor_chain` check (issue #256).
//!
//! Wraps the pure `pipeline::lint::author_lifecycle::check_author_lifecycle`
//! function and maps its output into the lint verb's `Finding` shape.
//!
//! Per-record check: chain-shape violations (no `Author`, duplicate
//! `Author`, role-order violation) and unknown-issuer cases surface as
//! `Kind::BrokenActorChain` at `Severity::Error`; currently-revoked
//! issuers surface at `Severity::Error` (in-flight transitions) or
//! `Severity::Warning` (terminal) — see `severity_for` for the
//! rationale and the persistent-issue note.
//!
//! `LintInputs.author_states` is required (not `Option`) so a caller
//! cannot silently degrade lint into a no-op against revoked / unknown
//! / pending issuers. Empty map = "no chain authors / no resolvable
//! identities," not "skip the check."
//!
//! Real Ed25519 verification of `record.signature` and body-integrity
//! (`target_hash` recompute) remain follow-ups; this leaf is the
//! cheapest defensive slice that ships without schema changes — see
//! `docs/design/2026-04-30-issue-256-signature-lint-design.md`.

use crate::domain::ChainRole;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::pipeline::lint::author_lifecycle::{
    AuthorLifecycleFinding, AuthorState, ChainStatus, check_author_lifecycle,
};
use crate::verbs::lint::{LintInputs, finding, target_record};

const TRACKING_ISSUE: i64 = 256;

/// Run the §6.2 broken-actor-chain check.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
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
                Some(id) => match inputs.author_states.get(id) {
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

fn into_finding(lf: AuthorLifecycleFinding) -> Finding {
    let mut f = finding(Kind::BrokenActorChain, severity_for(lf.status), lf.message);
    f.target = Some(target_record(&lf.record_id));
    f.tracking_issue = Some(TRACKING_ISSUE);
    f.suggested_fix = Some(suggested_fix_for(lf.status).to_owned());
    f
}

fn severity_for(status: ChainStatus) -> Severity {
    // Severity policy — final position after three rounds of review
    // oscillation (rounds 5/7/8) on terminal `Revoked`/`Purged`:
    //
    // - Terminal revocation → Warning. The integration test
    //   `revocation_after_write_now_flags_record` concretely
    //   demonstrates the legitimate case: a record written while the
    //   author was Active, then later revoked. Without persisted
    //   `key_version` or real Ed25519 re-verify (P1), this leaf
    //   cannot tell pre- from post-revocation, and treating that
    //   ambiguity as `Error` poisons every historical record by the
    //   same author with a blocking verdict. Operators see the audit
    //   trail without destructive remediation pressure.
    // - In-flight revocation/purge → Error. A record landing while a
    //   withdrawal is already in motion is the suspicious-write case
    //   (bypassed gate, race, tamper) — not "old history under a
    //   now-revoked key." The pre/post-revocation ambiguity that
    //   protects terminal Revoked does NOT apply.
    // - Malformed (chain shape, unknown issuer, Pending issuer) →
    //   Error. Real corruption / missing truth source.
    //
    // The post-revocation-write fail-open risk that argued for
    // upgrading terminal Revoked to Error is real but unrecoverable
    // without P1 evidence (key_version + Ed25519). Documented as a
    // P1-resolvable gap rather than masked behind a noisy Warning.
    match status {
        ChainStatus::Revoked => Severity::Warning,
        ChainStatus::RevocationInFlight | ChainStatus::Malformed => Severity::Error,
    }
}

fn suggested_fix_for(status: ChainStatus) -> &'static str {
    match status {
        ChainStatus::Revoked => {
            "audit the affected records — author identity is terminally revoked, but historical \
             signatures may still be valid; do NOT auto-tombstone until P1 ships real signature \
             verification + key_version persistence (records signed pre-revocation remain valid)"
        }
        ChainStatus::RevocationInFlight => {
            "investigate the affected records — author identity was undergoing revocation/purge \
             when this record was written; treat as a candidate bypassed-gate or race-against-\
             withdrawal incident and quarantine before re-ingest"
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
        author_states: &'a HashMap<Identity, ProvisioningState>,
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
    fn shape_violations_surface_with_empty_states_map() {
        // Chain-shape is pure — even an empty author_states map still
        // catches direct DB tamper of the chain.
        let mut record = sample_record();
        record.actor_chain.push(ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("hmn:other").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let cfg = CairnConfig::default();
        let recs = [lint_record(record)];
        let states: HashMap<Identity, ProvisioningState> = HashMap::new();
        let inp = inputs(&recs, &states, &cfg);
        let findings = run(&inp);
        assert_eq!(
            findings.len(),
            1,
            "duplicate Author surfaces as a single BrokenActorChain"
        );
        assert!(matches!(findings[0].kind, Kind::BrokenActorChain));
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].target.is_some());
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
        let inp = inputs(&recs, &states, &cfg);
        assert!(
            run(&inp).is_empty(),
            "active author + clean chain → no findings"
        );
    }

    #[test]
    fn revoked_author_with_states_emits_warning_not_error() {
        // Persistent-issue resolution (rounds 5/7/8): terminal Revoked
        // is non-blocking. The integration test
        // `revocation_after_write_now_flags_record` concretely
        // demonstrates the legitimate case (record written under
        // Active, author revoked later); without P1 evidence we cannot
        // distinguish that benign history from a post-revocation
        // write. Warning surfaces the audit trail without poisoning
        // legitimate history with a blocking verdict.
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
        let inp = inputs(&recs, &states, &cfg);
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
            "remediation must steer operators away from destructive cleanup"
        );
    }

    #[test]
    fn revoke_pending_author_emits_error() {
        // In-flight revocation: a write that landed during a
        // withdrawal-in-motion is the suspicious-write case (bypassed
        // gate, race, tamper); pre/post-revocation ambiguity that
        // protects terminal Revoked does not apply.
        let cfg = CairnConfig::default();
        let r = sample_record();
        let author_id = r
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone())
            .expect("author");
        let mut states = HashMap::new();
        states.insert(author_id, ProvisioningState::RevokePending);
        let recs = [lint_record(r)];
        let inp = inputs(&recs, &states, &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn missing_from_registry_with_states_emits_error() {
        // Author identity not in map → AuthorState::MissingFromRegistry
        // → ChainStatus::Malformed → Severity::Error.
        let cfg = CairnConfig::default();
        let states: HashMap<Identity, ProvisioningState> = HashMap::new();
        let recs = [lint_record(sample_record())];
        let inp = inputs(&recs, &states, &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenActorChain);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no row in IdentityRegistry"));
    }
}
