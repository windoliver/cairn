//! T20b — `ConsentRevoked` gate.
//!
//! `DenyAllJournal` always returns `ConnectorConsentLookup::Revoked`. The
//! framework must reject the emit with `ConnectorError::ConsentRevoked` and
//! must never call `PipelineEmit::emit`.
//!
//! Issue #130, brief §14 consent gate.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_connectors_core::fixture::{FixtureConnector, default_grant};
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// DenyAllJournal — always returns Revoked
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DenyAllJournal;

#[async_trait::async_trait]
impl ConnectorConsentJournal for DenyAllJournal {
    async fn put_grant(&self, _: ConsentGrant) -> Result<ConsentGrantId, String> {
        Ok(ConsentGrantId::new("gnt:x"))
    }

    async fn lookup(
        &self,
        _connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        Ok(ConnectorConsentLookup::Revoked)
    }

    async fn revoke(&self, _: &ConsentGrantId) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PanicEmit — test guard: emit must never be called when consent is revoked
// ---------------------------------------------------------------------------

struct PanicEmit;

#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called when consent is revoked");
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consent_revoked_blocks_emit() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(DenyAllJournal))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("fixture")
        .await
        .expect_err("poll_now must fail when consent is revoked");

    assert!(
        matches!(err, ConnectorError::ConsentRevoked { ref connector } if connector == "fixture"),
        "expected ConsentRevoked{{connector: \"fixture\"}}, got {err:?}",
    );

    reg.shutdown().await;
}
