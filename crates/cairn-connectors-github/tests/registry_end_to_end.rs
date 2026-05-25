//! End-to-end: `GitHubConnector` → `ConnectorRegistry` → `PipelineEmit` recorder.
//!
//! Verifies the adapter integrates cleanly with the substrate's real registry
//! plumbing — registration, sensor-identity binding, consent journal, poll-now,
//! and `PipelineEmit` dispatch — without going through any adapter-side shortcuts.
//!
//! Brief §9.1 source sensors, §19 v0.3. Issue #131.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_connectors_core::{
    ConnectorRegistry, CredentialStore, InMemoryCredentialStore, PipelineEmit,
};
use cairn_connectors_core::error::ConnectorError;
use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_github::GitHubConnector;
use cairn_core::contract::connector_consent::ConsentGrant;
use cairn_core::domain::capture::CaptureEvent;
use cairn_core::domain::Identity;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// PipelineEmit recorder
// ---------------------------------------------------------------------------

/// Recorder that counts every [`CaptureEvent`] dispatched by the registry.
///
/// Uses a direct `Arc<Recorder>` rather than `Arc<dyn PipelineEmit>` so we can
/// assert on `recorder.count()` after the poll without Any-downcasting.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<CaptureEvent>>,
}

#[async_trait]
impl PipelineEmit for Recorder {
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
        self.events
            .lock()
            .expect("invariant: Recorder mutex unpoisoned")
            .push(event);
        Ok(())
    }
}

impl Recorder {
    /// Return the number of events the recorder has received so far.
    fn count(&self) -> usize {
        self.events
            .lock()
            .expect("invariant: Recorder mutex unpoisoned")
            .len()
    }
}

// ---------------------------------------------------------------------------
// Helper: build a ConsentGrant for the GitHub connector
// ---------------------------------------------------------------------------

/// Build a [`ConsentGrant`] whose `manifest_hash` matches the GitHub
/// connector's compiled-in `connector.toml` so the framework's manifest-drift
/// check in `process_event` passes.
fn github_grant() -> ConsentGrant {
    // Re-parse the same TOML the adapter embeds so the hash is guaranteed
    // to be byte-identical to whatever `GitHubConnector::manifest().hash()`
    // returns at runtime.
    let manifest_hash = ConnectorManifest::parse_toml(cairn_connectors_github::MANIFEST_TOML)
        .expect("invariant: GitHub connector.toml must be valid")
        .hash();

    ConsentGrant::new(
        "github",
        manifest_hash,
        // Must be a superset of every label the adapter emits.
        BTreeSet::from([
            "source:github".to_string(),
            "kind:issue".to_string(),
            "kind:pr".to_string(),
            "kind:commit".to_string(),
            "kind:comment".to_string(),
        ]),
        // Wildcard covers any project scope the adapter returns.
        vec!["project:*".to_string()],
        // Arbitrary past timestamp — the framework does not enforce expiry in P0.
        1_700_000_000,
        // Invariant: "hmn:alice" is a valid human identity wire form.
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is a valid Identity"),
    )
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Drives one poll tick through the real registry, asserting that all
/// five events from the three wiremocked resources reach the recorder.
///
/// Exercises:
/// - `ConnectorRegistry::register` + sensor-identity binding (Finding C).
/// - `ConnectorRegistry::enable` + consent journal write.
/// - `ConnectorRegistry::poll_now` + per-resource cursor fan-out.
/// - `process_event` pipeline: label check, consent lookup, redaction,
///   spool write, `build_capture_event`, `PipelineEmit::emit`.
#[tokio::test]
async fn registry_poll_dispatches_events_to_pipeline_emit() {
    // ---- 1. Spin up wiremock for all three GitHub resources. ----------------
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/issues_page_1.json")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/prs_page_1.json")),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/o/r/commits"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(include_str!("fixtures/commits_page_1.json")),
        )
        .mount(&server)
        .await;

    // ---- 2. Wire up substrate machinery. -----------------------------------
    // Credential store: the registry looks up "connector/github".
    let creds = Arc::new(InMemoryCredentialStore::default());
    let token_bytes = serde_json::json!({"kind": "pat", "token": "smoke-token"})
        .to_string()
        .into_bytes();
    creds
        .put("connector/github", token_bytes)
        .await
        .expect("credential put must succeed");

    // Consent journal: AcceptAllConsent records every grant and returns
    // `Granted` for any connector that has a stored grant.
    let consent = Arc::new(AcceptAllConsent::default());

    // PipelineEmit recorder: keep a direct typed `Arc<Recorder>` so we can
    // assert on `recorder.count()` after the poll without Any-downcasting.
    // `Arc<Recorder>` coerces to `Arc<dyn PipelineEmit>` via unsize.
    let recorder = Arc::new(Recorder::default());
    let emit_dyn: Arc<dyn PipelineEmit> = recorder.clone();

    // Spool root: integration tests write to a temp dir so the process
    // cannot leave orphan files in the working directory.
    let spool_dir = tempdir().expect("tempdir must create successfully");

    // ---- 3. Build the registry and register the connector. -----------------
    let mut registry = ConnectorRegistry::builder()
        .credentials(creds)
        .consent(consent)
        .emit(emit_dyn)
        .spool_root(spool_dir.path().to_path_buf())
        .build();

    let connector = GitHubConnector::with_base_url("o", "r", server.uri())
        .expect("GitHubConnector::with_base_url must succeed");

    registry
        .register(connector)
        .expect("register must succeed: sensor-identity binding is valid");

    // ---- 4. Enable with a grant whose manifest_hash matches the adapter. ---
    registry
        .enable("github", github_grant())
        .await
        .expect("enable must succeed: grant is well-formed and AcceptAllConsent accepts all");

    // ---- 5. Drive one poll synchronously. ----------------------------------
    registry
        .poll_now("github")
        .await
        .expect("poll_now must succeed: wiremock serves all three resource endpoints");

    // ---- 6. Assert the recorder received events from all three resources. --
    // The fixture JSON contains 2 issues + 1 PR + 2 commits = 5 events total.
    // (Verified by the pre-existing `full_poll_emits_events_from_all_resources`
    //  test in connector_poll_full_cycle.rs.)
    let event_count = recorder.count();
    assert!(
        event_count >= 5,
        "expected at least 5 events (2 issues + 1 PR + 2 commits) \
         to reach the PipelineEmit recorder through the registry; got {event_count}",
    );

    // ---- 7. Tear down. -----------------------------------------------------
    registry.shutdown().await;
    // `spool_dir` drops here; the temp directory and any spool files are
    // cleaned up automatically by `tempfile::TempDir`.
}
