//! Axum router assembly.
//!
//! Composition:
//! - `/healthz`, `/readyz`, `/metrics` — no auth, no OpenAPI.
//! - `/api/openapi.json`, `/api/docs/*` — served from the OpenAPI spec
//!   collected across `#[utoipa::path]` handlers.
//! - `/api/auth/login` — session-issuing handler on its own sub-router so a
//!   per-IP rate limit attaches to just that path.
//! - `/api/auth/logout` — its own sub-router, unauthenticated, no rate
//!   limit. Logout must stay idempotent for anonymous callers (e.g.
//!   stale tab clicking "Log out" after cookie expiry); placing it
//!   behind `login_required!` would break that UX.
//! - Every other `/api/*` handler — collected by `utoipa_axum::routes!`
//!   and gated by `axum_login::login_required!` at the router layer.
//!
//! Middleware layering (outside → in on the assembled router):
//! 1. `tower_http::trace::TraceLayer` — request/response logs.
//! 2. `tower_http::compression::CompressionLayer` — gzip for large JSON.
//! 3. `axum_prometheus::PrometheusMetricLayerBuilder` — HTTP request
//!    metrics (`meshmon_service_http_requests_*`). Sits outside
//!    `auth_layer` so 401s from `login_required!` and from the
//!    `/metrics` basic-auth gate are still counted.
//! 4. `axum_login::AuthManagerLayer` (wrapping the `tower_sessions`
//!    session layer) — applied to the full app so every handler can
//!    extract an optional [`auth::AuthSession`].
//! 5. `login_required!(ConfigAuthBackend)` — attached to the
//!    OpenAPI-collected `/api/*` sub-router. Returns 401 for anonymous
//!    callers (API-friendly: SPAs bounce to `/login` themselves).
//! 6. `auth::login_rate_limit_layer` — attached to the login sub-router
//!    only.
//! 7. `metrics_auth::require_basic_auth` — attached to the `/metrics`
//!    sub-router only; no-ops when `[service.metrics_auth]` is unset.
//!    `/healthz` and `/readyz` stay ungated for k8s probes.
//!
//! The embedded React SPA is served via `memory-serve` attached as the
//! axum `fallback_service`:
//! - `/`, `/index.html`, and any unregistered, non-backend path serve
//!   `index.html` (HTML with a `no-cache, no-store, must-revalidate`
//!   header so SPA deploys take effect on next navigation).
//! - Hashed asset paths (e.g. `/assets/index-<hash>.js`) get
//!   `Cache-Control: public, max-age=31536000, immutable` plus an
//!   ETag; `If-None-Match` returns 304.
//! - Unknown `/api/*` paths are caught by a `/api/{*rest}` guard route
//!   that returns 404 explicitly, so the SPA fallback never hijacks a
//!   genuine backend miss. `/healthz`, `/readyz`, and `/metrics` are
//!   exact-match routes and therefore cannot be hijacked.

pub mod alerts_proxy;
pub mod auth;
pub mod health;
pub mod http_client;
pub mod metrics_auth;
pub mod metrics_proxy;
pub mod openapi;
pub mod path_overview;
pub mod user_api;
pub mod web_config;

use crate::state::AppState;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

/// Build the full axum router. Callers pass in `AppState`; the router is
/// ready to hand to `axum::serve`. The OpenAPI schema is collected from
/// whatever `#[utoipa::path]` handlers are currently attached to
/// [`openapi::api_router`], then served at `/api/openapi.json` via Swagger UI.
///
/// Router composition:
///
/// - `/api/auth/login` lives on a standalone sub-router so the per-IP
///   login rate-limit layer attaches to just that path.
/// - `/api/auth/logout` lives on a second standalone sub-router —
///   unauthenticated and unrate-limited so it remains idempotent for
///   anonymous callers.
/// - Everything else under `/api/*` (including [`web_config::web_config`])
///   is collected via `utoipa_axum::routes!` and guarded by the
///   `login_required!` layer, which returns 401 when no session is
///   present.
/// - Health, metrics, OpenAPI JSON and Swagger UI stay outside the
///   authenticated surface.
///
/// Callers must hand the returned router to
/// `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())`
/// so the peer-addr key extractor on the login rate limit can read
/// `ConnectInfo<SocketAddr>`.
pub fn router(state: AppState) -> Router {
    use crate::http::auth::{
        auth_manager_layer, login, login_rate_limit_layer, logout, session_layer, ConfigAuthBackend,
    };
    use axum::routing::{get, post};
    use axum_login::login_required;
    use axum_prometheus::PrometheusMetricLayerBuilder;

    let (api_axum, api_schema) = openapi::api_router().split_for_parts();

    let backend = ConfigAuthBackend::new(state.config.clone());
    let (session_mgr, _store) = session_layer();
    let auth_layer = auth_manager_layer(backend, session_mgr);

    let login_limit = login_rate_limit_layer(state.config().service.trust_forwarded_headers);
    let login_router = Router::<AppState>::new()
        .route("/api/auth/login", post(login))
        .layer(login_limit);

    // Logout stays unauthenticated so anonymous or already-expired
    // sessions can still hit it without hitting the 401 wall — no rate
    // limit (sessions are scarce to begin with).
    let logout_router = Router::<AppState>::new().route("/api/auth/logout", post(logout));

    // Every handler registered via `utoipa_axum::routes!` on
    // [`openapi::api_router`] is protected. Returning a bare 401 (no
    // `login_url`) keeps this API-friendly: SPAs decide their own
    // redirect target rather than following a server-issued 307.
    let api_protected = api_axum.route_layer(login_required!(ConfigAuthBackend));

    let grpc_router = crate::grpc::routes(state.clone());

    // axum-prometheus: emits
    //   meshmon_service_http_requests_total{method, endpoint, status}
    //   meshmon_service_http_requests_duration_seconds{...}
    //   meshmon_service_http_requests_pending{...}
    //
    // The metric names are renamed at build time via the `AXUM_HTTP_*`
    // env vars in `.cargo/config.toml`. We intentionally do NOT use
    // `PrometheusMetricLayerBuilder::with_prefix` — in axum-prometheus 0.10
    // that path only activates (`set_prefix` via `from_layer_only`) when
    // the builder transitions through `with_default_metrics` /
    // `with_metrics_from_fn` to land in the `Paired` state. The plain
    // `.build()` path silently drops the prefix. The build-time env-var
    // knob is the simplest consistent mechanism and is additionally
    // idempotent across integration tests that rebuild the router many
    // times per process (`set_prefix` is `OnceLock::set`-backed and would
    // panic on the second call).
    let prom_layer = PrometheusMetricLayerBuilder::new().build();

    // Split health endpoints so Basic auth attaches to `/metrics` only.
    // `/healthz` and `/readyz` stay ungated so k8s probes and readiness
    // checks never get a 401.
    let metrics_route = Router::<AppState>::new()
        .route("/metrics", get(health::metrics))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            metrics_auth::require_basic_auth,
        ));
    let health_router = Router::<AppState>::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .merge(metrics_route);

    // --- embedded SPA ---
    // memory-serve owns: asset lookup, MIME detection, ETag / 304,
    // per-class Cache-Control, pre-compressed brotli+gzip responses,
    // and SPA fallback-to-index.html for unknown deep links.
    use memory_serve::{load, CacheControl};
    let memory_router = load!()
        .html_cache_control(CacheControl::Custom("no-cache, no-store, must-revalidate"))
        .cache_control(CacheControl::Long)
        .fallback(Some("/index.html"))
        .fallback_status(axum::http::StatusCode::OK)
        .into_router();

    // Unknown `/api` and `/api/*` paths must return a clean JSON 404
    // rather than falling through to the SPA; otherwise TanStack Query
    // can't distinguish a real API error from an HTML error page. Both
    // the bare `/api` and the wildcard are registered — matchit treats
    // them as distinct paths. `/metrics`, `/healthz`, `/readyz` are
    // exact-match routes already registered above, so they can't be
    // hijacked by the SPA fallback either.
    async fn backend_path_404() -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "not_found" })),
        )
    }

    // Layer order (inside → out): auth_layer wraps the routes first so
    // handlers can see the session, then prom_layer so unauthenticated
    // 401s still get counted in the request metrics, then compression,
    // then trace at the outside so logs cover every request.
    Router::new()
        .merge(health_router)
        .merge(openapi::swagger_router(api_schema))
        .merge(login_router)
        .merge(logout_router)
        .merge(grpc_router)
        .merge(api_protected)
        .route("/api", axum::routing::any(backend_path_404))
        .route("/api/{*rest}", axum::routing::any(backend_path_404))
        .layer(auth_layer)
        .layer(prom_layer)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .fallback_service(memory_router)
        .with_state(state)
}

/// Re-export [`openapi::openapi_document`] so `xtask` and tests can call it
/// without knowing the submodule layout.
pub use openapi::openapi_document;
