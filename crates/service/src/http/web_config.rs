//! `GET /api/web-config` — session-gated runtime config for the frontend.
//!
//! The frontend calls this endpoint before rendering any authenticated
//! UI:
//! - 401 means "bounce to the login page" (the `login_required!` layer
//!   returns that status directly; there is no hand-written 401 branch
//!   here).
//! - 200 returns the JSON body described by [`WebConfigResponse`].
//!
//! MVP ships `grafana_base_url = None` and `grafana_dashboards` empty.
//! A later task populates them from `meshmon.toml` once the Grafana
//! integration surface is defined in config.

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
    /// Base URL for embedding Grafana panels (e.g., `https://grafana.example/`).
    /// `None` if Grafana is not configured — omitted from the JSON body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grafana_base_url: Option<String>,
    /// Map of logical dashboard name → Grafana dashboard UID. Empty
    /// when Grafana integration is not configured.
    pub grafana_dashboards: HashMap<String, String>,
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
pub async fn web_config(State(state): State<AppState>) -> Json<WebConfigResponse> {
    Json(WebConfigResponse {
        version: state.build.version.to_string(),
        grafana_base_url: None,
        grafana_dashboards: HashMap::new(),
    })
}
