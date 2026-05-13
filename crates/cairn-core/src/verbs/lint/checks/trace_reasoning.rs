//! Reasoning-signature contradiction detection for structured trace blocks.
//!
//! Issue #290 requires lint to flag cases where the same
//! `(session_id, block_index, signature)` tuple appears with mutated
//! reasoning text. Signatures are opaque provider values; Cairn only
//! checks for internal self-contradiction across stored rows.

use std::collections::BTreeMap;

use crate::domain::trace::TraceBlock;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_record};

#[derive(Debug, Clone)]
struct SeenReasoningBlock {
    record_id: String,
    text: String,
}

/// Run the reasoning-signature contradiction check.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut seen: BTreeMap<(String, String, usize, String), SeenReasoningBlock> = BTreeMap::new();
    let mut findings = Vec::new();

    for lint_record in inputs.records {
        let record = &lint_record.stored.record;
        let Some(blocks_value) = record.extra_frontmatter.get("trace_blocks") else {
            continue;
        };
        let Ok(blocks) = serde_json::from_value::<Vec<TraceBlock>>(blocks_value.clone()) else {
            continue;
        };
        let Some(session_id) = record
            .extra_frontmatter
            .get("trace")
            .and_then(|value| value.get("session_id"))
            .and_then(serde_json::Value::as_str)
            .or(record.scope.session_id.as_deref())
        else {
            continue;
        };

        let trace = record.extra_frontmatter.get("trace");
        let turn_id = trace
            .and_then(|value| value.get("turn_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let stored_block_index = trace
            .and_then(|value| value.get("block_index"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());

        for (array_index, block) in blocks.into_iter().enumerate() {
            let block_index = stored_block_index.unwrap_or(array_index);
            let TraceBlock::Reasoning {
                text,
                signature: Some(signature),
            } = block
            else {
                continue;
            };

            let key = (
                session_id.to_owned(),
                turn_id.to_owned(),
                block_index,
                signature.clone(),
            );
            if let Some(prior) = seen.get(&key) {
                if prior.text != text {
                    let mut conflict = finding(
                        Kind::Contradiction,
                        Severity::Error,
                        format!(
                            "reasoning signature contradiction: session_id={session_id} turn_id={turn_id} block_index={block_index} signature={signature} changed text between record `{}` and `{}`",
                            prior.record_id,
                            record.id.as_str(),
                        ),
                    );
                    conflict.target = Some(target_record(&record.id));
                    conflict.suggested_fix = Some(
                        "re-import the authoritative trace or forget the conflicting duplicate"
                            .to_owned(),
                    );
                    findings.push(conflict);
                }
                continue;
            }

            seen.insert(
                key,
                SeenReasoningBlock {
                    record_id: record.id.as_str().to_owned(),
                    text,
                },
            );
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{ConsentModel, LintInputs, LintRecord, SchemaVersion};

    fn lint_record(
        record_id: &str,
        session_id: &str,
        reasoning_text: &str,
        signature: &str,
        block_index_padding: bool,
    ) -> LintRecord {
        let mut record = sample_record();
        record.id = crate::domain::record::RecordId::parse(record_id).expect("valid record id");
        record.scope.session_id = Some(session_id.to_owned());
        record.extra_frontmatter.insert(
            "trace".to_owned(),
            serde_json::json!({
                "session_id": session_id,
                "turn_id": "trace-blocks-import-a",
                "sequence": 0,
                "capture_event_id": "01ARZ3NDEKTSV4RRFFQ69G5FAZ",
                "payload_hash": "sha256:abcd",
                "payload_ref": "sources/trace_blocks/test.json"
            }),
        );
        let mut blocks = vec![serde_json::json!({
            "kind": "reasoning",
            "text": reasoning_text,
            "signature": signature
        })];
        if block_index_padding {
            blocks.insert(0, serde_json::json!({"kind": "text", "text": "padding"}));
        }
        record
            .extra_frontmatter
            .insert("trace_blocks".to_owned(), serde_json::Value::Array(blocks));
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn inputs<'a>(records: &'a [LintRecord], cfg: &'a CairnConfig) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
        }
    }

    #[test]
    fn identical_reasoning_signature_and_text_is_clean() {
        let cfg = CairnConfig::default();
        let records = [
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "same text",
                "sig-1",
                false,
            ),
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "same text",
                "sig-1",
                false,
            ),
        ];
        assert!(run(&inputs(&records, &cfg)).is_empty());
    }

    #[test]
    fn mutated_reasoning_text_for_same_signature_is_contradiction() {
        let cfg = CairnConfig::default();
        let records = [
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "first text",
                "sig-1",
                false,
            ),
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "second text",
                "sig-1",
                false,
            ),
        ];
        let findings = run(&inputs(&records, &cfg));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::Contradiction);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("block_index=0"));
        assert!(findings[0].message.contains("sig-1"));
    }

    #[test]
    fn different_block_index_does_not_conflict() {
        let cfg = CairnConfig::default();
        let records = [
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "first text",
                "sig-1",
                false,
            ),
            lint_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "second text",
                "sig-1",
                true,
            ),
        ];
        assert!(run(&inputs(&records, &cfg)).is_empty());
    }

    #[test]
    fn different_turn_id_does_not_conflict() {
        let cfg = CairnConfig::default();
        let mut a = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "first text",
            "sig-1",
            false,
        );
        let mut b = lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "second text",
            "sig-1",
            false,
        );
        a.stored.record.extra_frontmatter.insert(
            "trace".to_owned(),
            serde_json::json!({
                "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn_id": "trace-blocks-import-a",
                "sequence": 0,
                "capture_event_id": "01ARZ3NDEKTSV4RRFFQ69G5FAZ",
                "payload_hash": "sha256:abcd",
                "payload_ref": "sources/trace_blocks/test-a.json",
                "block_index": 0
            }),
        );
        b.stored.record.extra_frontmatter.insert(
            "trace".to_owned(),
            serde_json::json!({
                "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn_id": "trace-blocks-import-b",
                "sequence": 0,
                "capture_event_id": "01ARZ3NDEKTSV4RRFFQ69G5FB0",
                "payload_hash": "sha256:efgh",
                "payload_ref": "sources/trace_blocks/test-b.json",
                "block_index": 0
            }),
        );
        assert!(run(&inputs(&[a, b], &cfg)).is_empty());
    }
}
