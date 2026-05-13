//! §6.2 — `broken_actor_chain` check (issue #256).
//!
//! Wraps the pure `pipeline::lint::author_lifecycle::check_author_lifecycle`
//! function and maps its output into the lint verb's `Finding` shape.
//!
//! Per-record check: chain-shape violations (no `Author`, duplicate
//! `Author`, role-order violation), unknown-issuer cases, and
//! `RevocationInFlight` surface as `Kind::BrokenActorChain` at
//! `Severity::Error` — these are registry-state-driven or chain-shape
//! driven, not chain.at-dependent. `Revoked`, `PostRevocationWrite`,
//! and `PreActivationWrite` surface at `Severity::Warning` at P0
//! because the underlying distinction depends on
//! `actor_chain.author.at`, which is unauthenticated until P1 ships
//! signature verification. See `severity_for` for the rationale.
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
            // If the registry lookup for this author failed (per-id
            // fault, surfaced as its own DeferredCheck Error by the
            // dispatch layer), do not synthesize a MissingFromRegistry
            // BrokenActorChain on top — that would manufacture
            // corruption findings out of an infrastructure fault.
            // Chain-shape is still cheap; force the check fn into
            // its shape-only branch by short-circuiting after the
            // shape pass via the `MissingFromRegistry` arm only when
            // chain-shape is also clean (the shape branch fires
            // first inside check_author_lifecycle).
            if let Some(id) = author.as_ref()
                && inputs.unresolvable_authors.contains(id)
            {
                // Run only the chain-shape part by passing a
                // throwaway state; the shape branch fires first and
                // any non-shape verdict is suppressed below by
                // filtering ChainStatus::Malformed messages that did
                // not originate from validate_chain. Simpler: inline
                // the chain-shape check directly.
                use crate::domain::actor_chain::validate_chain;
                use crate::pipeline::lint::author_lifecycle::{
                    AuthorLifecycleFinding, ChainStatus,
                };
                if let Err(e) = validate_chain(&r.stored.record.actor_chain) {
                    return Some(into_finding(AuthorLifecycleFinding {
                        record_id: r.stored.record.id.clone(),
                        author: None,
                        status: ChainStatus::Malformed,
                        message: format!(
                            "actor_chain failed shape validation: {e} — at-rest signature cannot be attributed to a single issuer"
                        ),
                    }));
                }
                return None;
            }
            let state = match author.as_ref() {
                Some(id) => match inputs.author_states.get(id) {
                    Some(lc) => AuthorState::Resolved(lc.clone()),
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
    // Severity policy: a verdict is `Error` only when it is *not*
    // derived from unauthenticated `actor_chain.author.at`. At P0
    // `record.signature` is not verified and `target_hash` is not
    // recomputed, so any classification that depends on chain.at
    // (Revoked-with-pre-revocation-history shape,
    // PostRevocationWrite shape, PreActivationWrite shape) cannot
    // independently prove a trust violation — an at-rest attacker
    // could move the timestamp either direction. Those gate as
    // `Warning`: visible audit signal, no operator-blocking failure
    // for routine offboarding/key-rotation. Verdicts that are
    // registry-state-driven (RevocationInFlight: registry is
    // RevokePending/PurgePending *now*) or chain-shape-driven
    // (Malformed: missing/duplicate Author, role-order violation,
    // Pending issuer, registry corruption, MissingFromRegistry) gate
    // as `Error`. Once P1 ships Ed25519 verification + key_version
    // persistence, chain.at is authenticated and the timestamp-aware
    // statuses can escalate to Error.
    match status {
        ChainStatus::Revoked
        | ChainStatus::PostRevocationWrite
        | ChainStatus::PreActivationWrite => Severity::Warning,
        ChainStatus::RevocationInFlight | ChainStatus::Malformed => Severity::Error,
    }
}

fn suggested_fix_for(status: ChainStatus) -> &'static str {
    match status {
        ChainStatus::Revoked => {
            "audit the affected records — author identity is terminally revoked and chain.at \
             predates revoked_at. Non-blocking at P0: routine offboarding/key-rotation must not \
             retroactively fail every historical record. do NOT auto-tombstone (records signed \
             pre-revocation under a verified key remain legitimate). Once P1 ships Ed25519 \
             verification + key_version persistence, the chain timestamp becomes authenticated \
             and post-revocation-write evidence may escalate to Error"
        }
        ChainStatus::PostRevocationWrite => {
            "audit the affected records — chain.at is at-or-after the identity's revoked_at, \
             which under authenticated timestamps would be a trust-boundary breach. At P0 \
             chain.at is unauthenticated so this remains advisory; quarantine candidates if the \
             surrounding context (write path, sibling records) corroborates, and re-evaluate \
             once P1 ships Ed25519 verification + key_version persistence"
        }
        ChainStatus::PreActivationWrite => {
            "audit the affected records — chain.at is before the identity's activated_at, \
             which under authenticated timestamps would be a pre-activation write. At P0 \
             chain.at is unauthenticated so this remains advisory; investigate the write path \
             and re-evaluate once P1 ships Ed25519 verification + key_version persistence"
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
    use crate::pipeline::lint::author_lifecycle::AuthorLifecycle;
    use crate::verbs::lint::{ConsentModel, LintRecord, SchemaVersion};
    use std::collections::HashMap;

    fn lint_record(record: crate::domain::MemoryRecord) -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn inputs<'a>(
        records: &'a [LintRecord],
        author_states: &'a HashMap<Identity, AuthorLifecycle>,
        cfg: &'a CairnConfig,
    ) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states,
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
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
        let states: HashMap<Identity, AuthorLifecycle> = HashMap::new();
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

    // (Original deferred-info stub test removed: actor_chain is now a
    // real check. The reorganized cases above already cover empty
    // records / missing-from-registry / lifecycle states.)

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
        states.insert(
            author_id,
            AuthorLifecycle {
                state: ProvisioningState::Active,
                // Far-past activation so the chain timestamp ordering
                // check trivially holds for the sample record.
                activated_at: Some(Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid")),
                revoked_at: None,
                purge_requested_at: None,
                purged_at: None,
            },
        );
        let recs = [lint_record(r)];
        let inp = inputs(&recs, &states, &cfg);
        assert!(
            run(&inp).is_empty(),
            "active author + clean chain → no findings"
        );
    }

    #[test]
    fn revoked_author_with_pre_revocation_chain_at_emits_warning() {
        // Round-6 resolution: chain.at-derived statuses (Revoked,
        // PostRevocationWrite, PreActivationWrite) surface as Warning
        // at P0 because chain.at is unauthenticated. Routine
        // offboarding/key-rotation must not retroactively flip every
        // historical record into a blocking failure (round 6
        // recommendation), but the audit trail still surfaces.
        // Registry-state-driven (RevocationInFlight) and chain-shape
        // driven (Malformed) verdicts remain Error.
        let cfg = CairnConfig::default();
        let r = sample_record();
        let author_id = r
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone())
            .expect("author");
        let mut states = HashMap::new();
        states.insert(
            author_id,
            AuthorLifecycle {
                state: ProvisioningState::Revoked,
                activated_at: Some(Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid")),
                // Far-future revoked_at → chain `at` predates
                // revocation → Warning per round-6 policy (chain.at
                // unauthenticated, but visible audit signal).
                revoked_at: Some(Rfc3339Timestamp::parse("2099-12-31T23:59:59Z").expect("valid")),
                purge_requested_at: None,
                purged_at: None,
            },
        );
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
        states.insert(
            author_id,
            AuthorLifecycle {
                state: ProvisioningState::RevokePending,
                activated_at: Some(Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid")),
                // RevokePending with revoked_at present → falls
                // through to RevocationInFlight (Error). Without
                // revoked_at the round-7 fix routes this to
                // ChainStatus::Revoked Warning, so set revoked_at
                // here to keep the test asserting the in-flight
                // suspicious-write path.
                revoked_at: Some(Rfc3339Timestamp::parse("2000-01-02T00:00:00Z").expect("valid")),
                purge_requested_at: None,
                purged_at: None,
            },
        );
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
        let states: HashMap<Identity, AuthorLifecycle> = HashMap::new();
        let recs = [lint_record(sample_record())];
        let inp = inputs(&recs, &states, &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenActorChain);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no row in IdentityRegistry"));
    }
}
