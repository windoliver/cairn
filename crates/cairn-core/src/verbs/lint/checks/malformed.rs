//! §6.1 — `malformed_record` check.
//!
//! Re-runs [`MemoryRecord::validate`](crate::domain::record::MemoryRecord::validate) over each record. Any failure
//! produces an error-severity finding pointing at the record id. PR-1
//! covers in-record invariants only; on-disk projection round-trip is
//! deferred to #254 (lint-fix-WAL advisory lock + projection drift).

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_record};

/// Run the `malformed_record` check (§6.1).
///
/// Returns one error-severity [`Finding`] per record that fails
/// [`MemoryRecord::validate`](crate::domain::record::MemoryRecord::validate), targeting the offending record id.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    for lr in inputs.records {
        if let Err(e) = lr.stored.record.validate() {
            let mut f = finding(
                Kind::MalformedRecord,
                Severity::Error,
                format!("record fails domain validation: {e}"),
            );
            f.target = Some(target_record(&lr.stored.record.id));
            f.suggested_fix = Some(
                "the record violates a domain invariant; re-ingest from source or forget"
                    .to_owned(),
            );
            out.push(f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{ConsentModel, LintRecord, SchemaVersion};

    fn legacy(record: crate::domain::record::MemoryRecord) -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn make_inputs<'a>(records: &'a [LintRecord], cfg: &'a CairnConfig) -> LintInputs<'a> {
        let n = records.len() as u64;
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(n, n),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: None,
            hot_body_loader: None,
        }
    }

    #[test]
    fn clean_record_produces_no_finding() {
        let cfg = CairnConfig::default();
        let r = legacy(sample_record());
        let inputs = make_inputs(std::slice::from_ref(&r), &cfg);
        assert!(run(&inputs).is_empty());
    }

    #[test]
    fn empty_actor_chain_flagged_as_error_with_record_target() {
        let cfg = CairnConfig::default();
        let mut record = sample_record();
        record.actor_chain.clear();
        let r = legacy(record.clone());
        let inputs = make_inputs(std::slice::from_ref(&r), &cfg);
        let findings = run(&inputs);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MalformedRecord);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].suggested_fix.is_some());
        let target = findings[0].target.as_ref().expect("target set");
        let id = target.record_id.as_ref().expect("record_id set");
        assert_eq!(id.0, record.id.as_str());
    }

    #[test]
    fn empty_body_flagged_as_error() {
        let cfg = CairnConfig::default();
        let mut record = sample_record();
        record.body.clear();
        let r = legacy(record);
        let inputs = make_inputs(std::slice::from_ref(&r), &cfg);
        let findings = run(&inputs);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MalformedRecord);
    }
}
