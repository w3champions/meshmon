//! Outbound emitter: batches metrics every 60 s, pushes route snapshots
//! immediately, retries on retriable failures with jittered exponential
//! backoff, and buffers up to 65 failed RPCs in a drop-oldest ring queue.
//!
//! This module is under active construction across T16. Only the
//! supervisor → emitter message type is stable here; the runtime + retry
//! worker land in subsequent tasks.

use std::time::SystemTime;

use meshmon_protocol::Protocol;

use crate::state::ProtoHealth;
use crate::stats::Summary;

/// One per-(target, protocol) metrics record produced by the supervisor's
/// 60 s metrics tick. The emitter batches these into a `MetricsBatch`.
///
/// `health` is always `Some`-equivalent: the supervisor drops samples where
/// the last-evaluated `TargetSnapshot` health for that protocol is `None`,
/// so this struct carries a concrete `ProtoHealth`. That matches the
/// wire-protocol rule that `ProtocolHealth::Unspecified` is an illegal
/// payload (rejected by the service with `INVALID_ARGUMENT`).
#[derive(Debug, Clone)]
pub struct PathMetricsMsg {
    pub target_id: String,
    pub protocol: Protocol,
    pub window_start: SystemTime,
    pub window_end: SystemTime,
    pub stats: Summary,
    pub health: ProtoHealth,
}
