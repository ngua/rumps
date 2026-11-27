//! Axum router and HTTP handlers.

use axum::extract::State;
use axum::routing::{get, post};
use axum::Json;
use bytes::Bytes;

use crate::error::{Error, Result};
use crate::rpc::Request;
use crate::AppState;

/// HTTP router for the RUMPS server.
pub(crate) struct Router;

impl Router {
    /// Builds the [`axum::Router`] with all routes wired to `state`.
    pub(crate) fn build(state: AppState) -> axum::Router {
        axum::Router::new()
            .route("/health", get(Self::health))
            .route("/status", get(Self::status))
            .route("/rpc", post(Self::rpc))
            .with_state(state)
    }

    async fn health() -> Json<serde_json::Value> {
        Json(serde_json::json!({"ok": true}))
    }

    async fn status(State(_state): State<AppState>) -> Json<serde_json::Value> {
        // Stub; filled in once session/transaction managers land (task 11.2).
        Json(serde_json::json!({}))
    }

    async fn rpc(
        State(state): State<AppState>,
        body: Bytes,
    ) -> Result<axum::response::Response> {
        let req = serde_json::from_slice::<Request>(&body)
            .map_err(|e| Error::BadRequest(e.to_string()))?;

        req.dispatch(&state)
            .await
            .map(axum::response::IntoResponse::into_response)
    }
}
