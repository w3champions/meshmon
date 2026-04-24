//! Server-side VictoriaMetrics read client. Issues typed PromQL instant
//! queries against `/api/v1/query` and parses the native JSON into typed
//! samples. Used by the campaign evaluator to pull agent-to-agent baseline
//! RTT/loss from continuous-mesh data so the evaluator does not need the
//! active campaign to re-measure the mesh.
//!
//! Read-only companion to the FE-facing `http/metrics_proxy.rs`, which
//! forwards arbitrary PromQL from the browser. This module issues a
//! narrow set of service-owned queries server-side.

use crate::http::http_client::proxy_client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

/// Errors surfaced by [`fetch_agent_baselines`].
#[derive(Debug, thiserror::Error)]
pub enum VmQueryError {
    /// The service config does not set `[upstream.vm_url]`. Operator-actionable.
    #[error("VictoriaMetrics URL not configured — set [upstream.vm_url]")]
    NotConfigured,
    /// `reqwest` failed the send (connection refused, DNS error, timeout,
    /// TLS, etc.).
    #[error("upstream request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// VM replied with a non-2xx status.
    #[error("upstream returned status {0}")]
    UpstreamStatus(u16),
    /// Upstream returned 2xx but the body didn't deserialise into the
    /// expected vector-result shape, or carried `status=error`.
    #[error("upstream returned malformed response: {0}")]
    MalformedResponse(String),
}

/// One agent-to-agent baseline sample from VictoriaMetrics.
///
/// Field convention:
/// * `latency_avg_ms` / `latency_stddev_ms` are ms (VM values are in
///   microseconds and divided by 1000 before landing here).
/// * `loss_ratio` is a fraction (0.0 – 1.0), matching the DB + agent
///   convention after the `loss_pct` → `loss_ratio` rename.
#[derive(Debug, Clone)]
pub struct AgentBaselineSample {
    /// Source agent id (`source` label from the metric).
    pub source_agent_id: String,
    /// Target agent id (`target` label from the metric).
    pub target_agent_id: String,
    /// Mean RTT in milliseconds. `None` when VM surfaced no `_avg_micros`
    /// sample for the pair inside the lookback window.
    pub latency_avg_ms: Option<f32>,
    /// RTT stddev in milliseconds. `None` when VM surfaced no
    /// `_stddev_micros` sample for the pair inside the lookback window.
    pub latency_stddev_ms: Option<f32>,
    /// Loss fraction (0.0 – 1.0). `None` when VM surfaced no
    /// `_failure_rate` sample for the pair inside the lookback window.
    pub loss_ratio: Option<f32>,
}

// ---------------------------------------------------------------------------
// VM response shape
// ---------------------------------------------------------------------------

/// Top-level envelope returned by `GET /api/v1/query`.
#[derive(Debug, Deserialize)]
struct VmEnvelope {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    data: Option<VmData>,
}

/// `data` sub-object of an instant query envelope.
#[derive(Debug, Deserialize)]
struct VmData {
    #[serde(rename = "resultType")]
    result_type: String,
    result: Vec<VmVectorSample>,
}

/// One sample row inside a `resultType: "vector"` response.
#[derive(Debug, Deserialize)]
struct VmVectorSample {
    metric: HashMap<String, String>,
    /// `[<timestamp>, "<value-as-string>"]` per Prometheus convention.
    value: (f64, String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch agent-to-agent baseline RTT + loss for a protocol, averaged
/// over the lookback window. `vm_url` is the trimmed base (no trailing
/// slash); `agent_ids` are the IDs that should appear in either the
/// `source` or `target` label (regex-joined with `|`); `protocol` is
/// the label value.
///
/// Returns one [`AgentBaselineSample`] per `(source, target)` pair that
/// VM surfaced in the result set. Absent pairs are simply missing — not
/// an error — the caller treats a missing pair as "no baseline available
/// for that direction".
pub async fn fetch_agent_baselines(
    vm_url: &str,
    agent_ids: &[String],
    protocol: &str,
    lookback: Duration,
) -> Result<Vec<AgentBaselineSample>, VmQueryError> {
    if agent_ids.is_empty() {
        return Ok(Vec::new());
    }
    let BaselineQueries {
        rtt: rtt_query,
        stddev: stddev_query,
        loss: loss_query,
    } = build_baseline_queries(agent_ids, protocol, lookback);

    let rtt_samples = run_instant_query(vm_url, &rtt_query).await?;
    let stddev_samples = run_instant_query(vm_url, &stddev_query).await?;
    let loss_samples = run_instant_query(vm_url, &loss_query).await?;

    Ok(merge_samples(rtt_samples, stddev_samples, loss_samples))
}

/// The three PromQL instant queries built for one call. Pulled out of
/// [`fetch_agent_baselines`] so the escape/quote plumbing is unit-testable
/// without spinning up an HTTP round-trip.
struct BaselineQueries {
    rtt: String,
    stddev: String,
    loss: String,
}

/// Build the three instant-query strings. Two-stage escape on each
/// agent id: RE2 metacharacters first (so the regex alternation stays a
/// valid regex), then PromQL label-value escaping (so a `"` or `\` in
/// the id can't break out of the surrounding double-quoted string
/// literal). The ingestion validators don't enforce a strict charset;
/// defence in depth.
fn build_baseline_queries(
    agent_ids: &[String],
    protocol: &str,
    lookback: Duration,
) -> BaselineQueries {
    let id_alternation: String = agent_ids
        .iter()
        .map(|id| escape_label_value(&regex_escape(id)))
        .collect::<Vec<_>>()
        .join("|");
    let lookback_s = format!("{}s", lookback.as_secs().max(1));
    let proto = escape_label_value(protocol);

    let rtt = format!(
        "avg_over_time(meshmon_path_rtt_avg_micros{{source=~\"{ids}\",target=~\"{ids}\",protocol=\"{proto}\"}}[{win}]) / 1000",
        ids = id_alternation,
        proto = proto,
        win = lookback_s,
    );
    let stddev = format!(
        "avg_over_time(meshmon_path_rtt_stddev_micros{{source=~\"{ids}\",target=~\"{ids}\",protocol=\"{proto}\"}}[{win}]) / 1000",
        ids = id_alternation,
        proto = proto,
        win = lookback_s,
    );
    let loss = format!(
        "avg_over_time(meshmon_path_failure_rate{{source=~\"{ids}\",target=~\"{ids}\",protocol=\"{proto}\"}}[{win}])",
        ids = id_alternation,
        proto = proto,
        win = lookback_s,
    );
    BaselineQueries { rtt, stddev, loss }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Execute one PromQL instant query against VM and return the parsed
/// `[ (source, target, value) ]` tuples. Drops self-pairs and any row
/// whose value is not a finite float.
async fn run_instant_query(
    vm_url: &str,
    query: &str,
) -> Result<Vec<(String, String, f32)>, VmQueryError> {
    let url = format!("{vm_url}/api/v1/query");
    let response = proxy_client()
        .get(&url)
        .query(&[("query", query)])
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        tracing::warn!(
            upstream_status = status,
            query = %query,
            "vm_query: VictoriaMetrics returned non-2xx"
        );
        return Err(VmQueryError::UpstreamStatus(status));
    }

    let envelope: VmEnvelope = response.json().await?;
    if envelope.status != "success" {
        return Err(VmQueryError::MalformedResponse(format!(
            "status={}{}",
            envelope.status,
            envelope
                .error
                .as_deref()
                .map(|e| format!(" error={e}"))
                .unwrap_or_default(),
        )));
    }
    let Some(data) = envelope.data else {
        return Err(VmQueryError::MalformedResponse("missing data field".into()));
    };
    if data.result_type != "vector" {
        return Err(VmQueryError::MalformedResponse(format!(
            "unexpected resultType {}",
            data.result_type
        )));
    }

    let mut out: Vec<(String, String, f32)> = Vec::with_capacity(data.result.len());
    for sample in data.result {
        let source = match sample.metric.get("source") {
            Some(v) => v.clone(),
            None => continue,
        };
        let target = match sample.metric.get("target") {
            Some(v) => v.clone(),
            None => continue,
        };
        if source == target {
            continue;
        }
        let parsed: f32 = match sample.value.1.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !parsed.is_finite() {
            continue;
        }
        out.push((source, target, parsed));
    }
    Ok(out)
}

/// Left-join three per-metric result sets on `(source, target)` into a
/// single sample per pair. The RTT result drives presence — pairs that
/// appear only in the stddev or loss set are dropped because they have
/// no usable latency baseline downstream.
fn merge_samples(
    rtt: Vec<(String, String, f32)>,
    stddev: Vec<(String, String, f32)>,
    loss: Vec<(String, String, f32)>,
) -> Vec<AgentBaselineSample> {
    let mut stddev_map: HashMap<(String, String), f32> = HashMap::with_capacity(stddev.len());
    for (s, t, v) in stddev {
        stddev_map.insert((s, t), v);
    }
    let mut loss_map: HashMap<(String, String), f32> = HashMap::with_capacity(loss.len());
    for (s, t, v) in loss {
        loss_map.insert((s, t), v);
    }

    let mut out = Vec::with_capacity(rtt.len());
    for (source, target, rtt_ms) in rtt {
        let key = (source.clone(), target.clone());
        let stddev_ms = stddev_map.get(&key).copied();
        let loss = loss_map.get(&key).copied();
        out.push(AgentBaselineSample {
            source_agent_id: source,
            target_agent_id: target,
            latency_avg_ms: Some(rtt_ms),
            latency_stddev_ms: stddev_ms,
            loss_ratio: loss,
        });
    }
    out
}

/// Escape a string so it is safe to embed inside a PromQL regex
/// alternation. VictoriaMetrics parses label regexes as RE2, which
/// rejects escapes of non-metacharacters (e.g. `\-` → parse error).
/// So this mirrors Go's `regexp.QuoteMeta`: only the true RE2
/// metacharacters outside character classes are escaped.
fn regex_escape(input: &str) -> String {
    const METACHARS: &[char] = &[
        '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$',
    ];
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if METACHARS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape a PromQL label-value so a `"` or `\` in the input can't break
/// out of the selector. Very minimal — the protocol values we pass are
/// enum-constrained, but keep the helper around as defence in depth.
fn escape_label_value(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_escape_escapes_re2_metacharacters_only() {
        // Hyphen is not a RE2 metacharacter outside a character class —
        // escaping it produces `\-`, which RE2 rejects with a parse error.
        assert_eq!(regex_escape("agent-01"), "agent-01");
        assert_eq!(regex_escape("dev-agent-1"), "dev-agent-1");
        // Real metacharacters still get the backslash.
        assert_eq!(regex_escape("a|b"), "a\\|b");
        assert_eq!(regex_escape("a.b{c}"), "a\\.b\\{c\\}");
        assert_eq!(regex_escape("a(b)*"), "a\\(b\\)\\*");
    }

    #[test]
    fn escape_label_value_escapes_quotes_and_backslashes() {
        assert_eq!(escape_label_value("icmp"), "icmp");
        assert_eq!(escape_label_value("a\"b"), "a\\\"b");
        assert_eq!(escape_label_value("a\\b"), "a\\\\b");
    }

    #[test]
    fn build_baseline_queries_escapes_quotes_in_agent_ids() {
        // Regression for T54-05: a malicious agent id containing `"`
        // must stay inside the label-value string literal. Before the
        // two-stage escape, `x"injection` would close the string early
        // and splice arbitrary PromQL into the selector.
        let ids = vec!["ok-id".to_string(), "x\"injection".to_string()];
        let q = build_baseline_queries(&ids, "icmp", Duration::from_secs(60));

        // The escaped `"` must appear as `\"`; there must be no bare `"`
        // that would close the selector and let the id-alternation leak
        // into surrounding PromQL.
        assert!(
            q.rtt.contains("x\\\"injection"),
            "agent id quote must be escaped, got: {}",
            q.rtt
        );
        // Every `"` in the final string must be preceded by `\` — i.e.
        // no bare quote leaked from an unescaped id.
        let bytes = q.rtt.as_bytes();
        for i in 0..bytes.len() {
            if bytes[i] == b'"' {
                assert!(
                    i > 0 && (bytes[i - 1] == b'\\' || is_outer_quote(bytes, i)),
                    "unescaped `\"` at byte {i} in query: {}",
                    q.rtt
                );
            }
        }
    }

    /// True if the `"` at byte offset `i` is one of the outer quotes that
    /// opens/closes a selector label-value. Those are the only bare `"`
    /// bytes we expect: `{source=~"…"`, `,target=~"…"`, `,protocol="…"`.
    /// An unescaped `"` anywhere else signals an injection.
    fn is_outer_quote(bytes: &[u8], i: usize) -> bool {
        // Bytes that precede an opening outer quote in our template.
        // Either `~` (for regex-match operators) or `=` (for eq match).
        i > 0 && (bytes[i - 1] == b'~' || bytes[i - 1] == b'=')
            // or a closing outer quote: followed by `,` (next label),
            // `}` (selector end).
            || (i + 1 < bytes.len() && (bytes[i + 1] == b',' || bytes[i + 1] == b'}'))
    }

    #[test]
    fn build_baseline_queries_embeds_ids_and_protocol() {
        // Sanity-check: a plain id shows up unescaped; the protocol
        // label binds with `=` not `=~`.
        let q = build_baseline_queries(
            &["agent-a".into(), "agent-b".into()],
            "icmp",
            Duration::from_secs(3600),
        );
        assert!(q.rtt.contains("source=~\"agent-a|agent-b\""), "{}", q.rtt);
        assert!(q.rtt.contains("target=~\"agent-a|agent-b\""), "{}", q.rtt);
        assert!(q.rtt.contains("protocol=\"icmp\""), "{}", q.rtt);
        assert!(q.stddev.contains("[3600s]"), "{}", q.stddev);
        assert!(q.loss.contains("meshmon_path_failure_rate"), "{}", q.loss);
    }

    #[test]
    fn merge_samples_left_joins_on_rtt_presence() {
        let rtt = vec![
            ("a".into(), "b".into(), 12.5),
            ("a".into(), "c".into(), 20.0),
        ];
        let stddev = vec![("a".into(), "b".into(), 1.2)];
        let loss = vec![
            ("a".into(), "b".into(), 0.01),
            ("a".into(), "c".into(), 0.0),
        ];
        let merged = merge_samples(rtt, stddev, loss);
        assert_eq!(merged.len(), 2);
        let ab = merged
            .iter()
            .find(|s| s.source_agent_id == "a" && s.target_agent_id == "b")
            .unwrap();
        assert_eq!(ab.latency_avg_ms, Some(12.5));
        assert_eq!(ab.latency_stddev_ms, Some(1.2));
        assert_eq!(ab.loss_ratio, Some(0.01));
        let ac = merged
            .iter()
            .find(|s| s.source_agent_id == "a" && s.target_agent_id == "c")
            .unwrap();
        assert_eq!(ac.latency_avg_ms, Some(20.0));
        assert_eq!(ac.latency_stddev_ms, None);
        assert_eq!(ac.loss_ratio, Some(0.0));
    }

    #[test]
    fn merge_samples_drops_pairs_without_rtt() {
        // Stddev-only or loss-only pairs must not synthesise a sample.
        let merged = merge_samples(
            Vec::new(),
            vec![("a".into(), "b".into(), 1.0)],
            vec![("a".into(), "b".into(), 0.0)],
        );
        assert!(merged.is_empty());
    }
}
