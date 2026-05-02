//! §6.2 — `broken_actor_chain` check (issue #256).
//!
//! Wraps the pure `pipeline::lint::author_lifecycle::check_author_lifecycle`
//! function and maps its output into the lint verb's `Finding` shape.
//!
//! Per-record check: chain-shape violations (no `Author`, duplicate
//! `Author`, role-order violation), unknown-issuer cases, and every
//! revocation-related verdict surface as `Kind::BrokenActorChain` at
//! `Severity::Error`. The structural distinction between `Revoked`,
//! `PostRevocationWrite`, `RevocationInFlight`, and `Malformed` lives
//! in the message and the `ChainStatus` variant for diagnostic value
//! — severity does not depend on `actor_chain.author.at` because that
//! field is unauthenticated at P0. See `severity_for` for the
//! rationale.
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
    // Severity policy: every revocation-related verdict is blocking
    // until P1 cryptographic verification ships. Earlier rounds split
    // `Revoked` (chain `at` < `revoked_at` → Warning) from
    // `PostRevocationWrite` (chain `at` >= `revoked_at` → Error) on
    // the assumption that the chain timestamp could be trusted to
    // distinguish legitimate pre-revocation history from a
    // post-revocation tamper. At P0, `record.signature` is *not*
    // verified and `target_hash` is *not* recomputed (see module
    // docs + the cli's §6.2 deferred advisory), so an at-rest
    // attacker can backdate `actor_chain.author.at` to convert a
    // real post-revocation write into the lower-severity branch.
    // Severity must therefore not depend on unauthenticated record
    // content. The structural distinction stays in the message + the
    // `ChainStatus` variant for diagnostic value, but every
    // revocation-related status gates as Error. Once P1 verifies the
    // signature, the chain timestamp becomes authenticated and the
    // pre-revocation-history downgrade can return.
    match status {
        ChainStatus::Revoked
        | ChainStatus::PostRevocationWrite
        | ChainStatus::RevocationInFlight
        | ChainStatus::Malformed => Severity::Error,
    }
}

fn suggested_fix_for(status: ChainStatus) -> &'static str {
    match status {
        ChainStatus::Revoked => {
            "audit the affected records — author identity is terminally revoked. The chain \
             timestamp suggests the write predates revocation, BUT chain.at is unauthenticated at \
             P0 (record.signature is not verified, target_hash is not recomputed) so backdating \
             cannot be ruled out; treat as blocking until P1 ships Ed25519 verification + \
             key_version persistence — do NOT auto-tombstone (records signed pre-revocation under \
             a verified key are legitimate)"
        }
        ChainStatus::PostRevocationWrite => {
            "investigate as a suspected trust-boundary breach — the chain author timestamp is \
             at-or-after the identity's revoked_at, so the record was signed under withdrawn \
             signing right; quarantine the affected records and trace the write path before \
             re-ingest or tombstone"
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
            stored: StoredRecord { record, version: 1 },
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
            schema_version: SchemaVersion { major: 0, minor: 1 },
            author_states,
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
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
    fn revoked_author_with_pre_revocation_chain_at_emits_blocking_error() {
        // Round-5 fix: chain.at is unauthenticated at P0
        // (record.signature is not verified, target_hash is not
        // recomputed) so the pre-revocation downgrade cannot rely on
        // it — a tamper could backdate `actor_chain.author.at`.
        // Severity must therefore not depend on unauthenticated record
        // content; every revocation-related verdict gates as Error
        // until P1 ships Ed25519 verification + key_version
        // persistence. The structural Revoked-vs-PostRevocationWrite
        // distinction stays in the message + the ChainStatus variant
        // for diagnostic value.
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
                // revocation → would have been Warning under the
                // pre-round-5 policy; now Error because chain.at is
                // unauthenticated.
                revoked_at: Some(Rfc3339Timestamp::parse("2099-12-31T23:59:59Z").expect("valid")),
            },
        );
        let recs = [lint_record(r)];
        let inp = inputs(&recs, &states, &cfg);
        let findings = run(&inp);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenActorChain);
        assert_eq!(findings[0].severity, Severity::Error);
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
                // No revoked_at → falls through to RevocationInFlight
                // (Error). With timestamps the legitimate-history
                // case for in-flight is exercised by a separate test.
                revoked_at: None,
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
