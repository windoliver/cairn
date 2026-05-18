//! Snapshot the canonical JSON shape of `SummarizeData`.
//!
//! This pins the stable `cairn summarize --json` data payload shape for
//! issue #312 without depending on a live vault or keychain-backed CLI
//! fixture.

use cairn_core::generated::common::Ulid;
use cairn_core::generated::verbs::summarize::{
    ConceptKind, ConfidenceTag, SummarizeData, SummaryConcept, SummaryFact,
};

#[test]
fn summarize_data_canonical_json() {
    let data = SummarizeData {
        concepts: vec![
            SummaryConcept {
                kind: ConceptKind::Entity,
                name: "Alpha".to_owned(),
                salience: 1.0,
            },
            SummaryConcept {
                kind: ConceptKind::Topic,
                name: "project".to_owned(),
                salience: 0.5,
            },
        ],
        digest: "Alpha project status".to_owned(),
        facts: vec![SummaryFact {
            confidence: ConfidenceTag::Extracted,
            object: "Alpha detail for the project".to_owned(),
            predicate: "states".to_owned(),
            source_record_ids: vec![Ulid("01HQZX9F5N0000000000000001".to_owned())],
            subject: "Alpha".to_owned(),
        }],
        narrative: "Alpha is the current project focus.".to_owned(),
        persisted_record_id: None,
    };

    insta::assert_snapshot!(serde_json::to_string_pretty(&data).unwrap());
}
