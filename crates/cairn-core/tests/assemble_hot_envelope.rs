//! Trust-boundary integration tests for `assemble_hot`. Pin the
//! invariant that validation runs inside `Deserialize` itself, so
//! every code path (envelope decode, direct `serde_json::from_str`,
//! MCP, SDK, tests) cannot bypass it.

use cairn_core::generated::verbs::assemble_hot::AssembleHotData;

#[test]
fn envelope_decode_rejects_malformed_bytes() {
    // Legacy-shape payload (no segments) with bytes != prefix.len().
    let json = r#"{"bytes": 5, "prefix": "abc"}"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("bytes") || msg.contains("mismatch"),
        "got: {}",
        err
    );
}

#[test]
fn envelope_decode_accepts_legacy_well_formed() {
    let json = r#"{"bytes": 3, "prefix": "abc"}"#;
    let data: AssembleHotData = serde_json::from_str(json).unwrap();
    assert_eq!(data.segments, None);
}

#[test]
fn envelope_decode_round_trips_canonical_empty() {
    let json = r#"{"bytes": 0, "prefix": "", "segments": []}"#;
    let data: AssembleHotData = serde_json::from_str(json).unwrap();
    assert_eq!(data.segments, Some(vec![]));
    let re = serde_json::to_string(&data).unwrap();
    assert!(re.contains("\"segments\":[]"), "got: {}", re);
}

#[test]
fn envelope_decode_rejects_empty_segments_with_non_empty_prefix() {
    let json = r#"{"bytes": 3, "prefix": "abc", "segments": []}"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("empty") || msg.contains("prefix"),
        "got: {}",
        err
    );
}

#[test]
fn envelope_decode_rejects_too_many_segments() {
    // 65 zero-length segments with sha256("") = e3b0c44...
    let mut segments = String::from("[");
    for i in 0..65 {
        if i > 0 {
            segments.push(',');
        }
        segments.push_str(r#"{"step":"purpose","byte_start":0,"byte_end":0,"stability":"stable_1h","content_hash":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}"#);
    }
    segments.push(']');
    let json = format!(r#"{{"bytes": 0, "prefix": "", "segments": {}}}"#, segments);
    let err = serde_json::from_str::<AssembleHotData>(&json).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("64") || msg.to_lowercase().contains("too many"),
        "got: {}",
        err
    );
}

#[test]
fn envelope_decode_rejects_stability_mismatch() {
    // Purpose with stability=volatile (should be stable_1h).
    let json = r#"{
        "bytes": 0,
        "prefix": "",
        "segments": [{
            "step": "purpose",
            "byte_start": 0,
            "byte_end": 0,
            "stability": "volatile",
            "content_hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }]
    }"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(msg.contains("stability"), "got: {}", err);
}
