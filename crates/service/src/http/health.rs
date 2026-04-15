//! Liveness, readiness, and self-metrics endpoints.
//!
//! All three endpoints pass through the session and auth-manager layers but
//! never touch the session extension, so `tower-sessions` is transparent —
//! infrastructure probes work without credentials. They read the
//! process-wide readiness flag exposed by [`AppState::is_ready`].

use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

/// `/healthz` — liveness probe. Returns 200 for as long as the process is
/// up. Never checks downstream dependencies; use `/readyz` for that.
async fn healthz() -> &'static str {
    "ok"
}

/// `/readyz` — readiness probe. Returns 200 once `AppState::mark_ready()`
/// has been called; 503 otherwise (before startup completes or after
/// shutdown begins).
async fn readyz(State(state): State<AppState>) -> Response {
    if state.is_ready() {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

/// `/metrics` — Prometheus text-format self-metrics. T04 ships only
/// `meshmon_service_build_info`; T10 replaces this handler with a real
/// `prometheus` crate registry emitter.
async fn metrics(State(state): State<AppState>) -> Response {
    let body = format!(
        "# HELP meshmon_service_build_info Service build metadata.\n\
         # TYPE meshmon_service_build_info gauge\n\
         meshmon_service_build_info{{version=\"{v}\",commit=\"{c}\"}} 1\n",
        v = state.build.version,
        c = state.build.commit,
    );
    (
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Assemble the health router. Health endpoints live outside `/api/*` so
/// they never conflict with the OpenAPI schema or session middleware.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}
