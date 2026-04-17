//! OpenAPI 3.1 schema emission and Swagger UI mounting.
//!
//! Handlers under `/api/*` are annotated with `#[utoipa::path(...)]` and
//! registered through [`utoipa_axum::routes!`]. `utoipa-axum` collects
//! those registrations at build time into a single [`utoipa::openapi::OpenApi`]
//! document, served at `/api/openapi.json`. Swagger UI at `/api/docs`
//! loads that document at runtime.
//!
//! Handlers that live *outside* the `utoipa_axum`-collected router
//! (login, logout — both hosted on dedicated sub-routers so they can
//! bypass the `login_required!` gate) still need their `#[utoipa::path]`
//! metadata in the emitted schema, so they're listed explicitly in the
//! `paths(...)` attribute on `ApiDoc` below. Handlers that live on
//! [`api_router`] are picked up automatically.
//!
//! The `cargo xtask openapi` command (see `xtask/`) imports
//! [`openapi_document`] to regenerate `frontend/src/api/openapi.gen.json`
//! for the TS codegen pipeline.

use crate::state::AppState;
use axum::Router;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

/// Top-level `#[derive(OpenApi)]` anchor. `utoipa_axum` merges route-level
/// specs into this at build time; the `info` block below is the only
/// hand-authored part of the schema.
///
/// `/api/auth/login` and `/api/auth/logout` are listed in `paths(...)`
/// because their handlers are wired onto standalone sub-routers
/// (unauthenticated; login has its own rate-limit layer). The
/// `#[utoipa::path]` metadata still has to land in the document, so we
/// name the handlers explicitly. Everything else comes from
/// [`api_router`].
#[derive(OpenApi)]
#[openapi(
    info(
        title = "meshmon Service API",
        description = "Operator-facing HTTP API for the meshmon service. \
                       Agent-facing endpoints (Protobuf) are documented separately in \
                       `crates/protocol/proto/meshmon.proto`.",
        version = env!("CARGO_PKG_VERSION"),
    ),
    paths(crate::http::auth::login, crate::http::auth::logout),
    components(schemas(
        crate::http::alerts_proxy::AlertSummary,
        crate::http::auth::LoginRequest,
        crate::http::auth::LoginResponse,
        crate::http::metrics_proxy::InstantQuery,
        crate::http::metrics_proxy::RangeQuery,
        crate::http::path_overview::LatestByProtocol,
        crate::http::path_overview::PathMetrics,
        crate::http::path_overview::PathOverviewResponse,
        crate::http::path_overview::WindowBounds,
        crate::http::user_api::AgentSummary,
        crate::http::user_api::RouteSnapshotDetail,
        crate::http::user_api::RouteSnapshotSummary,
        crate::http::user_api::RoutesPage,
        crate::ingestion::json_shapes::HopJson,
        crate::ingestion::json_shapes::HopIpJson,
        crate::ingestion::json_shapes::PathSummaryJson,
        crate::http::web_config::WebConfigResponse,
    )),
)]
struct ApiDoc;

/// Assemble the `/api/*` router seeded with the `ApiDoc` metadata. Later
/// tasks add handlers here via `.routes(utoipa_axum::routes!(...))`
/// chained onto the returned value; callers split the result into the
/// axum `Router` and the collected [`utoipa::openapi::OpenApi`] via
/// `.split_for_parts()` at wire-up time.
///
/// Every handler registered through this router runs behind the
/// `login_required!` layer once assembled by [`crate::http::router`]. If
/// a handler must stay anonymous (like login/logout), mount it on its
/// own sub-router and document it via `paths(...)` on `ApiDoc` above.
pub fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        .routes(utoipa_axum::routes!(crate::http::user_api::list_agents))
        .routes(utoipa_axum::routes!(crate::http::user_api::get_agent))
        // routes/latest MUST come before routes/{snapshot_id} so the static
        // segment "latest" is matched before the catch-all path parameter.
        .routes(utoipa_axum::routes!(
            crate::http::user_api::get_route_latest
        ))
        .routes(utoipa_axum::routes!(crate::http::user_api::get_route_by_id))
        .routes(utoipa_axum::routes!(crate::http::user_api::list_routes))
        .routes(utoipa_axum::routes!(
            crate::http::path_overview::path_overview
        ))
        .routes(utoipa_axum::routes!(
            crate::http::user_api::list_recent_routes
        ))
        .routes(utoipa_axum::routes!(crate::http::web_config::web_config))
        .routes(utoipa_axum::routes!(crate::http::alerts_proxy::list_alerts))
        .routes(utoipa_axum::routes!(crate::http::alerts_proxy::get_alert))
        .routes(utoipa_axum::routes!(
            crate::http::metrics_proxy::query_instant
        ))
        .routes(utoipa_axum::routes!(
            crate::http::metrics_proxy::query_range
        ))
}

/// Build the full OpenAPI document, including every `#[utoipa::path]`
/// handler attached to [`api_router`]. Callable at runtime (to serve
/// `/api/openapi.json`) and at build time (from `xtask`).
pub fn openapi_document() -> utoipa::openapi::OpenApi {
    let (_router, schema) = api_router().split_for_parts();
    schema
}

/// Router that mounts Swagger UI at `/api/docs` and serves the supplied
/// schema at `/api/openapi.json`. The caller is responsible for producing
/// a schema that reflects every attached handler (typically via
/// [`api_router`] + `.split_for_parts()`).
pub fn swagger_router(schema: utoipa::openapi::OpenApi) -> Router<AppState> {
    SwaggerUi::new("/api/docs")
        .url("/api/openapi.json", schema)
        .into()
}
