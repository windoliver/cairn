//! Finding Q — spool write-then-rename atomicity: a failed emit must never
//! delete a spool file that was committed by a prior successful attempt.
//!
//! Under at-least-once retry semantics, the same event_id can be processed
//! more than once. If the first attempt succeeded (spool file committed), a
//! second attempt that fails at `emit` must not wipe the committed file.
//!
//! The fix requires that cleanup on emit failure targets only the per-attempt
//! `.tmp.<random>` staging file — never the final content-addressed path.
//!
//! Issue #130, brief §9.1 source sensors (Round-5 review, Finding Q).

#![allow(missing_docs)]
// doc_markdown: backtick rules are relaxed in test files where prose-style
// comments are preferred over API-grade markup.
#![allow(clippy::doc_markdown)]
// items_after_statements: inline struct/impl blocks inside async test bodies
// are idiomatic in Rust integration tests; extracting them would obscure the
// test's narrative.
#![allow(clippy::items_after_statements)]

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// Shared manifest (budget = 100 — not the limiting factor in these tests)
// ---------------------------------------------------------------------------

const SPOOL_MANIFEST: &str = r#"
[connector]
name = "spool-test"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:spool-test:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 100
max_bytes_per_day = "50MiB"

[labels]
allowed = ["note"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Sig"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 10
"#;

fn spool_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(SPOOL_MANIFEST).expect("SPOOL_MANIFEST must parse")
}

fn spool_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "spool-test",
        spool_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    )
}

// ---------------------------------------------------------------------------
// SingleEventConnector — always returns the same event on every poll call
// ---------------------------------------------------------------------------

/// A connector that returns one hardcoded event per poll call.
///
/// The event_id and body are fixed so every poll attempt produces identical
/// spool bytes, and therefore the same content-addressed filename.
struct SingleEventConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    event_id: &'static str,
    body: serde_json::Value,
}

impl SingleEventConnector {
    fn new(event_id: &'static str) -> Self {
        Self {
            manifest: spool_manifest(),
            sensor: Identity::parse("snr:local:connector:spool-test:v1")
                .expect("sensor identity must parse"),
            event_id,
            body: serde_json::json!({"id": event_id, "data": "fixed-content"}),
        }
    }
}

#[async_trait]
impl Connector for SingleEventConnector {
    fn name(&self) -> &'static str {
        "spool-test"
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: true,
            webhook: false,
            backfill: false,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    async fn poll(&self, _: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new(self.event_id),
                "spool-test",
                SourceRef::new("issue", self.event_id, None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: self.body.clone(),
                },
                DeliveryMode::Poll { cursor: None },
            )],
            next_cursor: None,
            rate_limit_hint: None,
        })
    }

    async fn ingest_webhook(
        &self,
        _: &WebhookRequest,
        _: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for SingleEventConnector {
    const NAME: &'static str = "spool-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// AlwaysFailEmit — every emit call returns Err(Transient)
// ---------------------------------------------------------------------------

/// `PipelineEmit` that always fails with a transient error.
struct AlwaysFailEmit;

#[async_trait]
impl PipelineEmit for AlwaysFailEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        Err(ConnectorError::transient_msg(
            "simulated emit failure (Finding Q test)",
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A failed emit must not delete a spool file committed by a prior successful
/// attempt (Finding Q).
///
/// Setup:
/// 1. Two registries sharing the same spool root. The first uses a succeed-emit;
///    the second uses `AlwaysFailEmit`.
/// 2. Registry A runs `poll_now` → succeeds → committed spool file exists at the
///    final content-addressed path.
/// 3. Registry B (same connector, same event_id, same spool root) runs `poll_now`
///    → `AlwaysFailEmit` returns Err → cleanup must NOT delete the committed file.
///
/// Assertion: the committed spool file still exists after Registry B's failed emit.
#[tokio::test]
async fn retry_after_successful_emit_does_not_delete_original() {
    // Use the same deterministic event_id to get the same spool filename.
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();

    // --- Registry A: succeed-emit -------------------------------------------
    // Count-emit that records events but always succeeds.
    #[derive(Default)]
    struct SucceedEmit(std::sync::Mutex<usize>);

    #[async_trait]
    impl PipelineEmit for SucceedEmit {
        async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
            *self.0.lock().expect("mutex") += 1;
            Ok(())
        }
    }

    let succeed_emit = Arc::new(SucceedEmit::default());

    {
        let mut reg_a = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(succeed_emit.clone() as Arc<dyn PipelineEmit>)
            .spool_root(spool_root.clone())
            .build();

        reg_a
            .register(SingleEventConnector::new(event_id))
            .expect("register must succeed");
        reg_a
            .enable("spool-test", spool_grant())
            .await
            .expect("enable must succeed");

        reg_a
            .poll_now("spool-test")
            .await
            .expect("first poll_now (succeed-emit) must succeed");

        reg_a.shutdown().await;
    }

    // Verify that exactly one committed spool file was written.
    let spool_dir = spool_root.join("connector").join("spool-test");
    let committed_files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must exist after first poll")
        .filter_map(std::result::Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(
        committed_files.len(),
        1,
        "exactly 1 committed spool file must exist after first poll; found {committed_files:?}",
    );
    let committed_path = committed_files[0].path();

    // --- Registry B: always-fail-emit ---------------------------------------
    // Same connector, same event_id, same spool root. Emit always fails.
    {
        let mut reg_b = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(Arc::new(AlwaysFailEmit) as Arc<dyn PipelineEmit>)
            .spool_root(spool_root.clone())
            .build();

        reg_b
            .register(SingleEventConnector::new(event_id))
            .expect("register must succeed");
        reg_b
            .enable("spool-test", spool_grant())
            .await
            .expect("enable must succeed");

        let err = reg_b
            .poll_now("spool-test")
            .await
            .expect_err("second poll_now (fail-emit) must fail");
        assert!(
            matches!(err, ConnectorError::Transient(_)),
            "expected Transient from AlwaysFailEmit, got {err:?}",
        );

        reg_b.shutdown().await;
    }

    // The committed file from the first attempt must still exist.
    assert!(
        committed_path.exists(),
        "committed spool file must survive a failed retry — \
         cleanup must only delete the per-attempt tmp file (Finding Q)",
    );

    // There must be no leftover .tmp files (the cleanup must have removed them).
    let tmp_files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must still exist")
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(
        tmp_files.len(),
        0,
        "no .tmp files must remain after a failed emit — tmp cleanup must run (Finding Q); \
         found {tmp_files:?}",
    );
}

/// A failed emit must clean up only its own tmp staging file, not the final
/// committed path (Finding Q — second scenario).
///
/// This test verifies the simpler case: one registry, one event, emit always
/// fails. The spool dir must be clean of both committed files AND tmp files
/// when there was never a prior committed attempt.
#[tokio::test]
async fn failed_emit_cleans_up_only_its_tmp_no_committed_file() {
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();
    let spool_dir = spool_root.join("connector").join("spool-test");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(AlwaysFailEmit) as Arc<dyn PipelineEmit>)
        .spool_root(spool_root.clone())
        .build();

    reg.register(SingleEventConnector::new(event_id))
        .expect("register must succeed");
    reg.enable("spool-test", spool_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("spool-test")
        .await
        .expect_err("poll_now must fail when emit always fails");
    assert!(
        matches!(err, ConnectorError::Transient(_)),
        "expected Transient, got {err:?}",
    );

    reg.shutdown().await;

    // After a failed-emit with no prior committed file, the spool dir should
    // have zero committed files AND zero .tmp files.
    let all_files: Vec<_> = match std::fs::read_dir(&spool_dir) {
        Ok(entries) => entries.filter_map(std::result::Result::ok).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => panic!("unexpected read_dir error: {e}"),
    };
    assert_eq!(
        all_files.len(),
        0,
        "spool dir must be empty (no committed files, no tmp files) after a failed emit \
         with no prior committed attempt; found {all_files:?}",
    );
}
