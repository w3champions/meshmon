//! OpenAPI 3.1 schema emission and Swagger UI mounting.
//!
//! Handlers under `/api/*` are annotated with `#[utoipa::path(...)]` and
//! registered through [`utoipa_axum::routes!`]. `utoipa-axum` collects
//! those registrations at build time into a single [`utoipa::openapi::OpenApi`]
//! document, served at `/api/openapi.json`. Swagger UI at `/api/docs`
//! loads that document at runtime.
//!
//! T04 registers **zero** annotated handlers — the document ships with an
//! empty `paths` object. T06 (agent API is Protobuf-only, excluded) and T09
//! (user API) are the first producers.
//!
//! The `cargo xtask openapi` command (see `xtask/`) imports
//! [`openapi_document`] to regenerate `frontend/src/api/openapi.json` for
//! the TS codegen pipeline (T17).

use crate::state::AppState;
use axum::Router;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

/// Top-level `#[derive(OpenApi)]` anchor. `utoipa_axum` merges route-level
/// specs into this at build time; the `info` block below is the only
/// hand-authored part of the schema.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "meshmon Service API",
        description = "Operator-facing HTTP API for the meshmon service. \
                       Agent-facing endpoints (Protobuf) are documented separately in \
                       `crates/protocol/proto/meshmon.proto`.",
        version = env!("CARGO_PKG_VERSION"),
    )
)]
struct ApiDoc;

/// Assemble the `/api/*` router seeded with the `ApiDoc` metadata. T06+
/// add handlers here via `.routes(utoipa_axum::routes!(...))` chained onto
/// the returned value; callers split the result into the axum `Router`
/// and the collected [`utoipa::openapi::OpenApi`] via `.split_for_parts()`
/// at wire-up time.
pub fn api_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
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
