//! §6.5 — provenance source-link hygiene checks.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::contract::consent_journal::{
    MalformedSourceForget, MalformedSourceForgetReason, SourceForget,
};
use crate::contract::source_resolver::SourceResolverError;
use crate::domain::{SourceId, SourceRef};
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::pipeline::canonical::replay_hash;
use crate::verbs::lint::{
    LintInputs, LintRecord, SourceArtifactState, finding, target_path, target_record,
};

/// Emit source-link hygiene findings for every active record.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let source_forgets = inputs.consent_journal.forgotten_source_forgets();
    let mut source_forgets_by_hash: HashMap<&str, Vec<&SourceForget>> = HashMap::new();
    for source_forget in &source_forgets {
        source_forgets_by_hash
            .entry(source_forget.source_bytes_hash.as_str())
            .or_default()
            .push(source_forget);
    }
    let mut surfaced_malformed = HashSet::new();

    for record in inputs.records {
        check_record(
            record,
            inputs,
            &source_forgets,
            &source_forgets_by_hash,
            &mut surfaced_malformed,
            &mut findings,
        );
    }

    for malformed in inputs.consent_journal.malformed_source_forget_rows() {
        if surfaced_malformed.contains(&malformed_key(&malformed)) {
            continue;
        }
        let mut f = finding(
            Kind::SourceAfterForgetUnknownVersion,
            Severity::Error,
            malformed_message(&malformed),
        );
        f.target = Some(target_path("consent_journal"));
        findings.push(f);
    }

    findings
}

fn check_record(
    record: &LintRecord,
    inputs: &LintInputs<'_>,
    source_forgets: &[SourceForget],
    source_forgets_by_hash: &HashMap<&str, Vec<&SourceForget>>,
    surfaced_malformed: &mut HashSet<String>,
    findings: &mut Vec<Finding>,
) {
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

    check_source_ids(record, inputs, source_forgotten, findings);

    if provenance.source_refs.is_empty() {
        // Severity = Warning during rollout: ingest, capture_trace,
        // turn, and folder-ingest planner all still construct
        // records with empty `source_refs`. Emitting Error here
        // would make lint permanently red on records the product
        // still legitimately produces. Escalate to Error once every
        // active writer is on the populated path (tracking: spec
        // Component 4 rollout, follow-up to #257).
        let mut f = finding(
            Kind::SourceLinkMissing,
            Severity::Warning,
            "record provenance has no source_refs",
        );
        f.target = Some(target_record(&record.stored.record.id));
        findings.push(f);
        return;
    }

    let source_hashes: HashSet<&str> = provenance
        .source_refs
        .iter()
        .map(|source_ref| source_ref.hash.as_str())
        .collect();

    for source_ref in &provenance.source_refs {
        check_source_ref(
            record,
            source_ref,
            inputs,
            source_forgets_by_hash,
            surfaced_malformed,
            findings,
        );
    }

    if redact_on_forget_enabled(inputs) {
        check_redact_on_forget(record, inputs, source_forgets, &source_hashes, findings);
    }

    check_target_scope_forget(record, inputs, source_forgets, findings);
    check_legacy_duplicates(record, inputs, &source_hashes, findings);
}

fn check_source_ids(
    record: &LintRecord,
    inputs: &LintInputs<'_>,
    source_forgotten: bool,
    findings: &mut Vec<Finding>,
) {
    let provenance = &record.stored.record.provenance;
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
        return;
    }

    let mut source_ids = provenance.source_ids.clone();
    source_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for source_id in &source_ids {
        check_source_artifact(record, source_id, inputs, source_forgotten, findings);
    }
}

fn check_source_artifact(
    record: &LintRecord,
    source_id: &SourceId,
    inputs: &LintInputs<'_>,
    source_forgotten: bool,
    findings: &mut Vec<Finding>,
) {
    let provenance = &record.stored.record.provenance;
    let expected_path = source_id.as_str();
    let Some(artifact) = inputs.source_artifacts.get(source_id) else {
        findings.push(dangling(
            &record.stored.record.id,
            source_id.as_str(),
            expected_path,
            "source artifact not present in lint snapshot",
        ));
        return;
    };

    if source_forgotten && redact_on_forget_enabled(inputs) {
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

fn check_target_scope_forget(
    record: &LintRecord,
    inputs: &LintInputs<'_>,
    source_forgets: &[SourceForget],
    findings: &mut Vec<Finding>,
) {
    let versions = inputs.consent_journal.forgotten_target_replay_versions();
    for v in versions {
        let Some(computed) = replay_hash::compute(&record.stored.record, v) else {
            continue;
        };
        for row in source_forgets {
            let Some(target) = row.target.as_ref() else {
                continue;
            };
            if target.version == v && target.hash == computed {
                let mut f = finding(
                    Kind::SourceAfterForget,
                    Severity::Error,
                    format!(
                        "record matches target-scope forget under replay-hash v{v} (source `{}`, op `{}`)",
                        row.source_id, row.op_id
                    ),
                );
                f.target = Some(target_record(&record.stored.record.id));
                findings.push(f);
            }
        }
    }
}

fn check_source_ref(
    record: &LintRecord,
    source_ref: &SourceRef,
    inputs: &LintInputs<'_>,
    source_forgets_by_hash: &HashMap<&str, Vec<&SourceForget>>,
    surfaced_malformed: &mut HashSet<String>,
    findings: &mut Vec<Finding>,
) {
    for malformed in inputs
        .consent_journal
        .malformed_source_forget_rows_for_source(&source_ref.hash)
    {
        surfaced_malformed.insert(malformed_key(&malformed));
        let mut f = finding(
            Kind::SourceAfterForgetUnknownVersion,
            Severity::Error,
            malformed_message(&malformed),
        );
        f.target = Some(target_record(&record.stored.record.id));
        findings.push(f);
    }

    if let Some(rows) = source_forgets_by_hash.get(source_ref.hash.as_str()) {
        for row in rows {
            let mut f = finding(
                Kind::SourceAfterForget,
                Severity::Error,
                format!(
                    "record source_ref `{}` points at forgotten source `{}` (op `{}`)",
                    source_ref.id, row.source_id, row.op_id
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            findings.push(f);
        }
    }
    let locator = inputs.source_resolver.locator(&source_ref.id);
    let bytes = match inputs.source_resolver.read(&source_ref.id) {
        Ok(bytes) => bytes,
        Err(SourceResolverError::NotFound) => {
            let mut f = finding(
                Kind::SourceLinkDangling,
                Severity::Error,
                format!(
                    "record source_ref `{}` does not resolve (expected `{locator}`)",
                    source_ref.id
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            findings.push(f);
            return;
        }
        Err(SourceResolverError::Io { detail }) => {
            let mut f = finding(
                Kind::SourceLinkDangling,
                Severity::Error,
                format!(
                    "record source_ref `{}` could not be read at `{locator}`: {detail}",
                    source_ref.id
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            findings.push(f);
            return;
        }
    };

    let Some(actual_hash) = recompute_hash(&bytes, &source_ref.hash) else {
        return;
    };
    if actual_hash != source_ref.hash {
        let mut f = finding(
            Kind::SourceHashMismatch,
            Severity::Error,
            format!(
                "record source_ref `{}` hash mismatch: expected `{}`, got `{actual_hash}`",
                source_ref.id, source_ref.hash
            ),
        );
        f.target = Some(target_record(&record.stored.record.id));
        findings.push(f);
    }
}

fn check_redact_on_forget(
    record: &LintRecord,
    inputs: &LintInputs<'_>,
    source_forgets: &[SourceForget],
    source_hashes: &HashSet<&str>,
    findings: &mut Vec<Finding>,
) {
    for source_forget in source_forgets
        .iter()
        .filter(|row| source_hashes.contains(row.source_bytes_hash.as_str()))
    {
        let Ok(bytes) = inputs.source_resolver.read(&source_forget.source_id) else {
            continue;
        };
        if recompute_hash(&bytes, &source_forget.source_bytes_hash).as_deref()
            == Some(source_forget.source_bytes_hash.as_str())
        {
            let mut f = finding(
                Kind::SourceRedactSkipped,
                Severity::Error,
                format!(
                    "forgotten source `{}` still retains original bytes (op `{}`)",
                    source_forget.source_id, source_forget.op_id
                ),
            );
            f.target = Some(target_record(&record.stored.record.id));
            findings.push(f);
        }
    }
}

fn check_legacy_duplicates(
    record: &LintRecord,
    inputs: &LintInputs<'_>,
    source_hashes: &HashSet<&str>,
    findings: &mut Vec<Finding>,
) {
    for other in inputs.records.iter().filter(|other| {
        other.stored.record.id != record.stored.record.id
            && other.stored.record.provenance.source_refs.is_empty()
            && source_hashes.contains(other.stored.record.provenance.source_hash.as_str())
    }) {
        let mut f = finding(
            Kind::SourceLinkLegacyDuplicate,
            Severity::Error,
            format!(
                "record shares source hash `{}` with legacy record `{}` whose source_refs are empty",
                other.stored.record.provenance.source_hash,
                other.stored.record.id.as_str()
            ),
        );
        f.target = Some(target_record(&record.stored.record.id));
        findings.push(f);
    }
}

fn target_id_hash(target_id: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

fn redact_on_forget_enabled(inputs: &LintInputs<'_>) -> bool {
    inputs.config.source.redact_on_forget || inputs.config.vault.source.redact_on_forget
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

fn malformed_key(row: &MalformedSourceForget) -> String {
    format!(
        "{}|{}|{:?}|{:?}",
        row.op_id, row.source_id, row.source_bytes_hash, row.reason
    )
}

fn malformed_message(row: &MalformedSourceForget) -> String {
    let reason = match &row.reason {
        MalformedSourceForgetReason::UnsupportedReplayHashVersion { version } => {
            format!("unsupported replay-hash version `{version}`")
        }
        MalformedSourceForgetReason::MalformedReplayHashFormat => {
            "malformed replay-hash format".to_owned()
        }
        MalformedSourceForgetReason::MalformedSourceBytesHashFormat => {
            "malformed source-bytes hash format".to_owned()
        }
    };
    format!(
        "source_forget row for source `{}` (op `{}`) is malformed: {reason}",
        row.source_id, row.op_id
    )
}

fn recompute_hash(bytes: &[u8], expected: &str) -> Option<String> {
    let (algo, _) = expected.split_once(':')?;
    let hex = match algo {
        "sha256" => {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(bytes);
            format!("{digest:x}")
        }
        "sha512" => {
            use sha2::Digest as _;
            let digest = sha2::Sha512::digest(bytes);
            format!("{digest:x}")
        }
        "blake3" => blake3::hash(bytes).to_hex().to_string(),
        _ => return None,
    };
    Some(format!("{algo}:{hex}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::consent_journal::{
        ConsentJournalReader, MalformedSourceForget, MalformedSourceForgetReason, SourceForget,
        TargetReplayKey,
    };
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::contract::source_resolver::{SourceResolver, SourceResolverError};
    use crate::contract::version::SchemaVersion;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::{MemoryVisibility, Provenance, RecordId};
    use crate::verbs::lint::{
        ConsentModel, LintRecord, SourceArtifact, SourceForgetLedger, empty_author_states,
        empty_unresolvable_authors,
    };

    struct StaticResolver {
        by_id: HashMap<String, Vec<u8>>,
    }

    struct StaticJournal {
        forgotten: HashSet<String>,
        source_forgets: Vec<SourceForget>,
        malformed: Vec<MalformedSourceForget>,
    }

    impl ConsentJournalReader for StaticJournal {
        fn forgotten_source_bytes_hashes(&self) -> HashSet<String> {
            self.forgotten.clone()
        }

        fn forgotten_source_forgets(&self) -> Vec<SourceForget> {
            self.source_forgets.clone()
        }

        fn malformed_source_forget_rows(&self) -> Vec<MalformedSourceForget> {
            self.malformed.clone()
        }

        fn malformed_source_forget_rows_for_source(
            &self,
            source_bytes_hash: &str,
        ) -> Vec<MalformedSourceForget> {
            self.malformed
                .iter()
                .filter(|row| row.source_bytes_hash.as_deref() == Some(source_bytes_hash))
                .cloned()
                .collect()
        }
    }

    impl SourceResolver for StaticResolver {
        fn exists(&self, id: &str) -> bool {
            self.by_id.contains_key(id)
        }

        fn read(&self, id: &str) -> Result<Vec<u8>, SourceResolverError> {
            self.by_id
                .get(id)
                .cloned()
                .ok_or(SourceResolverError::NotFound)
        }

        fn locator(&self, id: &str) -> String {
            format!("memory:{id}")
        }
    }

    fn lint_record_with_provenance(provenance: Provenance) -> LintRecord {
        let mut record = sample_record();
        record.visibility = MemoryVisibility::Private;
        record.provenance = provenance;
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn sample_provenance() -> Provenance {
        let source_id =
            crate::domain::SourceId::parse("01HQZX9F5N0000000000000000").expect("valid");
        let source_hash = format!("sha256:{}", "a".repeat(64));
        Provenance {
            source_sensor: crate::domain::Identity::parse("snr:local:hook:cc-session:v1")
                .expect("valid sensor"),
            created_at: crate::domain::Rfc3339Timestamp::parse("2026-05-10T12:00:00Z")
                .expect("valid ts"),
            originating_agent_id: crate::domain::Identity::parse("hmn:tafeng").expect("valid id"),
            source_ids: vec![source_id.clone()],
            source_hash: source_hash.clone(),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
            source_refs: vec![SourceRef {
                id: "sources/chat/session-1.md".to_owned(),
                hash: source_hash,
            }],
        }
    }

    fn source_artifacts_for(
        provenance: &Provenance,
    ) -> HashMap<crate::domain::SourceId, SourceArtifact> {
        HashMap::from([(
            provenance.source_ids[0].clone(),
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: provenance.source_hash.clone(),
                },
            },
        )])
    }

    fn lint_inputs<'a>(
        cfg: &'a CairnConfig,
        records: &'a [LintRecord],
        source_artifacts: &'a HashMap<crate::domain::SourceId, SourceArtifact>,
        source_forgets: &'a HashMap<String, SourceForgetLedger>,
        resolver: &'a StaticResolver,
        journal: &'a StaticJournal,
    ) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts,
            source_forgets,
            vault_root: None,
            hot_body_loader: None,
            source_resolver: resolver,
            consent_journal: journal,
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(bytes);
        format!("sha256:{digest:x}")
    }

    #[test]
    fn missing_source_ids_emits_error() {
        let cfg = CairnConfig::default();
        let mut provenance = sample_provenance();
        provenance.source_ids.clear();
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            crate::verbs::lint::empty_source_artifacts(),
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::MissingProvenance && f.message.contains("missing provenance.source_ids")
        }));
    }

    #[test]
    fn emits_source_link_missing_when_source_refs_empty() {
        let cfg = CairnConfig::default();
        let mut provenance = sample_provenance();
        provenance.source_refs.clear();
        let source_artifacts = source_artifacts_for(&provenance);
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| f.kind == Kind::SourceLinkMissing));
    }

    #[test]
    fn missing_source_artifact_emits_error() {
        let cfg = CairnConfig::default();
        let record = lint_record_with_provenance(sample_provenance());
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            crate::verbs::lint::empty_source_artifacts(),
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::MissingProvenance && f.message.contains("does not resolve")
        }));
    }

    #[test]
    fn emits_source_link_dangling_when_resolver_cannot_find_source() {
        let cfg = CairnConfig::default();
        let provenance = sample_provenance();
        let source_artifacts = source_artifacts_for(&provenance);
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::SourceLinkDangling
                && f.message.contains("sources/chat/session-1.md")
                && f.message.contains("memory:sources/chat/session-1.md")
        }));
    }

    #[test]
    fn hash_mismatch_emits_errors_for_artifact_and_source_ref() {
        let cfg = CairnConfig::default();
        let provenance = sample_provenance();
        let mut source_artifacts = source_artifacts_for(&provenance);
        source_artifacts.insert(
            provenance.source_ids[0].clone(),
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: format!("sha256:{}", "b".repeat(64)),
                },
            },
        );
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::from([(
                "sources/chat/session-1.md".to_owned(),
                b"source bytes that do not hash to aaaa".to_vec(),
            )]),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| f.kind == Kind::MissingProvenance));
        assert!(findings.iter().any(|f| f.kind == Kind::SourceHashMismatch));
    }

    #[test]
    fn emits_source_after_forget_and_redaction_findings() {
        let mut cfg = CairnConfig::default();
        cfg.source.redact_on_forget = true;
        cfg.vault.source.redact_on_forget = true;
        let bytes = b"full source bytes still present".to_vec();
        let hash = sha256(&bytes);
        let mut provenance = sample_provenance();
        provenance.source_hash = hash.clone();
        provenance.source_refs[0].hash = hash.clone();
        let source_artifacts = HashMap::from([(
            provenance.source_ids[0].clone(),
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: hash.clone(),
                },
            },
        )]);
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let source_forgets = HashMap::from([(
            hash.clone(),
            SourceForgetLedger {
                forgotten_target_hashes: std::iter::once(target_id_hash(
                    record.stored.record.target_id.as_str(),
                ))
                .collect(),
            },
        )]);
        let journal = StaticJournal {
            forgotten: HashSet::from([hash.clone()]),
            source_forgets: vec![SourceForget {
                op_id: "forget-op-redact".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: hash,
                target: None,
            }],
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            &source_forgets,
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| f.kind == Kind::SourceAfterForget));
        assert!(findings.iter().any(|f| f.kind == Kind::SourceRedactSkipped));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::MissingProvenance && f.message.contains("forgotten source hash")
        }));
    }

    #[test]
    fn emits_source_after_forget_unknown_version_for_matching_malformed_row() {
        let cfg = CairnConfig::default();
        let bytes = b"source bytes".to_vec();
        let hash = sha256(&bytes);
        let mut provenance = sample_provenance();
        provenance.source_hash = hash.clone();
        provenance.source_refs[0].hash = hash.clone();
        let source_artifacts = HashMap::from([(
            provenance.source_ids[0].clone(),
            SourceArtifact {
                path: "sources/cli/fixture.txt".to_owned(),
                state: SourceArtifactState::Present {
                    sha256: hash.clone(),
                },
            },
        )]);
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: vec![MalformedSourceForget {
                op_id: "forget-op-unknown".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: Some(hash),
                reason: MalformedSourceForgetReason::UnsupportedReplayHashVersion { version: 9 },
            }],
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::SourceAfterForgetUnknownVersion
                && f.message.contains("forget-op-unknown")
        }));
    }

    #[test]
    fn emits_source_link_legacy_duplicate_for_legacy_and_modern_pair() {
        let cfg = CairnConfig::default();
        let bytes = b"modern source bytes".to_vec();
        let hash = sha256(&bytes);

        let mut legacy_provenance = sample_provenance();
        legacy_provenance.source_hash = hash.clone();
        legacy_provenance.source_refs.clear();
        let mut legacy = lint_record_with_provenance(legacy_provenance);
        legacy.stored.record.id = RecordId::parse("01HQZX9F5N0000000000000AAA").expect("valid id");

        let mut modern_provenance = sample_provenance();
        modern_provenance.source_hash = hash.clone();
        modern_provenance.source_refs[0].hash = hash;
        let source_artifacts = source_artifacts_for(&modern_provenance);
        let mut modern = lint_record_with_provenance(modern_provenance);
        modern.stored.record.id = RecordId::parse("01HQZX9F5N0000000000000BBB").expect("valid id");

        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let records = vec![legacy, modern];
        let findings = run(&lint_inputs(
            &cfg,
            &records,
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(
            findings
                .iter()
                .any(|f| f.kind == Kind::SourceLinkLegacyDuplicate)
        );
    }

    #[test]
    fn emits_source_after_forget_for_target_scope_replay_hash_match() {
        let cfg = CairnConfig::default();
        let provenance = sample_provenance();
        let source_artifacts = source_artifacts_for(&provenance);
        let record = lint_record_with_provenance(provenance);
        let replay = replay_hash::compute(&record.stored.record, 1).expect("v1");
        let resolver = StaticResolver {
            by_id: HashMap::from([(
                "sources/chat/session-1.md".to_owned(),
                b"unrelated bytes".to_vec(),
            )]),
        };
        let journal = StaticJournal {
            forgotten: HashSet::new(),
            source_forgets: vec![SourceForget {
                op_id: "forget-op-target".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: format!("sha256:{}", "b".repeat(64)),
                target: Some(TargetReplayKey {
                    hash: replay,
                    version: 1,
                }),
            }],
            malformed: Vec::new(),
        };
        let findings = run(&lint_inputs(
            &cfg,
            std::slice::from_ref(&record),
            &source_artifacts,
            crate::verbs::lint::empty_source_forgets(),
            &resolver,
            &journal,
        ));
        assert!(findings.iter().any(|f| {
            f.kind == Kind::SourceAfterForget
                && f.message.contains("target-scope forget")
                && f.message.contains("forget-op-target")
        }));
    }
}
