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

/// A metrics batch that has passed all shape and range checks.
#[derive(Debug, Clone)]
pub struct ValidatedMetrics {
    /// Identifier of the agent that sent the batch.
    pub source_id: String,
    /// Batch wall-clock timestamp in microseconds since the Unix epoch.
    pub batch_timestamp_micros: i64,
    /// Agent software version, or `None` if the agent did not report one.
    pub agent_version: Option<String>,
    /// Validated per-path measurements included in this batch.
    pub paths: Vec<ValidPath>,
}

/// A single per-(target, protocol) measurement that has passed validation.
#[derive(Debug, Clone)]
pub struct ValidPath {
    /// Identifier of the remote agent being measured.
    pub target_id: String,
    /// Network protocol used for probing.
    pub protocol: Protocol,
    /// Measurement window start, microseconds since the Unix epoch.
    pub window_start_micros: i64,
    /// Measurement window end, microseconds since the Unix epoch.
    pub window_end_micros: i64,
    /// Total number of probes sent during the window.
    pub probes_sent: u64,
    /// Number of probes that received a successful response.
    pub probes_successful: u64,
    /// Fraction of probes that failed; always in `[0.0, 1.0]`.
    pub failure_rate: f64,
    /// Mean RTT across successful probes, in microseconds.
    pub rtt_avg_micros: u32,
    /// Minimum RTT observed, in microseconds.
    pub rtt_min_micros: u32,
    /// Maximum RTT observed, in microseconds.
    pub rtt_max_micros: u32,
    /// Standard deviation of RTT, in microseconds.
    pub rtt_stddev_micros: u32,
    /// 50th-percentile RTT, in microseconds.
    pub rtt_p50_micros: u32,
    /// 95th-percentile RTT, in microseconds.
    pub rtt_p95_micros: u32,
    /// 99th-percentile RTT, in microseconds.
    pub rtt_p99_micros: u32,
    /// Derived health classification for this path.
    pub health: ProtocolHealth,
}

/// A route snapshot that has passed all shape and range checks.
#[derive(Debug, Clone)]
pub struct ValidatedSnapshot {
    /// Identifier of the agent that captured the snapshot.
    pub source_id: String,
    /// Identifier of the destination agent.
    pub target_id: String,
    /// Network protocol used for the traceroute.
    pub protocol: Protocol,
    /// Capture timestamp, microseconds since the Unix epoch.
    pub observed_at_micros: i64,
    /// Ordered sequence of validated hops.
    pub hops: Vec<ValidHop>,
    /// Aggregate summary computed from the hop sequence.
    pub path_summary: ValidSummary,
}

/// A single validated traceroute hop.
#[derive(Debug, Clone)]
pub struct ValidHop {
    /// 1-indexed position of this hop in the route (matches the protocol contract).
    pub position: u32,
    /// IP addresses observed at this hop position and their frequencies.
    pub observed_ips: Vec<ValidObservedIp>,
    /// Mean RTT to this hop, in microseconds.
    pub avg_rtt_micros: u32,
    /// Standard deviation of RTT to this hop, in microseconds.
    pub stddev_rtt_micros: u32,
    /// Fraction of probes that received no response at this hop; in `[0.0, 1.0]`.
    pub loss_pct: f64,
}

/// An IP address observed at a particular traceroute hop.
#[derive(Debug, Clone)]
pub struct ValidObservedIp {
    /// The IP address.
    pub ip: std::net::IpAddr,
    /// Fraction of probes that reached this IP at this hop; in `[0.0, 1.0]`.
    pub frequency: f64,
}

/// Aggregate path summary derived from a validated route snapshot.
#[derive(Debug, Clone)]
pub struct ValidSummary {
    /// Mean RTT across all hops, in microseconds.
    pub avg_rtt_micros: u32,
    /// Overall path loss fraction; in `[0.0, 1.0]`.
    pub loss_pct: f64,
    /// Total number of hops in the route.
    pub hop_count: u32,
}

/// Validation errors. Handlers map these to HTTP statuses; ingestion never
/// sees them (only validated payloads make it through).
#[derive(Debug, Error)]
pub enum ValidationError {
    /// The batch `source_id` field is empty.
    #[error("source_id is empty")]
    EmptySourceId,
    /// A path entry has an empty `target_id` field.
    #[error("target_id is empty")]
    EmptyTargetId,
    /// A path entry carries `Protocol::Unspecified`.
    #[error("protocol is unspecified")]
    UnspecifiedProtocol,
    /// The batch contains more paths than `MAX_PATHS_PER_BATCH`.
    #[error("batch contains {count} paths; cap is {MAX_PATHS_PER_BATCH}")]
    TooManyPaths {
        /// Number of paths present in the batch.
        count: usize,
    },
    /// A snapshot contains more hops than `MAX_HOPS_PER_SNAPSHOT`.
    #[error("snapshot contains {count} hops; cap is {MAX_HOPS_PER_SNAPSHOT}")]
    TooManyHops {
        /// Number of hops present in the snapshot.
        count: usize,
    },
    /// A path's `failure_rate` is outside `[0.0, 1.0]` or is NaN.
    #[error("failure_rate {value} outside [0.0, 1.0] for target={target}")]
    FailureRateOutOfRange {
        /// Target agent identifier.
        target: String,
        /// The invalid failure_rate value.
        value: f64,
    },
    /// One of the RTT fields exceeds `MAX_RTT_MICROS`.
    #[error("rtt_*_micros {value} > {MAX_RTT_MICROS} for target={target}")]
    RttOutOfRange {
        /// Target agent identifier.
        target: String,
        /// The invalid RTT value in microseconds.
        value: u32,
    },
    /// `probes_sent` exceeds `MAX_PROBES_PER_WINDOW`.
    #[error("probes_sent {value} > {MAX_PROBES_PER_WINDOW} for target={target}")]
    ProbeCountOutOfRange {
        /// Target agent identifier.
        target: String,
        /// The invalid probe count.
        value: u64,
    },
    /// `probes_successful` is greater than `probes_sent`.
    #[error("probes_successful {ok} > probes_sent {sent} for target={target}")]
    ProbesSuccessfulExceedsSent {
        /// Target agent identifier.
        target: String,
        /// Number of successful probes reported.
        ok: u64,
        /// Number of probes sent.
        sent: u64,
    },
    /// The measurement window end is before its start.
    #[error("invalid window: end {end} < start {start} for target={target}")]
    InvalidWindow {
        /// Target agent identifier.
        target: String,
        /// Window start timestamp in microseconds.
        start: i64,
        /// Window end timestamp in microseconds.
        end: i64,
    },
    /// A hop IP frequency is outside `[0.0, 1.0]`.
    #[error("hop frequency {value} outside [0.0, 1.0] at position {position}")]
    HopFrequencyOutOfRange {
        /// Hop position index.
        position: u32,
        /// The invalid frequency value.
        value: f64,
    },
    /// A hop loss percentage is outside `[0.0, 1.0]`.
    #[error("hop loss_pct {value} outside [0.0, 1.0] at position {position}")]
    HopLossOutOfRange {
        /// Hop position index.
        position: u32,
        /// The invalid loss fraction.
        value: f64,
    },
    /// A hop carries `position == 0`; `HopSummary.position` is 1-indexed
    /// per `meshmon.proto`.
    #[error("hop position 0 is invalid (positions are 1-indexed)")]
    InvalidHopPosition,
    /// A hop IP address byte slice has an unexpected length (must be 4 or 16).
    #[error("hop ip bytes len {len} (must be 4 or 16) at position {position}")]
    InvalidHopIp {
        /// Hop position index.
        position: u32,
        /// Actual byte length received.
        len: usize,
    },
    /// The snapshot's `hop_count` summary field disagrees with `hops.len()`.
    #[error("hop_count {summary} disagrees with hops.len() {actual}")]
    HopCountMismatch {
        /// The hop count reported in the summary.
        summary: u32,
        /// The actual number of hops in the list.
        actual: usize,
    },
    /// The route snapshot is missing its `path_summary` field.
    #[error("missing path_summary in route snapshot")]
    MissingPathSummary,
    /// The metrics batch is missing its `agent_metadata` field.
    #[error("missing agent_metadata in metrics batch")]
    MissingAgentMetadata,
}

// Re-exports from the protocol crate so callers don't double-import.
pub use meshmon_protocol::{
    HopIp, MetricsBatch as RawMetricsBatch, RouteSnapshotRequest as RawRouteSnapshotRequest,
};

/// Validate a `MetricsBatch` decoded from the wire. Returns owned data so
/// the original Protobuf can be dropped.
pub fn validate_metrics(batch: MetricsBatch) -> Result<ValidatedMetrics, ValidationError> {
    if batch.source_id.is_empty() {
        return Err(ValidationError::EmptySourceId);
    }
    if batch.paths.len() > MAX_PATHS_PER_BATCH {
        return Err(ValidationError::TooManyPaths {
            count: batch.paths.len(),
        });
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

/// Validate a `RouteSnapshotRequest` decoded from the wire. Returns owned
/// data so the original Protobuf can be dropped.
pub fn validate_snapshot(req: RouteSnapshotRequest) -> Result<ValidatedSnapshot, ValidationError> {
    if req.source_id.is_empty() {
        return Err(ValidationError::EmptySourceId);
    }
    if req.target_id.is_empty() {
        return Err(ValidationError::EmptyTargetId);
    }
    let protocol = Protocol::try_from(req.protocol).unwrap_or(Protocol::Unspecified);
    if matches!(protocol, Protocol::Unspecified) {
        return Err(ValidationError::UnspecifiedProtocol);
    }
    if req.hops.len() > MAX_HOPS_PER_SNAPSHOT {
        return Err(ValidationError::TooManyHops {
            count: req.hops.len(),
        });
    }
    let path_summary = req
        .path_summary
        .ok_or(ValidationError::MissingPathSummary)?;
    if path_summary.hop_count as usize != req.hops.len() {
        return Err(ValidationError::HopCountMismatch {
            summary: path_summary.hop_count,
            actual: req.hops.len(),
        });
    }
    if !(0.0..=1.0).contains(&path_summary.loss_pct) || path_summary.loss_pct.is_nan() {
        return Err(ValidationError::HopLossOutOfRange {
            position: 0,
            value: path_summary.loss_pct,
        });
    }

    let mut hops = Vec::with_capacity(req.hops.len());
    for h in req.hops {
        hops.push(validate_hop(h)?);
    }

    Ok(ValidatedSnapshot {
        source_id: req.source_id,
        target_id: req.target_id,
        protocol,
        observed_at_micros: req.observed_at_micros,
        hops,
        path_summary: ValidSummary {
            avg_rtt_micros: path_summary.avg_rtt_micros,
            loss_pct: path_summary.loss_pct,
            hop_count: path_summary.hop_count,
        },
    })
}

fn validate_hop(h: HopSummary) -> Result<ValidHop, ValidationError> {
    if h.position == 0 {
        return Err(ValidationError::InvalidHopPosition);
    }
    if !(0.0..=1.0).contains(&h.loss_pct) || h.loss_pct.is_nan() {
        return Err(ValidationError::HopLossOutOfRange {
            position: h.position,
            value: h.loss_pct,
        });
    }
    let mut ips = Vec::with_capacity(h.observed_ips.len());
    for o in h.observed_ips {
        if !(0.0..=1.0).contains(&o.frequency) || o.frequency.is_nan() {
            return Err(ValidationError::HopFrequencyOutOfRange {
                position: h.position,
                value: o.frequency,
            });
        }
        let ip =
            meshmon_protocol::ip::to_ipaddr(&o.ip).map_err(|_| ValidationError::InvalidHopIp {
                position: h.position,
                len: o.ip.len(),
            })?;
        ips.push(ValidObservedIp {
            ip,
            frequency: o.frequency,
        });
    }
    Ok(ValidHop {
        position: h.position,
        observed_ips: ips,
        avg_rtt_micros: h.avg_rtt_micros,
        stddev_rtt_micros: h.stddev_rtt_micros,
        loss_pct: h.loss_pct,
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
        p.rtt_avg_micros,
        p.rtt_min_micros,
        p.rtt_max_micros,
        p.rtt_stddev_micros,
        p.rtt_p50_micros,
        p.rtt_p95_micros,
        p.rtt_p99_micros,
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
