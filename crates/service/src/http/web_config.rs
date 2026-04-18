//! Stub — replaced by `session.rs` in T34 Task 7.

use crate::http::auth::AuthSession;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Serialize;
use std::collections::HashMap;
use utoipa::ToSchema;

/// Runtime config the frontend needs before it can render dashboards.
///
/// Returned from `GET /api/web-config`; doubles as the SPA's session probe
/// (401 when the caller has no valid session cookie).
///
/// Write-only on the server — we only ever construct and serialize this
/// type, never parse one back in — so only `Serialize` is derived.
/// Matches the pattern used by [`crate::http::auth::LoginResponse`].
#[derive(Debug, Serialize, ToSchema)]
pub struct WebConfigResponse {
    /// `CARGO_PKG_VERSION` of the running service.
    pub version: String,
    /// Signed-in username. The SPA hydrates its auth store from this so the
    /// user menu shows the right handle after a hard refresh.
    pub username: String,
    /// Base URL for embedding Grafana panels (e.g., `https://grafana.example/`).
    /// `None` if Grafana is not configured — omitted from the JSON body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grafana_base_url: Option<String>,
    /// Map of logical dashboard name → Grafana dashboard UID. Empty
    /// when Grafana integration is not configured.
    pub grafana_dashboards: HashMap<String, String>,
    /// Alertmanager base URL for constructing "view in Alertmanager"
    /// deep links (e.g., `https://alertmanager.example/`). `None` when
    /// Alertmanager is not configured — omitted from the JSON body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alertmanager_base_url: Option<String>,
}

/// `GET /api/web-config` — return the frontend runtime config.
///
/// This handler is registered behind the `login_required!` layer, so
/// unauthenticated requests never reach it — they are short-circuited by
/// the middleware with a plain 401. The 401 response is documented here so
/// the generated OpenAPI schema tells SPA clients exactly what to expect.
#[utoipa::path(
    get,
    path = "/api/web-config",
    tag = "web",
    responses(
        (status = 200, description = "Frontend runtime config", body = WebConfigResponse),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn web_config(
    State(state): State<AppState>,
    auth_session: AuthSession,
) -> Json<WebConfigResponse> {
    // `login_required!` on this router guarantees an authenticated user
    // before the handler runs. If `user` is `None` here, the layer config
    // is broken — expect-panic is the right response because a 200 with
    // no identity would silently leak config to anonymous callers.
    let username = auth_session
        .user
        .as_ref()
        .expect("login_required guarantees an authenticated user")
        .username
        .clone();
    let cfg = state.config();
    // NOTE: The `[web]` TOML section was removed in T34 Task 2; the
    // Grafana embed surface moves to the transparent `/grafana/*` proxy
    // in later T34 tasks, which replace this handler with a session
    // endpoint. Until then, keep the field in the response shape but
    // populate with empty/None so the SPA's iframe renders its
    // broken-iframe fallback state.
    Json(WebConfigResponse {
        version: state.build.version.to_string(),
        username,
        grafana_base_url: None,
        grafana_dashboards: HashMap::new(),
        alertmanager_base_url: cfg.upstream.alertmanager_url.clone(),
    })
}
