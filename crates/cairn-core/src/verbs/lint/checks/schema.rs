//! §6.4 — `stale_schema` check.
//!
//! Compares each active record's per-row [`SchemaVersion`] stamp against
//! the host's current contract version. Spec
//! `docs/superpowers/specs/2026-04-30-lint-checks-design.md` §6.4:
//!
//! - same `(major, minor)` → no finding;
//! - one minor behind → `Warning`;
//! - two or more minors behind, or major mismatch → `Error`;
//! - forward skew (record stamped above host, e.g. after a rollback)
//!   → `Error` — the older binary may not understand the newer row
//!   shape, and silently treating it as `Same` would hide a real
//!   incompatibility.
//!
//! Suggested fix: re-ingest the record (any ingest path re-stamps the
//! row at the current schema). The eventual `cairn migrate --to
//! <current>` verb is referenced in the brief but not yet implemented.

use crate::contract::version::{SchemaSkew, SchemaVersion};
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_record};

const SUGGESTED_FIX: &str = "re-ingest the affected record(s) — every \
                             ingest path re-stamps the row at the current \
                             schema and clears this finding";

/// Run the §6.4 `stale_schema` check against `inputs`.
///
/// The "current" version is read directly from
/// [`SchemaVersion::current()`] — the same constant the store stamps
/// rows with. The check deliberately does *not* accept an externally
/// supplied schema version, because the lint and the store would
/// otherwise have two different definitions of "current" and a caller
/// passing `SchemaVersion::from_contract(CONTRACT_VERSION)` would mark
/// every freshly written row stale (Issue #258 review round 8).
///
/// Per-row findings fire on `Behind` / `Ahead` / `MajorMismatch` skew
/// against the host contract.
///
/// Unstamped legacy rows (pre-Issue #258 migration, hydrated as `None`)
/// each surface as a per-row `Info` finding (so operators can enumerate
/// which records to re-ingest), AND we emit one aggregate `Warning`
/// carrying the count. The aggregate keeps CI gates that fail-on-warning
/// from missing the upgrade signal — without it, an upgraded vault
/// could appear warning-clean even though every historical row was
/// missing schema provenance (Issue #258 review round 8).
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    run_against(inputs, SchemaVersion::current())
}

/// Like [`run`], but with a caller-supplied host version. Restricted to
/// `pub(crate)` so dispatch can only ever pass `SchemaVersion::current()`
/// — tests use this seam to drive Behind/Ahead/MajorMismatch scenarios
/// without bumping the global constant.
#[must_use]
pub(crate) fn run_against(inputs: &LintInputs<'_>, current: SchemaVersion) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut legacy_count: u64 = 0;
    for r in inputs.records {
        let Some(record_version) = r.stored.schema_version else {
            legacy_count += 1;
            let mut f = finding(
                Kind::StaleSchema,
                Severity::Info,
                format!(
                    "record `{}` carries no schema-version stamp (legacy \
                     row predating Issue #258); authored schema is \
                     unknown so parity with the current contract ({current}) \
                     cannot be verified",
                    r.stored.record.id.as_str(),
                ),
            );
            f.target = Some(target_record(&r.stored.record.id));
            f.suggested_fix = Some(SUGGESTED_FIX.to_owned());
            findings.push(f);
            continue;
        };
        let (severity, hint) = match current.compare(record_version) {
            SchemaSkew::Same => continue,
            SchemaSkew::BehindBy(1) => (Severity::Warning, "one minor behind"),
            SchemaSkew::BehindBy(_) => (Severity::Error, "two or more minors behind"),
            SchemaSkew::AheadBy(_) => (
                Severity::Error,
                "stamped above host (rollback / forward skew)",
            ),
            SchemaSkew::MajorMismatch => (Severity::Error, "major version mismatch"),
        };
        let mut f = finding(
            Kind::StaleSchema,
            severity,
            format!(
                "record `{}` is stamped with schema {} but the current contract is {} ({hint})",
                r.stored.record.id.as_str(),
                record_version,
                current,
            ),
        );
        f.target = Some(target_record(&r.stored.record.id));
        f.suggested_fix = Some(SUGGESTED_FIX.to_owned());
        findings.push(f);
    }
    if legacy_count > 0 {
        let mut aggregate = finding(
            Kind::StaleSchema,
            Severity::Warning,
            format!(
                "{legacy_count} record(s) carry no schema-version stamp \
                 (legacy rows predating Issue #258); their authored \
                 schema is unknown so parity with the current contract \
                 ({current}) cannot be verified — see the per-record \
                 Info findings above for the affected ids"
            ),
        );
        aggregate.suggested_fix = Some(SUGGESTED_FIX.to_owned());
        findings.push(aggregate);
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::contract::version::SchemaVersion;
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{ConsentModel, LintRecord};

    fn record_at(version: Option<SchemaVersion>) -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record: sample_record(),
                version: 1,
                schema_version: version,
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn stamped(v: SchemaVersion) -> LintRecord {
        record_at(Some(v))
    }

    fn inputs<'a>(cfg: &'a CairnConfig, records: &'a [LintRecord]) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
        }
    }

    #[test]
    fn same_version_emits_no_finding() {
        let cfg = CairnConfig::default();
        let r = [stamped(SchemaVersion::new(0, 3))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert!(f.is_empty(), "got: {f:?}");
    }

    #[test]
    fn one_minor_behind_warns() {
        let cfg = CairnConfig::default();
        let r = [stamped(SchemaVersion::new(0, 2))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::StaleSchema);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].target.is_some());
        assert!(f[0].suggested_fix.as_deref().unwrap().contains("re-ingest"));
    }

    #[test]
    fn two_minors_behind_errors() {
        let cfg = CairnConfig::default();
        let r = [stamped(SchemaVersion::new(0, 1))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn major_mismatch_errors() {
        let cfg = CairnConfig::default();
        let r = [stamped(SchemaVersion::new(1, 0))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn unstamped_legacy_rows_emit_per_row_info_plus_aggregate_warning() {
        // Pre-0041 rows (NULL columns hydrated to None) surface as:
        //   - one per-row Info each (carrying the record_id target so
        //     operators can enumerate which rows to re-ingest), AND
        //   - one aggregate Warning carrying the count, so CI gates
        //     that fail-on-warning don't miss the upgrade signal.
        // Round 4 had aggregated to a single Warning (no targets).
        // Round 7 flipped to per-row Info only (no Warning).
        // Round 8 keeps both: per-row actionability AND the
        // Warning-level safety signal.
        let cfg = CairnConfig::default();
        let r = [record_at(None), record_at(None), record_at(None)];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(
            f.len(),
            4,
            "expected 3 per-row Info + 1 aggregate Warning, got {f:?}"
        );
        let infos: Vec<_> = f.iter().filter(|x| x.severity == Severity::Info).collect();
        assert_eq!(infos.len(), 3);
        for x in &infos {
            assert!(x.target.is_some(), "legacy Info must carry record target");
            assert!(x.message.contains("legacy row"));
        }
        let warnings: Vec<_> = f
            .iter()
            .filter(|x| x.severity == Severity::Warning)
            .collect();
        assert_eq!(warnings.len(), 1, "expected exactly one aggregate Warning");
        assert!(
            warnings[0]
                .message
                .contains("3 record(s) carry no schema-version stamp"),
            "aggregate must carry the count: {}",
            warnings[0].message,
        );
        assert!(
            warnings[0].target.is_none(),
            "aggregate has no per-row target"
        );
    }

    #[test]
    fn legacy_findings_coexist_with_per_row_skew_findings() {
        // Vault carries both: an unstamped legacy row AND a stale-
        // stamped row. Per-row Info for the legacy + aggregate
        // Warning for the legacy bucket + per-row Error for the skew.
        let cfg = CairnConfig::default();
        let r = [record_at(None), stamped(SchemaVersion::new(0, 1))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(
            f.len(),
            3,
            "expected 1 legacy Info + 1 aggregate Warning + 1 stale Error: {f:?}"
        );
        assert!(f.iter().any(|x| x.severity == Severity::Error));
        assert!(
            f.iter()
                .any(|x| x.severity == Severity::Info && x.message.contains("legacy row"))
        );
        assert!(f.iter().any(|x| {
            x.severity == Severity::Warning
                && x.message
                    .contains("1 record(s) carry no schema-version stamp")
        }));
    }

    #[test]
    fn newer_record_errors_on_rollback() {
        // Host has been rolled back to an older binary; row was written
        // under a newer contract. The older binary may not understand
        // the newer row shape — surface as Error rather than swallow
        // the forward skew.
        let cfg = CairnConfig::default();
        let r = [stamped(SchemaVersion::new(0, 5))];
        let f = run_against(&inputs(&cfg, &r), SchemaVersion::new(0, 3));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(
            f[0].message.contains("rollback") || f[0].message.contains("forward"),
            "expected rollback/forward hint in: {}",
            f[0].message,
        );
    }
}
