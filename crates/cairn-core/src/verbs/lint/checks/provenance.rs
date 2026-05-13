//! §6.3 — `missing_provenance` check.
//!
//! Source-link hygiene over `Provenance`.

use std::collections::{HashMap, HashSet};

use crate::contract::consent_journal::{
    MalformedSourceForget, MalformedSourceForgetReason, SourceForget,
};
use crate::contract::source_resolver::SourceResolverError;
use crate::domain::SourceRef;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::pipeline::canonical::replay_hash;
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_path, target_record};

/// Runs the §6.3 provenance checks.
#[must_use]
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

    if inputs.config.source.redact_on_forget {
        check_redact_on_forget(record, inputs, source_forgets, &source_hashes, findings);
    }

    check_target_scope_forget(record, inputs, source_forgets, findings);
    check_legacy_duplicates(record, inputs, &source_hashes, findings);
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
            // Unsupported version on disk — already surfaced via the
            // malformed-row path; do not double-emit here.
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
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::consent_journal::{
        ConsentJournalReader, MalformedSourceForget, MalformedSourceForgetReason, SourceForget,
    };
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::contract::source_resolver::{SourceResolver, SourceResolverError};
    use crate::domain::RecordId;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::{MemoryVisibility, Provenance, SourceRef};
    use crate::generated::verbs::lint::{Kind, Severity};
    use crate::verbs::lint::{
        ConsentModel, LintInputs, LintRecord, empty_author_states, empty_unresolvable_authors,
    };
    use std::collections::HashMap;

    struct StaticResolver {
        by_id: HashMap<String, Vec<u8>>,
    }

    struct StaticJournal {
        forgotten: std::collections::HashSet<String>,
        source_forgets: Vec<SourceForget>,
        malformed: Vec<MalformedSourceForget>,
    }

    impl ConsentJournalReader for StaticJournal {
        fn forgotten_source_bytes_hashes(&self) -> std::collections::HashSet<String> {
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
                schema_version: None,
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn sample_provenance() -> Provenance {
        Provenance {
            source_sensor: crate::domain::Identity::parse("snr:local:hook:cc-session:v1")
                .expect("valid sensor"),
            created_at: crate::domain::Rfc3339Timestamp::parse("2026-05-10T12:00:00Z")
                .expect("valid ts"),
            originating_agent_id: crate::domain::Identity::parse("hmn:tafeng").expect("valid id"),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
            source_refs: vec![SourceRef {
                id: "sources/chat/session-1.md".to_owned(),
                hash: format!("sha256:{}", "a".repeat(64)),
            }],
        }
    }

    #[test]
    fn emits_source_link_missing_when_source_refs_empty() {
        let cfg = CairnConfig::default();
        let mut provenance = sample_provenance();
        provenance.source_refs.clear();
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::SourceLinkMissing);
        // Warning during rollout — see comment in `check_record`.
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn emits_source_link_dangling_when_resolver_cannot_find_source() {
        let cfg = CairnConfig::default();
        let record = lint_record_with_provenance(sample_provenance());
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::SourceLinkDangling);
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("sources/chat/session-1.md"));
        assert!(f[0].message.contains("memory:sources/chat/session-1.md"));
    }

    #[test]
    fn emits_source_hash_mismatch_when_bytes_do_not_match_provenance_hash() {
        let cfg = CairnConfig::default();
        let record = lint_record_with_provenance(sample_provenance());
        let resolver = StaticResolver {
            by_id: HashMap::from([(
                "sources/chat/session-1.md".to_owned(),
                b"source bytes that do not hash to aaaa".to_vec(),
            )]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::SourceHashMismatch);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn emits_source_after_forget_when_source_hash_was_forgotten() {
        let cfg = CairnConfig::default();
        let record = lint_record_with_provenance(sample_provenance());
        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), b"aaaaaaaa".to_vec())]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::from([format!("sha256:{}", "a".repeat(64))]),
            source_forgets: vec![SourceForget {
                op_id: "forget-op-1".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: format!("sha256:{}", "a".repeat(64)),
                target: None,
            }],
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let f = run(&inputs);
        assert_eq!(f.len(), 2);
        assert!(f.iter().any(|x| x.kind == Kind::SourceAfterForget));
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(bytes);
        format!("sha256:{digest:x}")
    }

    #[test]
    fn emits_source_after_forget_unknown_version_for_matching_malformed_row() {
        let cfg = CairnConfig::default();
        let bytes = b"source bytes".to_vec();
        let hash = sha256(&bytes);
        let mut provenance = sample_provenance();
        provenance.source_hash = hash.clone();
        provenance.source_refs[0].hash = hash.clone();
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: vec![MalformedSourceForget {
                op_id: "forget-op-unknown".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: Some(hash),
                reason: MalformedSourceForgetReason::UnsupportedReplayHashVersion { version: 9 },
            }],
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let findings = run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == Kind::SourceAfterForgetUnknownVersion
                    && f.message.contains("forget-op-unknown"))
        );
    }

    #[test]
    fn emits_source_redact_skipped_when_redact_on_forget_enabled_but_full_bytes_remain() {
        let mut cfg = CairnConfig::default();
        cfg.source.redact_on_forget = true;
        let bytes = b"full source bytes still present".to_vec();
        let hash = sha256(&bytes);
        let mut provenance = sample_provenance();
        provenance.source_hash = hash.clone();
        provenance.source_refs[0].hash = hash.clone();
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::from([hash.clone()]),
            source_forgets: vec![SourceForget {
                op_id: "forget-op-redact".to_owned(),
                source_id: "sources/chat/session-1.md".to_owned(),
                source_bytes_hash: hash,
                target: None,
            }],
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let findings = run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == Kind::SourceRedactSkipped
                    && f.message.contains("forget-op-redact"))
        );
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
        let mut modern = lint_record_with_provenance(modern_provenance);
        modern.stored.record.id = RecordId::parse("01HQZX9F5N0000000000000BBB").expect("valid id");

        let resolver = StaticResolver {
            by_id: HashMap::from([("sources/chat/session-1.md".to_owned(), bytes)]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let records = vec![legacy, modern];
        let inputs = LintInputs {
            records: &records,
            config: &cfg,
            index_stats: IndexStats::new(2, 2),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let findings = run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == Kind::SourceLinkLegacyDuplicate)
        );
    }

    #[test]
    fn emits_source_after_forget_for_target_scope_replay_hash_match() {
        use crate::contract::TargetReplayKey;
        use crate::pipeline::canonical::replay_hash;

        let cfg = CairnConfig::default();
        let record = lint_record_with_provenance(sample_provenance());
        let replay = replay_hash::compute(&record.stored.record, 1).expect("v1");
        let resolver = StaticResolver {
            by_id: HashMap::from([(
                "sources/chat/session-1.md".to_owned(),
                b"unrelated bytes".to_vec(),
            )]),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
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
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let findings = run(&inputs);
        assert!(
            findings.iter().any(|f| f.kind == Kind::SourceAfterForget
                && f.message.contains("target-scope forget")
                && f.message.contains("forget-op-target")),
            "findings: {findings:?}"
        );
    }

    #[test]
    fn finding_shape_snapshot_for_source_link_missing() {
        let cfg = CairnConfig::default();
        let mut provenance = sample_provenance();
        provenance.source_refs.clear();
        let record = lint_record_with_provenance(provenance);
        let resolver = StaticResolver {
            by_id: HashMap::new(),
        };
        let journal = StaticJournal {
            forgotten: std::collections::HashSet::new(),
            source_forgets: Vec::new(),
            malformed: Vec::new(),
        };
        let inputs = LintInputs {
            records: std::slice::from_ref(&record),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            source_resolver: &resolver,
            consent_journal: &journal,
        };

        let mut findings = run(&inputs);
        // Strip the record-id from the snapshot — it's a ULID and
        // varies between sample_record invocations.
        for f in &mut findings {
            if let Some(t) = f.target.as_mut() {
                t.record_id = None;
            }
        }
        insta::assert_yaml_snapshot!("finding_shape_source_link_missing", findings);
    }
}
