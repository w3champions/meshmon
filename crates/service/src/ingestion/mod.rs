//! Service ingestion pipeline: agent payloads → VM/Postgres.
//!
//! Public surface:
//! - [`IngestionPipeline`] — runtime handle constructed at startup,
//!   exposing non-blocking `push_metrics` / `push_snapshot` entry points
//!   that handlers (T06) call once they've validated the wire payload.
//! - [`IngestionConfig`] — tunables.
//!
//! Submodules:
//! - [`validator`] — pure shape/range checks producing the `Validated*` structs.
//! - [`vm_writer`] — Prometheus remote-write batcher.
//! - [`pg_writer`] — single-row `route_snapshots` inserter.
//! - [`last_seen`] — debounced `agents.last_seen_at` updater.
//! - [`queue`] — drop-oldest bounded queue primitive.
//! - [`metrics`] — `metrics` facade handles for self-observability.
//! - [`json_shapes`] — serde shapes for the JSONB columns.

pub mod json_shapes;
pub mod last_seen;
pub mod metrics;
pub mod pg_writer;
pub mod queue;
pub mod validator;
pub mod vm_writer;

use crate::ingestion::last_seen::LastSeenUpdater;
use crate::ingestion::metrics::{ingest_dropped, DropSource};
use crate::ingestion::queue::DropOldest;
use crate::ingestion::validator::{ValidatedMetrics, ValidatedSnapshot};
use crate::ingestion::vm_writer::{PromSample, VmWriterCfg};
use meshmon_protocol::Protocol;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Runtime tunables for the ingestion pipeline.
#[derive(Debug, Clone)]
pub struct IngestionConfig {
    /// Full URL of the VictoriaMetrics remote-write endpoint
    /// (e.g. `http://.../api/v1/write`).
    pub vm_url: String,
    /// Maximum samples per VM batch.
    pub vm_batch_size: usize,
    /// Max wait between flushes when the queue is below batch size.
    pub vm_batch_interval: Duration,
    /// Capacity of the VM sample queue (drop-oldest on overflow).
    pub vm_buffer_capacity: usize,
    /// Capacity of the snapshot queue (drop-oldest on overflow).
    pub snapshot_buffer_capacity: usize,
    /// Debounce window for per-agent `last_seen_at` updates.
    pub last_seen_debounce: Duration,
    /// Maximum total retry window for a single failing VM batch.
    pub vm_max_retry: Duration,
}

impl IngestionConfig {
    /// Production defaults tuned for the spec's ~600 samples/sec budget.
    pub fn default_with_url(vm_url: String) -> Self {
        Self {
            vm_url,
            vm_batch_size: 500,
            vm_batch_interval: Duration::from_secs(5),
            // ~28 min of buffer at steady-state (~600 samples/sec across the
            // mesh). Exceeds the spec's literal "5 min in-RAM" because we'd
            // rather burn ~250 MB transient RAM during a long outage than
            // drop measurements. Steady-state RAM is unchanged: the buffer
            // is empty when VM is reachable.
            vm_buffer_capacity: 1_000_000,
            snapshot_buffer_capacity: 1024,
            last_seen_debounce: Duration::from_secs(30),
            vm_max_retry: Duration::from_secs(300),
        }
    }
}

/// Runtime handle to the ingestion pipeline. Cheap to clone (channels/Arc).
#[derive(Clone)]
pub struct IngestionPipeline {
    vm_queue: Arc<DropOldest<PromSample>>,
    snapshot_queue: Arc<DropOldest<ValidatedSnapshot>>,
    last_seen: LastSeenUpdater,
    workers: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
}

impl IngestionPipeline {
    /// Spawn the workers and return the handle. Workers run until `token`
    /// is cancelled.
    pub fn spawn(cfg: IngestionConfig, pool: PgPool, token: CancellationToken) -> Self {
        let vm_queue: Arc<DropOldest<PromSample>> =
            Arc::new(DropOldest::new(cfg.vm_buffer_capacity));
        let snapshot_queue: Arc<DropOldest<ValidatedSnapshot>> =
            Arc::new(DropOldest::new(cfg.snapshot_buffer_capacity));

        let last_seen = LastSeenUpdater::spawn(pool.clone(), cfg.last_seen_debounce, token.clone());

        let vm_handle = tokio::spawn(vm_writer::run(
            vm_queue.clone(),
            VmWriterCfg {
                url: cfg.vm_url.clone(),
                batch_size: cfg.vm_batch_size,
                batch_interval: cfg.vm_batch_interval,
                max_retry: cfg.vm_max_retry,
            },
            token.clone(),
        ));

        let pg_handle = tokio::spawn(pg_writer_loop(
            snapshot_queue.clone(),
            vm_queue.clone(),
            pool.clone(),
            last_seen.clone(),
            token.clone(),
        ));

        Self {
            vm_queue,
            snapshot_queue,
            last_seen,
            workers: Arc::new(tokio::sync::Mutex::new(vec![vm_handle, pg_handle])),
        }
    }

    /// Non-blocking: enqueue samples derived from a validated metrics batch
    /// and fire-and-forget a last-seen touch.
    pub fn push_metrics(&self, batch: ValidatedMetrics) {
        self.last_seen
            .touch(&batch.source_id, batch.agent_version.clone());

        for sample in samples_from_metrics(&batch) {
            if self.vm_queue.push(sample) {
                ingest_dropped(DropSource::Metrics).increment(1);
            }
        }
    }

    /// Non-blocking: enqueue a snapshot for `route_snapshots` insert and
    /// fire-and-forget a last-seen touch.
    pub fn push_snapshot(&self, snap: ValidatedSnapshot) {
        self.last_seen.touch(&snap.source_id, None);
        if self.snapshot_queue.push(snap) {
            ingest_dropped(DropSource::Snapshot).increment(1);
        }
    }

    /// Wait for all workers (and the last-seen updater) to exit. Safe to
    /// call multiple times.
    pub async fn join(&self) {
        let mut g = self.workers.lock().await;
        for h in g.drain(..) {
            let _ = h.await;
        }
        self.last_seen.join().await;
    }
}

async fn pg_writer_loop(
    queue: Arc<DropOldest<ValidatedSnapshot>>,
    vm_queue: Arc<DropOldest<PromSample>>,
    pool: PgPool,
    last_seen: LastSeenUpdater,
    token: CancellationToken,
) {
    // Cumulative counter state for `meshmon_route_changes_total`. Resets to
    // 0 on service restart — Prometheus' counter-reset detection makes
    // `rate()` handle that correctly. Emitting the cumulative value (not a
    // flat 1.0) is what makes `rate()` actually report changes/sec.
    //
    // Memory footprint is bounded by the agent registry (source × target ×
    // protocol). For the ~36-agent mesh the spec targets, that's ~3,900
    // entries. The map grows if agents are re-registered under new IDs; a
    // future task can prune against the live registry if churn becomes a
    // concern.
    let mut route_change_counts: std::collections::HashMap<(String, String, Protocol), u64> =
        std::collections::HashMap::new();

    loop {
        if token.is_cancelled() {
            let mut buf = Vec::new();
            queue.drain_into(&mut buf, usize::MAX);
            for snap in buf {
                let _ = pg_writer::insert_snapshot(&pool, &snap).await;
            }
            return;
        }

        tokio::select! {
            _ = token.cancelled() => continue,
            _ = queue.wait() => {}
        }

        let mut buf = Vec::new();
        queue.drain_into(&mut buf, 32);
        for snap in buf {
            match pg_writer::insert_snapshot(&pool, &snap).await {
                Ok(_id) => {
                    let key = (
                        snap.source_id.clone(),
                        snap.target_id.clone(),
                        snap.protocol,
                    );
                    let count = route_change_counts.entry(key).or_insert(0);
                    *count += 1;
                    let cumulative = *count;

                    let labels = vec![
                        ("source".to_string(), snap.source_id.clone()),
                        ("target".to_string(), snap.target_id.clone()),
                        (
                            "protocol".to_string(),
                            protocol_label(snap.protocol).to_string(),
                        ),
                    ];
                    if vm_queue.push(PromSample {
                        metric: "meshmon_route_changes_total".to_string(),
                        labels,
                        value: cumulative as f64,
                        timestamp_ms: snap.observed_at_micros / 1000,
                    }) {
                        ingest_dropped(DropSource::Metrics).increment(1);
                    }
                    last_seen.touch(&snap.source_id, None);
                }
                Err(e) => warn!(error = %e, "route snapshot insert failed"),
            }
        }
    }
}

fn samples_from_metrics(batch: &ValidatedMetrics) -> Vec<PromSample> {
    let ts_ms = batch.batch_timestamp_micros / 1000;
    let mut out = Vec::with_capacity(batch.paths.len() * 10);
    for p in &batch.paths {
        let labels = vec![
            ("source".to_string(), batch.source_id.clone()),
            ("target".to_string(), p.target_id.clone()),
            (
                "protocol".to_string(),
                protocol_label(p.protocol).to_string(),
            ),
        ];
        let mut push_gauge = |name: &str, value: f64| {
            out.push(PromSample {
                metric: name.to_string(),
                labels: labels.clone(),
                value,
                timestamp_ms: ts_ms,
            });
        };
        push_gauge("meshmon_path_failure_rate", p.failure_rate);
        push_gauge("meshmon_path_rtt_avg_micros", p.rtt_avg_micros as f64);
        push_gauge("meshmon_path_rtt_min_micros", p.rtt_min_micros as f64);
        push_gauge("meshmon_path_rtt_max_micros", p.rtt_max_micros as f64);
        push_gauge("meshmon_path_rtt_stddev_micros", p.rtt_stddev_micros as f64);
        push_gauge("meshmon_path_probe_count", p.probes_sent as f64);

        for (q, val) in [
            ("0.50", p.rtt_p50_micros as f64),
            ("0.95", p.rtt_p95_micros as f64),
            ("0.99", p.rtt_p99_micros as f64),
        ] {
            let mut q_labels = labels.clone();
            q_labels.push(("quantile".to_string(), q.to_string()));
            out.push(PromSample {
                metric: "meshmon_path_rtt_quantile_micros".to_string(),
                labels: q_labels,
                value: val,
                timestamp_ms: ts_ms,
            });
        }
    }
    out
}

fn protocol_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Unspecified => "icmp",
        Protocol::Icmp => "icmp",
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
    }
}
