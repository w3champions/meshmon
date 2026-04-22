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
use std::borrow::Cow;
use std::fmt::Write;

// ---------------------------------------------------------------------------
// Agent metric name constants — single source of truth for names used in the
// custom text-format block appended to /metrics.  Alert rules in
// deploy/alerts/rules.yaml reference these metric names; the cross-check
// (the alert_metrics_contract integration test) looks for the quoted string
// literal in the service source, which these constants provide.
// ---------------------------------------------------------------------------

/// Gauge (= 1): registered agent metadata. Labels: `source`, `agent_version`.
pub const AGENT_INFO: &str = "meshmon_agent_info";
/// Gauge: Unix timestamp of the agent's last ingestion push. Label: `source`.
pub const AGENT_LAST_SEEN_SECONDS: &str = "meshmon_agent_last_seen_seconds";

/// `/healthz` — liveness probe. Returns 200 for as long as the process
/// is up. Never gated by auth.
pub async fn healthz() -> &'static str {
    "ok"
}

/// `/readyz` — readiness probe. 200 once [`AppState::is_ready`] is set
/// (DB migrations applied, registry warmed), otherwise 503 so k8s keeps
/// draining traffic from the pod. Never gated by auth.
pub async fn readyz(State(state): State<AppState>) -> Response {
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

fn append_agent_metrics(body: &mut String, mut agents: Vec<AgentInfo>) {
    if agents.is_empty() {
        return;
    }
    // Stable iteration order across scrapes: `RegistrySnapshot::all()`
    // yields agents in `HashMap` order (randomized per-process), which
    // reshuffles Grafana legend colors and breaks body diffs. Sort once
    // by id — O(n log n) is dwarfed by the writeln! calls that follow.
    agents.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    let _ = writeln!(
        body,
        "# HELP {AGENT_INFO} Registered agent metadata (gauge=1 per known agent)."
    );
    let _ = writeln!(body, "# TYPE {AGENT_INFO} gauge");
    for a in &agents {
        let version = a.agent_version.as_deref().unwrap_or("");
        let _ = writeln!(
            body,
            "{AGENT_INFO}{{source=\"{}\",agent_version=\"{}\"}} 1",
            prom_escape(&a.id),
            prom_escape(version),
        );
    }
    let _ = writeln!(
        body,
        "# HELP {AGENT_LAST_SEEN_SECONDS} Unix timestamp of the agent's last push."
    );
    let _ = writeln!(body, "# TYPE {AGENT_LAST_SEEN_SECONDS} gauge");
    for a in &agents {
        let _ = writeln!(
            body,
            "{AGENT_LAST_SEEN_SECONDS}{{source=\"{}\"}} {}",
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

    #[test]
    fn append_agent_metrics_emits_sorted_gauges_with_help_and_type() {
        use chrono::{DateTime, Utc};
        use sqlx::types::ipnetwork::IpNetwork;
        use std::str::FromStr;

        fn mk(id: &str, version: Option<&str>, ts: i64) -> AgentInfo {
            AgentInfo {
                id: id.to_string(),
                display_name: String::new(),
                location: None,
                ip: IpNetwork::from_str("127.0.0.1/32").unwrap(),
                latitude: None,
                longitude: None,
                tcp_probe_port: 8002,
                udp_probe_port: 8005,
                agent_version: version.map(str::to_string),
                registered_at: Utc::now(),
                last_seen_at: DateTime::from_timestamp(ts, 0).expect("valid timestamp"),
                campaign_max_concurrency: None,
            }
        }

        // Intentionally out of id order so the sort inside
        // `append_agent_metrics` is the only thing that can order them
        // as asserted below.
        let agents = vec![
            mk("node-2", Some("0.3.1"), 1_700_000_042),
            mk("node-1", Some("0.2.0"), 1_700_000_000),
        ];

        let mut body = String::new();
        append_agent_metrics(&mut body, agents);

        // HELP + TYPE lines for both gauges.
        assert!(
            body.contains(
                "# HELP meshmon_agent_info Registered agent metadata (gauge=1 per known agent).\n"
            ),
            "missing meshmon_agent_info HELP: {body}"
        );
        assert!(
            body.contains("# TYPE meshmon_agent_info gauge\n"),
            "missing meshmon_agent_info TYPE: {body}"
        );
        assert!(
            body.contains(
                "# HELP meshmon_agent_last_seen_seconds Unix timestamp of the agent's last push.\n"
            ),
            "missing meshmon_agent_last_seen_seconds HELP: {body}"
        );
        assert!(
            body.contains("# TYPE meshmon_agent_last_seen_seconds gauge\n"),
            "missing meshmon_agent_last_seen_seconds TYPE: {body}"
        );

        // Exact sample lines for both agents, covering the non-empty
        // `append_agent_metrics` branch end-to-end.
        let info_1 = r#"meshmon_agent_info{source="node-1",agent_version="0.2.0"} 1"#;
        let info_2 = r#"meshmon_agent_info{source="node-2",agent_version="0.3.1"} 1"#;
        let seen_1 = r#"meshmon_agent_last_seen_seconds{source="node-1"} 1700000000"#;
        let seen_2 = r#"meshmon_agent_last_seen_seconds{source="node-2"} 1700000042"#;
        for expected in [info_1, info_2, seen_1, seen_2] {
            assert!(
                body.lines().any(|l| l == expected),
                "missing line {expected:?} in:\n{body}"
            );
        }

        // Sort order: node-1 must precede node-2 in both sections even
        // though the input vec was reversed.
        let info_1_pos = body.find(info_1).expect("info_1 present");
        let info_2_pos = body.find(info_2).expect("info_2 present");
        assert!(
            info_1_pos < info_2_pos,
            "expected info lines sorted by id: node-1 before node-2\n{body}"
        );
        let seen_1_pos = body.find(seen_1).expect("seen_1 present");
        let seen_2_pos = body.find(seen_2).expect("seen_2 present");
        assert!(
            seen_1_pos < seen_2_pos,
            "expected last-seen lines sorted by id: node-1 before node-2\n{body}"
        );
    }
}
