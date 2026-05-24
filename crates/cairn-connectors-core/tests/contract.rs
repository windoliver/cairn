//! Happy-path contract test: register → enable → poll → emit yields a
//! `CaptureEvent` with `source_family == SourceFamily::External`.
//!
//! This test drives the full production code path in `ConnectorRegistry`:
//!   1. `register` — add the `FixtureConnector` plugin.
//!   2. `enable`   — write the consent grant and arm the poll scheduler.
//!   3. `poll_now` — trigger one synchronous poll cycle (test-only helper).
//!   4. `shutdown` — drain the scheduler.
//!
//! The `fixture` feature is included in `default = ["fixture"]`, so this
//! integration test file sees `cairn_connectors_core::fixture` without any
//! extra `--features` flags.

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector, default_grant};
use cairn_connectors_core::{ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit};
use cairn_core::domain::capture::{CaptureEvent, SourceFamily};

// ---------------------------------------------------------------------------
// Recording PipelineEmit
// ---------------------------------------------------------------------------

/// Records every emitted [`CaptureEvent`] in an in-memory buffer for
/// post-test assertions.
#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
        self.0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned")
            .push(event);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Contract test
// ---------------------------------------------------------------------------

/// Full happy-path: register → enable → `poll_now` → shutdown.
///
/// Asserts that the emitted [`CaptureEvent`] carries
/// `source_family == SourceFamily::External` — the invariant that ties
/// the connector framework to the cairn-core taxonomy (brief §9.1, §19 v0.3).
#[tokio::test]
async fn happy_path_emits_external_capture_event() {
    let capturer = Arc::new(Capturer::default());

    let mut registry = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .build();

    // 1. register
    registry
        .register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");

    // 2. enable — writes consent grant and arms poll scheduler
    registry
        .enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    // 3. poll_now — one synchronous poll cycle (fixture returns one event)
    registry
        .poll_now("fixture")
        .await
        .expect("poll_now must succeed");

    // 4. shutdown — drain the scheduler
    registry.shutdown().await;

    // Assert: exactly one event with source_family == External
    let events = capturer
        .0
        .lock()
        .expect("invariant: Capturer mutex unpoisoned");
    assert_eq!(events.len(), 1, "expected exactly one emitted CaptureEvent");
    assert_eq!(
        events[0].source_family,
        SourceFamily::External,
        "emitted event must carry source_family == External"
    );
}
