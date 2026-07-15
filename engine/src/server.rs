//! Control API HTTP server (inbound — PHP calls into the engine).
//!
//! Every route below requires header `X-Engine-Api-Key` to match
//! `control_api.api_key` from the config, except `/health` which is
//! unauthenticated (basic liveness probing).
//!
//! Phase 3 wired the two queue routes into the real playback engine, and
//! `/skip`/`/metadata` via the shared `ControlSignals` handle in
//! `control.rs`. Phase 4 wires the last remaining stub, `/streamer/disconnect`,
//! directly to `harbor::LiveState::force_disconnect` -- this route doesn't
//! need `ControlSignals`' poll-once-per-loop-iteration indirection at all
//! (unlike `/skip`/`/metadata`, which affect *what the pipeline loop plays
//! next*): forcibly closing a TCP connection is an immediate, self-contained
//! action independent of the pipeline's loop cadence.

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
use std::sync::Arc;

use crate::control::ControlSignals;
use crate::harbor::LiveState;
use crate::queue::TrackQueues;

#[derive(Clone)]
pub struct AppState {
    pub control_api_key: String,
    /// Phase 3: the real priority queues (`requests`/`interrupting_requests`)
    /// that `queue_push_handler`/`queue_empty_handler` now actually mutate,
    /// and that the playback pipeline (`pipeline.rs`) pops from.
    pub queues: Arc<TrackQueues>,
    /// `/skip` + `/metadata` signal handle shared with `pipeline.rs`'s loop
    /// -- see `control.rs`'s module doc.
    pub control: Arc<ControlSignals>,
    /// Phase 4: shared live-DJ harbor state (`harbor.rs`) -- used here only
    /// by `streamer_disconnect_handler`.
    pub live: Arc<LiveState>,
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

/// SPEC.md C.9's `add_skip_command` (`source.skip(s)`): signals
/// `pipeline.rs`'s loop to abandon the rest of the currently-playing
/// track's body and jump straight to the crossfade into the next track, as
/// if the body had naturally ended here. Fire-and-forget -- dispatches the
/// signal and returns immediately without waiting for the pipeline to act
/// on it (matching the rest of this control API's async, non-blocking
/// handlers). See `control.rs` and `pipeline.rs` for why this takes effect
/// on the pipeline's next loop check rather than instantaneously.
async fn skip_handler(State(state): State<AppState>) -> impl IntoResponse {
    tracing::info!("skip requested");
    state.control.request_skip();
    (StatusCode::OK, Json(json!({"ok": true})))
}

#[derive(Debug, Deserialize)]
struct PushBody {
    uri: String,
}

async fn queue_push_handler(
    State(state): State<AppState>,
    Path(queue): Path<String>,
    Json(body): Json<PushBody>,
) -> impl IntoResponse {
    match state.queues.push(&queue, body.uri.clone()) {
        Ok(()) => {
            tracing::info!("enqueued to {queue}: {}", body.uri);
            (StatusCode::OK, Json(json!({"ok": true}))).into_response()
        }
        Err(e) => {
            tracing::warn!("rejected enqueue to {queue}: {e}");
            (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))).into_response()
        }
    }
}

async fn queue_empty_handler(
    State(state): State<AppState>,
    Path(queue): Path<String>,
) -> impl IntoResponse {
    let empty = state.queues.is_empty(&queue);
    tracing::info!("queue empty check for {queue}: {empty}");
    (StatusCode::OK, Json(json!({"empty": empty})))
}

/// SPEC.md C.9's `add_custom_metadata_command` (`insert_metadata`):
/// stages `meta` as an override to be merged onto the currently-playing
/// track's metadata and re-pushed through `FeedbackDedup::maybe_send` on
/// `pipeline.rs`'s next loop iteration (see `control.rs` and `pipeline.rs`
/// for the exact mechanism and the same "next check, not instantaneous"
/// caveat as `/skip`). Fire-and-forget, same as every other handler here.
async fn metadata_handler(
    State(state): State<AppState>,
    Json(meta): Json<HashMap<String, String>>,
) -> impl IntoResponse {
    tracing::info!("received metadata: {meta:?}");
    state.control.set_metadata_override(meta);
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// Real as of Phase 4: forcibly closes the currently-live harbor TCP
/// connection, if any (SPEC.md's `input_streamer.stop` telnet/HTTP
/// command), triggering the same disconnect sequence (SPEC.md C.4) a
/// voluntary client disconnect would -- see `harbor::LiveState::force_disconnect`.
/// Fire-and-forget, same as every other handler here: `200 {"ok": true}` is
/// returned regardless of whether a connection was actually present to
/// disconnect (matching `/skip`/`/metadata`'s "dispatch and don't wait"
/// style; the caller has no obvious use for a false-vs-true distinction
/// here since "nothing to disconnect" isn't an error condition).
async fn streamer_disconnect_handler(State(state): State<AppState>) -> impl IntoResponse {
    let disconnected = state.live.force_disconnect();
    tracing::info!("streamer disconnect requested (had active session: {disconnected})");
    (StatusCode::OK, Json(json!({"ok": true})))
}
