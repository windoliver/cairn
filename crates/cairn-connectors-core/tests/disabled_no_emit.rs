//! T20c — Disabled connector does not emit.
//!
//! Register a connector but do **not** call `enable`. Driving `poll_now`
//! must return `ConnectorError::Fatal` because the connector is in the
//! `Disabled` state. `PipelineEmit::emit` must never be called.
//!
//! Issue #130, brief §9.1 source sensors + registry lifecycle.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector};
use cairn_connectors_core::{ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit};
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// PanicEmit — test guard: emit must never be called for a disabled connector
// ---------------------------------------------------------------------------

struct PanicEmit;

#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called for a disabled connector");
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_connector_does_not_emit() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");

    // Intentionally do NOT call enable — connector remains in Disabled state.
    // Driving poll_now should refuse because state == Disabled.
    let err = reg
        .poll_now("fixture")
        .await
        .expect_err("poll_now must fail for a disabled connector");

    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal, got {err:?}",
    );

    reg.shutdown().await;
}
