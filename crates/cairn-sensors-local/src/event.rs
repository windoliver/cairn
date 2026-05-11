//! Shared local sensor event construction.

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, DomainError, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use sha2::{Digest as _, Sha256};

/// Compute the canonical SHA-256 payload hash.
pub(crate) fn payload_hash(bytes: &[u8]) -> Result<PayloadHash, DomainError> {
    PayloadHash::parse(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Build the vault-relative source payload ref for an event.
pub(crate) fn payload_ref(family: SourceFamily, event_id: &CaptureEventId) -> String {
    format!("sources/{family}/{event_id}.json")
}

/// Build a validated Mode A event authored by the emitting sensor.
pub(crate) fn build_auto_event(
    event_id: CaptureEventId,
    captured_at: Rfc3339Timestamp,
    sensor_label: &'static str,
    payload: CapturePayload,
    source_family: SourceFamily,
    refs: Option<CaptureRefs>,
    sanitized_payload_bytes: &[u8],
) -> Result<CaptureEvent, DomainError> {
    let sensor_id = Identity::parse(format!("snr:{sensor_label}"))?;
    let actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: sensor_id.clone(),
        at: captured_at.clone(),
    }];
    let payload_hash = payload_hash(sanitized_payload_bytes)?;
    let payload_ref = payload_ref(source_family, &event_id);

    CaptureEvent::try_new(
        event_id,
        sensor_id,
        CaptureMode::Auto,
        actor_chain,
        refs,
        payload_hash,
        payload_ref,
        captured_at,
        payload,
        source_family,
    )
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::{
        CaptureEventId, CaptureMode, CapturePayload, Rfc3339Timestamp, SourceFamily,
    };

    use super::{build_auto_event, payload_hash, payload_ref};

    fn event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid test ULID")
    }

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
    }

    #[test]
    fn payload_hash_uses_sha256_prefix() {
        let hash = payload_hash(b"hello").expect("hash parses");

        assert_eq!(
            hash.as_str(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn payload_ref_is_sources_family_event_json() {
        assert_eq!(
            payload_ref(SourceFamily::Hook, &event_id()),
            "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json"
        );
    }

    #[test]
    fn build_auto_event_binds_sensor_author_to_sensor_id() {
        let event = build_auto_event(
            event_id(),
            ts(),
            "local:hook:cc-session:v1",
            CapturePayload::Hook {
                hook_name: "UserPromptSubmit".to_owned(),
                tool_name: None,
            },
            SourceFamily::Hook,
            None,
            b"{\"prompt\":\"hi\"}",
        )
        .expect("event validates");

        assert_eq!(event.capture_mode, CaptureMode::Auto);
        assert_eq!(event.sensor_id.as_str(), "snr:local:hook:cc-session:v1");
        assert_eq!(event.actor_chain.len(), 1);
        assert_eq!(
            event.actor_chain[0].identity.as_str(),
            "snr:local:hook:cc-session:v1"
        );
        event.validate_for_capture().expect("fresh event is valid");
    }
}
