//! `GET /api/session` — session probe + minimal body the SPA hydrates
//! its auth store from.
//!
//! The SPA calls this endpoint before rendering any authenticated
//! view. A 401 means "bounce to `/login`"; a 200 returns the JSON body
//! below. The response is intentionally small: version + username.
//!
//! Grafana / Alertmanager base URLs used to live in this endpoint's
//! body; they no longer do. Those services are now same-origin
//! (`/grafana`, `/alertmanager`) and their bases are hardcoded in
//! the frontend.

use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

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
pub async fn session(
    State(state): State<AppState>,
    auth_session: AuthSession,
) -> Json<SessionResponse> {
    // `login_required!` on this router guarantees an authenticated user
    // before the handler runs. If `user` is `None` here, the layer
    // config is broken — expect-panic is the right response because a
    // 200 with no identity would silently leak build info to anonymous
    // callers.
    let username = auth_session
        .user
        .as_ref()
        .expect("login_required guarantees an authenticated user")
        .username
        .clone();
    Json(SessionResponse {
        version: state.build.version.to_string(),
        username,
    })
}
