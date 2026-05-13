//! Integration tests for typed provenance source identifiers.

#![allow(missing_docs)]

use cairn_core::domain::SourceId;

#[test]
fn source_id_round_trips() {
    let id = SourceId::parse("src:01HQZX9F5N0000000000000000").expect("valid");
    assert_eq!(id.as_str(), "src:01HQZX9F5N0000000000000000");

    let json = serde_json::to_string(&id).expect("ser");
    let back: SourceId = serde_json::from_str(&json).expect("de");
    assert_eq!(id, back);
}

#[test]
fn source_id_rejects_empty_values() {
    assert!(SourceId::parse("").is_err(), "empty source ids must reject");
}
