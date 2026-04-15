//! Self-metric handles for the ingestion pipeline.
//!
//! Emitted via the `metrics` facade. T10 wires the Prometheus exporter
//! that exposes them at `/metrics`. Until then they are silent no-ops in
//! production but observable via `metrics_util::debugging::DebuggingRecorder`
//! in tests if needed.
//!
//! Names mirror spec 03 §"Self-metrics" so the wire surface is stable as
//! soon as T10 lands.

use metrics::{counter, histogram, Counter, Histogram};

/// Counter: batches processed, labeled by outcome.
pub fn ingest_batch(outcome: BatchOutcome) -> Counter {
    counter!("meshmon_service_ingest_batches_total", "outcome" => outcome.as_str())
}

/// Counter: total samples handed to the VM writer (successfully sent).
pub fn ingest_samples() -> Counter {
    counter!("meshmon_service_ingest_samples_total")
}

/// Counter: items dropped before reaching a sink, labeled by origin.
pub fn ingest_dropped(source: DropSource) -> Counter {
    counter!("meshmon_service_ingest_dropped_total", "source" => source.as_str())
}

/// Histogram: end-to-end VM remote-write POST duration in seconds.
pub fn vm_write_duration() -> Histogram {
    histogram!("meshmon_service_vm_write_duration_seconds")
}

/// Histogram: route snapshot INSERT duration in seconds.
pub fn pg_snapshot_duration() -> Histogram {
    histogram!("meshmon_service_pg_snapshot_duration_seconds")
}

/// Counter: successful `agents.last_seen_at` updates (post-debounce).
pub fn last_seen_writes() -> Counter {
    counter!("meshmon_service_last_seen_writes_total")
}

/// Outcome label for the batch counter.
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

/// Source label for the dropped counter.
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
