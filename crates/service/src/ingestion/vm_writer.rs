//! Prometheus remote-write batcher and HTTP poster.
//!
//! Spec-driven design (specs 03 §VM writer, 04 §Remote-write):
//! - Pull samples from a [`DropOldest`] queue.
//! - Batch up to `batch_size` samples or `batch_interval` elapsed.
//! - Encode as a Prometheus `WriteRequest` proto (types from the
//!   `prometheus-reqwest-remote-write` crate), snappy-compress, POST with
//!   the canonical headers.
//! - Retry with exponential backoff (max `max_retry`).
//!
//! Per-batch allocation overhead: each sample carries its own labels
//! (no dedup of common labels across the batch). For our cardinality
//! (~40k series, ~670 samples/sec) the cost is well under network I/O.

use crate::ingestion::metrics::{
    ingest_batch, ingest_dropped, ingest_samples, vm_write_duration, BatchOutcome, DropSource,
};
use crate::ingestion::queue::DropOldest;
use prometheus_reqwest_remote_write::{
    Label, Sample, TimeSeries, WriteRequest, CONTENT_TYPE, HEADER_NAME_REMOTE_WRITE_VERSION,
    LABEL_NAME, REMOTE_WRITE_VERSION_01,
};
use reqwest::Client;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// One sample to push to VictoriaMetrics. The producer supplies valid
/// (low-cardinality) labels and a metric name.
#[derive(Debug, Clone)]
pub struct PromSample {
    /// Prometheus metric name (bound to the `__name__` label).
    pub metric: String,
    /// Additional labels. Must NOT duplicate `__name__`.
    pub labels: Vec<(String, String)>,
    /// Sample value.
    pub value: f64,
    /// Milliseconds since Unix epoch.
    pub timestamp_ms: i64,
}

/// Runtime config for the VM writer.
#[derive(Debug, Clone)]
pub struct VmWriterCfg {
    /// Full URL including `/api/v1/write` path.
    pub url: String,
    /// Maximum samples per POST batch.
    pub batch_size: usize,
    /// Maximum time to wait between flushes (when queue stays below size).
    pub batch_interval: Duration,
    /// Max total time spent retrying a single failing batch before giving
    /// up. Spec: "up to 5 min".
    pub max_retry: Duration,
}

/// Run the VM writer until `token` is cancelled. On cancel, drains the
/// remaining queue and sends a final POST if any samples are buffered.
///
/// `pg_drain_complete` is fired by `pg_writer_loop` once it has finished
/// processing every queued snapshot (and therefore every `vm_queue.push`
/// that shutdown-time snapshots can produce). vm_writer must keep draining
/// until it observes that signal; only afterwards does the idle timer
/// become authoritative.
pub async fn run(
    queue: Arc<DropOldest<PromSample>>,
    cfg: VmWriterCfg,
    token: CancellationToken,
    pg_drain_complete: CancellationToken,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");

    let mut buf: Vec<PromSample> = Vec::with_capacity(cfg.batch_size.max(1));

    // On cancellation, keep draining while pg_writer is still pushing. The
    // `pg_drain_complete` signal fires only after pg_writer's drain loop
    // finishes its final `process_snapshot`, so any `vm_queue.push` from
    // shutdown-time snapshots is guaranteed to land before we observe it.
    // The idle grace window only kicks in *after* pg signals done, to catch
    // the sub-timeslice race between a final push and our `queue.wait()`.
    //
    // Shutdown latency bound:
    //   `pg_drain_time + DRAIN_GRACE_PERIOD + in-flight_batch_send_bound`
    // where `in-flight_batch_send_bound` is capped by the cancel+2s unified
    // select in `post_batch`. With defaults (snapshot_buffer_capacity=1024,
    // ~50ms/INSERT) pg_drain_time can approach ~51s for a full queue;
    // typical steady-state (~0.04 snapshots/sec) stays <= 500ms.
    const DRAIN_GRACE_PERIOD: Duration = Duration::from_millis(500);

    loop {
        if token.is_cancelled() {
            // Drain in batch-sized chunks so we don't build one oversized
            // POST that VM will reject. `post_batch` is cancellation-aware
            // and short-circuits retries once the token is set.
            loop {
                buf.clear();
                let drained = queue.drain_into(&mut buf, cfg.batch_size);
                if drained > 0 {
                    flush(&client, &cfg, &buf, drained, &token).await;
                    continue;
                }
                // Queue empty. While pg_writer is still draining we MUST
                // keep waiting (no idle exit) — otherwise a late push from
                // pg would land on an orphaned queue. Once pg signals done
                // the sleep arm becomes active and we take the final grace
                // window to catch any sub-500ms race with `queue.wait()`.
                tokio::select! {
                    _ = queue.wait() => continue,
                    _ = pg_drain_complete.cancelled(), if !pg_drain_complete.is_cancelled() => continue,
                    _ = sleep(DRAIN_GRACE_PERIOD), if pg_drain_complete.is_cancelled() => break,
                }
            }
            return;
        }

        // When empty, block until the first item lands so we don't burn
        // CPU looping through empty batch intervals.
        if queue.is_empty() {
            tokio::select! {
                _ = token.cancelled() => continue,
                _ = queue.wait() => {}
            }
        }

        // Once non-empty: wait up to `batch_interval` so more samples can
        // accumulate before we flush. If the queue already has a full
        // batch, skip the interval and flush immediately.
        if queue.len() < cfg.batch_size {
            tokio::select! {
                _ = token.cancelled() => continue,
                _ = sleep(cfg.batch_interval) => {}
            }
        }

        buf.clear();
        let drained = queue.drain_into(&mut buf, cfg.batch_size);
        if drained == 0 {
            continue;
        }
        flush(&client, &cfg, &buf, drained, &token).await;
    }
}

async fn flush(
    client: &Client,
    cfg: &VmWriterCfg,
    buf: &[PromSample],
    drained: usize,
    token: &CancellationToken,
) {
    match post_batch(client, cfg, buf, token).await {
        Ok(()) => {
            ingest_batch(BatchOutcome::Ok).increment(1);
            ingest_samples().increment(drained as u64);
            debug!(samples = drained, "vm batch flushed");
        }
        Err(VmWriteError::Cancelled) => {
            // Clean-shutdown drop — expected behavior when the token fires
            // mid-retry. Not a data-loss alert condition; don't noise-
            // alarm operators or double-count against runtime drops.
            debug!(samples = drained, "vm batch abandoned on shutdown");
        }
        Err(e) => {
            ingest_batch(BatchOutcome::WriteError).increment(1);
            // Samples discarded after retry exhaustion are a data-loss path
            // equivalent to buffer overflow — mirror that in the counter.
            ingest_dropped(DropSource::Metrics).increment(drained as u64);
            warn!(error = %e, samples = drained, "vm batch dropped after retry exhaustion");
        }
    }
}

async fn post_batch(
    client: &Client,
    cfg: &VmWriterCfg,
    samples: &[PromSample],
    token: &CancellationToken,
) -> Result<(), VmWriteError> {
    let body = encode_batch(samples);
    let started = Instant::now();
    let mut backoff = Duration::from_millis(250);
    let deadline = Instant::now() + cfg.max_retry;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let send_fut = client
            .post(&cfg.url)
            .header("Content-Type", CONTENT_TYPE)
            .header("Content-Encoding", "snappy")
            .header(HEADER_NAME_REMOTE_WRITE_VERSION, REMOTE_WRITE_VERSION_01)
            .body(body.clone())
            .send();
        // Race the send against a post-cancel 2s budget. Pre-cancel,
        // reqwest's 15s client timeout governs normal-path latency;
        // post-cancel, this bounds *each* send (not cumulative drain time,
        // which is `N x 2s` across N queued batches in the worst case).
        let cancel_budget = async {
            token.cancelled().await;
            sleep(Duration::from_secs(2)).await;
        };
        let resp = tokio::select! {
            r = send_fut => r,
            _ = cancel_budget => {
                vm_write_duration().record(started.elapsed().as_secs_f64());
                return Err(VmWriteError::Cancelled);
            }
        };
        let elapsed = started.elapsed().as_secs_f64();
        match resp {
            Ok(r) if r.status().is_success() => {
                vm_write_duration().record(elapsed);
                return Ok(());
            }
            Ok(r) => {
                let status = r.status();
                debug!(attempt, %status, "vm write non-success");
                if Instant::now() + backoff > deadline {
                    vm_write_duration().record(elapsed);
                    return Err(VmWriteError::HttpStatus(status.as_u16()));
                }
            }
            Err(e) => {
                debug!(attempt, error = %e, "vm write transport error");
                if Instant::now() + backoff > deadline {
                    vm_write_duration().record(elapsed);
                    return Err(VmWriteError::Transport(e.to_string()));
                }
            }
        }
        // Cancellation short-circuits further retries so shutdown doesn't
        // block for the full `max_retry` window when VM is unreachable.
        // `biased;` ensures cancel deterministically wins a tied race with
        // sleep completion (otherwise tokio's random selection could leak
        // one extra retry attempt past cancellation).
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                vm_write_duration().record(started.elapsed().as_secs_f64());
                return Err(VmWriteError::Cancelled);
            }
            _ = sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn encode_batch(samples: &[PromSample]) -> Vec<u8> {
    // Group samples by label set into one TimeSeries per unique series.
    let mut groups: std::collections::HashMap<Vec<(String, String)>, Vec<Sample>> =
        std::collections::HashMap::new();
    for s in samples {
        let mut labels = Vec::with_capacity(s.labels.len() + 1);
        labels.push((LABEL_NAME.to_string(), s.metric.clone()));
        for (k, v) in &s.labels {
            if k == LABEL_NAME {
                continue;
            }
            labels.push((k.clone(), v.clone()));
        }
        // Label sort is required by the spec, but the crate's
        // encode_compressed() handles it via WriteRequest::sort(); no need
        // to sort here.
        groups.entry(labels).or_default().push(Sample {
            value: s.value,
            timestamp: s.timestamp_ms,
        });
    }

    let req = WriteRequest {
        timeseries: groups
            .into_iter()
            .map(|(labels, samples)| TimeSeries {
                labels: labels
                    .into_iter()
                    .map(|(name, value)| Label { name, value })
                    .collect(),
                samples,
            })
            .collect(),
    };
    // Sorts labels (per spec) + samples (monotonic ts), proto3-encodes,
    // snappy-compresses. Returns Vec<u8>.
    req.encode_compressed().expect("snappy compress")
}

#[derive(Debug, thiserror::Error)]
enum VmWriteError {
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("transport: {0}")]
    Transport(String),
    #[error("cancelled during retry backoff")]
    Cancelled,
}
