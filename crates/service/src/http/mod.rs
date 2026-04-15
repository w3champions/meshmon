//! Axum router assembly.
//!
//! Composition:
//! - `/healthz`, `/readyz`, `/metrics` — no auth, no OpenAPI (Task 9).
//! - `/api/openapi.json`, `/api/docs/*` — served from the OpenAPI spec
//!   collected across `#[utoipa::path]` handlers (Task 10).
//! - `/api/auth/login` — session-issuing handler on its own sub-router so a
//!   per-IP rate limit attaches to just that path (T05).
//! - `/api/auth/logout` and other `/api/*` handlers — live on the
//!   OpenAPI-collected router (T05, T06, T09).
//!
//! Middleware layering (outside → in on the assembled router):
//! 1. `tower_http::trace::TraceLayer` — request/response logs.
//! 2. `tower_http::compression::CompressionLayer` — gzip for large JSON.
//! 3. `axum_login::AuthManagerLayer` (wrapping the `tower_sessions`
//!    session layer) — applied to the full app so every handler can
//!    extract an optional [`auth::AuthSession`].
//! 4. `auth::login_rate_limit_layer` — attached to the login sub-router
//!    only.

pub mod auth;
pub mod health;
pub mod openapi;

use crate::state::AppState;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

/// Build the full axum router. Callers pass in `AppState`; the router is
/// ready to hand to `axum::serve`. The OpenAPI schema is collected from
/// whatever `#[utoipa::path]` handlers are currently attached to
/// [`openapi::api_router`], then served at `/api/openapi.json` via Swagger UI.
///
/// `/api/auth/login` is wired through a standalone sub-router so the per-IP
/// login rate-limit layer attaches to that single path; every other route
/// (including `/api/auth/logout`) sees only the global session/auth layer.
/// The session middleware is transparent for requests that don't touch the
/// session (health, metrics, OpenAPI JSON, Swagger UI).
///
/// Callers must hand the returned router to
/// `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())`
/// so the peer-addr key extractor on the login rate limit can read
/// `ConnectInfo<SocketAddr>`.
pub fn router(state: AppState) -> Router {
    use crate::http::auth::{
        auth_manager_layer, login, login_rate_limit_layer, session_layer, ConfigAuthBackend,
    };
    use axum::routing::post;

    let (api_axum, api_schema) = openapi::api_router().split_for_parts();

    let backend = ConfigAuthBackend::new(state.config.clone());
    let (session_mgr, _store) = session_layer();
    let auth_layer = auth_manager_layer(backend, session_mgr);

    let login_limit = login_rate_limit_layer(state.config().service.trust_forwarded_headers);
    let login_router = Router::<AppState>::new()
        .route("/api/auth/login", post(login))
        .layer(login_limit);

    let grpc_router = crate::grpc::routes(state.clone());

    Router::new()
        .merge(health::router())
        .merge(openapi::swagger_router(api_schema))
        .merge(login_router)
        .merge(grpc_router)
        .merge(api_axum)
        .layer(auth_layer)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Re-export [`openapi::openapi_document`] so `xtask` and tests can call it
/// without knowing the submodule layout.
pub use openapi::openapi_document;
