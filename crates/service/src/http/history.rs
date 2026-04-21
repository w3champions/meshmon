//! `/api/history/*` endpoints.
//!
//! Discovery surfaces for the historic pair view (spec 04 §6):
//! - `GET /api/history/sources` — agents that appear as `source_agent_id`
//!   in any `measurements` row.
//! - `GET /api/history/destinations` — every `destination_ip` with ≥ 1
//!   measurement from the chosen source.
//! - `GET /api/history/measurements` — measurements (+ joined mtr_traces)
//!   for a (source, destination) over a time range.
//!
//! A fourth endpoint `GET /api/campaigns/{id}/measurements` (T49 addition)
//! feeds the Results browser's Raw tab — it lives here for locality with
//! the measurements-attribution SQL.
//!
//! Auth is inherited from the user-API middleware layer; handlers do not
//! take an `AuthSession` extractor.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::types::ipnetwork::IpNetwork;
use utoipa::ToSchema;

use crate::campaign::dto::ErrorEnvelope;
use crate::campaign::model::{MeasurementKind, ProbeProtocol};
use crate::state::AppState;

// All history handlers use `&state.pool` (PgPool is a public field on
// AppState — there is no `pg_pool()` accessor). Auth is inherited from
// the user-API middleware; handlers do not take an `AuthSession` extractor.

/// Shared error mapper for history handlers — all failures collapse to 500.
fn internal_error(scope: &str, err: sqlx::Error) -> Response {
    tracing::error!(scope, error = %err, "history db error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorEnvelope {
            error: "internal".into(),
        }),
    )
        .into_response()
}

// --- sources -----------------------------------------------------------

/// One entry in the `/api/history/sources` list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct HistorySourceDto {
    /// Agent id.
    pub source_agent_id: String,
    /// Display name from `agents_with_catalogue` — the catalogue-derived
    /// name when set, else the agent's own `display_name`, else the id.
    pub display_name: String,
}

/// `GET /api/history/sources` — agents with at least one measurement.
#[utoipa::path(
    get,
    path = "/api/history/sources",
    tag = "history",
    operation_id = "history_sources",
    responses(
        (status = 200, description = "Source list", body = Vec<HistorySourceDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn sources(State(state): State<AppState>) -> Response {
    let pool = &state.pool;
    match sqlx::query_as!(
        HistorySourceDto,
        r#"
        SELECT DISTINCT
               awc.agent_id AS "source_agent_id!",
               COALESCE(awc.catalogue_display_name, awc.agent_display_name, awc.agent_id)
                 AS "display_name!"
          FROM agents_with_catalogue awc
          JOIN measurements m ON m.source_agent_id = awc.agent_id
         ORDER BY 2 ASC
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error("history::sources", e),
    }
}

// --- destinations ------------------------------------------------------

/// Query params for `GET /api/history/destinations`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HistoryDestinationsQuery {
    /// Source agent id — required.
    pub source: String,
    /// Partial match on `destination_ip` or catalogue `display_name`.
    #[serde(default)]
    pub q: Option<String>,
}

/// One destination reachable from a given source.
///
/// `display_name` falls back to `host(destination_ip)` when no catalogue
/// row exists (either never enriched or later deleted). The frontend
/// renders this as "raw IP — no metadata", a supported state rather than
/// a rendering bug — `city`, `country_code`, and `asn` all stay NULL.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct HistoryDestinationDto {
    /// Raw destination IP as a host string.
    pub destination_ip: String,
    /// Catalogue-derived label when known, else the IP string.
    pub display_name: String,
    /// Catalogue city (nullable).
    pub city: Option<String>,
    /// Catalogue country code (nullable).
    pub country_code: Option<String>,
    /// Catalogue ASN (nullable). Postgres `INTEGER` → `i32`.
    pub asn: Option<i32>,
    /// Whether the destination IP is itself a mesh-agent IP.
    pub is_mesh_member: bool,
}

/// `GET /api/history/destinations` — every destination reachable from
/// `source`, optionally narrowed by partial match on IP or display
/// name (case-insensitive).
#[utoipa::path(
    get,
    path = "/api/history/destinations",
    tag = "history",
    operation_id = "history_destinations",
    params(HistoryDestinationsQuery),
    responses(
        (status = 200, description = "Destination list", body = Vec<HistoryDestinationDto>),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn destinations(
    State(state): State<AppState>,
    Query(q): Query<HistoryDestinationsQuery>,
) -> Response {
    let pool = &state.pool;
    let pattern = q.q.as_deref().map(|s| format!("%{}%", s.to_lowercase()));

    match sqlx::query_as!(
        HistoryDestinationDto,
        r#"
        SELECT
          host(m.destination_ip)                           AS "destination_ip!",
          COALESCE(c.display_name, host(m.destination_ip)) AS "display_name!",
          c.city,
          c.country_code,
          c.asn,
          EXISTS (SELECT 1 FROM agents a WHERE a.ip = m.destination_ip)
                                                            AS "is_mesh_member!"
        FROM (
          SELECT DISTINCT destination_ip
            FROM measurements
           WHERE source_agent_id = $1
        ) m
        LEFT JOIN ip_catalogue c ON c.ip = m.destination_ip
        WHERE $2::text IS NULL
           OR LOWER(host(m.destination_ip)) LIKE $2
           OR (c.display_name IS NOT NULL AND LOWER(c.display_name) LIKE $2)
        ORDER BY 2 ASC
        "#,
        q.source,
        pattern,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error("history::destinations", e),
    }
}

// --- measurements ------------------------------------------------------

/// Query params for `GET /api/history/measurements`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HistoryMeasurementsQuery {
    /// Source agent id.
    pub source: String,
    /// Destination IP (v4 or v6 host string).
    pub destination: String,
    /// Comma-separated list: `icmp,tcp,udp`. Empty/absent = all protocols.
    #[serde(default)]
    pub protocols: Option<String>,
    /// RFC 3339 lower bound (inclusive).
    #[serde(default)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    /// RFC 3339 upper bound (inclusive).
    #[serde(default)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// One joined `measurements` + optional `mtr_traces` row for the
/// `/history/pair` page.
///
/// `mtr_hops` decodes the `mtr_traces.hops` JSONB column. sqlx requires
/// the `Json<_>` wrapper for JSONB columns in `query_as!`; the wire JSON
/// stays flat thanks to `sqlx::types::Json`'s transparent serde impl.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HistoryMeasurementDto {
    /// `measurements.id`.
    pub id: i64,
    /// Source agent id that produced the row.
    pub source_agent_id: String,
    /// Destination IP as a host string.
    pub destination_ip: String,
    /// Protocol the probe used.
    pub protocol: ProbeProtocol,
    /// Measurement kind (`campaign`, `detail_ping`, `detail_mtr`).
    pub kind: MeasurementKind,
    /// Number of probes in the sample.
    pub probe_count: i16,
    /// When the row was produced (UTC).
    pub measured_at: chrono::DateTime<chrono::Utc>,
    /// Minimum round-trip latency in ms.
    pub latency_min_ms: Option<f32>,
    /// Average round-trip latency in ms.
    pub latency_avg_ms: Option<f32>,
    /// 95th-percentile round-trip latency in ms.
    pub latency_p95_ms: Option<f32>,
    /// Maximum round-trip latency in ms.
    pub latency_max_ms: Option<f32>,
    /// Latency standard deviation in ms.
    pub latency_stddev_ms: Option<f32>,
    /// Observed loss percentage ([0, 100]).
    pub loss_pct: f32,
    /// MTR hop array; populated when the measurement has an `mtr_id`.
    #[schema(value_type = Option<Object>)]
    pub mtr_hops: Option<sqlx::types::Json<serde_json::Value>>,
    /// When the associated `mtr_traces` row was captured.
    pub mtr_captured_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn parse_protocols(raw: Option<&str>) -> Option<Vec<ProbeProtocol>> {
    raw.map(|s| {
        s.split(',')
            .filter_map(|t| match t.trim() {
                "icmp" => Some(ProbeProtocol::Icmp),
                "tcp" => Some(ProbeProtocol::Tcp),
                "udp" => Some(ProbeProtocol::Udp),
                _ => None,
            })
            .collect()
    })
}

/// `GET /api/history/measurements` — measurement rows (+ optional MTR
/// hops) for a (source, destination) range. Hard-capped at 5 000 rows
/// so a pathologically long history can't blow a browser tab; the
/// frontend surfaces the cap explicitly.
#[utoipa::path(
    get,
    path = "/api/history/measurements",
    tag = "history",
    operation_id = "history_measurements",
    params(HistoryMeasurementsQuery),
    responses(
        (status = 200, description = "Measurement list", body = Vec<HistoryMeasurementDto>),
        (status = 400, description = "Malformed destination", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn measurements(
    State(state): State<AppState>,
    Query(q): Query<HistoryMeasurementsQuery>,
) -> Response {
    let pool = &state.pool;
    // sqlx-postgres does NOT implement `Encode<Postgres>` for
    // `std::net::IpAddr` against INET, so the destination goes through
    // `IpNetwork::from(IpAddr)` first.
    let dest: std::net::IpAddr = match q.destination.parse() {
        Ok(ip) => ip,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorEnvelope {
                    error: "invalid_destination_ip".into(),
                }),
            )
                .into_response();
        }
    };
    let dest_net = IpNetwork::from(dest);
    let protocols = parse_protocols(q.protocols.as_deref());

    // Hard cap at 5 000 rows. A (source, destination) pair measured
    // every 5 min for 90 days produces ~ 25 000 rows — the cap is
    // explicit so operators don't blow a browser tab on a pathologically
    // long history. If a future pair crosses the cap, the chart clips
    // to the most recent 5 000 samples; the UI shows a "showing most
    // recent 5 000 of N" notice.
    match sqlx::query_as!(
        HistoryMeasurementDto,
        r#"
        SELECT
          m.id                             AS "id!",
          m.source_agent_id                AS "source_agent_id!",
          host(m.destination_ip)           AS "destination_ip!",
          m.protocol                       AS "protocol!: ProbeProtocol",
          m.kind                           AS "kind!: MeasurementKind",
          m.probe_count                    AS "probe_count!",
          m.measured_at                    AS "measured_at!",
          m.latency_min_ms,
          m.latency_avg_ms,
          m.latency_p95_ms,
          m.latency_max_ms,
          m.latency_stddev_ms,
          m.loss_pct                       AS "loss_pct!",
          t.hops                           AS "mtr_hops?: sqlx::types::Json<serde_json::Value>",
          t.captured_at                    AS "mtr_captured_at?"
        FROM measurements m
        LEFT JOIN mtr_traces t ON t.id = m.mtr_id
        WHERE m.source_agent_id = $1
          AND m.destination_ip  = $2
          AND ($3::probe_protocol[] IS NULL OR m.protocol = ANY($3))
          AND ($4::timestamptz IS NULL OR m.measured_at >= $4)
          AND ($5::timestamptz IS NULL OR m.measured_at <= $5)
        ORDER BY m.measured_at DESC
        LIMIT 5000
        "#,
        q.source,
        dest_net,
        protocols.as_deref() as _,
        q.from,
        q.to,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => internal_error("history::measurements", e),
    }
}
