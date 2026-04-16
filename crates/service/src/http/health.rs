//! Liveness, readiness, and self-metrics endpoints.
//!
//! - `/healthz`: 200 while the process is up. Never gated.
//! - `/readyz`: 200 once `AppState::is_ready()` is set; 503 otherwise.
//!   Ungated.
//! - `/metrics`: Prometheus text-format exposition. Renders the shared
//!   `PrometheusHandle`, then appends per-agent gauges from the
//!   registry snapshot. When `[service.metrics_auth]` is configured,
//!   the router (see `http::mod`) wraps this route in the Basic auth
//!   middleware; deregistered agents disappear on the next scrape
//!   because these gauges are appended directly rather than emitted
//!   through the stateful `metrics` facade.

use crate::registry::AgentInfo;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::borrow::Cow;
use std::fmt::Write;

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> Response {
    if state.is_ready() {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

/// `/metrics` — Prometheus text-format exposition. Renders the shared
/// [`crate::metrics::Handle`], refreshes `meshmon_service_uptime_seconds`
/// at scrape time, then appends per-agent gauges from the registry
/// snapshot so deregistered agents drop out on the next scrape without
/// stale state stuck in the `metrics` facade.
pub async fn metrics(State(state): State<AppState>) -> Response {
    // Refresh uptime at scrape time — always fresh, no timer drift.
    crate::metrics::uptime_seconds().set(state.started_at.elapsed().as_secs_f64());

    let mut body = state.prom.render();
    append_agent_metrics(&mut body, state.registry.snapshot().all());

    (
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

fn append_agent_metrics(body: &mut String, agents: Vec<AgentInfo>) {
    if agents.is_empty() {
        return;
    }
    body.push_str(
        "# HELP meshmon_agent_info Registered agent metadata (gauge=1 per known agent).\n",
    );
    body.push_str("# TYPE meshmon_agent_info gauge\n");
    for a in &agents {
        let version = a.agent_version.as_deref().unwrap_or("");
        let _ = writeln!(
            body,
            "meshmon_agent_info{{source=\"{}\",agent_version=\"{}\"}} 1",
            prom_escape(&a.id),
            prom_escape(version),
        );
    }
    body.push_str(
        "# HELP meshmon_agent_last_seen_seconds Unix timestamp of the agent's last push.\n",
    );
    body.push_str("# TYPE meshmon_agent_last_seen_seconds gauge\n");
    for a in &agents {
        let _ = writeln!(
            body,
            "meshmon_agent_last_seen_seconds{{source=\"{}\"}} {}",
            prom_escape(&a.id),
            a.last_seen_at.timestamp(),
        );
    }
}

/// Escape a Prometheus label value per the exposition format spec:
/// backslash → `\\`, double-quote → `\"`, newline → `\n`.
fn prom_escape(s: &str) -> Cow<'_, str> {
    if !s
        .as_bytes()
        .iter()
        .any(|&b| b == b'\\' || b == b'"' || b == b'\n')
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Build the health sub-router. `/metrics` auth is attached at the
/// outer router level (see `http::mod`) so this function knows nothing
/// about it.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prom_escape_passthrough_for_plain_input() {
        assert_eq!(prom_escape("agent-1"), "agent-1");
    }

    #[test]
    fn prom_escape_handles_quote_backslash_newline() {
        assert_eq!(prom_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(prom_escape("a\\b"), r"a\\b");
        assert_eq!(prom_escape("a\nb"), r"a\nb");
    }

    #[test]
    fn append_agent_metrics_noop_when_empty() {
        let mut body = String::new();
        append_agent_metrics(&mut body, Vec::new());
        assert!(body.is_empty());
    }
}
