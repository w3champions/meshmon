// crates/agent/src/probing/mod.rs
//! Probing shared types.
//!
//! `ProbeObservation` is emitted by each prober task (T12: trippy, TCP, UDP)
//! and collected by the per-target supervisor. This module defines only the
//! types; prober implementations arrive in T12.

use std::net::IpAddr;

/// Single probe result emitted by a prober into the supervisor's mpsc channel.
#[derive(Debug, Clone)]
pub struct ProbeObservation {
    /// Which protocol produced this observation.
    pub protocol: meshmon_protocol::Protocol,
    /// Target agent ID.
    pub target_id: String,
    /// Whether the probe completed successfully.
    pub success: bool,
    /// Round-trip time in microseconds. `None` on failure (timeout/error).
    pub rtt_micros: Option<u32>,
    /// Hop-level detail from traceroute probes (trippy). `None` for TCP/UDP
    /// direct pings.
    pub hops: Option<Vec<HopObservation>>,
    /// Monotonic instant when the probe was sent. Used for window math
    /// (rolling stats, state-machine dwell timers). This is NOT wall-clock
    /// time — convert to `SystemTime::now()` at emit-tick time if a
    /// wall-clock timestamp is needed on the wire.
    pub observed_at: tokio::time::Instant,
}

/// One hop observed during a traceroute probe.
#[derive(Debug, Clone)]
pub struct HopObservation {
    /// 1-indexed hop position (TTL).
    pub position: u8,
    /// IP of the responding router. `None` if the hop timed out (star).
    pub ip: Option<IpAddr>,
    /// RTT in microseconds. `None` if the hop timed out.
    pub rtt_micros: Option<u32>,
}
