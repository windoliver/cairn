//! Salience inspection check for `cairn lint`.

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_record};

/// Surface records whose salience has fallen below the configured
/// auto-eviction threshold. This is informational: eviction still requires
/// age, pin, and consent guardrails in the workflow/store layer.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let threshold = inputs.config.vault.salience.eviction_threshold;
    inputs
        .records
        .iter()
        .filter(|record| record.stored.record.salience < threshold)
        .map(|record| {
            let mut f = finding(
                Kind::Stale,
                Severity::Info,
                format!(
                    "record salience {:.3} is below eviction threshold {:.3}; \
                     decay eviction still requires age, pin, and consent guardrails",
                    record.stored.record.salience, threshold,
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            f.tracking_issue = Some(313);
            f
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::consent_timeline::ConsentModel;
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{LintInputs, LintRecord, SchemaVersion};

    fn inputs<'a>(cfg: &'a CairnConfig, records: &'a [LintRecord]) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
        }
    }

    #[test]
    fn emits_info_for_below_threshold_record() {
        let mut record = sample_record();
        record.salience = 0.05;
        let lint_record = LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        };
        let cfg = CairnConfig::default();

        let findings = run(&inputs(&cfg, &[lint_record]));

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::Stale);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("salience 0.050"));
    }

    #[test]
    fn skips_record_at_or_above_threshold() {
        let mut record = sample_record();
        record.salience = 0.10;
        let lint_record = LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        };
        let cfg = CairnConfig::default();

        let findings = run(&inputs(&cfg, &[lint_record]));

        assert!(findings.is_empty());
    }
}
