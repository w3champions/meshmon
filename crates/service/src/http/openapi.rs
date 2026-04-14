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

/// Assemble the `/api/*` router, collecting `#[utoipa::path]` annotations
/// as they're added. Returns the router plus the merged schema.
pub fn api_router() -> (OpenApiRouter<AppState>, utoipa::openapi::OpenApi) {
    let (_router, api) =
        OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi()).split_for_parts();
    (OpenApiRouter::<AppState>::with_openapi(api.clone()), api)
}

/// Return the full OpenAPI document. Callable at runtime (to serve
/// `/api/openapi.json`) and at build time (from the xtask).
pub fn openapi_document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

/// Router that mounts Swagger UI at `/api/docs` and serves the raw schema
/// at `/api/openapi.json`. Merged into the root axum router.
pub fn swagger_router() -> Router<AppState> {
    let schema = openapi_document();
    SwaggerUi::new("/api/docs")
        .url("/api/openapi.json", schema)
        .into()
}
