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
    // Alias for BC: main-era route_snapshots / mtr_traces JSONB rows use "loss_pct".
    #[serde(alias = "loss_pct")]
    pub loss_ratio: f64,
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
    // Alias for BC: main-era route_snapshots / mtr_traces JSONB rows use "loss_pct".
    #[serde(alias = "loss_pct")]
    pub loss_ratio: f64,
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
            loss_ratio: h.loss_ratio,
        }
    }
}

impl From<&crate::ingestion::validator::ValidSummary> for PathSummaryJson {
    fn from(s: &crate::ingestion::validator::ValidSummary) -> Self {
        PathSummaryJson {
            avg_rtt_micros: s.avg_rtt_micros,
            loss_ratio: s.loss_ratio,
            hop_count: s.hop_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_json_deserializes_main_era_loss_pct_alias() {
        let payload = r#"{"position":1,"observed_ips":[],"avg_rtt_micros":0,"stddev_rtt_micros":0,"loss_pct":0.25}"#;
        let hop: HopJson = serde_json::from_str(payload).unwrap();
        assert!((hop.loss_ratio - 0.25).abs() < 1e-6);
    }

    #[test]
    fn path_summary_json_deserializes_main_era_loss_pct_alias() {
        let payload = r#"{"avg_rtt_micros":0,"loss_pct":0.42,"hop_count":3}"#;
        let s: PathSummaryJson = serde_json::from_str(payload).unwrap();
        assert!((s.loss_ratio - 0.42).abs() < 1e-6);
    }
}
