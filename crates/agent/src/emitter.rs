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
/// `health` is a concrete `ProtoHealth` (never `Unspecified`): the
/// supervisor drops the emission when its last-evaluated
/// `TargetSnapshot` still carries `None` for that protocol, which
/// happens only before the first eval tick has fired. After the first
/// eval tick, the state machine always classifies every protocol as
/// `Healthy` or `Unhealthy`, so this struct carries a real verdict.
/// The service rejects `ProtocolHealth::Unspecified` with
/// `INVALID_ARGUMENT`, so this invariant is load-bearing for wire
/// validity.
#[derive(Debug, Clone)]
pub struct PathMetricsMsg {
    pub target_id: String,
    pub protocol: Protocol,
    pub window_start: SystemTime,
    pub window_end: SystemTime,
    pub stats: Summary,
    pub health: ProtoHealth,
}
