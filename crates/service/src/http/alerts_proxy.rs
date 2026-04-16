//! Alertmanager proxy endpoints.
//!
//! - `GET /api/alerts` — list active alerts, normalized to [`AlertSummary`].
//! - `GET /api/alerts/{fingerprint}` — single alert by fingerprint.
//!
//! Both endpoints proxy to the Alertmanager v2 API configured in
//! `config.upstream.alertmanager_url`. When the URL is not set the handlers
//! return 503 (Service Unavailable). When the upstream is unreachable or
//! returns a non-2xx status the handlers return 502 (Bad Gateway).
//!
//! The response is intentionally normalized: the full Alertmanager
//! [`AlertmanagerV2Alert`] payload is mapped to [`AlertSummary`] so the
//! frontend receives a stable, minimal shape that hides upstream schema
//! drift.

use crate::http::http_client::proxy_client;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};

// ---------------------------------------------------------------------------
// Public DTO — stable shape for the frontend
// ---------------------------------------------------------------------------

/// Normalized alert summary exposed by the meshmon API.
///
/// Derived from the Alertmanager v2 alert model, keeping only the fields
/// the frontend needs. `generatorURL` and other upstream-only fields are
/// intentionally dropped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AlertSummary {
    /// Alertmanager fingerprint (hex string), unique per alert group.
    pub fingerprint: String,
    /// Label set attached to the alert (alertname, severity, etc.).
    pub labels: HashMap<String, String>,
    /// Short human-readable summary from the `summary` annotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Longer description from the `description` annotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Alert state: `active`, `suppressed`, or `unprocessed`.
    pub state: String,
    /// RFC 3339 timestamp when the alert started firing.
    pub starts_at: String,
    /// RFC 3339 timestamp when the alert resolved (may be empty/zero for
    /// active alerts).
    pub ends_at: String,
}

// ---------------------------------------------------------------------------
// Private deserialization types — Alertmanager v2 wire format
// ---------------------------------------------------------------------------

/// Alertmanager v2 `/api/v2/alerts` array element.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlertmanagerV2Alert {
    fingerprint: String,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    annotations: HashMap<String, String>,
    #[serde(default)]
    status: AlertmanagerStatus,
    starts_at: String,
    ends_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct AlertmanagerStatus {
    #[serde(default)]
    state: String,
}

impl From<AlertmanagerV2Alert> for AlertSummary {
    fn from(a: AlertmanagerV2Alert) -> Self {
        let summary = a.annotations.get("summary").cloned();
        let description = a.annotations.get("description").cloned();
        Self {
            fingerprint: a.fingerprint,
            labels: a.labels,
            summary,
            description,
            state: a.status.state,
            starts_at: a.starts_at,
            ends_at: a.ends_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Query parameters forwarded to the upstream Alertmanager.
#[derive(Debug, Deserialize, IntoParams)]
pub struct AlertsQuery {
    /// Include active alerts.
    pub active: Option<bool>,
    /// Include silenced alerts.
    pub silenced: Option<bool>,
    /// Include inhibited alerts.
    pub inhibited: Option<bool>,
    /// Include unprocessed alerts.
    pub unprocessed: Option<bool>,
    /// PromQL-style label matchers, e.g. `alertname="HighLatency"`.
    /// May be repeated.
    pub filter: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the configured Alertmanager base URL, if any.
fn alertmanager_base(state: &AppState) -> Option<String> {
    state.config().upstream.alertmanager_url.clone()
}

/// Build `Vec<(key, value)>` pairs from [`AlertsQuery`] for forwarding to
/// upstream. Repeated `filter` entries each become a separate pair so
/// reqwest serializes them as `?filter=a&filter=b`.
fn query_pairs(q: &AlertsQuery) -> Vec<(&str, String)> {
    let mut pairs = Vec::new();
    if let Some(v) = q.active {
        pairs.push(("active", v.to_string()));
    }
    if let Some(v) = q.silenced {
        pairs.push(("silenced", v.to_string()));
    }
    if let Some(v) = q.inhibited {
        pairs.push(("inhibited", v.to_string()));
    }
    if let Some(v) = q.unprocessed {
        pairs.push(("unprocessed", v.to_string()));
    }
    if let Some(filters) = &q.filter {
        for f in filters {
            pairs.push(("filter", f.clone()));
        }
    }
    pairs
}

/// Fetch alerts from the Alertmanager v2 API and normalize.
async fn fetch_alerts(base: &str, q: &AlertsQuery) -> Result<Vec<AlertSummary>, ProxyError> {
    let url = format!("{base}/api/v2/alerts");
    let pairs = query_pairs(q);
    let resp = proxy_client()
        .get(&url)
        .query(&pairs)
        .send()
        .await
        .map_err(|e| ProxyError::Upstream(format!("request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProxyError::Upstream(format!(
            "upstream returned {status}: {body}"
        )));
    }

    let alerts: Vec<AlertmanagerV2Alert> = resp
        .json()
        .await
        .map_err(|e| ProxyError::Upstream(format!("failed to parse response: {e}")))?;

    Ok(alerts.into_iter().map(AlertSummary::from).collect())
}

/// Internal error type — mapped to HTTP status codes by the handlers.
enum ProxyError {
    /// Alertmanager URL not configured.
    NotConfigured,
    /// Upstream request failed or returned non-2xx.
    Upstream(String),
}

impl ProxyError {
    fn into_response(self) -> Response {
        match self {
            Self::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "alertmanager_url not configured" })),
            )
                .into_response(),
            Self::Upstream(msg) => {
                tracing::warn!(error = %msg, "alertmanager proxy error");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "error": msg })),
                )
                    .into_response()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/alerts` — proxy active alerts from Alertmanager.
///
/// Returns a normalized `Vec<AlertSummary>`. Upstream query parameters
/// (`active`, `silenced`, `inhibited`, `unprocessed`, `filter`) are
/// forwarded verbatim.
///
/// - **503** when `upstream.alertmanager_url` is not configured.
/// - **502** when the upstream is unreachable or returns a non-2xx status.
#[utoipa::path(
    get,
    path = "/api/alerts",
    tag = "alerts",
    params(AlertsQuery),
    responses(
        (status = 200, description = "Active alerts", body = Vec<AlertSummary>),
        (status = 401, description = "No active session"),
        (status = 502, description = "Upstream error"),
        (status = 503, description = "Alertmanager not configured"),
    ),
)]
pub async fn list_alerts(State(state): State<AppState>, Query(q): Query<AlertsQuery>) -> Response {
    let base = match alertmanager_base(&state) {
        Some(b) => b,
        None => return ProxyError::NotConfigured.into_response(),
    };

    match fetch_alerts(&base, &q).await {
        Ok(alerts) => Json(alerts).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /api/alerts/{fingerprint}` — single alert by fingerprint.
///
/// Fetches the full alert list from Alertmanager and filters client-side
/// by fingerprint (the v2 API has no single-alert endpoint).
///
/// - **404** when no alert matches the fingerprint.
/// - **502** when the upstream is unreachable or returns a non-2xx status.
/// - **503** when `upstream.alertmanager_url` is not configured.
#[utoipa::path(
    get,
    path = "/api/alerts/{fingerprint}",
    tag = "alerts",
    params(
        ("fingerprint" = String, Path, description = "Alert fingerprint (hex)"),
    ),
    responses(
        (status = 200, description = "Alert detail", body = AlertSummary),
        (status = 401, description = "No active session"),
        (status = 404, description = "Alert not found"),
        (status = 502, description = "Upstream error"),
        (status = 503, description = "Alertmanager not configured"),
    ),
)]
pub async fn get_alert(State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    let base = match alertmanager_base(&state) {
        Some(b) => b,
        None => return ProxyError::NotConfigured.into_response(),
    };

    // Fetch all alerts (the v2 API doesn't support single-fingerprint lookup).
    let empty_query = AlertsQuery {
        active: None,
        silenced: None,
        inhibited: None,
        unprocessed: None,
        filter: None,
    };

    match fetch_alerts(&base, &empty_query).await {
        Ok(alerts) => match alerts.into_iter().find(|a| a.fingerprint == fingerprint) {
            Some(alert) => Json(alert).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "alert not found" })),
            )
                .into_response(),
        },
        Err(e) => e.into_response(),
    }
}
