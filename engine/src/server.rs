//! Control API HTTP server (inbound — PHP calls into the engine).
//!
//! Every route below requires header `X-Engine-Api-Key` to match
//! `control_api.api_key` from the config, except `/health` which is
//! unauthenticated (basic liveness probing).
//!
//! This phase has no real audio/queue/metadata engine behind it: handlers
//! just log what they were asked to do and return a canned success
//! response. Phases 3-5 wire these into the real playback engine.

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Clone)]
pub struct AppState {
    pub control_api_key: String,
}

/// Builds the full router: `/health` is unauthenticated, everything else
/// requires `X-Engine-Api-Key`.
pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/skip", post(skip_handler))
        .route("/queue/:queue/push", post(queue_push_handler))
        .route("/queue/:queue/empty", get(queue_empty_handler))
        .route("/metadata", post(metadata_handler))
        .route("/streamer/disconnect", post(streamer_disconnect_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ));

    Router::new()
        .route("/health", get(health_handler))
        .merge(protected)
        .with_state(state)
}

/// Middleware enforcing `X-Engine-Api-Key` on every route it's applied to.
async fn require_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let provided = headers
        .get("X-Engine-Api-Key")
        .and_then(|v| v.to_str().ok());

    if provided != Some(state.control_api_key.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    next.run(request).await
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn skip_handler() -> impl IntoResponse {
    tracing::info!("skip requested");
    (StatusCode::OK, Json(json!({"ok": true})))
}

#[derive(Debug, Deserialize)]
struct PushBody {
    uri: String,
}

async fn queue_push_handler(
    Path(queue): Path<String>,
    Json(body): Json<PushBody>,
) -> impl IntoResponse {
    tracing::info!("enqueue to {queue}: {}", body.uri);
    (StatusCode::OK, Json(json!({"ok": true})))
}

async fn queue_empty_handler(Path(queue): Path<String>) -> impl IntoResponse {
    tracing::info!("queue empty check for {queue}: no real queue in this phase, reporting empty");
    (StatusCode::OK, Json(json!({"empty": true})))
}

async fn metadata_handler(Json(meta): Json<HashMap<String, String>>) -> impl IntoResponse {
    tracing::info!("received metadata: {meta:?}");
    (StatusCode::OK, Json(json!({"ok": true})))
}

async fn streamer_disconnect_handler() -> impl IntoResponse {
    tracing::info!("streamer disconnect requested");
    (StatusCode::OK, Json(json!({"ok": true})))
}
