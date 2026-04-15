//! Axum router assembly.
//!
//! Composition:
//! - `/healthz`, `/readyz`, `/metrics` — no auth, no OpenAPI (Task 9).
//! - `/api/openapi.json`, `/api/docs/*` — served from the OpenAPI spec
//!   collected across `#[utoipa::path]` handlers (Task 10).
//! - `/api/*` — user + agent API handlers (added in T05, T06, T09).
//!
//! Middleware layering (outside → in):
//! 1. `tower_http::trace::TraceLayer` — request/response logs.
//! 2. `tower_http::compression::CompressionLayer` — gzip for large JSON.
//!
//! Session middleware (T05) and rate limiting (T05) layer between `1` and
//! `2` once they land.

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
pub fn router(state: AppState) -> Router {
    let (api_axum, api_schema) = openapi::api_router().split_for_parts();

    Router::new()
        .merge(health::router())
        .merge(openapi::swagger_router(api_schema))
        .merge(api_axum)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Re-export [`openapi::openapi_document`] so `xtask` and tests can call it
/// without knowing the submodule layout.
pub use openapi::openapi_document;
