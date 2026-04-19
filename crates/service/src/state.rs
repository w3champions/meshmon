//! Shared application state injected into axum handlers.
//!
//! Cheap to `Clone` — heavyweight fields are `Arc`-backed; `Instant` and
//! `BuildInfo` are small `Copy` types (a few words each).

use crate::catalogue::events::CatalogueBroker;
use crate::config::Config;
use crate::ingestion::IngestionPipeline;
use crate::metrics::Handle as PrometheusHandle;
use crate::registry::AgentRegistry;
use arc_swap::ArcSwap;
use meshmon_revtunnel::TunnelManager;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

/// Build info populated at compile time. Used by `/metrics` and
/// `/api/session` to expose version/commit to operators and the UI.
#[derive(Debug, Clone, Copy)]
pub struct BuildInfo {
    /// Crate version from `CARGO_PKG_VERSION`.
    pub version: &'static str,
    /// Git commit hash (or `"unknown"` if unset at compile time).
    pub commit: &'static str,
}

impl BuildInfo {
    /// Populated from `CARGO_PKG_VERSION` and the `MESHMON_GIT_COMMIT` env var
    /// (set by `build.rs` in a later task — for now defaults to `"unknown"`).
    pub const fn compile_time() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            commit: match option_env!("MESHMON_GIT_COMMIT") {
                Some(c) => c,
                None => "unknown",
            },
        }
    }
}

/// Shared handle exposed to every axum handler.
#[derive(Clone)]
pub struct AppState {
    /// Live config pointer. Swappable via SIGHUP in Task 13.
    pub config: Arc<ArcSwap<Config>>,
    /// Notification channel for config changes. Subscribers receive the
    /// new `Arc<Config>` when a reload completes. T04 has no subscribers;
    /// T06+ will use this to re-plumb probing config to agents.
    pub config_rx: watch::Receiver<Arc<Config>>,
    /// Shared Postgres pool.
    pub pool: PgPool,
    /// `true` after startup completes (migrations ran, listeners bound).
    /// Set back to `false` when shutdown begins so `/readyz` drains.
    ready: Arc<AtomicBool>,
    /// Build-time metadata; surfaced through `/metrics` and
    /// `/api/session`.
    pub build: BuildInfo,
    /// Ingestion pipeline handle. Handlers call
    /// [`IngestionPipeline::push_metrics`] / [`IngestionPipeline::push_snapshot`]
    /// after validating agent payloads. Cheap to clone.
    pub ingestion: IngestionPipeline,
    /// Live agent registry. Keeps a periodically-refreshed snapshot of every
    /// known agent and their last-seen timestamps. Used by handler endpoints
    /// that need to validate source agents or list active targets.
    pub registry: Arc<AgentRegistry>,
    /// Moment state was constructed. Baseline for
    /// `meshmon_service_uptime_seconds`.
    pub started_at: Instant,
    /// Prometheus render handle. Cheap clone; handlers call
    /// `prom.render()` at scrape time.
    pub prom: PrometheusHandle,
    /// Reverse-tunnel registry. Per-agent `tonic::Channel` keyed by
    /// `source_id`. Populated by the `OpenTunnel` handler; consumed by
    /// `commands::spawn_config_watcher` to broadcast `RefreshConfig`.
    pub tunnel_manager: Arc<TunnelManager>,
    /// In-process catalogue event broker. Every mutating catalogue
    /// handler publishes here; the SSE handler in
    /// [`crate::catalogue::sse`] forwards events to connected clients.
    /// Capacity is fixed at [`crate::catalogue::events::DEFAULT_CAPACITY`]
    /// — overflow surfaces to the client as a `lag` frame rather than
    /// blocking the publisher.
    pub catalogue_broker: CatalogueBroker,
}

impl AppState {
    /// Construct an `AppState` in the "not yet ready" state.
    pub fn new(
        config: Arc<ArcSwap<Config>>,
        config_rx: watch::Receiver<Arc<Config>>,
        pool: PgPool,
        ingestion: IngestionPipeline,
        registry: Arc<AgentRegistry>,
        prom: PrometheusHandle,
    ) -> Self {
        Self {
            config,
            config_rx,
            pool,
            ready: Arc::new(AtomicBool::new(false)),
            build: BuildInfo::compile_time(),
            ingestion,
            registry,
            started_at: Instant::now(),
            prom,
            // Wire the manager's observer to the typed gauge accessor so the
            // meshmon_service_tunnel_agents metric stays in lockstep with the
            // registry. Keeping the literal in `crate::metrics` preserves the
            // "one place for every meshmon_* name" invariant.
            tunnel_manager: Arc::new(TunnelManager::with_observer(|len| {
                crate::metrics::tunnel_agents().set(len as f64);
            })),
            // Catalogue broker: single process-wide broadcast channel. The
            // capacity is a fixed constant rather than a caller argument
            // because tuning is driven by the paste-flow burst size, not by
            // any deployment-specific knob. See
            // [`crate::catalogue::events::DEFAULT_CAPACITY`].
            catalogue_broker: CatalogueBroker::default(),
        }
    }

    /// Mark the service ready. Called after all fallible startup steps
    /// complete.
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Mark the service not ready. Called from shutdown to drain in-flight
    /// connections while load balancers redirect traffic.
    pub fn mark_not_ready(&self) {
        self.ready.store(false, Ordering::SeqCst);
    }

    /// Readiness probe result.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Snapshot the current config. Cheap; returns an `Arc`.
    pub fn config(&self) -> Arc<Config> {
        self.config.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_toggles() {
        let cfg = Arc::new(
            Config::from_str(
                r#"
[database]
url = "postgres://a@b/c"

[probing]
udp_probe_secret = "hex:6d73686d6e2d7631"
"#,
                "test.toml",
            )
            .expect("parse"),
        );
        let (tx, rx) = watch::channel(cfg.clone());
        let _ = tx; // suppress unused

        // Pool: we don't need a live connection for state construction, but
        // `AppState::new` requires one. Defer full state tests to integration
        // tests. This unit test only exercises the ready flag on an
        // `AtomicBool` — exercise it directly.
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::SeqCst));
        flag.store(true, Ordering::SeqCst);
        assert!(flag.load(Ordering::SeqCst));
        let _ = rx;
    }

    #[test]
    fn build_info_reports_version_and_commit() {
        let b = BuildInfo::compile_time();
        // Sanity check that compile_time() wires CARGO_PKG_VERSION; guards
        // against a future refactor that hardcodes a literal.
        assert_eq!(b.version, env!("CARGO_PKG_VERSION"));
        // Either a real short sha (hex, >= 7 chars) or the literal
        // fallback `"unknown"` — build.rs never leaves the env unset.
        assert!(b.commit == "unknown" || b.commit.len() >= 7);
        assert!(
            b.commit == "unknown" || b.commit.chars().all(|c| c.is_ascii_hexdigit()),
            "commit {} is neither 'unknown' nor hex",
            b.commit
        );
    }
}
