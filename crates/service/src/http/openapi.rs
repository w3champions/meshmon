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
        crate::campaign::dto::CampaignDto,
        crate::campaign::dto::CreateCampaignRequest,
        crate::campaign::dto::DetailPairIdentifier,
        crate::campaign::dto::DetailRequest,
        crate::campaign::dto::DetailResponse,
        crate::campaign::dto::DetailScope,
        crate::campaign::dto::EditCampaignRequest,
        crate::campaign::dto::EditPairDto,
        crate::campaign::dto::ErrorEnvelope,
        crate::campaign::dto::EvaluationCandidateDto,
        crate::campaign::dto::EvaluationDto,
        crate::campaign::dto::EvaluationPairDetailDto,
        crate::campaign::dto::EvaluationResultsDto,
        crate::campaign::dto::ForcePairRequest,
        crate::campaign::dto::PairDto,
        crate::campaign::dto::PatchCampaignRequest,
        crate::campaign::dto::PreviewDispatchResponse,
        crate::campaign::broker::CampaignStreamEvent,
        crate::campaign::model::CampaignState,
        crate::campaign::model::EvaluationMode,
        crate::campaign::model::MeasurementKind,
        crate::campaign::model::PairResolutionState,
        crate::campaign::model::ProbeProtocol,
        crate::catalogue::dto::BulkReenrichRequest,
        crate::catalogue::dto::CatalogueEntryDto,
        crate::catalogue::dto::ErrorEnvelope,
        crate::catalogue::dto::ListResponse,
        crate::catalogue::dto::MapBucket,
        crate::catalogue::dto::MapResponse,
        crate::catalogue::dto::PasteInvalid,
        crate::catalogue::dto::PasteMetadata,
        crate::catalogue::dto::PasteRequest,
        crate::catalogue::dto::PasteResponse,
        crate::catalogue::dto::PasteSkippedSummary,
        crate::catalogue::dto::PatchRequest,
        crate::catalogue::dto::SortBy,
        crate::catalogue::dto::SortDir,
        crate::catalogue::shapes::Polygon,
        crate::catalogue::events::CatalogueEvent,
        crate::catalogue::model::CatalogueSource,
        crate::catalogue::model::EnrichmentStatus,
        crate::catalogue::repo::AsnFacet,
        crate::catalogue::repo::CityFacet,
        crate::catalogue::repo::CountryFacet,
        crate::catalogue::repo::FacetsResponse,
        crate::catalogue::repo::NetworkFacet,
        crate::http::alerts_proxy::AlertSummary,
        crate::http::auth::LoginRequest,
        crate::http::auth::LoginResponse,
        crate::http::history::HistoryDestinationDto,
        crate::http::history::HistoryMeasurementDto,
        crate::http::history::HistorySourceDto,
        crate::http::metrics_proxy::InstantQuery,
        crate::http::metrics_proxy::RangeQuery,
        crate::http::path_overview::LatestByProtocol,
        crate::http::path_overview::PathMetrics,
        crate::http::path_overview::PathOverviewResponse,
        crate::http::path_overview::WindowBounds,
        crate::http::user_api::AgentSummary,
        crate::http::user_api::CatalogueCoordinates,
        crate::http::user_api::RouteSnapshotDetail,
        crate::http::user_api::RouteSnapshotSummary,
        crate::http::user_api::RoutesPage,
        crate::ingestion::json_shapes::HopJson,
        crate::ingestion::json_shapes::HopIpJson,
        crate::ingestion::json_shapes::PathSummaryJson,
        crate::http::session::SessionResponse,
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
        // Static segments are matched before `{id}` path params by `matchit`
        // regardless of insertion order; registering routes/latest first here
        // is a readability convention.
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
        .routes(utoipa_axum::routes!(crate::http::session::session))
        .routes(utoipa_axum::routes!(crate::http::alerts_proxy::list_alerts))
        .routes(utoipa_axum::routes!(crate::http::alerts_proxy::get_alert))
        .routes(utoipa_axum::routes!(
            crate::http::metrics_proxy::query_instant
        ))
        .routes(utoipa_axum::routes!(
            crate::http::metrics_proxy::query_range
        ))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::paste))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::list))
        // Static segments are matched before `{id}` path params by `matchit`
        // regardless of insertion order; registering `/api/catalogue/map`
        // and `/api/catalogue/facets` before `/api/catalogue/{id}` here is
        // a readability convention.
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::map))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::facets))
        // SSE stream lives alongside the other catalogue routes. The static
        // `/stream` segment is matched before the `{id}` path param by
        // `matchit`; registering it here keeps the readability convention.
        .routes(utoipa_axum::routes!(
            crate::catalogue::sse::catalogue_stream
        ))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::get_one))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::patch))
        .routes(utoipa_axum::routes!(crate::catalogue::handlers::delete))
        // Static `/api/catalogue/reenrich` listed before `/{id}/reenrich` as
        // the same readability convention; `matchit` would prefer the static
        // segment regardless.
        .routes(utoipa_axum::routes!(
            crate::catalogue::handlers::reenrich_many
        ))
        .routes(utoipa_axum::routes!(
            crate::catalogue::handlers::reenrich_one
        ))
        // Campaign CRUD. Static segments (`/preview-dispatch-count`,
        // `/force_pair`, `/start`, `/stop`, `/edit`, `/pairs`) are matched
        // before the `{id}`-prefixed routes by `matchit` regardless of
        // registration order; keeping list/create first is a readability
        // convention consistent with the catalogue block above.
        .routes(utoipa_axum::routes!(crate::campaign::handlers::create))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::list))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::get_one))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::patch))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::delete))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::start))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::stop))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::edit))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::force_pair))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::pairs))
        .routes(utoipa_axum::routes!(
            crate::campaign::handlers::preview_dispatch_count
        ))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::evaluate))
        .routes(utoipa_axum::routes!(
            crate::campaign::handlers::get_evaluation
        ))
        .routes(utoipa_axum::routes!(crate::campaign::handlers::detail))
        // SSE stream carries campaign lifecycle + pair-settle events. The
        // static `/stream` segment is matched before any future `{id}`
        // path param by `matchit`; registering it alongside the other
        // campaign routes keeps the readability convention consistent
        // with the catalogue block above.
        .routes(utoipa_axum::routes!(crate::campaign::sse::campaign_stream))
        // History discovery surfaces backing the `/history/pair` page
        // (spec 04 §6) plus the campaign Raw-tab's measurements feed
        // (T49 addition — joined campaign_pairs + measurements + mtr_traces).
        .routes(utoipa_axum::routes!(crate::http::history::sources))
        .routes(utoipa_axum::routes!(crate::http::history::destinations))
        .routes(utoipa_axum::routes!(crate::http::history::measurements))
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
