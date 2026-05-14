//! HTTP API tests for the desktop backend.

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn health_and_records_endpoints_return_fixture_data() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(health.status(), StatusCode::OK);

    let records = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/records")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(records.status(), StatusCode::OK);
    let body = to_bytes(records.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json.as_array().expect("array").len(), 3);
}

#[tokio::test]
async fn reconcile_preview_rejects_immutable_field_over_http() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/reconcile/preview")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "targetId": "rec-alpha-001",
                        "expectedVersion": 2,
                        "backendHash": "sha256:fixture-alpha-001",
                        "fieldDiff": { "confidence": 0.99 }
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["accepted"], false);
    assert_eq!(json["rejectedFields"][0]["code"], "immutable_field_changed");
}

#[tokio::test]
async fn smoke_loads_all_desktop_alpha_surfaces() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    for path in [
        "/api/v1/vault",
        "/api/v1/folders",
        "/api/v1/records",
        "/api/v1/graph",
        "/api/v1/search?q=alpha",
        "/api/v1/lint",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}
