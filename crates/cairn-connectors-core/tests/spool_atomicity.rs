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
use sha2::Digest as _;

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

// ---------------------------------------------------------------------------
// Finding S tests — rename BEFORE emit
// ---------------------------------------------------------------------------

/// When the spool directory doesn't exist (simulates a rename failure before
/// emit), `process_event` must NOT call emit and must NOT leave a committed
/// spool file behind.
///
/// Finding S fix: the rename happens BEFORE emit. If the rename fails, we
/// return Err without calling emit — so no `CaptureEvent` is ever committed.
///
/// We simulate a rename failure by pointing the spool root at a path where
/// the spool dir is a *file* rather than a directory, which forces the
/// dir-creation to fail (and therefore the tmp write, which precedes rename).
/// This tests the "no commit without a durable file" half of Finding S.
#[tokio::test]
async fn rename_failure_before_emit_does_not_commit_capture() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    // Track how many times emit is called.
    #[derive(Default)]
    struct CountEmit(AtomicUsize);
    #[async_trait::async_trait]
    impl PipelineEmit for CountEmit {
        async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let counter = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    // Place a *file* at the path where the spool dir would be created.
    // This causes `create_dir_all` (called by `spool_bytes_to_tmp`) to fail
    // because a file already exists at that location, which in turn means
    // no tmp is written and thus no rename can occur.
    let blocker = tmp.path().join("connector");
    std::fs::write(&blocker, b"blocker").expect("create blocker file");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(counter.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(SingleEventConnector::new("01ARZ3NDEKTSV4RRFFQ69G5FAC"))
        .expect("register must succeed");
    reg.enable("spool-test", spool_grant())
        .await
        .expect("enable must succeed");

    // poll_now must return an error (spool write fails → no rename → no emit).
    let err = reg
        .poll_now("spool-test")
        .await
        .expect_err("poll_now must fail when spool dir is blocked");
    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal from spool failure, got {err:?}",
    );

    // emit must NOT have been called — no CaptureEvent committed.
    let emit_count = counter.0.load(Ordering::SeqCst);
    assert_eq!(
        emit_count, 0,
        "emit must NOT be called when spool write fails (Finding S)",
    );

    // The blocker file must still exist (we only placed it; we didn't try to
    // delete it on this path).
    assert!(blocker.is_file(), "blocker file must still exist");

    reg.shutdown().await;
}

/// When emit fails after a successful rename, the final spool file MUST
/// survive on disk (documented orphan tradeoff), and no further commits occur.
///
/// Finding S fix: rename BEFORE emit. Emit failure after a successful rename
/// leaves an orphan file — that is intentional and recoverable by the sweep
/// workflow (#131). What must NOT happen is silently deleting the file (that
/// would make the bug undetectable) or not returning Err (that would advance
/// the cursor).
#[tokio::test]
async fn emit_failure_after_rename_leaves_orphan_file_but_does_not_commit_capture() {
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAD";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();

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

    // poll_now must return an error because emit always fails.
    let err = reg
        .poll_now("spool-test")
        .await
        .expect_err("poll_now must fail when emit always fails");
    assert!(
        matches!(err, ConnectorError::Transient(_)),
        "expected Transient from AlwaysFailEmit, got {err:?}",
    );

    reg.shutdown().await;

    // (a) process_event returned Err — verified above.

    // (b) The final spool file DOES exist (orphan — documented tradeoff).
    //     After a successful rename the bytes are on disk; emit failure must
    //     NOT delete them. This is the key invariant Finding S preserves:
    //     the file is durable even though no CaptureEvent was committed.
    let spool_dir = spool_root.join("connector").join("spool-test");
    let committed_files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must exist after rename succeeded")
        .filter_map(std::result::Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(
        committed_files.len(),
        1,
        "exactly 1 orphan spool file must exist after emit failure (Finding S documented tradeoff); \
         found {committed_files:?}",
    );

    // (c) No further commits occurred — there must be no .tmp files (the
    //     rename promoted tmp → final, so no tmp should remain).
    let tmp_files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must still exist")
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(
        tmp_files.len(),
        0,
        "no .tmp files must remain after rename succeeded (tmp was promoted to final); \
         found {tmp_files:?}",
    );
}

/// A failed emit must leave exactly one committed file (the orphan from the
/// rename-before-emit fix) and zero `.tmp` files (Finding Q updated for
/// Finding S).
///
/// Finding S reorders the sequence to: write-tmp → rename-tmp-to-final →
/// emit. When emit fails the final content-addressed file is already on disk
/// (the rename succeeded). Finding Q's invariant still holds: only the tmp is
/// affected; the final path survives and no new tmp remains. The spool dir
/// therefore has exactly 1 committed (orphan) file and 0 tmp files.
///
/// The orphan is recoverable by the sweep workflow (issue #131).
#[tokio::test]
async fn failed_emit_leaves_one_orphan_no_tmp_files() {
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

    // Finding S: rename happens BEFORE emit. Emit failure leaves the final
    // file in place (orphan) — exactly 1 committed file, 0 tmp files.
    let committed_files: Vec<_> = match std::fs::read_dir(&spool_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => panic!("unexpected read_dir error: {e}"),
    };
    assert_eq!(
        committed_files.len(),
        1,
        "exactly 1 orphan committed file must exist after a failed emit \
         (Finding S rename-before-emit tradeoff); found {committed_files:?}",
    );

    let tmp_files: Vec<_> = match std::fs::read_dir(&spool_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => panic!("unexpected read_dir error on tmp check: {e}"),
    };
    assert_eq!(
        tmp_files.len(),
        0,
        "no .tmp files must remain after a failed emit (tmp was renamed to final \
         before emit was called); found {tmp_files:?}",
    );
}

// ---------------------------------------------------------------------------
// Finding X — full SHA-256 in spool filename + collision detection
// ---------------------------------------------------------------------------

/// The spool filename must contain exactly 64 hex characters (the full
/// SHA-256 digest) between the event_id separator and the file extension
/// (Finding X).
///
/// Setup: emit one fixture event; scan the spool dir; assert the committed
/// filename has the form `<event_id>_<64-hex-chars>.json`.
#[tokio::test]
async fn spool_uses_full_sha256_in_filename() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct SucceedEmit(Mutex<Vec<cairn_core::domain::capture::CaptureEvent>>);
    #[async_trait::async_trait]
    impl PipelineEmit for SucceedEmit {
        async fn emit(
            &self,
            ev: cairn_core::domain::capture::CaptureEvent,
        ) -> Result<(), ConnectorError> {
            self.0.lock().expect("mutex").push(ev);
            Ok(())
        }
    }

    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAE";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();

    let emit = Arc::new(SucceedEmit::default());

    {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(emit.clone() as Arc<dyn PipelineEmit>)
            .spool_root(spool_root.clone())
            .build();

        reg.register(SingleEventConnector::new(event_id))
            .expect("register must succeed");
        reg.enable("spool-test", spool_grant())
            .await
            .expect("enable must succeed");

        reg.poll_now("spool-test")
            .await
            .expect("poll_now must succeed");

        reg.shutdown().await;
    }

    // Scan the spool directory for committed (non-tmp) files.
    let spool_dir = spool_root.join("connector").join("spool-test");
    let committed_files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must exist after poll")
        .filter_map(std::result::Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();

    assert_eq!(
        committed_files.len(),
        1,
        "exactly 1 committed spool file must exist; found {committed_files:?}",
    );

    let filename = committed_files[0].file_name().to_string_lossy().to_string();

    // Filename form: `<event_id>_<hex>.<ext>`.
    // Strip the event_id prefix and the extension, then verify the hex part.
    let after_id = filename
        .strip_prefix(&format!("{event_id}_"))
        .unwrap_or_else(|| panic!("filename {filename:?} must start with event_id {event_id:?}"));
    let (hex_part, _ext) = after_id
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("filename {filename:?} must have an extension"));

    assert_eq!(
        hex_part.len(),
        64,
        "spool filename must contain exactly 64 hex chars (full SHA-256); \
         got {} chars in {filename:?}",
        hex_part.len(),
    );
    assert!(
        hex_part.chars().all(|c| c.is_ascii_hexdigit()),
        "spool filename hex part must contain only hex chars; got {hex_part:?} in {filename:?}",
    );
}

/// When the spool path already exists with DIFFERENT content, `poll_now` must
/// return a `Fatal` error and leave the existing file unmodified (Finding X).
///
/// Setup: pre-place a file at the would-be spool path with different content.
/// Drive `poll_now`; assert Fatal is returned; assert the pre-placed file is
/// unchanged.
#[tokio::test]
async fn spool_collision_with_different_content_rejected() {
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAF";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();

    // Compute the SHA-256 of the body that SingleEventConnector will produce
    // so we can compute the expected spool path.
    let connector_body = serde_json::json!({"id": event_id, "data": "fixed-content"});
    let connector_bytes = serde_json::to_vec(&connector_body).expect("serialize");
    let hash_hex = hex::encode(sha2::Sha256::digest(&connector_bytes));

    // Pre-create the spool directory and place a file at the would-be path
    // with DIFFERENT content (so the SHA-256 check fails).
    let spool_dir = spool_root.join("connector").join("spool-test");
    std::fs::create_dir_all(&spool_dir).expect("create spool dir");
    let spool_path = spool_dir.join(format!("{event_id}_{hash_hex}.json"));
    let different_content = b"this is different content that does not match";
    std::fs::write(&spool_path, different_content).expect("write pre-placed file");

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

    // poll_now must return a Fatal error (spool collision detected).
    let err = reg
        .poll_now("spool-test")
        .await
        .expect_err("poll_now must fail on spool collision");
    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal from spool collision, got {err:?}",
    );

    reg.shutdown().await;

    // The pre-placed file must be unmodified (content is unchanged).
    let actual_content = std::fs::read(&spool_path).expect("read pre-placed file");
    assert_eq!(
        actual_content, different_content,
        "pre-placed spool file must be unmodified after collision detection",
    );
}

/// When the spool path already exists with IDENTICAL content, `poll_now` must
/// succeed (idempotent retry) and leave the existing file in place (Finding X).
///
/// Setup: pre-place a file at the would-be spool path with the EXACT same
/// content the connector will produce. Drive `poll_now`; assert success; assert
/// the file is still present and unchanged.
#[tokio::test]
async fn spool_idempotent_for_identical_content() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct SucceedEmit2(Mutex<usize>);
    #[async_trait::async_trait]
    impl PipelineEmit for SucceedEmit2 {
        async fn emit(
            &self,
            _: cairn_core::domain::capture::CaptureEvent,
        ) -> Result<(), ConnectorError> {
            *self.0.lock().expect("mutex") += 1;
            Ok(())
        }
    }

    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAG";
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_root = tmp.path().to_path_buf();

    // Compute the SHA-256 of the body that SingleEventConnector will produce.
    let connector_body = serde_json::json!({"id": event_id, "data": "fixed-content"});
    let connector_bytes = serde_json::to_vec(&connector_body).expect("serialize");
    let hash_hex = hex::encode(sha2::Sha256::digest(&connector_bytes));

    // Pre-create the spool directory and place a file at the would-be path
    // with IDENTICAL content.
    let spool_dir = spool_root.join("connector").join("spool-test");
    std::fs::create_dir_all(&spool_dir).expect("create spool dir");
    let spool_path = spool_dir.join(format!("{event_id}_{hash_hex}.json"));
    std::fs::write(&spool_path, &connector_bytes).expect("write pre-placed file");

    let emit = Arc::new(SucceedEmit2::default());

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(spool_root.clone())
        .build();

    reg.register(SingleEventConnector::new(event_id))
        .expect("register must succeed");
    reg.enable("spool-test", spool_grant())
        .await
        .expect("enable must succeed");

    // poll_now must succeed (idempotent: identical content already present).
    reg.poll_now("spool-test")
        .await
        .expect("poll_now must succeed for idempotent retry with identical content");

    reg.shutdown().await;

    // The file must still exist and be unchanged.
    let actual_content = std::fs::read(&spool_path).expect("read pre-placed file");
    assert_eq!(
        actual_content, connector_bytes,
        "pre-placed spool file must be unchanged after idempotent retry",
    );

    // Exactly one event must have been emitted.
    assert_eq!(
        *emit.0.lock().expect("mutex"),
        1,
        "exactly 1 event must be emitted on idempotent retry",
    );
}
