//! `PipelineEmit` — the single outbound entrypoint from
//! `cairn-connectors-core` into the cairn-core ingestion pipeline.
//!
//! After redaction + consent the framework calls
//! [`build_capture_event`] to convert a [`ConnectorEvent`] into a
//! [`CaptureEvent`] with `source_family: External`, then hands it to
//! whatever [`PipelineEmit`] implementation the host has wired in.
//!
//! Brief §9.1 source sensors, §19 v0.3.

use std::collections::BTreeSet;

use cairn_core::domain::{
    ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp,
    capture::{
        CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, PayloadHash, SourceFamily,
    },
};
use cairn_core::pipeline::filter::redact::RedactionSpan;

use crate::error::ConnectorError;
use crate::event::ConnectorEvent;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// The single outbound interface through which the connector framework
/// hands a fully-validated, redacted [`CaptureEvent`] to the cairn-core
/// ingestion pipeline.
///
/// The trait is `async` (via `async_trait`) so implementors can do
/// async I/O (e.g. write to the WAL, fan-out to a channel) without
/// blocking the framework task. The blanket bound `Send + Sync` is
/// required because the framework passes `Arc<dyn PipelineEmit>` across
/// tokio tasks.
#[async_trait::async_trait]
pub trait PipelineEmit: Send + Sync {
    /// Accept a fully-formed [`CaptureEvent`] and hand it to the
    /// pipeline. The event's `source_family` is always
    /// [`SourceFamily::External`] when called by the connector
    /// framework.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError`] if the downstream pipeline rejects the
    /// event or encounters a transport error. A [`ConnectorError::Transient`]
    /// indicates the caller should retry; [`ConnectorError::Fatal`] means
    /// the connector should be disabled.
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError>;
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Convert a redacted [`ConnectorEvent`] into a [`CaptureEvent`] with
/// `source_family: External` for handoff to the cairn-core pipeline.
///
/// # Parameters
///
/// - `event`: the connector event *after* redaction has run (all PII
///   replaced with `[REDACTED:<tag>]` placeholders by
///   [`crate::redact::RedactionPipeline`]).
/// - `sensor`: the sensor [`Identity`] for this connector
///   (`snr:local:connector:<name>:v<n>`). The framework owns this
///   identity; the connector adapter does not construct it directly.
/// - `redacted_spans`: spans collected by the redaction pipeline —
///   forwarded verbatim into [`CapturePayload::External::redacted_spans`].
/// - `payload_ref`: vault-relative path of the spooled raw bytes,
///   **must begin with `sources/`** (brief §3 trust boundary). The
///   caller is responsible for providing the correct leading prefix.
///   Example: `"sources/connector/github/01ARZ3NDEKTSV4RRFFQ69G5FAV"`.
/// - `payload_hash`: SHA-256 of the raw payload bytes (before
///   redaction), formatted as `"sha256:<64 lowercase hex>"`. The
///   caller computes this when the bytes are spooled; the framework
///   passes it through here so `build_capture_event` does not need
///   access to the spool path.
///
/// # Errors
///
/// Returns [`ConnectorError::MalformedPayload`] if:
/// - `payload_ref` does not begin with `"sources/"` (vault-path trust
///   boundary violation, brief §3).
/// - `event.occurred_at` is negative (pre-epoch timestamps are not
///   supported).
/// - Any other cairn-core domain validation fails (malformed
///   `CaptureEventId`, invalid identity kind, etc.).
pub fn build_capture_event(
    event: &ConnectorEvent,
    sensor: &Identity,
    redacted_spans: Vec<RedactionSpan>,
    payload_ref: &str,
    payload_hash: PayloadHash,
) -> Result<CaptureEvent, ConnectorError> {
    // Enforce the vault-relative path trust boundary (brief §3).
    if !payload_ref.starts_with("sources/") {
        return Err(ConnectorError::MalformedPayload(format!(
            "payload_ref `{payload_ref}` must begin with `sources/` (brief §3 vault boundary)"
        )));
    }

    // Map the connector's SourceRef to the cairn-core SourceRef.
    let core_source_ref = cairn_core::domain::capture::SourceRef::new(
        &event.source_ref.kind,
        &event.source_ref.system_id,
        event.source_ref.sub_id.clone(),
    );

    let payload = CapturePayload::External {
        connector: event.connector.clone(),
        source_ref: core_source_ref,
        labels: event.labels.iter().cloned().collect::<BTreeSet<_>>(),
        mime: event.payload.mime().to_owned(),
        redacted_spans,
    };

    // Parse the ULID from the connector event id (must be exactly 26
    // Crockford-base32 characters; the connector is responsible for
    // minting a valid ULID).
    let event_id = CaptureEventId::parse(event.event_id.as_str())
        .map_err(|e| ConnectorError::MalformedPayload(e.to_string()))?;

    // Convert the Unix-epoch timestamp. Negative values (pre-1970) are
    // rejected — Cairn has no pre-epoch use case.
    let captured_at = Rfc3339Timestamp::from_unix_secs(event.occurred_at)
        .map_err(|e| ConnectorError::MalformedPayload(e.to_string()))?;

    // Build a single-entry Author chain. Per brief §4.2:
    //   "humans, agents, AND sensors author records — sensors are the
    //    canonical authors of sensor_observation and raw-event records."
    // A connector sensor is the canonical author of its own raw events
    // (Mode A). The chain shape `[Author(snr:…)]` passes validate_chain.
    let actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: sensor.clone(),
        at: captured_at.clone(),
    }];

    CaptureEvent::try_new(
        event_id,
        sensor.clone(),
        CaptureMode::Auto,
        actor_chain,
        None, // refs: connectors have no session/turn context
        payload_hash,
        payload_ref.to_owned(),
        captured_at,
        payload,
        SourceFamily::External,
    )
    .map_err(|e| ConnectorError::MalformedPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
    };
    use cairn_core::domain::capture::{CapturePayload, SourceFamily};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    // ---- fixture helpers ---------------------------------------------------

    /// A valid 26-character Crockford-base32 ULID for test events.
    const FIXTURE_ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    /// A deterministic all-zero SHA-256 hash — fine for fixtures.
    fn zero_hash() -> PayloadHash {
        PayloadHash::parse(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("zero hash must be valid")
    }

    /// Sensor identity for a connector named `fixture`.
    fn fixture_sensor() -> Identity {
        Identity::parse("snr:local:connector:fixture:v1").expect("valid connector sensor identity")
    }

    /// Build a minimal `ConnectorEvent` suitable for use across tests.
    fn fixture_event() -> ConnectorEvent {
        ConnectorEvent {
            event_id: ConnectorEventId::new(FIXTURE_ULID),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "gh:owner/repo#1", None),
            occurred_at: 1_700_000_000,
            labels: BTreeSet::from(["note".to_string()]),
            scope: ConnectorScope::project("owner/repo"),
            payload: ConnectorPayload::Json {
                mime: "application/json".into(),
                body: serde_json::json!({"key": "value"}),
            },
            delivery: DeliveryMode::Poll { cursor: None },
        }
    }

    // ---- PipelineEmit recorder --------------------------------------------

    #[derive(Default)]
    struct Recorder(Mutex<Vec<CaptureEvent>>);

    #[async_trait::async_trait]
    impl PipelineEmit for Recorder {
        async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
            self.0
                .lock()
                .expect("invariant: recorder mutex unpoisoned")
                .push(event);
            Ok(())
        }
    }

    // ---- tests ------------------------------------------------------------

    /// `build_capture_event` must produce a `CaptureEvent` with
    /// `source_family == SourceFamily::External`.
    #[test]
    fn build_capture_event_produces_external_source_family() {
        let event = fixture_event();
        let sensor = fixture_sensor();

        let result = build_capture_event(
            &event,
            &sensor,
            vec![],
            "sources/connector/fixture/01ARZ3NDEKTSV4RRFFQ69G5FAV",
            zero_hash(),
        )
        .expect("build_capture_event must succeed");

        assert_eq!(
            result.source_family,
            SourceFamily::External,
            "source_family must be External"
        );
        assert_eq!(
            result.actor_chain.len(),
            1,
            "connector events have a single-entry Author chain"
        );
        assert!(
            matches!(result.payload, CapturePayload::External { .. }),
            "payload must be CapturePayload::External"
        );
    }

    /// A `Recorder` implementation of `PipelineEmit` must record the
    /// emitted event; the count after one `emit` call must be 1.
    #[tokio::test]
    async fn recorder_pipeline_emit_records_event() {
        let recorder = Arc::new(Recorder::default());
        let event = fixture_event();
        let sensor = fixture_sensor();

        let capture_event = build_capture_event(
            &event,
            &sensor,
            vec![],
            "sources/connector/fixture/01ARZ3NDEKTSV4RRFFQ69G5FAV",
            zero_hash(),
        )
        .expect("build must succeed");

        recorder
            .emit(capture_event)
            .await
            .expect("emit must succeed");

        assert_eq!(
            recorder
                .0
                .lock()
                .expect("invariant: mutex unpoisoned")
                .len(),
            1,
            "recorder must have exactly one event after one emit call"
        );
    }

    /// `payload_ref` that does not begin with `"sources/"` must be
    /// rejected with `ConnectorError::MalformedPayload` — enforcing the
    /// vault-path trust boundary from brief §3.
    #[test]
    fn payload_ref_must_start_with_sources_prefix() {
        let event = fixture_event();
        let sensor = fixture_sensor();

        let err = build_capture_event(
            &event,
            &sensor,
            vec![],
            "absolute/path/without/sources_prefix",
            zero_hash(),
        )
        .expect_err("must reject payload_ref missing sources/ prefix");

        assert!(
            matches!(err, ConnectorError::MalformedPayload(_)),
            "expected MalformedPayload, got {err:?}"
        );
    }
}
