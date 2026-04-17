//! `GET /api/paths/{src}/{tgt}/overview` — aggregated path-detail payload.
//!
//! The handler collapses what the Path Detail page needs into a single
//! request:
//! - source/target agent metadata (registry snapshot),
//! - the latest route snapshot per protocol within the time window,
//! - a recent-snapshots list in that window (no hop detail, capped at 100),
//! - VM-sourced RTT avg and failure-rate series at a server-chosen step,
//! - a server-picked primary protocol (auto: `icmp > udp > tcp`, or
//!   `?protocol=` override),
//! - window/step echoes so the UI can align axes without guessing.
//!
//! # Primary-protocol rule
//!
//! Auto-pick uses a **fixed priority** `ICMP > UDP > TCP` — the first
//! protocol with at least one snapshot in the window wins. We deliberately
//! do NOT compare `observed_at` across protocols because per-protocol probe
//! cadences differ and "last observed" would jitter the pick between polls.
//! When `?protocol=` is supplied and valid, it short-circuits the rule.
//!
//! # VM queries
//!
//! Fired after the primary protocol is known so we only hit the upstream
//! for the protocol we're going to display. The RTT expression divides by
//! 1000 in MetricsQL so the resulting values ride through unmodified as
//! milliseconds. Loss is already a [0, 1] fraction. On VM unreachable or
//! non-2xx we log at `warn!` and return `metrics: null` in the 200
//! response — the UI then renders "metrics unavailable" without breaking
//! the page.

use crate::http::http_client::proxy_client;
use crate::http::user_api::{AgentSummary, RouteSnapshotDetail, RouteSnapshotSummary};
use crate::ingestion::json_shapes::{HopJson, PathSummaryJson};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlxJson;
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Aggregated response for the Path Detail page.
#[derive(Debug, Serialize, ToSchema)]
pub struct PathOverviewResponse {
    /// Source agent metadata.
    pub source: AgentSummary,
    /// Target agent metadata.
    pub target: AgentSummary,
    /// Server-picked primary protocol (`icmp`, `udp`, or `tcp`). Missing
    /// only when no protocol has any snapshot in the window; callers should
    /// treat that as "show a neutral empty state".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_protocol: Option<String>,
    /// Latest snapshot per protocol within the window (each field optional).
    pub latest_by_protocol: LatestByProtocol,
    /// Recent snapshots in the window in descending `observed_at` order.
    /// Capped at [`RECENT_LIMIT`]; no hop detail.
    pub recent_snapshots: Vec<RouteSnapshotSummary>,
    /// VM series for the primary protocol, or `null` when the VM was
    /// unreachable or misconfigured.
    pub metrics: Option<PathMetrics>,
    /// Echoed resolved window bounds (inclusive).
    pub window: WindowBounds,
    /// Server-chosen Prometheus step (`1m`, `5m`, `1h`, `6h`).
    pub step: String,
}

/// Per-protocol latest-snapshot map. Each field is independent so the UI can
/// render "icmp present, udp/tcp empty" without a sentinel value.
#[derive(Debug, Serialize, ToSchema)]
pub struct LatestByProtocol {
    /// Latest ICMP snapshot in window, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icmp: Option<RouteSnapshotDetail>,
    /// Latest UDP snapshot in window, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp: Option<RouteSnapshotDetail>,
    /// Latest TCP snapshot in window, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp: Option<RouteSnapshotDetail>,
}

/// VictoriaMetrics-sourced RTT / loss series for the primary protocol.
#[derive(Debug, Serialize, ToSchema)]
pub struct PathMetrics {
    /// `[epoch_ms, rtt_ms]` tuples.
    pub rtt_series: Vec<[f64; 2]>,
    /// `[epoch_ms, loss_fraction]` tuples. Fractional — `0.05` = 5%.
    pub loss_series: Vec<[f64; 2]>,
    /// Last RTT value (ms), or null if the series is empty.
    pub rtt_current: Option<f64>,
    /// Last loss value (fraction), or null if the series is empty.
    pub loss_current: Option<f64>,
}

/// Inclusive time window bounds echoed back to the caller.
#[derive(Debug, Serialize, ToSchema)]
pub struct WindowBounds {
    /// Start of the window (inclusive).
    pub from: DateTime<Utc>,
    /// End of the window (inclusive).
    pub to: DateTime<Utc>,
}

/// Query parameters accepted by the overview endpoint.
#[derive(Debug, Deserialize, IntoParams)]
pub struct PathOverviewParams {
    /// Start of the time window (inclusive). Defaults to `to - 24h`.
    pub from: Option<DateTime<Utc>>,
    /// End of the time window (inclusive). Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// Optional protocol override (`icmp`, `udp`, or `tcp`). Returns 400 on
    /// any other value.
    pub protocol: Option<String>,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Cap on `recent_snapshots`. Matches the spec-06 Path Detail requirement
/// for a bounded preview list.
const RECENT_LIMIT: i64 = 100;

// ---------------------------------------------------------------------------
// Step selection
// ---------------------------------------------------------------------------

/// Pick a MetricsQL `step` appropriate for the requested window. Matches
/// spec 06: dense enough for 24 h zoom-ins, coarse enough not to crush VM
/// on month-long views.
fn pick_step(window: Duration) -> &'static str {
    if window <= Duration::hours(24) {
        "1m"
    } else if window <= Duration::days(7) {
        "5m"
    } else if window <= Duration::days(30) {
        "1h"
    } else {
        "6h"
    }
}

// ---------------------------------------------------------------------------
// Primary-protocol picker
// ---------------------------------------------------------------------------

/// Pick the primary protocol by fixed priority `icmp > udp > tcp`, falling
/// back to the first one with a snapshot in window. An explicit override —
/// if valid — short-circuits the rule.
fn auto_primary(latest: &LatestByProtocol, override_: Option<&str>) -> Option<String> {
    if let Some(p) = override_ {
        // Invalid protocol strings are rejected earlier (400). This branch
        // trusts the caller passed `validate_protocol` first.
        if matches!(p, "icmp" | "udp" | "tcp") {
            return Some(p.to_owned());
        }
    }
    // Fixed priority — do NOT compare timestamps across protocols. Cadences
    // differ per-protocol so "last observed" would jitter the pick.
    if latest.icmp.is_some() {
        return Some("icmp".to_owned());
    }
    if latest.udp.is_some() {
        return Some("udp".to_owned());
    }
    if latest.tcp.is_some() {
        return Some("tcp".to_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// DB queries
// ---------------------------------------------------------------------------

/// Row shape returned by [`fetch_latest_by_protocol`] before conversion.
struct LatestRow {
    id: i64,
    source_id: String,
    target_id: String,
    protocol: String,
    observed_at: DateTime<Utc>,
    hops: SqlxJson<Vec<HopJson>>,
    path_summary: Option<SqlxJson<PathSummaryJson>>,
}

impl From<LatestRow> for RouteSnapshotDetail {
    fn from(r: LatestRow) -> Self {
        Self {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            protocol: r.protocol,
            observed_at: r.observed_at,
            hops: r.hops.0,
            path_summary: r.path_summary.map(|s| s.0),
        }
    }
}

/// Fetch the most recent snapshot per protocol for the given source/target
/// within `[from, to]`. Returns one row per protocol at most.
async fn fetch_latest_by_protocol(
    pool: &PgPool,
    source_id: &str,
    target_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<LatestByProtocol, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (protocol)
                  id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  hops AS "hops: SqlxJson<Vec<HopJson>>",
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM route_snapshots
           WHERE source_id   = $1
             AND target_id   = $2
             AND observed_at >= $3
             AND observed_at <= $4
           ORDER BY protocol, observed_at DESC, id DESC"#,
        source_id,
        target_id,
        from,
        to,
    )
    .fetch_all(pool)
    .await?;

    let mut out = LatestByProtocol {
        icmp: None,
        udp: None,
        tcp: None,
    };
    for r in rows {
        let detail: RouteSnapshotDetail = LatestRow {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            protocol: r.protocol.clone(),
            observed_at: r.observed_at,
            hops: r.hops,
            path_summary: r.path_summary,
        }
        .into();
        match r.protocol.as_str() {
            "icmp" => out.icmp = Some(detail),
            "udp" => out.udp = Some(detail),
            "tcp" => out.tcp = Some(detail),
            // Unknown protocols stored by an older agent build are simply
            // dropped from the per-protocol slots; they still show up in
            // the recent_snapshots list.
            _ => {}
        }
    }
    Ok(out)
}

/// Fetch recent snapshot summaries (no hops) for the pair within the
/// window, capped at [`RECENT_LIMIT`].
async fn fetch_recent_snapshots(
    pool: &PgPool,
    source_id: &str,
    target_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<RouteSnapshotSummary>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!",
                  source_id,
                  target_id,
                  protocol,
                  observed_at,
                  path_summary AS "path_summary: SqlxJson<PathSummaryJson>"
           FROM route_snapshots
           WHERE source_id   = $1
             AND target_id   = $2
             AND observed_at >= $3
             AND observed_at <= $4
           ORDER BY observed_at DESC, id DESC
           LIMIT $5"#,
        source_id,
        target_id,
        from,
        to,
        limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RouteSnapshotSummary {
            id: r.id,
            source_id: r.source_id,
            target_id: r.target_id,
            protocol: r.protocol,
            observed_at: r.observed_at,
            path_summary: r.path_summary.map(|s| s.0),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// VictoriaMetrics helpers
// ---------------------------------------------------------------------------

/// Read the configured VictoriaMetrics base URL, with any trailing slash
/// stripped. Mirrors `metrics_proxy::vm_base`.
fn vm_base(state: &AppState) -> Option<String> {
    state
        .config()
        .upstream
        .vm_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned())
}

/// Build the RTT-avg MetricsQL expression. The `/ 1000` divides micros
/// into milliseconds in-query so the handler can pass values through
/// untouched.
fn build_rtt_query(src: &str, tgt: &str, protocol: &str) -> String {
    format!(
        "avg by(source,target,protocol)(meshmon_path_rtt_avg_micros{{source=\"{src}\",target=\"{tgt}\",protocol=\"{protocol}\"}}) / 1000"
    )
}

/// Build the failure-rate MetricsQL expression. Already a [0, 1] fraction.
fn build_loss_query(src: &str, tgt: &str, protocol: &str) -> String {
    format!(
        "avg by(source,target,protocol)(meshmon_path_failure_rate{{source=\"{src}\",target=\"{tgt}\",protocol=\"{protocol}\"}})"
    )
}

/// Shape of a `matrix` result element from VM.
#[derive(Debug, Deserialize)]
struct VmMatrixResult {
    #[serde(default)]
    values: Vec<(f64, String)>,
}

#[derive(Debug, Deserialize)]
struct VmMatrixData {
    #[serde(default)]
    result: Vec<VmMatrixResult>,
}

#[derive(Debug, Deserialize)]
struct VmQueryEnvelope {
    data: VmMatrixData,
}

/// Fire a single VM range query and convert the first matrix result to the
/// `[epoch_ms, value]` tuple list the UI consumes. Returns `None` on any
/// upstream or parse failure — the caller short-circuits to `metrics: null`
/// in that case rather than 5xx'ing the whole overview response.
async fn fetch_series(
    base: &str,
    query: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    step: &str,
) -> Option<Vec<[f64; 2]>> {
    // Epoch seconds; VM accepts either Unix epoch or RFC 3339, but epoch
    // keeps the URL shorter and sidesteps timezone parsing cost on both
    // ends.
    let start_s = start.timestamp().to_string();
    let end_s = end.timestamp().to_string();
    let url = format!("{base}/api/v1/query_range");

    let params: [(&str, &str); 4] = [
        ("query", query),
        ("start", &start_s),
        ("end", &end_s),
        ("step", step),
    ];

    let resp = match proxy_client().get(&url).query(&params).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %url, "path_overview: VM request failed");
            return None;
        }
    };

    if !resp.status().is_success() {
        tracing::warn!(
            upstream_status = %resp.status(),
            %url,
            "path_overview: VM returned non-2xx"
        );
        return None;
    }

    let envelope: VmQueryEnvelope = match resp.json().await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, %url, "path_overview: VM body parse failed");
            return None;
        }
    };

    // Empty matrix (no data for this protocol in window) is a normal result,
    // not an error — return an empty series so the UI distinguishes
    // "series known to be empty" from "metrics unavailable".
    let samples = envelope
        .data
        .result
        .into_iter()
        .next()
        .map(|r| r.values)
        .unwrap_or_default();

    let series: Vec<[f64; 2]> = samples
        .into_iter()
        .filter_map(|(ts, val)| val.parse::<f64>().ok().map(|v| [ts * 1000.0, v]))
        .collect();

    Some(series)
}

/// Fetch RTT + loss series for the primary protocol. `None` collapses the
/// whole metrics block — the UI will render "metrics unavailable".
async fn fetch_metrics(
    state: &AppState,
    src: &str,
    tgt: &str,
    protocol: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    step: &str,
) -> Option<PathMetrics> {
    let base = vm_base(state)?;
    let rtt_q = build_rtt_query(src, tgt, protocol);
    let loss_q = build_loss_query(src, tgt, protocol);
    // Fan out the two queries concurrently — each hits `/api/v1/query_range`
    // with a different expression.
    let (rtt_series, loss_series) = tokio::join!(
        fetch_series(&base, &rtt_q, from, to, step),
        fetch_series(&base, &loss_q, from, to, step),
    );
    let rtt_series = rtt_series?;
    let loss_series = loss_series?;
    let rtt_current = rtt_series.last().map(|p| p[1]);
    let loss_current = loss_series.last().map(|p| p[1]);
    Some(PathMetrics {
        rtt_series,
        loss_series,
        rtt_current,
        loss_current,
    })
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Reject any `?protocol=` other than `icmp` / `udp` / `tcp`.
fn validate_protocol(p: &str) -> Option<Response> {
    match p {
        "icmp" | "udp" | "tcp" => None,
        _ => Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "protocol must be icmp, udp, or tcp" })),
            )
                .into_response(),
        ),
    }
}

/// `GET /api/paths/{src}/{tgt}/overview` — aggregate path-detail payload.
#[utoipa::path(
    get,
    path = "/api/paths/{src}/{tgt}/overview",
    tag = "paths",
    params(
        ("src" = String, Path, description = "Source agent id"),
        ("tgt" = String, Path, description = "Target agent id"),
        PathOverviewParams,
    ),
    responses(
        (status = 200, description = "Aggregated path detail", body = PathOverviewResponse),
        (status = 400, description = "Invalid protocol override"),
        (status = 401, description = "No active session"),
        (status = 404, description = "Source or target agent not found"),
    ),
)]
pub async fn path_overview(
    State(state): State<AppState>,
    Path((src, tgt)): Path<(String, String)>,
    Query(params): Query<PathOverviewParams>,
) -> Response {
    // 1. Validate the (optional) protocol override BEFORE any DB/VM work.
    if let Some(ref p) = params.protocol {
        if let Some(resp) = validate_protocol(p) {
            return resp;
        }
    }

    // 2. Resolve the window. Defaults: last 24 h, `to = now`.
    let now = Utc::now();
    let to = params.to.unwrap_or(now);
    let from = params.from.unwrap_or(to - Duration::hours(24));
    if from > to {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "from must be <= to" })),
        )
            .into_response();
    }

    // 3. Registry lookup — the source/target must exist. 404 keeps the
    //    response shape consistent with the other `/api/agents` endpoints.
    let snap = state.registry.snapshot();
    let source = match snap.get(&src).cloned() {
        Some(info) => AgentSummary::from(info),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "source agent not found" })),
            )
                .into_response()
        }
    };
    let target = match snap.get(&tgt).cloned() {
        Some(info) => AgentSummary::from(info),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "target agent not found" })),
            )
                .into_response()
        }
    };
    drop(snap);

    // 4. Fetch latest-per-protocol + recent list in parallel. Both hit the
    //    same pool so overlapped await points are essentially free.
    let latest_fut = fetch_latest_by_protocol(&state.pool, &src, &tgt, from, to);
    let recent_fut = fetch_recent_snapshots(&state.pool, &src, &tgt, from, to, RECENT_LIMIT);

    let (latest_by_protocol, recent_snapshots) = match tokio::try_join!(latest_fut, recent_fut) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(error = %e, %src, %tgt, "path_overview: DB fetch failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // 5. Pick the primary protocol. VM queries are only meaningful for a
    //    concrete protocol, so we skip them entirely when every slot is
    //    empty (and no valid override was supplied).
    let primary_protocol = auto_primary(&latest_by_protocol, params.protocol.as_deref());

    let step = pick_step(to - from);

    // 6. Fetch metrics only when we have a protocol to query against.
    let metrics = match &primary_protocol {
        Some(p) => fetch_metrics(&state, &src, &tgt, p, from, to, step).await,
        None => None,
    };

    Json(PathOverviewResponse {
        source,
        target,
        primary_protocol,
        latest_by_protocol,
        recent_snapshots,
        metrics,
        window: WindowBounds { from, to },
        step: step.to_string(),
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_latest() -> LatestByProtocol {
        LatestByProtocol {
            icmp: None,
            udp: None,
            tcp: None,
        }
    }

    fn with_protocol(proto: &str) -> LatestByProtocol {
        let detail = RouteSnapshotDetail {
            id: 1,
            source_id: "s".into(),
            target_id: "t".into(),
            protocol: proto.into(),
            observed_at: Utc::now(),
            hops: vec![],
            path_summary: None,
        };
        let mut l = empty_latest();
        match proto {
            "icmp" => l.icmp = Some(detail),
            "udp" => l.udp = Some(detail),
            "tcp" => l.tcp = Some(detail),
            _ => {}
        }
        l
    }

    #[test]
    fn pick_step_matches_window_bounds() {
        assert_eq!(pick_step(Duration::hours(1)), "1m");
        assert_eq!(pick_step(Duration::hours(24)), "1m");
        assert_eq!(pick_step(Duration::hours(25)), "5m");
        assert_eq!(pick_step(Duration::days(7)), "5m");
        assert_eq!(pick_step(Duration::days(8)), "1h");
        assert_eq!(pick_step(Duration::days(30)), "1h");
        assert_eq!(pick_step(Duration::days(31)), "6h");
    }

    #[test]
    fn auto_primary_respects_icmp_priority() {
        let mut l = with_protocol("icmp");
        l.tcp = with_protocol("tcp").tcp;
        l.udp = with_protocol("udp").udp;
        assert_eq!(auto_primary(&l, None).as_deref(), Some("icmp"));
    }

    #[test]
    fn auto_primary_falls_back_to_udp_then_tcp() {
        let l = with_protocol("udp");
        assert_eq!(auto_primary(&l, None).as_deref(), Some("udp"));
        let l = with_protocol("tcp");
        assert_eq!(auto_primary(&l, None).as_deref(), Some("tcp"));
    }

    #[test]
    fn auto_primary_returns_none_when_all_empty() {
        assert!(auto_primary(&empty_latest(), None).is_none());
    }

    #[test]
    fn auto_primary_honours_valid_override() {
        let l = with_protocol("icmp");
        assert_eq!(auto_primary(&l, Some("tcp")).as_deref(), Some("tcp"));
    }

    #[test]
    fn auto_primary_ignores_invalid_override() {
        // Handler validates first, but defense-in-depth check.
        let l = with_protocol("icmp");
        assert_eq!(auto_primary(&l, Some("bogus")).as_deref(), Some("icmp"));
    }

    #[test]
    fn validate_protocol_rejects_unknown_values() {
        assert!(validate_protocol("icmp").is_none());
        assert!(validate_protocol("udp").is_none());
        assert!(validate_protocol("tcp").is_none());
        assert!(validate_protocol("bogus").is_some());
        assert!(validate_protocol("").is_some());
    }
}
