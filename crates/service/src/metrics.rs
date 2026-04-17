//! Service self-metrics: one place for every metric the service emits.
//!
//! Design: each metric has three things **in this file**:
//! 1. a `pub const` for the metric name,
//! 2. a `describe_*!` entry inside [`describe_service_metrics`],
//! 3. a typed accessor (`fn name(...) -> Counter|Gauge|Histogram`).
//!
//! Call sites import the accessors. A typo or rename in the constant
//! becomes a compile error at every emission site. There are no other
//! `metrics::counter!("meshmon_*")` string literals in the crate.

use crate::state::BuildInfo;
use metrics::{
    counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram, Counter,
    Gauge, Histogram, Unit,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Metric name constants — single source of truth.
// ---------------------------------------------------------------------------

/// Gauge: seconds since the service process started.
pub const UPTIME_SECONDS: &str = "meshmon_service_uptime_seconds";
/// Gauge (= 1): service build metadata. Labels: `version`, `commit`.
pub const BUILD_INFO: &str = "meshmon_service_build_info";
/// Counter: ingestion batches processed. Label: `outcome`.
pub const INGEST_BATCHES_TOTAL: &str = "meshmon_service_ingest_batches_total";
/// Counter: samples successfully shipped to VictoriaMetrics.
pub const INGEST_SAMPLES_TOTAL: &str = "meshmon_service_ingest_samples_total";
/// Counter: items dropped from ingestion queues. Label: `source`.
pub const INGEST_DROPPED_TOTAL: &str = "meshmon_service_ingest_dropped_total";
/// Histogram: wall time of one VictoriaMetrics remote-write POST.
pub const VM_WRITE_DURATION_SECONDS: &str = "meshmon_service_vm_write_duration_seconds";
/// Histogram: wall time of one `route_snapshots` INSERT.
pub const PG_SNAPSHOT_DURATION_SECONDS: &str = "meshmon_service_pg_snapshot_duration_seconds";
/// Counter: successful `agents.last_seen_at` UPDATEs (post-debounce).
pub const LAST_SEEN_WRITES_TOTAL: &str = "meshmon_service_last_seen_writes_total";
/// Gauge: registered agents split by freshness. Label: `state`.
pub const REGISTRY_AGENTS: &str = "meshmon_service_registry_agents";
/// Gauge: seconds since the registry snapshot was last refreshed.
pub const REGISTRY_LAST_REFRESH_AGE_SECONDS: &str =
    "meshmon_service_registry_last_refresh_age_seconds";
/// Counter: periodic registry refreshes that failed.
pub const REGISTRY_REFRESH_ERRORS_TOTAL: &str = "meshmon_service_registry_refresh_errors_total";
/// Gauge: number of agents with an active reverse tunnel.
pub const TUNNEL_AGENTS: &str = "meshmon_service_tunnel_agents";
/// Counter: service → agent command RPCs. Labels: `method`, `outcome`.
pub const COMMAND_RPCS_TOTAL: &str = "meshmon_service_command_rpcs_total";

// HTTP request counter names (`meshmon_service_http_requests_total`,
// `..._duration_seconds`, `..._pending`) are not declared here — they
// are renamed at compile time via the `AXUM_HTTP_*` env vars in
// `.cargo/config.toml`. That is the only mechanism in axum-prometheus 0.10
// that propagates a static prefix without requiring the builder's
// `Paired` transition, which uses an `OnceLock` and therefore can only
// fire once per process (incompatible with integration tests that
// instantiate the router from multiple `#[tokio::test]`s).

/// Every self-metric that carries a stable HELP/TYPE pair. Drives the
/// in-module test that fails if a new metric is added without a
/// corresponding `describe_*!` entry.
///
/// Intentionally excludes [`BUILD_INFO`]: it is emitted once at startup
/// as a labeled gauge = 1, so its describe coverage is checked by a
/// separate assertion rather than through this slice.
#[cfg(test)]
const ALL_METRIC_NAMES: &[&str] = &[
    UPTIME_SECONDS,
    INGEST_BATCHES_TOTAL,
    INGEST_SAMPLES_TOTAL,
    INGEST_DROPPED_TOTAL,
    VM_WRITE_DURATION_SECONDS,
    PG_SNAPSHOT_DURATION_SECONDS,
    LAST_SEEN_WRITES_TOTAL,
    REGISTRY_AGENTS,
    REGISTRY_LAST_REFRESH_AGE_SECONDS,
    REGISTRY_REFRESH_ERRORS_TOTAL,
    TUNNEL_AGENTS,
    COMMAND_RPCS_TOTAL,
];

// ---------------------------------------------------------------------------
// Label enums — stringly-typed label values kept out of call sites.
// ---------------------------------------------------------------------------

/// Outcome label for `meshmon_service_ingest_batches_total`.
#[derive(Debug, Clone, Copy)]
pub enum BatchOutcome {
    /// Batch shipped successfully.
    Ok,
    /// Batch dropped after retry exhaustion or non-retryable error.
    WriteError,
}

impl BatchOutcome {
    fn as_str(self) -> &'static str {
        match self {
            BatchOutcome::Ok => "ok",
            BatchOutcome::WriteError => "write_error",
        }
    }
}

/// Source label for `meshmon_service_ingest_dropped_total`.
#[derive(Debug, Clone, Copy)]
pub enum DropSource {
    /// Dropped from the metrics sample queue.
    Metrics,
    /// Dropped from the route-snapshot queue.
    Snapshot,
    /// Dropped from the last-seen touch channel.
    Touch,
}

impl DropSource {
    fn as_str(self) -> &'static str {
        match self {
            DropSource::Metrics => "metrics",
            DropSource::Snapshot => "snapshot",
            DropSource::Touch => "touch",
        }
    }
}

/// Outcome label for [`COMMAND_RPCS_TOTAL`].
#[derive(Debug, Clone, Copy)]
pub enum CommandOutcome {
    /// RPC completed successfully.
    Ok,
    /// `tonic::Code::Unavailable` — tunnel dropped or agent gone.
    Unavailable,
    /// `tonic::Code::DeadlineExceeded` — per-call timer fired.
    DeadlineExceeded,
    /// Any other tonic status code.
    Other,
}

impl CommandOutcome {
    fn as_str(self) -> &'static str {
        match self {
            CommandOutcome::Ok => "ok",
            CommandOutcome::Unavailable => "unavailable",
            CommandOutcome::DeadlineExceeded => "deadline_exceeded",
            CommandOutcome::Other => "other",
        }
    }
}

/// State label for `meshmon_service_registry_agents`.
#[derive(Debug, Clone, Copy)]
pub enum AgentState {
    /// Agent seen recently (within freshness window).
    Active,
    /// Agent known but past the freshness window.
    Stale,
}

impl AgentState {
    fn as_str(self) -> &'static str {
        match self {
            AgentState::Active => "active",
            AgentState::Stale => "stale",
        }
    }
}

// ---------------------------------------------------------------------------
// Typed accessors — call sites use these only.
// ---------------------------------------------------------------------------

/// Gauge handle for [`UPTIME_SECONDS`].
pub fn uptime_seconds() -> Gauge {
    gauge!(UPTIME_SECONDS)
}

/// Counter handle for [`INGEST_BATCHES_TOTAL`] with the given outcome.
pub fn ingest_batches(outcome: BatchOutcome) -> Counter {
    counter!(INGEST_BATCHES_TOTAL, "outcome" => outcome.as_str())
}

/// Counter handle for [`INGEST_SAMPLES_TOTAL`].
pub fn ingest_samples() -> Counter {
    counter!(INGEST_SAMPLES_TOTAL)
}

/// Counter handle for [`INGEST_DROPPED_TOTAL`] with the given source.
pub fn ingest_dropped(source: DropSource) -> Counter {
    counter!(INGEST_DROPPED_TOTAL, "source" => source.as_str())
}

/// Histogram handle for [`VM_WRITE_DURATION_SECONDS`].
pub fn vm_write_duration() -> Histogram {
    histogram!(VM_WRITE_DURATION_SECONDS)
}

/// Histogram handle for [`PG_SNAPSHOT_DURATION_SECONDS`].
pub fn pg_snapshot_duration() -> Histogram {
    histogram!(PG_SNAPSHOT_DURATION_SECONDS)
}

/// Counter handle for [`LAST_SEEN_WRITES_TOTAL`].
pub fn last_seen_writes() -> Counter {
    counter!(LAST_SEEN_WRITES_TOTAL)
}

/// Gauge handle for [`REGISTRY_AGENTS`] with the given state.
pub fn registry_agents(state: AgentState) -> Gauge {
    gauge!(REGISTRY_AGENTS, "state" => state.as_str())
}

/// Gauge handle for [`REGISTRY_LAST_REFRESH_AGE_SECONDS`].
pub fn registry_last_refresh_age_seconds() -> Gauge {
    gauge!(REGISTRY_LAST_REFRESH_AGE_SECONDS)
}

/// Counter handle for [`REGISTRY_REFRESH_ERRORS_TOTAL`].
pub fn registry_refresh_errors() -> Counter {
    counter!(REGISTRY_REFRESH_ERRORS_TOTAL)
}

/// Gauge handle for [`TUNNEL_AGENTS`].
pub fn tunnel_agents() -> Gauge {
    gauge!(TUNNEL_AGENTS)
}

/// Counter handle for [`COMMAND_RPCS_TOTAL`] with the given method + outcome.
pub fn command_rpcs(method: &'static str, outcome: CommandOutcome) -> Counter {
    counter!(COMMAND_RPCS_TOTAL, "method" => method, "outcome" => outcome.as_str())
}

// ---------------------------------------------------------------------------
// One-shot: emit build_info with its two static labels.
// ---------------------------------------------------------------------------

/// Emit the one-shot `meshmon_service_build_info` gauge with `version`
/// and `commit` labels set to the current build's values. Callers hit
/// this once at startup; the gauge stays at `1` for the process lifetime.
pub fn emit_build_info(info: BuildInfo) {
    gauge!(
        BUILD_INFO,
        "version" => info.version,
        "commit" => info.commit,
    )
    .set(1.0);
}

// ---------------------------------------------------------------------------
// describe_* — HELP/TYPE metadata. Uses the same constants as the
// accessors, so a rename drifts everywhere at once.
// ---------------------------------------------------------------------------

/// Register HELP/TYPE metadata with the global recorder for every metric
/// in this file. Must be called once, after [`install_recorder`].
pub fn describe_service_metrics() {
    describe_gauge!(
        UPTIME_SECONDS,
        Unit::Seconds,
        "Seconds since the service process started"
    );
    describe_gauge!(
        BUILD_INFO,
        "Service build metadata (gauge = 1). Labels: version, commit."
    );
    describe_counter!(
        INGEST_BATCHES_TOTAL,
        "Ingestion batches processed, labeled by outcome (ok|write_error)"
    );
    describe_counter!(
        INGEST_SAMPLES_TOTAL,
        "Samples successfully shipped to VictoriaMetrics"
    );
    describe_counter!(
        INGEST_DROPPED_TOTAL,
        "Items dropped from ingestion queues (buffer overflow or retry exhaustion). Label: source"
    );
    describe_histogram!(
        VM_WRITE_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time for one VictoriaMetrics remote-write POST (success or final failure)"
    );
    describe_histogram!(
        PG_SNAPSHOT_DURATION_SECONDS,
        Unit::Seconds,
        "Wall time for one route_snapshots INSERT"
    );
    describe_counter!(
        LAST_SEEN_WRITES_TOTAL,
        "Successful agents.last_seen_at UPDATEs (post-debounce)"
    );
    describe_gauge!(
        REGISTRY_AGENTS,
        "Registered agents split by freshness. Label: state=active|stale"
    );
    describe_gauge!(
        REGISTRY_LAST_REFRESH_AGE_SECONDS,
        Unit::Seconds,
        "Seconds since the registry snapshot was last refreshed from Postgres"
    );
    describe_counter!(
        REGISTRY_REFRESH_ERRORS_TOTAL,
        "Periodic registry refreshes that failed (snapshot retained on failure)"
    );
    describe_gauge!(
        TUNNEL_AGENTS,
        Unit::Count,
        "Number of agents with an active reverse tunnel"
    );
    describe_counter!(
        COMMAND_RPCS_TOTAL,
        Unit::Count,
        "Service-to-agent command RPCs. Labels: method, outcome"
    );
    // HTTP request metrics: described by axum-prometheus at layer-build
    // time. Metric names are renamed at compile time via
    // `AXUM_HTTP_*` env vars in `.cargo/config.toml`.
}

// ---------------------------------------------------------------------------
// Recorder install + upkeep.
// ---------------------------------------------------------------------------

/// Histogram buckets for every `*_seconds` metric: ~1 ms to 10 s.
const SECONDS_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Install the Prometheus recorder as the process-global `metrics`
/// recorder. `metrics::set_global_recorder` rejects a second install;
/// hosting binaries call this once.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_seconds".to_owned()), SECONDS_BUCKETS)
        .expect("static seconds buckets are valid")
        .install_recorder()
        .expect("install Prometheus recorder")
}

/// Run `PrometheusHandle::run_upkeep` on a 1 Hz ticker until `token`
/// cancels. Required for histogram rolling-window quantiles to stay
/// accurate — without upkeep, quantiles drift as the internal window
/// grows stale.
pub fn spawn_upkeep(
    handle: PrometheusHandle,
    token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => return,
                _ = ticker.tick() => handle.run_upkeep(),
            }
        }
    })
}

/// Re-export so call sites don't need to import the exporter crate.
pub use metrics_exporter_prometheus::PrometheusHandle as Handle;

/// Test-only process-wide recorder install. `metrics::set_global_recorder`
/// rejects a second call, so every unit test in the lib's test binary must
/// share one handle. Integration-test binaries share via their own
/// `tests/common::test_prometheus_handle` helper — a separate compilation
/// unit, hence the duplicated pattern.
#[cfg(test)]
pub(crate) fn test_install() -> Handle {
    static ONCE: std::sync::OnceLock<Handle> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let h = install_recorder();
        describe_service_metrics();
        h
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single process-global install for every test in this binary.
    fn shared_install() -> Handle {
        test_install()
    }

    #[test]
    fn counter_accessor_increments_render() {
        let h = shared_install();
        ingest_batches(BatchOutcome::Ok).increment(1);
        ingest_batches(BatchOutcome::Ok).increment(2);
        let body = h.render();
        // Match the full Prometheus sample line so a different test that
        // also bumps this counter (running in the same binary) turns the
        // value into e.g. "30" and this assertion fails instead of
        // silently passing via `contains`.
        let expected = r#"meshmon_service_ingest_batches_total{outcome="ok"} 3"#;
        assert!(
            body.lines().any(|l| l == expected),
            "expected line {expected:?} not found in:\n{body}"
        );
    }

    #[test]
    fn describe_emits_help_and_type_for_every_metric() {
        let h = shared_install();
        // Touch each metric so render() includes it.
        uptime_seconds().set(1.0);
        ingest_batches(BatchOutcome::Ok).increment(0);
        ingest_samples().increment(0);
        ingest_dropped(DropSource::Metrics).increment(0);
        vm_write_duration().record(0.0);
        pg_snapshot_duration().record(0.0);
        last_seen_writes().increment(0);
        registry_agents(AgentState::Active).set(0.0);
        registry_last_refresh_age_seconds().set(0.0);
        registry_refresh_errors().increment(0);
        tunnel_agents().set(0.0);
        command_rpcs("refresh_config", CommandOutcome::Ok).increment(0);
        // BUILD_INFO is intentionally not in ALL_METRIC_NAMES (it's a
        // one-shot with non-enumerable labels). Exercise it separately so
        // this test's name ("every metric") doesn't lie.
        emit_build_info(BuildInfo::compile_time());

        let body = h.render();
        for name in ALL_METRIC_NAMES {
            assert!(
                body.contains(&format!("# HELP {name}")),
                "missing HELP for {name} in:\n{body}"
            );
            assert!(
                body.contains(&format!("# TYPE {name}")),
                "missing TYPE for {name} in:\n{body}"
            );
        }
        // BUILD_INFO: same HELP/TYPE contract, verified inline.
        assert!(
            body.contains(&format!("# HELP {BUILD_INFO}")),
            "missing HELP for {BUILD_INFO} in:\n{body}"
        );
        assert!(
            body.contains(&format!("# TYPE {BUILD_INFO}")),
            "missing TYPE for {BUILD_INFO} in:\n{body}"
        );
    }
}
