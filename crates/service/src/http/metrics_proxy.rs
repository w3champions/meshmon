//! VictoriaMetrics PromQL proxy.
//!
//! Forwards GET /api/metrics/query -> VM /api/v1/query and
//! GET /api/metrics/query_range -> VM /api/v1/query_range. Rejects
//! queries that don't start with `meshmon_<ident>` as defense-in-depth.

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
    /// PromQL expression. Must start with `meshmon_`.
    pub query: String,
    /// Optional evaluation timestamp (RFC 3339 or Unix epoch).
    pub time: Option<String>,
}

/// Query parameters for `GET /api/metrics/query_range` (range query).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RangeQuery {
    /// PromQL expression. Must start with `meshmon_`.
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

/// Return true when `q` starts with `meshmon_` followed by at least one
/// ASCII lowercase letter or underscore. The remainder of the query string
/// is forwarded as-is; this check is prefix-only defense-in-depth, not a
/// full PromQL parser (spec 03 § proxy endpoints).
fn is_meshmon_query(q: &str) -> bool {
    let Some(rest) = q.strip_prefix("meshmon_") else {
        return false;
    };
    matches!(rest.chars().next(), Some(c) if c.is_ascii_lowercase() || c == '_')
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
/// The query expression must start with `meshmon_` followed by a valid
/// identifier character (ASCII lowercase or underscore). Queries that do
/// not match are rejected with 400.
///
/// - **400** when the query doesn't start with `meshmon_<ident>`.
/// - **502** when the upstream is unreachable or returns an error.
/// - **503** when `upstream.vm_url` is not configured.
#[utoipa::path(
    get,
    path = "/api/metrics/query",
    tag = "metrics",
    params(
        ("query" = String, Query, description = "PromQL expression (must start with meshmon_)"),
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
            Json(serde_json::json!({ "error": "query must start with a meshmon_ metric name" })),
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
/// The query expression must start with `meshmon_` followed by a valid
/// identifier character (ASCII lowercase or underscore). Queries that do
/// not match are rejected with 400.
///
/// - **400** when the query doesn't start with `meshmon_<ident>`.
/// - **502** when the upstream is unreachable or returns an error.
/// - **503** when `upstream.vm_url` is not configured.
#[utoipa::path(
    get,
    path = "/api/metrics/query_range",
    tag = "metrics",
    params(
        ("query" = String, Query, description = "PromQL expression (must start with meshmon_)"),
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
            Json(serde_json::json!({ "error": "query must start with a meshmon_ metric name" })),
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
        // Operator expressions are allowed — whitelist is prefix-only
        // defense-in-depth, not a full PromQL parser.
        assert!(is_meshmon_query("meshmon_rtt > 0"));
    }

    #[test]
    fn whitelist_rejects_leading_whitespace() {
        // Valid PromQL at the wire doesn't start with whitespace; reject
        // to avoid unicode-whitespace bypass.
        assert!(!is_meshmon_query(" meshmon_foo"));
        assert!(!is_meshmon_query("\tmeshmon_foo"));
    }

    #[test]
    fn whitelist_rejects_non_meshmon() {
        assert!(!is_meshmon_query("up"));
        assert!(!is_meshmon_query("node_memory_MemFree_bytes"));
        assert!(!is_meshmon_query("sum(meshmon_path_probe_count)")); // starts with sum
        assert!(!is_meshmon_query(""));
    }

    #[test]
    fn whitelist_rejects_meshmon_with_non_ident_suffix() {
        assert!(!is_meshmon_query("meshmon_"));
        assert!(!is_meshmon_query("meshmon_1foo"));
    }
}
