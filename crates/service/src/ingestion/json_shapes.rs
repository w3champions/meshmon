//! Serde shapes for the `route_snapshots` JSONB columns.
//!
//! These shapes are part of the public API: the frontend reads them back
//! verbatim (spec 04 §"Example `hops` JSONB"). Renames here are UI
//! breaks.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// JSON representation of a single traceroute hop as stored in
/// `route_snapshots.hops`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HopJson {
    /// 1-indexed TTL / hop position (matches the protocol contract).
    pub position: u32,
    /// IP addresses observed at this hop and their frequencies.
    pub observed_ips: Vec<HopIpJson>,
    /// Mean RTT to this hop, in microseconds.
    pub avg_rtt_micros: u32,
    /// Standard deviation of RTT to this hop, in microseconds.
    pub stddev_rtt_micros: u32,
    /// Fraction of probes with no response at this hop.
    pub loss_pct: f64,
}

/// JSON representation of an observed IP at a hop.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HopIpJson {
    /// Human-readable IP string (IPv4 or IPv6 `to_string()` form).
    pub ip: String,
    /// Fraction of probes that observed this IP at this hop.
    pub freq: f64,
    /// Reverse-DNS hostname for this IP, populated server-side at
    /// response-serialize time only; never written to the
    /// `route_snapshots.hops` / `mtr_traces.hops` JSONB (guarded by
    /// `skip_serializing_if = "Option::is_none"`). Existing stored rows
    /// deserialize with `hostname: None` without a migration.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub hostname: Option<String>,
}

/// JSON representation of the aggregated path summary stored in
/// `route_snapshots.path_summary`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PathSummaryJson {
    /// Mean RTT across all hops, in microseconds.
    pub avg_rtt_micros: u32,
    /// Overall path loss fraction.
    pub loss_pct: f64,
    /// Total number of hops in the route.
    pub hop_count: u32,
}

impl From<&crate::ingestion::validator::ValidHop> for HopJson {
    fn from(h: &crate::ingestion::validator::ValidHop) -> Self {
        HopJson {
            position: h.position,
            observed_ips: h
                .observed_ips
                .iter()
                .map(|o| HopIpJson {
                    ip: o.ip.to_string(),
                    freq: o.frequency,
                    // Populated server-side at response-serialize time only;
                    // never written to JSONB storage.
                    hostname: None,
                })
                .collect(),
            avg_rtt_micros: h.avg_rtt_micros,
            stddev_rtt_micros: h.stddev_rtt_micros,
            loss_pct: h.loss_pct,
        }
    }
}

impl From<&crate::ingestion::validator::ValidSummary> for PathSummaryJson {
    fn from(s: &crate::ingestion::validator::ValidSummary) -> Self {
        PathSummaryJson {
            avg_rtt_micros: s.avg_rtt_micros,
            loss_pct: s.loss_pct,
            hop_count: s.hop_count,
        }
    }
}
