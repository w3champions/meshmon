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
//! Every handler sits behind the user-API middleware that enforces an
//! active session. `sources` and `campaign_measurements` consume auth
//! transparently through that layer; `destinations` and `measurements`
//! take an explicit [`AuthSession`] extractor so the hostname stamp can
//! attribute cold-miss resolver enqueues to the caller's session id.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use std::net::IpAddr;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::campaign::dto::ErrorEnvelope;
use crate::campaign::model::{MeasurementKind, PairResolutionState, ProbeProtocol};
use crate::hostname::session_id_from_auth;
use crate::hostname::stamp::bulk_hostnames_and_enqueue;
use crate::http::auth::AuthSession;
use crate::ingestion::json_shapes::HopJson;
use crate::state::AppState;

// All history handlers use `&state.pool` (PgPool is a public field on
// AppState — there is no `pg_pool()` accessor). Every handler sits
// behind the user-API middleware that enforces an active session.
// `destinations` and `measurements` additionally take an `AuthSession`
// extractor to attribute hostname cold-miss resolver enqueues to the
// caller's session id; `sources` and `campaign_measurements` don't
// stamp hostnames and consume auth transparently through middleware.

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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    // Semi-join via `WHERE EXISTS` — cheaper than `JOIN + DISTINCT`
    // against the `measurements` hypertable and lets
    // `measurements_reuse_idx (source_agent_id, …)` short-circuit the
    // agent-by-agent existence check. Blank catalogue / agent names are
    // filtered with NULLIF so the display_name never renders as "".
    match sqlx::query_as!(
        HistorySourceDto,
        r#"
        SELECT awc.agent_id AS "source_agent_id!",
               COALESCE(NULLIF(awc.catalogue_display_name, ''),
                        NULLIF(awc.agent_display_name, ''),
                        awc.agent_id) AS "display_name!"
          FROM agents_with_catalogue awc
         WHERE EXISTS (SELECT 1 FROM measurements m WHERE m.source_agent_id = awc.agent_id)
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    /// Reverse-DNS hostname for the destination IP, when cached.
    /// Absent on cold miss and negative-cached IPs (skip-none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
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
    auth: AuthSession,
    Query(q): Query<HistoryDestinationsQuery>,
) -> Response {
    let pool = &state.pool;
    let pattern = q.q.as_deref().map(|s| format!("%{}%", s.to_lowercase()));

    // `sqlx::query_as!` cannot handle the new `hostname` field because it is
    // not a DB column. We fetch only the DB columns and default `hostname` to
    // `None`; the stamp call below fills it in from the cache.
    #[derive(sqlx::FromRow)]
    struct DestRow {
        destination_ip: String,
        display_name: String,
        city: Option<String>,
        country_code: Option<String>,
        asn: Option<i32>,
        is_mesh_member: bool,
    }

    let rows: Vec<DestRow> = match sqlx::query_as::<_, DestRow>(
        r#"
        SELECT
          host(m.destination_ip)                           AS destination_ip,
          COALESCE(c.display_name, host(m.destination_ip)) AS display_name,
          c.city,
          c.country_code,
          c.asn,
          EXISTS (SELECT 1 FROM agents a WHERE a.ip = m.destination_ip) AS is_mesh_member
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
    )
    .bind(&q.source)
    .bind(&pattern)
    .fetch_all(pool)
    .await
    {
        Ok(v) => v,
        Err(e) => return internal_error("history::destinations", e),
    };

    let mut dtos: Vec<HistoryDestinationDto> = rows
        .into_iter()
        .map(|r| HistoryDestinationDto {
            destination_ip: r.destination_ip,
            display_name: r.display_name,
            city: r.city,
            country_code: r.country_code,
            asn: r.asn,
            is_mesh_member: r.is_mesh_member,
            hostname: None,
        })
        .collect();

    // Collect destination IPs for bulk hostname resolution.
    let ips: Vec<IpAddr> = dtos
        .iter()
        .filter_map(|d| d.destination_ip.parse::<IpAddr>().ok())
        .collect();

    if !ips.is_empty() {
        let session = session_id_from_auth(&auth);
        match bulk_hostnames_and_enqueue(&state, &session, &ips).await {
            Ok(map) => {
                for dto in dtos.iter_mut() {
                    if let Ok(ip) = dto.destination_ip.parse::<IpAddr>() {
                        if let Some(Some(h)) = map.get(&ip) {
                            dto.hostname = Some(h.clone());
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "history::destinations: hostname stamp failed; returning unhostnamed response");
            }
        }
    }

    (StatusCode::OK, Json(dtos)).into_response()
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
/// `mtr_hops` decodes the `mtr_traces.hops` JSONB column into the typed
/// `Vec<HopJson>` wire shape. sqlx requires the `Json<_>` wrapper for
/// JSONB columns in `query_as!`; the wire JSON stays flat thanks to
/// `sqlx::types::Json`'s transparent serde impl.
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
    #[schema(value_type = Option<Vec<HopJson>>)]
    pub mtr_hops: Option<sqlx::types::Json<Vec<HopJson>>>,
    /// When the associated `mtr_traces` row was captured.
    pub mtr_captured_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Reverse-DNS hostname for the destination IP, when cached.
    /// Absent on cold miss and negative-cached IPs (skip-none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_hostname: Option<String>,
}

/// Parse the comma-separated `protocols=` query param into a typed
/// filter.
///
/// Returns `Ok(None)` when the param is absent or resolves to zero
/// non-empty tokens (no filter — all protocols match). Returns
/// `Err(token)` on any unknown token so the handler can emit a 400
/// instead of silently swallowing the filter and returning an empty
/// list — the project's coding rule forbids silent error swallowing.
fn parse_protocols(raw: Option<&str>) -> Result<Option<Vec<ProbeProtocol>>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for token in raw.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        match t {
            "icmp" => out.push(ProbeProtocol::Icmp),
            "tcp" => out.push(ProbeProtocol::Tcp),
            "udp" => out.push(ProbeProtocol::Udp),
            other => return Err(other.to_string()),
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

/// Internal row shape for the `measurements` query (without hostname).
/// `sqlx::query_as!` requires every struct field to map to a SQL column;
/// `destination_hostname` is added after the DB round-trip.
struct MeasurementRow {
    id: i64,
    source_agent_id: String,
    destination_ip: String,
    protocol: ProbeProtocol,
    kind: MeasurementKind,
    probe_count: i16,
    measured_at: chrono::DateTime<chrono::Utc>,
    latency_min_ms: Option<f32>,
    latency_avg_ms: Option<f32>,
    latency_p95_ms: Option<f32>,
    latency_max_ms: Option<f32>,
    latency_stddev_ms: Option<f32>,
    loss_pct: f32,
    mtr_hops: Option<sqlx::types::Json<Vec<HopJson>>>,
    mtr_captured_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<MeasurementRow> for HistoryMeasurementDto {
    fn from(r: MeasurementRow) -> Self {
        Self {
            id: r.id,
            source_agent_id: r.source_agent_id,
            destination_ip: r.destination_ip,
            protocol: r.protocol,
            kind: r.kind,
            probe_count: r.probe_count,
            measured_at: r.measured_at,
            latency_min_ms: r.latency_min_ms,
            latency_avg_ms: r.latency_avg_ms,
            latency_p95_ms: r.latency_p95_ms,
            latency_max_ms: r.latency_max_ms,
            latency_stddev_ms: r.latency_stddev_ms,
            loss_pct: r.loss_pct,
            mtr_hops: r.mtr_hops,
            mtr_captured_at: r.mtr_captured_at,
            destination_hostname: None,
        }
    }
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
        (status = 400, description = "Malformed destination or invalid protocol token", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn measurements(
    State(state): State<AppState>,
    auth: AuthSession,
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
    let protocols = match parse_protocols(q.protocols.as_deref()) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorEnvelope {
                    error: "invalid_protocols".into(),
                }),
            )
                .into_response();
        }
    };

    // Hard cap at 5 000 rows. A (source, destination) pair measured
    // every 5 min for 90 days produces ~ 25 000 rows — the cap is
    // explicit so operators don't blow a browser tab on a pathologically
    // long history. The query asks for `cap + 1` so the frontend can
    // tell "exactly cap rows exist" (no truncation) apart from "more
    // than cap rows exist" (truncation): a response of `cap + 1` rows
    // means the underlying set is larger than the cap and the visible
    // view is the most recent `cap`. The frontend trims and surfaces
    // the cap notice on that signal.
    let db_rows = match sqlx::query_as!(
        MeasurementRow,
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
          t.hops                           AS "mtr_hops?: sqlx::types::Json<Vec<HopJson>>",
          t.captured_at                    AS "mtr_captured_at?"
        FROM measurements m
        LEFT JOIN mtr_traces t ON t.id = m.mtr_id
        WHERE m.source_agent_id = $1
          AND m.destination_ip  = $2
          AND ($3::probe_protocol[] IS NULL OR m.protocol = ANY($3))
          AND ($4::timestamptz IS NULL OR m.measured_at >= $4)
          AND ($5::timestamptz IS NULL OR m.measured_at <= $5)
        ORDER BY m.measured_at DESC
        LIMIT 5001
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
        Ok(rows) => rows,
        Err(e) => return internal_error("history::measurements", e),
    };

    let mut dtos: Vec<HistoryMeasurementDto> = db_rows.into_iter().map(Into::into).collect();

    // Collect all IPs for hostname stamping: destination IP + all MTR hop IPs.
    let session = session_id_from_auth(&auth);
    let mut all_ips: Vec<IpAddr> = Vec::new();

    // Destination IPs (all rows share the same destination, but include
    // in case of future query shape changes).
    for dto in dtos.iter() {
        if let Ok(ip) = dto.destination_ip.parse::<IpAddr>() {
            all_ips.push(ip);
        }
    }

    // MTR hop IPs.
    for dto in dtos.iter() {
        if let Some(hops_json) = &dto.mtr_hops {
            for hop in hops_json.iter() {
                for hop_ip in &hop.observed_ips {
                    if let Ok(ip) = hop_ip.ip.parse::<IpAddr>() {
                        all_ips.push(ip);
                    }
                }
            }
        }
    }

    if !all_ips.is_empty() {
        match bulk_hostnames_and_enqueue(&state, &session, &all_ips).await {
            Ok(map) => {
                for dto in dtos.iter_mut() {
                    // Stamp destination hostname.
                    if let Ok(ip) = dto.destination_ip.parse::<IpAddr>() {
                        if let Some(Some(h)) = map.get(&ip) {
                            dto.destination_hostname = Some(h.clone());
                        }
                    }
                    // Stamp MTR hop hostnames.
                    if let Some(hops_json) = &mut dto.mtr_hops {
                        for hop in hops_json.iter_mut() {
                            for hop_ip in hop.observed_ips.iter_mut() {
                                if let Ok(ip) = hop_ip.ip.parse::<IpAddr>() {
                                    if let Some(Some(h)) = map.get(&ip) {
                                        hop_ip.hostname = Some(h.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "history::measurements: hostname stamp failed; returning unhostnamed response");
            }
        }
    }

    (StatusCode::OK, Json(dtos)).into_response()
}

// --- campaign measurements ---------------------------------------------

/// Query params for `GET /api/campaigns/{id}/measurements`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct CampaignMeasurementsQuery {
    /// Narrow to a single `campaign_pairs.resolution_state`.
    #[serde(default)]
    pub resolution_state: Option<PairResolutionState>,
    /// Narrow to a single `measurements.protocol`. Pending/dispatched
    /// pairs (no joined measurement) stay visible under this filter so
    /// operators can monitor in-flight detail work.
    #[serde(default)]
    pub protocol: Option<ProbeProtocol>,
    /// Narrow to a single `campaign_pairs.kind` (`campaign`,
    /// `detail_ping`, `detail_mtr`).
    #[serde(default)]
    pub kind: Option<MeasurementKind>,
    /// Keyset cursor — base64(JSON) `{t, i}` pair from the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Resolve a single (pair, measurement) row by measurement id —
    /// used by the DrilldownDrawer for MTR lookup.
    #[serde(default)]
    pub measurement_id: Option<i64>,
    /// Page size. Defaults to 200, clamped to `[1, 1000]`.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One row for the Raw tab OR for the DrilldownDrawer's MTR resolution.
///
/// Every field but `pair_id`, `source_agent_id`, `destination_ip`,
/// `resolution_state`, and `pair_kind` is nullable — a `campaign_pairs`
/// row with `pending` or `dispatched` state has no joined `measurements`
/// row yet, but the Raw tab still renders it so operators can monitor
/// in-flight detail work.
///
/// `mtr_hops` is inlined rather than referenced by id so the
/// DrilldownDrawer can render MTR directly from this endpoint — there
/// is no separate `GET /api/measurements/:id` in the service. The
/// `Option<sqlx::types::Json<_>>` wrapper is mandatory for decoding
/// JSONB; serde renders it as a bare JSON array on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CampaignMeasurementDto {
    /// `campaign_pairs.id`.
    pub pair_id: i64,
    /// Source agent id from the pair envelope.
    pub source_agent_id: String,
    /// Destination IP as a host string.
    pub destination_ip: String,
    /// Current lifecycle of the pair.
    pub resolution_state: PairResolutionState,
    /// Kind of work the pair represents
    /// (`campaign`, `detail_ping`, `detail_mtr`).
    pub pair_kind: MeasurementKind,
    /// Populated when the pair has a joined `measurements` row.
    pub measurement_id: Option<i64>,
    /// Protocol of the joined measurement (null when pending/dispatched).
    pub protocol: Option<ProbeProtocol>,
    /// When the measurement was produced (null when pending/dispatched).
    pub measured_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Average round-trip latency in ms (nullable).
    pub latency_avg_ms: Option<f32>,
    /// Observed loss percentage ([0, 100]) (nullable).
    pub loss_pct: Option<f32>,
    /// `measurements.mtr_id` FK — reference to `mtr_traces.id`.
    pub mtr_id: Option<i64>,
    /// Inline MTR hops — populated iff `mtr_id` resolves to an
    /// `mtr_traces` row.
    #[schema(value_type = Option<Vec<HopJson>>)]
    pub mtr_hops: Option<sqlx::types::Json<Vec<HopJson>>>,
}

/// One page of the Raw-tab's joined campaign+measurements feed.
#[derive(Debug, Serialize, ToSchema)]
pub struct CampaignMeasurementsPage {
    /// Entries in `(measured_at DESC NULLS LAST, pair_id DESC)` order.
    pub entries: Vec<CampaignMeasurementDto>,
    /// Opaque cursor for the next page, or `null` when this is the
    /// final page. Only populated when the final row has a non-null
    /// `measured_at` (pending rows cannot be resumed via keyset).
    pub next_cursor: Option<String>,
}

/// `GET /api/campaigns/{id}/measurements` — joined campaign+measurements
/// feed for the Results browser's Raw tab. Paginated via a keyset
/// cursor; rows are ordered `measured_at DESC NULLS LAST, cp.id DESC`,
/// so settled rows lead each page and pending/dispatched pairs (no
/// `measured_at`) trail at the bottom of the first page. Pending rows
/// are not reachable via keyset pagination — see
/// [`fetch_campaign_measurements`] for the v1 contract.
#[utoipa::path(
    get,
    path = "/api/campaigns/{id}/measurements",
    tag = "campaigns",
    operation_id = "campaign_measurements",
    params(
        ("id" = Uuid, Path, description = "Campaign id"),
        CampaignMeasurementsQuery,
    ),
    responses(
        (status = 200, description = "Measurement page", body = CampaignMeasurementsPage),
        (status = 400, description = "Malformed cursor", body = ErrorEnvelope),
        (status = 401, description = "No active session"),
        (status = 500, description = "Internal error", body = ErrorEnvelope),
    ),
)]
pub async fn campaign_measurements(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<CampaignMeasurementsQuery>,
) -> Response {
    let pool = &state.pool;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    // A malformed cursor (URL-mangled, clipped by a proxy, or pre-schema-change)
    // must 400 rather than silently restart at page 1 — the client would
    // render stale rows as "next page" and duplicate entries otherwise.
    let cursor = match q.cursor.as_deref() {
        Some(s) => match decode_measurements_cursor(s) {
            Some(c) => Some(c),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorEnvelope {
                        error: "invalid_cursor".into(),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    match fetch_campaign_measurements(pool, id, &q, limit, cursor).await {
        Ok(page) => (StatusCode::OK, Json(page)).into_response(),
        Err(e) => internal_error("history::campaign_measurements", e),
    }
}

/// Private repo helper for `campaign_measurements`.
///
/// Uses LEFT JOINs on `measurements` and `mtr_traces` so pending /
/// dispatched pairs remain visible (the Raw tab must surface in-flight
/// detail work even when the pair has no measurement yet). The cursor
/// and ORDER BY use `NULLS LAST` on `measured_at` — pending rows
/// accumulate at the bottom of the first page and are unreachable by
/// keyset pagination in v1 (acceptable: operators narrow by
/// `resolution_state` when they want pending-only views).
async fn fetch_campaign_measurements(
    pool: &PgPool,
    id: Uuid,
    q: &CampaignMeasurementsQuery,
    limit: i64,
    cursor: Option<(chrono::DateTime<chrono::Utc>, i64)>,
) -> sqlx::Result<CampaignMeasurementsPage> {
    let cur_t = cursor.map(|(t, _)| t);
    let cur_i = cursor.map(|(_, i)| i);

    let rows: Vec<CampaignMeasurementDto> = sqlx::query_as!(
        CampaignMeasurementDto,
        r#"
        SELECT
          cp.id                         AS "pair_id!",
          cp.source_agent_id            AS "source_agent_id!",
          host(cp.destination_ip)       AS "destination_ip!",
          cp.resolution_state           AS "resolution_state!: PairResolutionState",
          cp.kind                       AS "pair_kind!: MeasurementKind",
          m.id                          AS "measurement_id?",
          m.protocol                    AS "protocol?: ProbeProtocol",
          m.measured_at                 AS "measured_at?",
          m.latency_avg_ms              AS "latency_avg_ms?",
          m.loss_pct                    AS "loss_pct?",
          m.mtr_id                      AS "mtr_id?",
          t.hops                        AS "mtr_hops?: sqlx::types::Json<Vec<HopJson>>"
        FROM campaign_pairs cp
        LEFT JOIN measurements m ON m.id = cp.measurement_id
        LEFT JOIN mtr_traces   t ON t.id = m.mtr_id
        WHERE cp.campaign_id = $1
          AND ($2::pair_resolution_state IS NULL OR cp.resolution_state = $2)
          -- Pending/dispatched pairs have no joined measurement, so
          -- `m.protocol` is NULL. We intentionally retain those rows
          -- under a protocol filter so operators can still see
          -- in-flight detail work; the UI renders "protocol: —" until
          -- the measurement lands.
          AND ($3::probe_protocol        IS NULL OR m.protocol IS NULL OR m.protocol = $3)
          AND ($4::measurement_kind      IS NULL OR cp.kind = $4)
          -- measurement_id filter short-circuits to the single pair that
          -- owns the requested measurement (DrilldownDrawer MTR resolver).
          AND ($5::bigint                IS NULL OR m.id = $5)
          -- Cursor predicate: rows with NULL measured_at can never be
          -- reached via keyset once the first page scrolls past them.
          -- That is acceptable in v1 per the Raw-tab contract.
          AND ($6::timestamptz IS NULL
               OR (m.measured_at IS NOT NULL
                   AND (m.measured_at, cp.id) < ($6, $7::bigint)))
        ORDER BY m.measured_at DESC NULLS LAST, cp.id DESC
        LIMIT $8
        "#,
        id,
        q.resolution_state as _,
        q.protocol as _,
        q.kind as _,
        q.measurement_id,
        cur_t,
        cur_i,
        limit,
    )
    .fetch_all(pool)
    .await?;

    // Emit next_cursor only when the page is full AND the last row has a
    // settled `measured_at`. Pending-row tails don't paginate; the
    // operator refines with `resolution_state` if they want more.
    let next_cursor = rows
        .last()
        .filter(|r| rows.len() as i64 == limit && r.measured_at.is_some())
        .and_then(|r| {
            r.measured_at
                .map(|t| encode_measurements_cursor(t, r.pair_id))
        });

    Ok(CampaignMeasurementsPage {
        entries: rows,
        next_cursor,
    })
}

/// Base64-JSON cursor payload. Private — callers round-trip the opaque
/// string surfaced on `CampaignMeasurementsPage.next_cursor`.
#[derive(Serialize, Deserialize)]
struct CursorPayload {
    t: chrono::DateTime<chrono::Utc>,
    i: i64,
}

fn encode_measurements_cursor(t: chrono::DateTime<chrono::Utc>, i: i64) -> String {
    use base64::Engine;
    let json =
        serde_json::to_vec(&CursorPayload { t, i }).expect("CursorPayload always serializes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_measurements_cursor(s: &str) -> Option<(chrono::DateTime<chrono::Utc>, i64)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()?;
    let p: CursorPayload = serde_json::from_slice(&bytes).ok()?;
    Some((p.t, p.i))
}
