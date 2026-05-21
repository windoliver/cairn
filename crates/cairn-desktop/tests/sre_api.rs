//! SRE dashboard HTTP API tests for the desktop backend.

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn sre_endpoint_returns_body_free_report() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/sre")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let raw = String::from_utf8(body.to_vec()).expect("utf8");
    let json: Value = serde_json::from_str(&raw).expect("json");

    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["vault"]["name"], "Desktop Alpha Fixture");
    for section in [
        "workflow",
        "rehydration",
        "projection",
        "search",
        "gates",
        "privacy",
    ] {
        assert!(json.get(section).is_some(), "missing {section}: {raw}");
    }
    for forbidden in [
        "SECRET_PRIVATE_TOKEN",
        "/Users/alice",
        "private body",
        "query text",
        "Markdown body with [[Reconcile review]].",
        "Edits must pass through backend validation.",
        "One stale source hash is intentionally present for lint.",
    ] {
        assert!(
            !raw.contains(forbidden),
            "SRE endpoint leaked forbidden fragment `{forbidden}`: {raw}"
        );
    }
}
