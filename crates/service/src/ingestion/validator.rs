//! Pure-function validation for incoming agent payloads.
//!
//! Validation never touches I/O — handlers (T06) do auth + Protobuf decode,
//! then hand the message here for shape/range checks. Validated payloads
//! become the input type for the ingestion workers.
//!
//! Source-agent existence is checked at the registry layer (T08 / handler
//! code). This module owns only what can be derived from the payload
//! itself.

use meshmon_protocol::{
    HopSummary, MetricsBatch, PathMetrics, Protocol, ProtocolHealth, RouteSnapshotRequest,
};
use thiserror::Error;

/// Hard cap on per-batch path entries. Defends against pathological agents.
/// Spec 04 cardinality estimate: ~36 paths × 3 protocols = 108 entries
/// per batch; 1024 leaves plenty of headroom.
pub const MAX_PATHS_PER_BATCH: usize = 1024;

/// Hard cap on per-batch probe count for a single (target, protocol).
/// Spec 03 validator: `probe_count ≤ 10_000`.
pub const MAX_PROBES_PER_WINDOW: u64 = 10_000;

/// Upper bound on RTT in microseconds. Spec 03: ≤ 60_000_000 (60s).
pub const MAX_RTT_MICROS: u32 = 60_000_000;

/// Hard cap on hops per route snapshot. Realistic traceroutes top out
/// around 30; 128 is generous.
pub const MAX_HOPS_PER_SNAPSHOT: usize = 128;

#[derive(Debug, Clone)]
pub struct ValidatedMetrics {
    pub source_id: String,
    pub batch_timestamp_micros: i64,
    pub agent_version: Option<String>,
    pub paths: Vec<ValidPath>,
}

#[derive(Debug, Clone)]
pub struct ValidPath {
    pub target_id: String,
    pub protocol: Protocol,
    pub window_start_micros: i64,
    pub window_end_micros: i64,
    pub probes_sent: u64,
    pub probes_successful: u64,
    pub failure_rate: f64,
    pub rtt_avg_micros: u32,
    pub rtt_min_micros: u32,
    pub rtt_max_micros: u32,
    pub rtt_stddev_micros: u32,
    pub rtt_p50_micros: u32,
    pub rtt_p95_micros: u32,
    pub rtt_p99_micros: u32,
    pub health: ProtocolHealth,
}

#[derive(Debug, Clone)]
pub struct ValidatedSnapshot {
    pub source_id: String,
    pub target_id: String,
    pub protocol: Protocol,
    pub observed_at_micros: i64,
    pub hops: Vec<ValidHop>,
    pub path_summary: ValidSummary,
}

#[derive(Debug, Clone)]
pub struct ValidHop {
    pub position: u32,
    pub observed_ips: Vec<ValidObservedIp>,
    pub avg_rtt_micros: u32,
    pub stddev_rtt_micros: u32,
    pub loss_pct: f64,
}

#[derive(Debug, Clone)]
pub struct ValidObservedIp {
    pub ip: std::net::IpAddr,
    pub frequency: f64,
}

#[derive(Debug, Clone)]
pub struct ValidSummary {
    pub avg_rtt_micros: u32,
    pub loss_pct: f64,
    pub hop_count: u32,
}

/// Validation errors. Handlers map these to HTTP statuses; ingestion never
/// sees them (only validated payloads make it through).
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("source_id is empty")]
    EmptySourceId,
    #[error("target_id is empty")]
    EmptyTargetId,
    #[error("protocol is unspecified")]
    UnspecifiedProtocol,
    #[error("batch contains {count} paths; cap is {MAX_PATHS_PER_BATCH}")]
    TooManyPaths { count: usize },
    #[error("snapshot contains {count} hops; cap is {MAX_HOPS_PER_SNAPSHOT}")]
    TooManyHops { count: usize },
    #[error("failure_rate {value} outside [0.0, 1.0] for target={target}")]
    FailureRateOutOfRange { target: String, value: f64 },
    #[error("rtt_*_micros {value} > {MAX_RTT_MICROS} for target={target}")]
    RttOutOfRange { target: String, value: u32 },
    #[error("probes_sent {value} > {MAX_PROBES_PER_WINDOW} for target={target}")]
    ProbeCountOutOfRange { target: String, value: u64 },
    #[error("probes_successful {ok} > probes_sent {sent} for target={target}")]
    ProbesSuccessfulExceedsSent { target: String, ok: u64, sent: u64 },
    #[error("invalid window: end {end} < start {start} for target={target}")]
    InvalidWindow { target: String, start: i64, end: i64 },
    #[error("hop frequency {value} outside [0.0, 1.0] at position {position}")]
    HopFrequencyOutOfRange { position: u32, value: f64 },
    #[error("hop loss_pct {value} outside [0.0, 1.0] at position {position}")]
    HopLossOutOfRange { position: u32, value: f64 },
    #[error("hop ip bytes len {len} (must be 4 or 16) at position {position}")]
    InvalidHopIp { position: u32, len: usize },
    #[error("hop_count {summary} disagrees with hops.len() {actual}")]
    HopCountMismatch { summary: u32, actual: usize },
    #[error("missing path_summary in route snapshot")]
    MissingPathSummary,
    #[error("missing agent_metadata in metrics batch")]
    MissingAgentMetadata,
}

// Re-exports from the protocol crate so callers don't double-import.
pub use meshmon_protocol::{HopIp, MetricsBatch as RawMetricsBatch,
    RouteSnapshotRequest as RawRouteSnapshotRequest};

/// Validate a `MetricsBatch` decoded from the wire. Returns owned data so
/// the original Protobuf can be dropped.
pub fn validate_metrics(batch: MetricsBatch) -> Result<ValidatedMetrics, ValidationError> {
    if batch.source_id.is_empty() {
        return Err(ValidationError::EmptySourceId);
    }
    if batch.paths.len() > MAX_PATHS_PER_BATCH {
        return Err(ValidationError::TooManyPaths { count: batch.paths.len() });
    }
    let agent_metadata = batch
        .agent_metadata
        .ok_or(ValidationError::MissingAgentMetadata)?;
    let agent_version = if agent_metadata.version.is_empty() {
        None
    } else {
        Some(agent_metadata.version)
    };

    let mut paths = Vec::with_capacity(batch.paths.len());
    for p in batch.paths {
        paths.push(validate_path(p)?);
    }

    Ok(ValidatedMetrics {
        source_id: batch.source_id,
        batch_timestamp_micros: batch.batch_timestamp_micros,
        agent_version,
        paths,
    })
}

fn validate_path(p: PathMetrics) -> Result<ValidPath, ValidationError> {
    if p.target_id.is_empty() {
        return Err(ValidationError::EmptyTargetId);
    }
    let protocol = Protocol::try_from(p.protocol).unwrap_or(Protocol::Unspecified);
    if matches!(protocol, Protocol::Unspecified) {
        return Err(ValidationError::UnspecifiedProtocol);
    }
    if !(0.0..=1.0).contains(&p.failure_rate) || p.failure_rate.is_nan() {
        return Err(ValidationError::FailureRateOutOfRange {
            target: p.target_id,
            value: p.failure_rate,
        });
    }
    for &rtt in &[
        p.rtt_avg_micros, p.rtt_min_micros, p.rtt_max_micros,
        p.rtt_stddev_micros, p.rtt_p50_micros, p.rtt_p95_micros, p.rtt_p99_micros,
    ] {
        if rtt > MAX_RTT_MICROS {
            return Err(ValidationError::RttOutOfRange {
                target: p.target_id,
                value: rtt,
            });
        }
    }
    if p.probes_sent > MAX_PROBES_PER_WINDOW {
        return Err(ValidationError::ProbeCountOutOfRange {
            target: p.target_id,
            value: p.probes_sent,
        });
    }
    if p.probes_successful > p.probes_sent {
        return Err(ValidationError::ProbesSuccessfulExceedsSent {
            target: p.target_id,
            ok: p.probes_successful,
            sent: p.probes_sent,
        });
    }
    if p.window_end_micros < p.window_start_micros {
        return Err(ValidationError::InvalidWindow {
            target: p.target_id,
            start: p.window_start_micros,
            end: p.window_end_micros,
        });
    }
    let health = ProtocolHealth::try_from(p.health).unwrap_or(ProtocolHealth::Unspecified);

    Ok(ValidPath {
        target_id: p.target_id,
        protocol,
        window_start_micros: p.window_start_micros,
        window_end_micros: p.window_end_micros,
        probes_sent: p.probes_sent,
        probes_successful: p.probes_successful,
        failure_rate: p.failure_rate,
        rtt_avg_micros: p.rtt_avg_micros,
        rtt_min_micros: p.rtt_min_micros,
        rtt_max_micros: p.rtt_max_micros,
        rtt_stddev_micros: p.rtt_stddev_micros,
        rtt_p50_micros: p.rtt_p50_micros,
        rtt_p95_micros: p.rtt_p95_micros,
        rtt_p99_micros: p.rtt_p99_micros,
        health,
    })
}
