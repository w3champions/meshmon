//! `GET /api/session` — session probe + minimal body the SPA hydrates
//! its auth store from.
//!
//! The SPA calls this endpoint before rendering any authenticated
//! view. A 401 means "bounce to `/login`"; a 200 returns the JSON body
//! below. The response is intentionally small: version + username +
//! the agent-liveness thresholds the UI needs to render online / stale
//! / offline badges client-side against `Date.now()`.
//!
//! Grafana / Alertmanager base URLs used to live in this endpoint's
//! body; they no longer do. Those services are now same-origin
//! (`/grafana`, `/alertmanager`) and their bases are hardcoded in
//! the frontend.

use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// Agent-liveness thresholds copied from `[agents]` in the service
/// config. The SPA uses these to compute online / stale / offline state
/// from `AgentSummary.last_seen_at` against the wall clock at render
/// time, instead of reading a server-computed boolean baked in at
/// snapshot-refresh moment (which can lag by `refresh_interval_seconds`
/// and produce a brief "offline" flicker after a fresh push).
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentLivenessConfig {
    /// `[agents].target_active_window_minutes`. After this many minutes
    /// without a `last_seen_at` update an agent is "offline".
    pub target_active_window_minutes: u32,
    /// `[agents].refresh_interval_seconds`. The UI uses `2 *` this as
    /// the "stale" threshold (the window during which the snapshot
    /// could reasonably still be in-flight from the registry refresh).
    pub refresh_interval_seconds: u32,
}

/// Minimal session probe response.
///
/// Write-only on the server — we only ever construct and serialize this
/// type, never parse one back in — so only `Serialize` is derived.
#[derive(Debug, Serialize, ToSchema)]
pub struct SessionResponse {
    /// `CARGO_PKG_VERSION` of the running service — surfaced so the SPA
    /// can warn when the user's loaded bundle is older than the server.
    pub version: String,
    /// Signed-in username. The SPA hydrates its auth store from this
    /// so a hard-refresh tab still knows who's signed in.
    pub username: String,
    /// Thresholds the SPA needs to interpret `AgentSummary.last_seen_at`
    /// without baking the values into the bundle. See
    /// [`AgentLivenessConfig`] for the contract.
    pub agents: AgentLivenessConfig,
}

/// `GET /api/session` — return the session probe body.
///
/// Wired behind the `login_required!` layer in [`crate::http::router`],
/// so unauthenticated requests are short-circuited with a bare 401
/// before reaching this handler. The 401 response is documented here
/// so the generated OpenAPI schema tells SPA clients exactly what to
/// expect.
#[utoipa::path(
    get,
    path = "/api/session",
    tag = "session",
    responses(
        (status = 200, description = "Active session", body = SessionResponse),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn session(State(state): State<AppState>, auth_session: AuthSession) -> Response {
    // `login_required!` on this router guarantees an authenticated user
    // before the handler runs. If `user` is `None` here, a router-wiring
    // regression has bypassed that layer — degrade with a 401 instead
    // of panicking so the error surface stays HTTP-shaped. Mirrors the
    // defensive pattern in `grafana_proxy::inject_grafana_headers`.
    let Some(principal) = auth_session.user.as_ref() else {
        return (StatusCode::UNAUTHORIZED, "not authenticated").into_response();
    };
    let cfg = state.config();
    Json(SessionResponse {
        version: state.build.version.to_string(),
        username: principal.username.clone(),
        agents: AgentLivenessConfig {
            target_active_window_minutes: cfg.agents.target_active_window_minutes,
            refresh_interval_seconds: cfg.agents.refresh_interval_seconds,
        },
    })
    .into_response()
}
