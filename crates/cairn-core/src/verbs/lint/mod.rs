//! `lint` verb — read-only health checks.
//!
//! Spec: `docs/superpowers/specs/2026-04-30-lint-checks-design.md`.
//! Issue: <https://github.com/windoliver/cairn/issues/96>.

use std::collections::{HashMap, HashSet};

use crate::config::CairnConfig;
use crate::contract::consent_journal::ConsentJournalReader;
use crate::contract::consent_lookup::ConsentLookup;
use crate::contract::memory_store::{IndexStats, StoredRecord};
use crate::contract::source_resolver::SourceResolver;
use crate::domain::record::RecordId;
use crate::domain::{Identity, SourceId};
use crate::generated::verbs::lint::{
    Finding, Kind, LintData, LintDataSummary, LintDataSummaryBySeverity, Severity, Target,
};
use crate::pipeline::lint::author_lifecycle::AuthorLifecycle;

pub mod checks;
pub mod report;

/// One linted record + the per-row `consent_model` gate from the records
/// table. PR-1 always carries `LegacyEvent` because the migration that
/// adds the column is part of #253; lint behavior in PR-1 is independent
/// of this value (the §6.5 deferred-info finding is emitted unconditionally).
#[derive(Debug, Clone)]
pub struct LintRecord {
    /// The stored record under audit.
    pub stored: StoredRecord,
    /// Per-row consent-model gate; see #253.
    pub consent_model: ConsentModel,
}

pub use crate::domain::consent_timeline::ConsentModel;

/// Read-only view of one source artifact referenced by provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceArtifact {
    /// Vault-relative path used to resolve the artifact.
    pub path: String,
    /// Resolution/hash state captured by the adapter.
    pub state: SourceArtifactState,
}

/// Result of resolving a provenance source artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceArtifactState {
    /// The artifact exists and its SHA-256 hash was computed successfully.
    Present {
        /// Lowercase hex `sha256:<64hex>` digest of the file bytes.
        sha256: String,
    },
    /// The artifact exists, but its body has been rewritten to a Cairn
    /// redaction marker after a source-forget receipt.
    Redacted {
        /// The original source hash preserved in the marker.
        original_sha256: String,
    },
    /// The artifact path did not exist when lint gathered the snapshot.
    Missing,
    /// The adapter could not read or hash the file.
    Unreadable {
        /// Short diagnostic string for operator-facing findings.
        message: String,
    },
}

/// Read-only forget ledger grouped by provenance `source_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceForgetLedger {
    /// Hashes of target ids previously forgotten from this source.
    pub forgotten_target_hashes: HashSet<String>,
}

/// Snapshot the check engine operates over. Pure inputs; no I/O.
///
/// `author_states` is the §6.2 author-lifecycle slice's pre-fetched
/// payload. The dispatch layer (`cairn-cli`) is responsible for
/// resolving every active record's chain author against the
/// `IdentityRegistry` under `IdentityVisibility::Audit` and assembling
/// the map. The reference is required (not `Option`) so a caller cannot
/// silently degrade lint into a no-op against revoked / unknown / pending
/// issuers — passing an empty map signals "no chain authors / no
/// resolvable identities", not "skip the check." An identity present on
/// a record but absent from the map is treated as `MissingFromRegistry`
/// (fail-closed, brief invariant 6).
pub struct LintInputs<'a> {
    /// Active records under audit.
    pub records: &'a [LintRecord],
    /// Resolved config snapshot.
    pub config: &'a CairnConfig,
    /// Counts driving the index-drift check.
    pub index_stats: IndexStats,
    /// Pre-fetched author identity → lifecycle state map. See struct
    /// docs.
    pub author_states: &'a HashMap<Identity, AuthorLifecycle>,
    /// Identities whose registry lookup failed during prefetch. The
    /// dispatch layer surfaces a per-identity `DeferredCheck` Error
    /// for each one; the §6.2 leaf must NOT additionally synthesize
    /// `MissingFromRegistry` Errors for these — that would manufacture
    /// false corruption findings out of an infrastructure fault.
    pub unresolvable_authors: &'a HashSet<Identity>,
    /// `ConsentLookup` adapter (Issue #253). `None` when the CLI hasn't
    /// wired one — the §6.5 check downgrades to a no-op so `lint` stays
    /// useful in fixture-only / pre-#253 contexts.
    pub consent_lookup: Option<&'a (dyn ConsentLookup + 'a)>,
    /// Read-only source-artifact snapshot keyed by `provenance.source_ids`.
    pub source_artifacts: &'a HashMap<SourceId, SourceArtifact>,
    /// Read-only source-forget receipts keyed by provenance `source_hash`.
    pub source_forgets: &'a HashMap<String, SourceForgetLedger>,
    /// Vault root for filesystem-backed lint checks (broken source
    /// links, missing summaries). `None` falls those checks back to
    /// no-ops so fixture-only tests of unrelated checks remain green.
    pub vault_root: Option<&'a std::path::Path>,
    /// Loads step bodies for the dry-run hot-memory walker. `None`
    /// keeps the over-budget check on the canary path.
    pub hot_body_loader: Option<
        &'a (
                dyn Fn(
            crate::generated::verbs::assemble_hot::HotRecipeStep,
        ) -> Result<String, String>
                    + Send
                    + Sync
                    + 'a
            ),
    >,
    /// Resolver for immutable source bytes referenced by
    /// `record.provenance.source_refs`.
    pub source_resolver: &'a (dyn SourceResolver + 'a),
    /// Read-only forget-related view of `consent_journal`.
    pub consent_journal: &'a (dyn ConsentJournalReader + 'a),
    /// Read-only adapter for `workflow_jobs` (issue #92, spec §4.8).
    /// `None` keeps the `workflow_health` check on the no-op path so
    /// fixture-only tests of unrelated checks stay green.
    pub workflow_jobs: Option<&'a (dyn crate::contract::workflow_jobs::WorkflowJobsReader + 'a)>,
    /// Wall-clock for time-based lint checks. The CLI passes
    /// `SystemClock::now_ms()`; tests pass a synthetic value.
    pub now_ms: i64,
}

impl std::fmt::Debug for LintInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LintInputs")
            .field("records", &self.records)
            .field("config", &"<CairnConfig>")
            .field("index_stats", &self.index_stats)
            .field("author_states", &self.author_states.len())
            .field("unresolvable_authors", &self.unresolvable_authors.len())
            .field("consent_lookup", &self.consent_lookup.is_some())
            .field("source_artifacts", &self.source_artifacts.len())
            .field("source_forgets", &self.source_forgets.len())
            .field("vault_root", &self.vault_root)
            .field("hot_body_loader", &self.hot_body_loader.is_some())
            .field("source_resolver", &"<dyn SourceResolver>")
            .field("consent_journal", &"<dyn ConsentJournalReader>")
            .field("workflow_jobs", &self.workflow_jobs.is_some())
            .field("now_ms", &self.now_ms)
            .finish()
    }
}

pub use crate::contract::version::SchemaVersion;

/// Returns a process-wide shared empty `author_states` map for tests
/// of checks that don't exercise §6.2. Avoids repeating
/// `let states = HashMap::new();` in every test fixture.
#[cfg(test)]
pub(crate) fn empty_author_states() -> &'static HashMap<Identity, AuthorLifecycle> {
    use std::sync::OnceLock;
    static M: OnceLock<HashMap<Identity, AuthorLifecycle>> = OnceLock::new();
    M.get_or_init(HashMap::new)
}

/// Companion test helper: process-wide empty `unresolvable_authors`
/// set for tests of checks that don't exercise registry-failure
/// degraded paths.
#[cfg(test)]
pub(crate) fn empty_unresolvable_authors() -> &'static HashSet<Identity> {
    use std::sync::OnceLock;
    static S: OnceLock<HashSet<Identity>> = OnceLock::new();
    S.get_or_init(HashSet::new)
}

#[cfg(test)]
struct EmptySourceResolver;

#[cfg(test)]
impl SourceResolver for EmptySourceResolver {
    fn exists(&self, _id: &str) -> bool {
        false
    }

    fn read(&self, _id: &str) -> Result<Vec<u8>, crate::contract::SourceResolverError> {
        Err(crate::contract::SourceResolverError::NotFound)
    }

    fn locator(&self, id: &str) -> String {
        format!("empty:{id}")
    }
}

#[cfg(test)]
pub(crate) fn empty_source_resolver() -> &'static dyn SourceResolver {
    static RESOLVER: EmptySourceResolver = EmptySourceResolver;
    &RESOLVER
}

#[cfg(test)]
struct EmptyConsentJournal;

#[cfg(test)]
impl ConsentJournalReader for EmptyConsentJournal {
    fn forgotten_source_bytes_hashes(&self) -> HashSet<String> {
        HashSet::new()
    }

    fn forgotten_source_forgets(&self) -> Vec<crate::contract::SourceForget> {
        Vec::new()
    }

    fn malformed_source_forget_rows(&self) -> Vec<crate::contract::MalformedSourceForget> {
        Vec::new()
    }

    fn malformed_source_forget_rows_for_source(
        &self,
        _source_bytes_hash: &str,
    ) -> Vec<crate::contract::MalformedSourceForget> {
        Vec::new()
    }
}

#[cfg(test)]
pub(crate) fn empty_consent_journal() -> &'static dyn ConsentJournalReader {
    static JOURNAL: EmptyConsentJournal = EmptyConsentJournal;
    &JOURNAL
}

/// Companion test helper: empty provenance source-artifact snapshot.
#[cfg(test)]
pub(crate) fn empty_source_artifacts() -> &'static HashMap<SourceId, SourceArtifact> {
    use std::sync::OnceLock;
    static M: OnceLock<HashMap<SourceId, SourceArtifact>> = OnceLock::new();
    M.get_or_init(HashMap::new)
}

/// Companion test helper: empty source-forget snapshot.
#[cfg(test)]
pub(crate) fn empty_source_forgets() -> &'static HashMap<String, SourceForgetLedger> {
    use std::sync::OnceLock;
    static M: OnceLock<HashMap<String, SourceForgetLedger>> = OnceLock::new();
    M.get_or_init(HashMap::new)
}

/// In-process fake of [`crate::contract::workflow_jobs::WorkflowJobsReader`]
/// for unit tests of the `workflow_health` check (issue #92, spec §4.10).
///
/// Builder-style helpers seed the relevant slice of state; unset fields
/// return the trait's "empty" sentinel.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MockWorkflowJobsReader {
    pub dead_letter: Vec<crate::contract::workflow_jobs::DeadLetterRow>,
    pub oldest_queued_age: Option<i64>,
    pub longest_lease: Option<i64>,
    pub last_success: std::collections::HashMap<String, i64>,
}

#[cfg(test)]
impl MockWorkflowJobsReader {
    pub fn with_dead_letter(mut self, row: crate::contract::workflow_jobs::DeadLetterRow) -> Self {
        self.dead_letter.push(row);
        self
    }

    pub fn with_oldest_queued_age(mut self, age_ms: i64) -> Self {
        self.oldest_queued_age = Some(age_ms);
        self
    }

    pub fn with_last_success(mut self, kind: &str, ms: i64) -> Self {
        self.last_success.insert(kind.to_string(), ms);
        self
    }
}

#[cfg(test)]
impl crate::contract::workflow_jobs::WorkflowJobsReader for MockWorkflowJobsReader {
    fn dead_letter_count(&self, _: Option<&crate::contract::job_store::JobKind>) -> usize {
        self.dead_letter.len()
    }
    fn oldest_queued_age_ms(
        &self,
        _: Option<&crate::contract::job_store::JobKind>,
        _: i64,
    ) -> Option<i64> {
        self.oldest_queued_age
    }
    fn longest_held_lease_ms(&self, _: i64) -> Option<i64> {
        self.longest_lease
    }
    fn last_success_ms(&self, kind: &crate::contract::job_store::JobKind) -> Option<i64> {
        self.last_success.get(kind.as_str()).copied()
    }
    fn dead_letter_rows(&self, limit: usize) -> Vec<crate::contract::workflow_jobs::DeadLetterRow> {
        self.dead_letter.iter().take(limit).cloned().collect()
    }
}

/// Construct a `LintInputs` populated only with the `workflow_jobs` reader
/// and a synthetic `now_ms`. Used by `workflow_health` unit tests so each
/// case stays one line and the rest of the input slots stay on the
/// process-wide empty sentinels.
#[cfg(test)]
pub(crate) fn empty_lint_inputs_with_reader(
    reader: &dyn crate::contract::workflow_jobs::WorkflowJobsReader,
    now_ms: i64,
) -> LintInputs<'_> {
    use std::sync::OnceLock;
    static CFG: OnceLock<CairnConfig> = OnceLock::new();
    let cfg = CFG.get_or_init(CairnConfig::default);
    LintInputs {
        records: &[],
        config: cfg,
        index_stats: crate::contract::memory_store::IndexStats::new(0, 0),
        author_states: empty_author_states(),
        unresolvable_authors: empty_unresolvable_authors(),
        consent_lookup: None,
        source_artifacts: empty_source_artifacts(),
        source_forgets: empty_source_forgets(),
        vault_root: None,
        hot_body_loader: None,
        source_resolver: empty_source_resolver(),
        consent_journal: empty_consent_journal(),
        workflow_jobs: Some(reader),
        now_ms,
    }
}

/// Run every check, aggregate findings, return the canonical `LintData`.
pub async fn run_checks(inputs: &LintInputs<'_>) -> LintData {
    let mut findings: Vec<Finding> = Vec::new();
    findings.extend(checks::malformed::run(inputs));
    findings.extend(checks::actor_chain::run(inputs));
    findings.extend(checks::provenance::run(inputs));
    findings.extend(checks::schema::run(inputs));
    findings.extend(checks::trace_reasoning::run(inputs));
    findings.extend(checks::taxonomy_conventions::run(inputs));
    findings.extend(checks::salience::run(inputs));
    findings.extend(checks::hot_memory::run(inputs));
    findings.extend(checks::index_drift::run(inputs));
    findings.extend(checks::workflow_health::run(inputs));
    findings.extend(checks::consent::run(inputs).await);
    let summary = summarize(&findings);
    LintData {
        findings,
        summary,
        report_path: None,
    }
}

fn summarize(findings: &[Finding]) -> LintDataSummary {
    let mut by_severity = LintDataSummaryBySeverity {
        error: 0,
        warning: 0,
        info: 0,
    };
    let mut by_kind = serde_json::Map::new();
    for f in findings {
        match f.severity {
            Severity::Error => by_severity.error += 1,
            Severity::Warning => by_severity.warning += 1,
            Severity::Info => by_severity.info += 1,
        }
        let key = kind_key(f.kind);
        let entry = by_kind
            .entry(key)
            .or_insert_with(|| serde_json::Value::Number(0.into()));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::Number((n + 1).into());
        }
    }
    LintDataSummary {
        auto_resolved: None,
        total: findings.len() as u64,
        by_severity,
        by_kind: serde_json::Value::Object(by_kind),
    }
}

fn kind_key(k: Kind) -> String {
    match k {
        Kind::Contradiction => "contradiction",
        Kind::ContradictoryEdge => "contradictory_edge",
        Kind::Orphan => "orphan",
        Kind::Stale => "stale",
        Kind::MissingConcept => "missing_concept",
        Kind::AmbiguousEdge => "ambiguous_edge",
        Kind::DataGap => "data_gap",
        Kind::MalformedRecord => "malformed_record",
        Kind::MisclassifiedProfile => "misclassified_profile",
        Kind::BrokenActorChain => "broken_actor_chain",
        Kind::BrokenSourceLink => "broken_source_link",
        Kind::MissingProvenance => "missing_provenance",
        Kind::MissingSummary => "missing_summary",
        Kind::OrphanInsight => "orphan_insight",
        Kind::StaleSchema => "stale_schema",
        Kind::StaleProfileLine => "stale_profile_line",
        Kind::HotMemoryOverBudget => "hot_memory_over_budget",
        Kind::IndexDrift => "index_drift",
        Kind::DeferredCheck => "deferred_check",
        Kind::ProjectionDrift => "projection_drift",
        Kind::ProjectionMissing => "projection_missing",
        Kind::WrongClassForKind => "wrong_class_for_kind",
        Kind::SourceAfterForget => "source_after_forget",
        Kind::SourceAfterForgetUnknownVersion => "source_after_forget_unknown_version",
        Kind::SourceHashMismatch => "source_hash_mismatch",
        Kind::SourceLinkDangling => "source_link_dangling",
        Kind::SourceLinkLegacyDuplicate => "source_link_legacy_duplicate",
        Kind::SourceLinkMissing => "source_link_missing",
        Kind::SourceRedactSkipped => "source_redact_skipped",
        Kind::WorkflowDeadLetter => "workflow_dead_letter",
        Kind::WorkflowOverdue => "workflow_overdue",
        Kind::WorkflowStaleSummary => "workflow_stale_summary",
        Kind::WorkflowStuck => "workflow_stuck",
    }
    .to_owned()
}

/// Construct a finding with no target / fix / tracking issue.
pub(crate) fn finding(kind: Kind, severity: Severity, message: impl Into<String>) -> Finding {
    Finding {
        entities: None,
        kind,
        message: message.into(),
        severity,
        suggested_fix: None,
        target: None,
        tracking_issue: None,
    }
}

/// Build a `Target` pointing at a record id.
///
/// `Ulid` is `pub struct Ulid(pub String)` with no infallible constructor
/// validation — the newtype wraps without copying the validation logic here
/// because `RecordId::as_str()` already guarantees a syntactically valid ULID
/// was accepted at parse time.
pub(crate) fn target_record(id: &RecordId) -> Target {
    Target {
        record_id: Some(crate::generated::common::Ulid(id.as_str().to_owned())),
        operation_id: None,
        path: None,
    }
}

/// Build a `Target` pointing at a vault path or table name.
pub(crate) fn target_path(path: impl Into<String>) -> Target {
    Target {
        record_id: None,
        operation_id: None,
        path: Some(path.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::record::tests_export::sample_record;

    fn sample_source_artifacts() -> HashMap<SourceId, SourceArtifact> {
        let record = sample_record();
        record
            .provenance
            .source_ids
            .iter()
            .cloned()
            .map(|source_id| {
                (
                    source_id,
                    SourceArtifact {
                        path: "sources/test/sample.txt".to_owned(),
                        state: SourceArtifactState::Present {
                            sha256: record.provenance.source_hash.clone(),
                        },
                    },
                )
            })
            .collect()
    }

    fn legacy_lint_record() -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record: sample_record(),
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    #[tokio::test]
    async fn run_checks_on_empty_inputs_returns_no_findings_yet() {
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };
        let data = run_checks(&inputs).await;
        // Empty records: consent (#253) is wired but has nothing to
        // classify, actor_chain (#256) the same, schema (#258) is
        // live. With no records, provenance (#257) emits nothing.
        // hot_memory (#83) still emits the dormant missing_summary
        // DeferredCheck Info even without a body loader.
        assert_eq!(data.summary.total, data.findings.len() as u64);
        assert_eq!(data.summary.by_severity.error, 0);
        assert_eq!(data.summary.by_severity.warning, 0);
        assert_eq!(
            data.findings
                .iter()
                .filter(|f| matches!(f.kind, Kind::DeferredCheck))
                .count(),
            1
        );
        assert_eq!(data.summary.by_severity.info, 1);
    }

    #[tokio::test]
    async fn run_checks_is_record_order_independent() {
        fn canonicalize(findings: &[crate::generated::verbs::lint::Finding]) -> Vec<String> {
            let mut keys: Vec<String> = findings
                .iter()
                .map(|f| {
                    let kind = format!("{:?}", f.kind);
                    let sev = format!("{:?}", f.severity);
                    format!("{kind}|{sev}|{}", f.message)
                })
                .collect();
            keys.sort();
            keys
        }

        let cfg = CairnConfig::default();
        let mk = |label: &str| -> LintRecord {
            let mut r = sample_record();
            r.tags = vec![label.to_owned()];
            LintRecord {
                stored: StoredRecord {
                    record: r,
                    version: 1,
                    schema_version: Some(SchemaVersion::current()),
                },
                consent_model: ConsentModel::LegacyEvent,
            }
        };
        let a = mk("first");
        let b = mk("second");
        let c = mk("third");

        let forward = [a.clone(), b.clone(), c.clone()];
        let reversed = [c, b, a];
        let source_artifacts = sample_source_artifacts();

        let inputs_fwd = LintInputs {
            records: &forward,
            config: &cfg,
            index_stats: IndexStats::new(forward.len() as u64, forward.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: &source_artifacts,
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };
        let inputs_rev = LintInputs {
            records: &reversed,
            config: &cfg,
            index_stats: IndexStats::new(reversed.len() as u64, reversed.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: &source_artifacts,
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };

        let fwd = canonicalize(&run_checks(&inputs_fwd).await.findings);
        let rev = canonicalize(&run_checks(&inputs_rev).await.findings);
        assert_eq!(fwd, rev, "run_checks is record-order-dependent");
    }

    #[test]
    fn kind_key_includes_new_hot_memory_kinds() {
        assert_eq!(
            super::kind_key(Kind::StaleProfileLine),
            "stale_profile_line"
        );
        assert_eq!(
            super::kind_key(Kind::BrokenSourceLink),
            "broken_source_link"
        );
        assert_eq!(super::kind_key(Kind::MissingSummary), "missing_summary");
    }

    #[tokio::test]
    async fn run_checks_with_one_record_aggregates_summary_correctly() {
        let cfg = CairnConfig::default();
        let r = legacy_lint_record();
        let source_artifacts = sample_source_artifacts();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: &source_artifacts,
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };
        let data = run_checks(&inputs).await;
        assert_eq!(data.summary.total, data.findings.len() as u64);
        // §6.2 actor_chain runs the real check now (registry required
        // at the API boundary). With an empty author_states map the
        // sample record's author resolves to MissingFromRegistry →
        // BrokenActorChain Error. consent (#253) is also live but
        // returns no findings for a LegacyEvent record without a
        // ConsentLookup wired. §6.4 schema (#258) is live: record and
        // host both stamp at `SchemaVersion::current()` so `compare`
        // returns `Same` and no finding fires. Provenance (#257) is
        // live, so the sample record's empty `source_refs` yields a
        // `SourceLinkMissing` Warning. hot_memory (#83) contributes
        // the missing_summary DeferredCheck Info.
        assert_eq!(data.summary.by_severity.error, 1);
        assert!(data.summary.by_severity.warning >= 1);
        let info_count = data
            .findings
            .iter()
            .filter(|f| matches!(f.severity, Severity::Info))
            .count() as u64;
        assert_eq!(data.summary.by_severity.info, info_count);
    }
}
