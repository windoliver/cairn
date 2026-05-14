//! Local HTTP server for the desktop GUI alpha.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use tower_http::cors::CorsLayer;

use crate::{
    model::{DesktopReconcileApplyRequest, DesktopReconcilePreviewRequest},
    repository::DesktopRepository,
};

/// Shared server state.
#[derive(Debug, Clone)]
pub struct DesktopServerState {
    repo: Arc<DesktopRepository>,
}

/// Build the desktop alpha router.
#[must_use]
pub fn router(repo: DesktopRepository) -> Router {
    let state = DesktopServerState {
        repo: Arc::new(repo),
    };
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/vault", get(vault))
        .route("/api/v1/folders", get(folders))
        .route("/api/v1/records", get(records))
        .route("/api/v1/records/{id}", get(record))
        .route("/api/v1/graph", get(graph))
        .route("/api/v1/search", get(search))
        .route("/api/v1/lint", get(lint))
        .route("/api/v1/reconcile/preview", post(reconcile_preview))
        .route("/api/v1/reconcile/apply", post(reconcile_apply))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn vault(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopVaultSummary> {
    Json(state.repo.vault())
}

async fn folders(
    State(state): State<DesktopServerState>,
) -> Json<Vec<crate::model::DesktopFolder>> {
    Json(state.repo.folders())
}

async fn records(
    State(state): State<DesktopServerState>,
) -> Json<Vec<crate::model::DesktopRecordSummary>> {
    Json(state.repo.records())
}

async fn record(
    State(state): State<DesktopServerState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.repo.record(&id) {
        Some(record) => (StatusCode::OK, Json(record)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "record_not_found",
                "message": "Record was not found"
            })),
        )
            .into_response(),
    }
}

async fn graph(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopGraph> {
    Json(state.repo.graph())
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn search(
    State(state): State<DesktopServerState>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<crate::model::DesktopSearchResult>> {
    let Some(q) = query.q.filter(|q| !q.trim().is_empty()) else {
        return Json(Vec::new());
    };
    Json(state.repo.search(&q))
}

async fn lint(
    State(state): State<DesktopServerState>,
) -> Json<Vec<crate::model::DesktopLintFinding>> {
    Json(state.repo.lint_findings())
}

async fn reconcile_preview(
    State(state): State<DesktopServerState>,
    Json(request): Json<DesktopReconcilePreviewRequest>,
) -> Json<crate::model::DesktopReconcilePreview> {
    Json(state.repo.preview_reconcile(request))
}

async fn reconcile_apply(
    State(state): State<DesktopServerState>,
    Json(request): Json<DesktopReconcileApplyRequest>,
) -> Json<crate::model::DesktopReconcileApplyResult> {
    Json(state.repo.apply_reconcile(request))
}
