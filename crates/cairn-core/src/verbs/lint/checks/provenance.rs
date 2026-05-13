//! §6.5 — provenance source-link hygiene checks.

use sha2::{Digest, Sha256};

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, SourceArtifactState, finding, target_path, target_record};

/// Emit source-link hygiene findings for every active record.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for record in inputs.records {
        let provenance = &record.stored.record.provenance;
        let target_hash = target_id_hash(record.stored.record.target_id.as_str());
        let source_forgotten = inputs
            .source_forgets
            .get(&provenance.source_hash)
            .is_some_and(|ledger| ledger.forgotten_target_hashes.contains(&target_hash));

        if source_forgotten {
            let mut f = finding(
                Kind::MissingProvenance,
                Severity::Error,
                format!(
                    "record {} still resolves to forgotten source hash `{}`",
                    record.stored.record.id.as_str(),
                    provenance.source_hash
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            f.suggested_fix = Some(
                "re-run `cairn forget --record` for this target or purge the resurrected record"
                    .to_owned(),
            );
            findings.push(f);
        }

        if provenance.source_ids.is_empty() {
            let mut f = finding(
                Kind::MissingProvenance,
                Severity::Error,
                format!(
                    "record {} is missing provenance.source_ids",
                    record.stored.record.id.as_str()
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            f.suggested_fix = Some(
                "re-ingest the record so provenance.source_ids names at least one source artifact"
                    .to_owned(),
            );
            findings.push(f);
            continue;
        }

        let mut source_ids = provenance.source_ids.clone();
        source_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for source_id in source_ids {
            let expected_path = source_id.as_str();
            let Some(artifact) = inputs.source_artifacts.get(&source_id) else {
                findings.push(dangling(
                    &record.stored.record.id,
                    source_id.as_str(),
                    expected_path,
                    "source artifact not present in lint snapshot",
                ));
                continue;
            };

            if source_forgotten && inputs.config.vault.source.redact_on_forget {
                match &artifact.state {
                    SourceArtifactState::Redacted { .. } => {}
                    _ => findings.push(redaction_skipped(
                        &record.stored.record.id,
                        source_id.as_str(),
                        &artifact.path,
                    )),
                }
            }

            match &artifact.state {
                SourceArtifactState::Redacted { original_sha256 } => {
                    if !source_forgotten {
                        findings.push(hash_mismatch(
                            &record.stored.record.id,
                            source_id.as_str(),
                            &artifact.path,
                            &provenance.source_hash,
                            original_sha256,
                        ));
                    }
                }
                SourceArtifactState::Missing => {
                    findings.push(dangling(
                        &record.stored.record.id,
                        source_id.as_str(),
                        &artifact.path,
                        "source artifact is missing",
                    ));
                }
                SourceArtifactState::Unreadable { message } => {
                    findings.push(dangling(
                        &record.stored.record.id,
                        source_id.as_str(),
                        &artifact.path,
                        &format!("source artifact is unreadable: {message}"),
                    ));
                }
                SourceArtifactState::Present { sha256 } => {
                    if sha256 != &provenance.source_hash {
                        findings.push(hash_mismatch(
                            &record.stored.record.id,
                            source_id.as_str(),
                            &artifact.path,
                            &provenance.source_hash,
                            sha256,
                        ));
                    }
                }
            }
        }
    }
    findings
}

fn target_id_hash(target_id: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

fn dangling(
    record_id: &crate::domain::RecordId,
    source_id: &str,
    path: &str,
    detail: &str,
) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "record {} source_id `{source_id}` does not resolve at `{path}`: {detail}",
            record_id.as_str()
        ),
    );
    f.target = Some(target_record(record_id));
    f.entities = Some(vec![source_id.to_owned()]);
    f.suggested_fix = Some("restore the source artifact or re-ingest the record".to_owned());
    f
}

fn hash_mismatch(
    record_id: &crate::domain::RecordId,
    source_id: &str,
    path: &str,
    expected: &str,
    actual: &str,
) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "record {} source_id `{source_id}` hash mismatch at `{path}`: expected `{expected}`, got `{actual}`",
            record_id.as_str()
        ),
    );
    f.target = Some(target_record(record_id));
    f.entities = Some(vec![source_id.to_owned()]);
    f.suggested_fix = Some(
        "restore the immutable source bytes or re-ingest so provenance matches the source artifact"
            .to_owned(),
    );
    f
}

fn redaction_skipped(record_id: &crate::domain::RecordId, source_id: &str, path: &str) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "record {} source_id `{source_id}` was forgotten but `{path}` was not redacted",
            record_id.as_str()
        ),
    );
    f.target = Some(target_path(path));
    f.entities = Some(vec![record_id.as_str().to_owned(), source_id.to_owned()]);
    f.suggested_fix = Some(
        "rewrite the source artifact to the Cairn redaction marker or re-run forget with redaction enabled"
            .to_owned(),
    );
    f
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::contract::version::SchemaVersion;
    use crate::domain::{SourceId, record::tests_export::sample_record};
    use crate::verbs::lint::{ConsentModel, LintRecord, SourceArtifact, SourceForgetLedger};

    fn lint_record() -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record: sample_record(),
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn inputs<'a>(
        records: &'a [LintRecord],
        source_artifacts: &'a HashMap<SourceId, SourceArtifact>,
    ) -> LintInputs<'a> {
        let cfg = Box::leak(Box::new(CairnConfig::default()));
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts,
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
        }
    }

    #[test]
    fn missing_source_ids_emits_error() {
        let mut r = lint_record();
        r.stored.record.provenance.source_ids.clear();
        let records = [r];
        let findings = run(&inputs(
            &records,
            crate::verbs::lint::empty_source_artifacts(),
        ));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(
            findings[0]
                .message
                .contains("missing provenance.source_ids")
        );
    }

    #[test]
    fn missing_source_artifact_emits_error() {
        let r = lint_record();
        let records = [r];
        let findings = run(&inputs(
            &records,
            crate::verbs::lint::empty_source_artifacts(),
        ));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("does not resolve"));
    }

    #[test]
    fn hash_mismatch_emits_error() {
        let r = lint_record();
        let source_id = r.stored.record.provenance.source_ids[0].clone();
        let records = [r];
        let source_artifacts = HashMap::from([(
            source_id,
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: format!("sha256:{}", "b".repeat(64)),
                },
            },
        )]);
        let findings = run(&inputs(&records, &source_artifacts));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("hash mismatch"));
    }

    #[test]
    fn forgotten_source_hash_emits_error() {
        let r = lint_record();
        let source_hash = r.stored.record.provenance.source_hash.clone();
        let target_hash = target_id_hash(r.stored.record.target_id.as_str());
        let source_id = r.stored.record.provenance.source_ids[0].clone();
        let records = [r];
        let source_artifacts = HashMap::from([(
            source_id,
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: source_hash.clone(),
                },
            },
        )]);
        let source_forgets = HashMap::from([(
            source_hash,
            SourceForgetLedger {
                forgotten_target_hashes: std::iter::once(target_hash).collect(),
            },
        )]);
        let cfg = Box::leak(Box::new(CairnConfig::default()));
        let findings = run(&LintInputs {
            records: &records,
            config: cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: &source_artifacts,
            source_forgets: &source_forgets,
            vault_root: None,
            hot_body_loader: None,
        });
        assert!(findings.iter().any(|finding| {
            finding.message.contains("forgotten source hash") && finding.severity == Severity::Error
        }));
    }

    #[test]
    fn redaction_skipped_emits_error_when_policy_enabled() {
        let r = lint_record();
        let source_hash = r.stored.record.provenance.source_hash.clone();
        let target_hash = target_id_hash(r.stored.record.target_id.as_str());
        let source_id = r.stored.record.provenance.source_ids[0].clone();
        let records = [r];
        let source_artifacts = HashMap::from([(
            source_id,
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: source_hash.clone(),
                },
            },
        )]);
        let source_forgets = HashMap::from([(
            source_hash,
            SourceForgetLedger {
                forgotten_target_hashes: std::iter::once(target_hash).collect(),
            },
        )]);
        let mut cfg = CairnConfig::default();
        cfg.vault.source.redact_on_forget = true;
        let cfg = Box::leak(Box::new(cfg));
        let findings = run(&LintInputs {
            records: &records,
            config: cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: &source_artifacts,
            source_forgets: &source_forgets,
            vault_root: None,
            hot_body_loader: None,
        });
        assert!(findings.iter().any(|finding| {
            finding.message.contains("was not redacted") && finding.severity == Severity::Error
        }));
    }
}
