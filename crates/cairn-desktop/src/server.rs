//! Local HTTP server for the desktop GUI alpha.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};

use crate::{
    model::{DesktopReconcileApplyRequest, DesktopReconcilePreviewRequest},
    repository::DesktopRepository,
};

/// Default localhost address used by the desktop renderer and preload bridge.
#[must_use]
pub fn default_desktop_server_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 4000))
}

/// Shared server state.
#[derive(Debug, Clone)]
pub struct DesktopServerState {
    repo: Arc<DesktopRepository>,
}

/// Build the desktop alpha router without auth — kept for tests and
/// development. Production callers should use [`router_with_auth`].
pub fn router(repo: DesktopRepository) -> Router {
    router_with_auth(repo, None)
}

/// Build the router with an optional bearer token. When `token` is
/// `Some`, every non-`/health` route requires `Authorization: Bearer <t>`
/// and CORS is restricted; otherwise the router is open (the existing
/// permissive shape) and intended only for tests / local dev.
pub fn router_with_auth(repo: DesktopRepository, token: Option<String>) -> Router {
    let state = DesktopServerState {
        repo: Arc::new(repo),
    };

    let api_routes = Router::new()
        .route("/api/v1/vault", get(vault))
        .route("/api/v1/folders", get(folders))
        .route("/api/v1/records", get(records))
        .route("/api/v1/records/{id}", get(record))
        .route("/api/v1/graph", get(graph))
        .route("/api/v1/session-tree", get(session_tree))
        .route("/api/v1/search", get(search))
        .route("/api/v1/lint", get(lint))
        .route("/api/v1/sre", get(sre))
        .route("/api/v1/reconcile/preview", post(reconcile_preview))
        .route("/api/v1/reconcile/apply", post(reconcile_apply))
        .with_state(state.clone());

    let (api_routes, cors) = if let Some(t) = token {
        let auth_state = AuthState {
            token: Arc::new(t),
        };
        let guarded = api_routes.layer(from_fn_with_state(auth_state, require_bearer));
        // Packaged renderer is the only legitimate caller; lock to its
        // origins. Electron's file:// renderer reports Origin: null;
        // localhost is for the Vite dev server.
        let restricted = CorsLayer::new()
            .allow_origin(AllowOrigin::list([
                HeaderValue::from_static("null"),
                HeaderValue::from_static("http://127.0.0.1:5173"),
                HeaderValue::from_static("http://localhost:5173"),
            ]))
            .allow_headers(AllowHeaders::list([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
            ]));
        (guarded, restricted)
    } else {
        (api_routes, CorsLayer::permissive())
    };

    Router::new()
        .route("/health", get(health))
        .with_state(state)
        .merge(api_routes)
        .layer(cors)
}

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
}

async fn require_bearer(
    State(state): State<AuthState>,
    req: Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let presented = header.strip_prefix("Bearer ").unwrap_or("");
    // constant-time-ish: presented and expected lengths first, then
    // byte compare. ConstantTimeEq isn't worth a new dep for a
    // local-only token in this slice.
    if presented.len() != state.token.len() || presented.as_bytes() != state.token.as_bytes() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
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

async fn session_tree(
    State(state): State<DesktopServerState>,
) -> Json<crate::model::DesktopSessionTree> {
    Json(state.repo.session_tree())
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

async fn sre(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopSreReport> {
    Json(state.repo.sre_report())
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
