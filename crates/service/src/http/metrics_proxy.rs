//! VictoriaMetrics PromQL proxy.
//!
//! Forwards GET /api/metrics/query -> VM /api/v1/query and
//! GET /api/metrics/query_range -> VM /api/v1/query_range. Rejects
//! queries that don't reference any `meshmon_<ident>` metric as
//! defense-in-depth (the endpoint is already behind `login_required!`).

use crate::http::http_client::proxy_client;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Query parameter structs
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/metrics/query` (instant query).
#[derive(Debug, Deserialize, ToSchema)]
pub struct InstantQuery {
    /// PromQL expression. Must reference at least one `meshmon_` metric.
    pub query: String,
    /// Optional evaluation timestamp (RFC 3339 or Unix epoch).
    pub time: Option<String>,
}

/// Query parameters for `GET /api/metrics/query_range` (range query).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RangeQuery {
    /// PromQL expression. Must reference at least one `meshmon_` metric.
    pub query: String,
    /// Start timestamp (RFC 3339 or Unix epoch).
    pub start: String,
    /// End timestamp (RFC 3339 or Unix epoch).
    pub end: String,
    /// Query resolution step (e.g. `15s`, `1m`).
    pub step: String,
}

// ---------------------------------------------------------------------------
// Whitelist
// ---------------------------------------------------------------------------

/// Return true when `q` contains at least one `meshmon_<ident>` token
/// (where `<ident>` starts with an ASCII lowercase letter or underscore).
/// Aggregators (`max by (...) (...)`), function wrappers
/// (`max_over_time(...)`), and arithmetic (`expr > 0.05`) are all allowed as
/// long as they reference a meshmon metric somewhere.
///
/// This is defense-in-depth only — the endpoint is already gated by
/// `login_required!`. We're not trying to parse PromQL; we just want to
/// refuse queries that don't touch any meshmon metric.
fn is_meshmon_query(q: &str) -> bool {
    const PREFIX: &str = "meshmon_";
    let mut idx = 0;
    while let Some(pos) = q[idx..].find(PREFIX) {
        // Reject if the previous character would make this part of a longer
        // identifier (e.g. `foo_meshmon_bar`) — meshmon_ must start a token.
        let abs = idx + pos;
        let boundary_ok = abs == 0
            || !q[..abs]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let after = abs + PREFIX.len();
        let suffix_ok = q[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
        if boundary_ok && suffix_ok {
            return true;
        }
        idx = abs + 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the configured VictoriaMetrics base URL, if any. Any trailing
/// slash is stripped so we don't build URLs like `http://vm:8428//api/v1/…`.
fn vm_base(state: &AppState) -> Option<String> {
    state
        .config()
        .upstream
        .vm_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned())
}

/// Shared 502 response used when the upstream request cannot be completed
/// or its body cannot be read.
fn upstream_error_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({ "error": "upstream request failed" })),
    )
        .into_response()
}

/// Forward a GET request to VictoriaMetrics and pass through a successful
/// response body as-is with `application/json` content-type and 200 OK.
///
/// - On reqwest send error -> 502.
/// - On VM returning non-2xx -> 502 (upstream status logged, body dropped to
///   avoid leaking upstream internals to the client).
/// - On VM returning 2xx -> pass through body as-is with 200 OK.
async fn forward(url: &str, params: &[(&str, &str)]) -> Response {
    let result = proxy_client().get(url).query(params).send().await;

    let resp = match result {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "vm proxy: upstream request failed");
            return upstream_error_response();
        }
    };

    if !resp.status().is_success() {
        tracing::warn!(
            upstream_status = %resp.status(),
            "vm proxy: upstream returned non-2xx"
        );
        return upstream_error_response();
    }

    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "vm proxy: failed to read upstream body");
            return upstream_error_response();
        }
    };

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/metrics/query` -- proxy an instant query to VictoriaMetrics.
///
/// The query expression must reference at least one `meshmon_<ident>`
/// metric. Aggregators and function wrappers around meshmon metrics are
/// allowed. Queries that touch no meshmon metric are rejected with 400.
///
/// - **400** when the query references no `meshmon_` metric.
/// - **502** when the upstream is unreachable or returns an error.
/// - **503** when `upstream.vm_url` is not configured.
#[utoipa::path(
    get,
    path = "/api/metrics/query",
    tag = "metrics",
    params(
        ("query" = String, Query, description = "PromQL expression (must reference a meshmon_ metric)"),
        ("time" = Option<String>, Query, description = "Evaluation timestamp"),
    ),
    responses(
        (status = 200, description = "Instant query result (VM JSON pass-through)"),
        (status = 400, description = "Query rejected by whitelist"),
        (status = 401, description = "No active session"),
        (status = 502, description = "Upstream error"),
        (status = 503, description = "VictoriaMetrics not configured"),
    ),
)]
pub async fn query_instant(
    State(state): State<AppState>,
    Query(q): Query<InstantQuery>,
) -> Response {
    if !is_meshmon_query(&q.query) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "error": "query must reference at least one meshmon_ metric" }),
            ),
        )
            .into_response();
    }

    let base = match vm_base(&state) {
        Some(b) => b,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "VictoriaMetrics not configured" })),
            )
                .into_response()
        }
    };

    let url = format!("{base}/api/v1/query");
    let mut params: Vec<(&str, &str)> = vec![("query", &q.query)];
    if let Some(ref t) = q.time {
        params.push(("time", t));
    }

    forward(&url, &params).await
}

/// `GET /api/metrics/query_range` -- proxy a range query to VictoriaMetrics.
///
/// The query expression must reference at least one `meshmon_<ident>`
/// metric. Aggregators and function wrappers around meshmon metrics are
/// allowed. Queries that touch no meshmon metric are rejected with 400.
///
/// - **400** when the query references no `meshmon_` metric.
/// - **502** when the upstream is unreachable or returns an error.
/// - **503** when `upstream.vm_url` is not configured.
#[utoipa::path(
    get,
    path = "/api/metrics/query_range",
    tag = "metrics",
    params(
        ("query" = String, Query, description = "PromQL expression (must reference a meshmon_ metric)"),
        ("start" = String, Query, description = "Start timestamp"),
        ("end" = String, Query, description = "End timestamp"),
        ("step" = String, Query, description = "Query resolution step"),
    ),
    responses(
        (status = 200, description = "Range query result (VM JSON pass-through)"),
        (status = 400, description = "Query rejected by whitelist"),
        (status = 401, description = "No active session"),
        (status = 502, description = "Upstream error"),
        (status = 503, description = "VictoriaMetrics not configured"),
    ),
)]
pub async fn query_range(State(state): State<AppState>, Query(q): Query<RangeQuery>) -> Response {
    if !is_meshmon_query(&q.query) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "error": "query must reference at least one meshmon_ metric" }),
            ),
        )
            .into_response();
    }

    let base = match vm_base(&state) {
        Some(b) => b,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "VictoriaMetrics not configured" })),
            )
                .into_response()
        }
    };

    let url = format!("{base}/api/v1/query_range");
    let params: [(&str, &str); 4] = [
        ("query", &q.query),
        ("start", &q.start),
        ("end", &q.end),
        ("step", &q.step),
    ];

    forward(&url, &params).await
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_accepts_meshmon_metric() {
        assert!(is_meshmon_query("meshmon_path_rtt_avg_micros"));
        assert!(is_meshmon_query("meshmon_path_failure_rate{source=\"a\"}"));
        assert!(is_meshmon_query("meshmon_rtt > 0"));
    }

    #[test]
    fn whitelist_accepts_aggregated_meshmon_query() {
        // Real dashboards wrap meshmon metrics in aggregators and
        // functions — allow anything that references a meshmon_ metric.
        assert!(is_meshmon_query("sum(meshmon_path_probe_count)"));
        assert!(is_meshmon_query(
            "max by (source, target) (max_over_time(meshmon_path_failure_rate[1m]))"
        ));
        assert!(is_meshmon_query(
            "rate(meshmon_path_rtt_avg_micros[5m]) / 1000"
        ));
        assert!(is_meshmon_query(" meshmon_foo"));
    }

    #[test]
    fn whitelist_rejects_non_meshmon() {
        assert!(!is_meshmon_query("up"));
        assert!(!is_meshmon_query("node_memory_MemFree_bytes"));
        assert!(!is_meshmon_query("sum(rate(node_cpu_seconds_total[5m]))"));
        assert!(!is_meshmon_query(""));
    }

    #[test]
    fn whitelist_rejects_meshmon_with_non_ident_suffix() {
        assert!(!is_meshmon_query("meshmon_"));
        assert!(!is_meshmon_query("meshmon_1foo"));
    }

    #[test]
    fn whitelist_rejects_meshmon_as_identifier_suffix() {
        // `foo_meshmon_bar` is a single identifier; must not count as a
        // meshmon metric reference.
        assert!(!is_meshmon_query("foo_meshmon_bar"));
        assert!(!is_meshmon_query("sum(foo_meshmon_baz)"));
    }
}
