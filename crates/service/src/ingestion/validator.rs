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
