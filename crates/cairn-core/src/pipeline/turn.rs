//! Turn-level pure helpers for trace persistence (issue #77, spec §4 Ordering).
//!
//! `cairn-core` is store-agnostic. These helpers operate on lightweight
//! references that the CLI verb (or tests) supplies; the actual `SQLite`
//! reads happen elsewhere.

use crate::domain::capture::CaptureEvent;
use crate::domain::trace::TraceEvent;

/// One entry in a turn's ordered event sequence.
#[derive(Debug, Clone, Copy)]
pub struct OrderedEntry<'a> {
    /// The originating capture envelope.
    pub event: &'a CaptureEvent,
    /// The trace-event classification produced by `classify`.
    pub classified: TraceEvent,
}

/// Sort the union of `persisted` and `incoming` events by
/// `(captured_at, capture_event_id)`. Stable across replays — identical
/// inputs always produce the same ordering.
///
/// `persisted` represents events that already live in the store for this
/// turn (decoded back into their original `CaptureEvent`s by the caller).
/// `incoming` represents the new events being imported.
///
/// Chronological ordering uses [`crate::domain::Rfc3339Timestamp::cmp_chronological`]
/// which accounts for timezone offsets, rather than lexical string comparison.
/// Ties in timestamp are broken by `capture_event_id` ascending (lexical on
/// the ULID string, which also sorts chronologically for ULIDs generated in
/// the same millisecond).
#[must_use]
pub fn order_by_captured_at<'a>(
    persisted: &'a [OrderedEntry<'a>],
    incoming: &'a [OrderedEntry<'a>],
) -> Vec<OrderedEntry<'a>> {
    let mut all: Vec<OrderedEntry<'a>> = Vec::with_capacity(persisted.len() + incoming.len());
    all.extend_from_slice(persisted);
    all.extend_from_slice(incoming);
    all.sort_by(|x, y| {
        x.event
            .captured_at
            .cmp_chronological(&y.event.captured_at)
            .then_with(|| x.event.event_id.as_str().cmp(y.event.event_id.as_str()))
    });
    all
}

/// Assign sequences `0..N` over an already-ordered slice. Returned
/// `(sequence, entry)` pairs preserve the input order.
#[must_use]
pub fn assign_sequences<'a>(ordered: &'a [OrderedEntry<'a>]) -> Vec<(u64, OrderedEntry<'a>)> {
    ordered
        .iter()
        .enumerate()
        // `i` is a `usize`; on all supported targets `usize` fits in `u64`
        // (both 32-bit and 64-bit platforms), so the fallback is unreachable
        // in practice. `unwrap_or` avoids a bare `unwrap`.
        .map(|(i, entry)| (u64::try_from(i).unwrap_or(u64::MAX), *entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp,
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily,
        },
    };

    /// Build a minimal hook `CaptureEvent` with the given `event_id` and
    /// `captured_at`. Mirrors `pipeline::capture_trace::tests::mk_hook_event`
    /// but parameterised on both fields so ordering tests can vary them
    /// independently.
    fn mk_at(event_id: &str, captured_at: &str) -> CaptureEvent {
        let ts = Rfc3339Timestamp::parse(captured_at)
            .expect("invariant: test timestamp must be valid RFC3339");
        CaptureEvent {
            event_id: CaptureEventId::parse(event_id)
                .expect("invariant: test event_id must be a valid ULID"),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1")
                .expect("invariant: fixed sensor identity is valid"),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("invariant: fixed sensor identity is valid"),
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess".into()),
                turn_id: Some("turn".into()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
            .expect("invariant: well-formed sha256 hash"),
            payload_ref: format!("sources/hook/{event_id}.txt"),
            captured_at: ts,
            payload: CapturePayload::Hook {
                hook_name: "UserPromptSubmit".into(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[test]
    fn orders_by_captured_at_then_event_id() {
        let a = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:01Z");
        let b = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:00Z");
        let c = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAC", "2026-05-02T00:00:00Z");
        let entries: Vec<OrderedEntry<'_>> = [&a, &b, &c]
            .into_iter()
            .map(|e| OrderedEntry {
                event: e,
                classified: TraceEvent::UserMessage,
            })
            .collect();
        let ordered = order_by_captured_at(&[], &entries);
        // Earlier timestamp first; ties broken by capture_event_id ascending.
        assert_eq!(
            ordered[0].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAB"
        );
        assert_eq!(
            ordered[1].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAC"
        );
        assert_eq!(
            ordered[2].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }

    #[test]
    fn assigns_sequences_zero_indexed() {
        let a = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:00Z");
        let b = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:01Z");
        let entries: Vec<OrderedEntry<'_>> = [&a, &b]
            .into_iter()
            .map(|e| OrderedEntry {
                event: e,
                classified: TraceEvent::UserMessage,
            })
            .collect();
        let ordered = order_by_captured_at(&[], &entries);
        let with_seq = assign_sequences(&ordered);
        assert_eq!(with_seq[0].0, 0);
        assert_eq!(with_seq[1].0, 1);
        assert_eq!(
            with_seq[0].1.event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }

    #[test]
    fn merges_persisted_and_incoming() {
        let p = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:01Z");
        let i = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:00Z");
        let persisted = [OrderedEntry {
            event: &p,
            classified: TraceEvent::UserMessage,
        }];
        let incoming = [OrderedEntry {
            event: &i,
            classified: TraceEvent::PreTool,
        }];
        let ordered = order_by_captured_at(&persisted, &incoming);
        // Incoming has earlier timestamp; should sort first.
        assert_eq!(
            ordered[0].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAB"
        );
        assert_eq!(
            ordered[1].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }
}
