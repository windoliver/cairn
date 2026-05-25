//! Live-API smoke harness — hits real `api.github.com`.
//!
//! These tests are **`#[ignore]`-gated** so CI and default `cargo nextest run`
//! skip them. Run manually with:
//!
//! ```bash
//! GITHUB_TOKEN=ghp_yourtoken \
//!   cargo nextest run -p cairn-connectors-github \
//!   --run-ignored only smoke_live_github --locked
//! ```
//!
//! What this proves vs the wiremock suite:
//! - The wire shape of GitHub responses matches our DTOs.
//! - HTTP headers (User-Agent, X-GitHub-Api-Version, Accept) are accepted by GitHub.
//! - Rate-limit headers (`X-RateLimit-Remaining`/`Reset`) parse on real responses.
//!
//! Target repo: `octocat/hello-world` — GitHub's canonical public test repo.

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, PollContext};
use cairn_connectors_github::{GitHubConnector, testkit};
use tokio_util::sync::CancellationToken;
use url::Url;

const TEST_OWNER: &str = "octocat";
const TEST_REPO: &str = "hello-world";

fn pat_handle() -> Arc<CredentialHandle> {
    let token = std::env::var("GITHUB_TOKEN").expect(
        "GITHUB_TOKEN env var must be set to run smoke tests. \
         Get a fine-grained PAT with public-repo read scope at \
         https://github.com/settings/tokens",
    );
    let env = serde_json::json!({"kind": "pat", "token": token});
    Arc::new(CredentialHandle::from_bytes(env.to_string().into_bytes()))
}

#[ignore = "live API; needs GITHUB_TOKEN. Run with --run-ignored only smoke_live_github"]
#[tokio::test]
async fn live_issues_poll_parses_real_wire_shape() {
    let handle = pat_handle();
    let base = Url::parse("https://api.github.com").unwrap();

    // CredentialHandle isn't Clone; pass through Arc::as_ref.
    let (events, cursor) =
        testkit::run_issues_poll(handle.as_ref(), &base, TEST_OWNER, TEST_REPO, None, 5)
            .await
            .expect("live issues poll");

    // octocat/hello-world has a handful of stable historical issues.
    // We don't pin a specific count (GitHub may add labels, etc.) — we just
    // verify the wire shape parses and at least one event came back.
    assert!(
        !events.is_empty(),
        "expected at least one issue event from {TEST_OWNER}/{TEST_REPO}"
    );
    for e in &events {
        assert!(e.labels.contains("kind:issue"), "every event has kind:issue label");
        assert_eq!(e.source_ref.kind, "issue");
        assert!(
            e.source_ref.system_id.starts_with("gh:octocat/hello-world#"),
            "system_id starts with gh:owner/repo#: got {}",
            e.source_ref.system_id
        );
    }
    assert!(cursor.since.is_some(), "cursor advanced");
}

#[ignore = "live API; needs GITHUB_TOKEN. Run with --run-ignored only smoke_live_github"]
#[tokio::test]
async fn live_prs_poll_parses_real_wire_shape() {
    let handle = pat_handle();
    let base = Url::parse("https://api.github.com").unwrap();

    let (events, _cursor) =
        testkit::run_prs_poll(handle.as_ref(), &base, TEST_OWNER, TEST_REPO, None, 5)
            .await
            .expect("live PRs poll");

    // hello-world has a few historical PRs (or zero if all closed/hidden).
    // Don't assert non-empty; just assert the parse succeeded and any events
    // carry the right labels.
    for e in &events {
        assert!(e.labels.contains("kind:pr"));
        assert_eq!(e.source_ref.kind, "pr");
    }
}

#[ignore = "live API; needs GITHUB_TOKEN. Run with --run-ignored only smoke_live_github"]
#[tokio::test]
async fn live_commits_poll_parses_real_wire_shape() {
    let handle = pat_handle();
    let base = Url::parse("https://api.github.com").unwrap();

    let (events, cursor) = testkit::run_commits_poll(
        handle.as_ref(),
        &base,
        TEST_OWNER,
        TEST_REPO,
        None,
        Some("master".into()), // hello-world uses `master`, not `main`
        5,
    )
    .await
    .expect("live commits poll");

    assert!(
        !events.is_empty(),
        "octocat/hello-world has stable commits on master"
    );
    for e in &events {
        assert!(e.labels.contains("kind:commit"));
        assert_eq!(e.source_ref.kind, "commit");
        assert!(e.source_ref.system_id.starts_with("gh:octocat/hello-world@"));
    }
    assert!(cursor.last_sha.is_some(), "cursor recorded a sha");
}

#[ignore = "live API; needs GITHUB_TOKEN. Run with --run-ignored only smoke_live_github"]
#[tokio::test]
async fn live_full_connector_poll_cycle() {
    let handle = pat_handle();
    let connector =
        GitHubConnector::with_base_url(TEST_OWNER, TEST_REPO, "https://api.github.com").unwrap();

    let outcome = connector
        .poll(&PollContext::new(
            handle,
            None,
            30, // small budget across 3 resources → ~10 per resource
            CancellationToken::new(),
        ))
        .await
        .expect("live full poll");

    // Don't pin a specific count — just verify the merged outcome shape.
    let kinds: std::collections::BTreeSet<String> = outcome
        .events
        .iter()
        .flat_map(|e| e.labels.iter().cloned())
        .collect();
    // At minimum issues and commits should be present; PRs may be empty on
    // octocat/hello-world depending on visibility.
    assert!(
        kinds.contains("kind:issue") || kinds.contains("kind:commit"),
        "at least one of issue/commit present in merged outcome"
    );

    let next = outcome.next_cursor.expect("cursor");
    let parsed: serde_json::Value = serde_json::from_str(&next).unwrap();
    assert_eq!(parsed["v"], 1);
}
