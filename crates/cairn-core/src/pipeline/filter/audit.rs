//! Body-free audit entries for blocked captures (brief §14). Stub.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{DiscardReason, RedactionTag};
use crate::domain::{
    CaptureEventId, CaptureMode, MemoryVisibility, Rfc3339Timestamp, SourceFamily,
};

/// Append-only audit row for a Filter-stage discard.
///
/// **Body-free by construction** — this struct deliberately holds no
/// payload text, no redaction substrings, and no fence content. Only
/// counts and tags. Safe to write to `.cairn/consent.log`, structured
/// logs at `info`, and metrics sinks per CLAUDE.md §6.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedAuditEntry {
    /// `CaptureEvent` that produced this discard.
    pub event_id: CaptureEventId,
    /// Source family the event came from.
    pub source_family: SourceFamily,
    /// Capture mode (auto / explicit / proactive).
    pub capture_mode: CaptureMode,
    /// Reason the Filter stage rejected the draft.
    pub reason: DiscardReason,
    /// Visibility the draft would have entered at.
    pub default_visibility: MemoryVisibility,
    /// Per-tag count of redaction detector hits. Empty when no PII fired.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub redaction_counts: HashMap<RedactionTag, u32>,
    /// Number of fence marks placed. `0` when nothing was fenced.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fence_count: u32,
    /// Timestamp the Filter stage emitted the decision.
    pub decided_at: Rfc3339Timestamp,
}

// `serde(skip_serializing_if = "...")` requires `fn(&T) -> bool`,
// hence the by-reference signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CaptureEventId, MemoryVisibility};

    fn sample() -> BlockedAuditEntry {
        let mut counts = HashMap::new();
        counts.insert(RedactionTag::Email, 2);
        counts.insert(RedactionTag::Ipv4, 1);
        BlockedAuditEntry {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            source_family: SourceFamily::Hook,
            capture_mode: CaptureMode::Auto,
            reason: DiscardReason::PiiBlocked,
            default_visibility: MemoryVisibility::Session,
            redaction_counts: counts,
            fence_count: 0,
            decided_at: Rfc3339Timestamp::parse("2026-04-27T12:00:00Z").expect("valid ts"),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let entry = sample();
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: BlockedAuditEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, entry);
    }

    #[test]
    fn snapshot_has_no_body_field() {
        // Allowlist of every field name `BlockedAuditEntry` is allowed to
        // emit. Anything outside this set risks being a body-bearing field.
        const ALLOWED: &[&str] = &[
            "event_id",
            "source_family",
            "capture_mode",
            "reason",
            "default_visibility",
            "redaction_counts",
            "fence_count",
            "decided_at",
        ];
        const BANNED: &[&str] = &[
            "body",
            "text",
            "payload",
            "raw",
            "content",
            "input",
            "snippet",
            "command",
            "url",
            "title",
            "file_path",
        ];
        let v: serde_json::Value = serde_json::to_value(sample()).expect("serialize");
        let obj = v.as_object().expect("object");
        for k in obj.keys() {
            assert!(ALLOWED.contains(&k.as_str()), "unexpected field `{k}`");
            assert!(
                !BANNED.contains(&k.as_str()),
                "banned body-bearing field `{k}`"
            );
        }
    }

    #[test]
    fn deny_unknown_fields_on_deserialize() {
        let bad = r#"{
            "event_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "source_family":"hook",
            "capture_mode":"auto",
            "reason":"pii_blocked",
            "default_visibility":"session",
            "decided_at":"2026-04-27T12:00:00Z",
            "body":"this should not be accepted"
        }"#;
        assert!(serde_json::from_str::<BlockedAuditEntry>(bad).is_err());
    }

    #[test]
    fn fence_count_and_redaction_counts_omitted_when_zero_or_empty() {
        let entry = BlockedAuditEntry {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            source_family: SourceFamily::Cli,
            capture_mode: CaptureMode::Explicit,
            reason: DiscardReason::Duplicate,
            default_visibility: MemoryVisibility::Private,
            redaction_counts: HashMap::new(),
            fence_count: 0,
            decided_at: Rfc3339Timestamp::parse("2026-04-27T12:00:00Z").expect("valid ts"),
        };
        let s = serde_json::to_string(&entry).expect("serialize");
        assert!(!s.contains("\"redaction_counts\""), "{s}");
        assert!(!s.contains("\"fence_count\""), "{s}");
    }
}
