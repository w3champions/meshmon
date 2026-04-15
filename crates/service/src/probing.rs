//! Probing configuration broadcast to agents via the `GetConfig` RPC.
//!
//! Defaults match spec 02 ("Rates per mode", state-machine thresholds,
//! window sizes, diff-detection thresholds, path-health thresholds).
//! Operators override any knob via `[probing]` in `meshmon.toml`; SIGHUP
//! reloads them through the existing `ArcSwap<Config>`.

use meshmon_protocol::{PathHealth, Protocol};

/// Full probing configuration, broadcast to every connected agent via
/// `GetConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbingSection {
    /// Protocols the agent should probe.
    pub enabled_protocols: Vec<Protocol>,
    /// Preference order for selecting the primary protocol.
    pub priority: Vec<Protocol>,
    /// Per-(primary, path-health) probe rate table.
    pub rates: Vec<ProbingRate>,
    /// State-machine thresholds for ICMP.
    pub icmp_thresholds: ProtocolThresholds,
    /// State-machine thresholds for TCP.
    pub tcp_thresholds: ProtocolThresholds,
    /// State-machine thresholds for UDP.
    pub udp_thresholds: ProtocolThresholds,
    /// Rolling-window sizes for primary and diversity probing.
    pub windows: ProbingWindows,
    /// Diff-detection thresholds for route-change events.
    pub diff_detection: ProbingDiffDetection,
    /// Path-level health state-machine thresholds.
    pub path_health_thresholds: PathHealthThresholds,
}

/// One row of the probe-rate table: for a given primary protocol and path
/// health state, how many probes per second to send for each protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbingRate {
    /// Primary protocol for this row.
    pub primary: Protocol,
    /// Path-health state for this row.
    pub health: PathHealth,
    /// ICMP probes per second.
    pub icmp_pps: f64,
    /// TCP probes per second.
    pub tcp_pps: f64,
    /// UDP probes per second.
    pub udp_pps: f64,
}

/// Per-protocol state-machine thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProtocolThresholds {
    /// Loss fraction that triggers the unhealthy state.
    pub unhealthy_trigger_pct: f64,
    /// Loss fraction below which healthy recovery is allowed.
    pub healthy_recovery_pct: f64,
    /// Seconds the unhealthy condition must persist before transitioning.
    pub unhealthy_hysteresis_sec: u32,
    /// Seconds the healthy condition must persist before recovering.
    pub healthy_hysteresis_sec: u32,
}

/// Rolling-window sizes for probing evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbingWindows {
    /// Primary-protocol evaluation window in seconds.
    pub primary_sec: u32,
    /// Diversity-protocol evaluation window in seconds.
    pub diversity_sec: u32,
}

/// Thresholds for detecting route-change events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbingDiffDetection {
    /// Minimum observed frequency for a new IP to be considered significant.
    pub new_ip_min_freq: f64,
    /// Maximum observed frequency for a missing IP to be considered significant.
    pub missing_ip_max_freq: f64,
    /// Hop-count change that triggers a diff event.
    pub hop_count_change: u32,
    /// RTT fractional shift that triggers a diff event.
    pub rtt_shift_frac: f64,
}

/// Path-level health state-machine thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathHealthThresholds {
    /// Loss fraction that triggers the degraded state.
    pub degraded_trigger_pct: f64,
    /// Seconds the degraded condition must persist before transitioning.
    pub degraded_trigger_sec: u32,
    /// Minimum number of samples required to evaluate degraded state.
    pub degraded_min_samples: u32,
    /// Loss fraction below which normal recovery is allowed.
    pub normal_recovery_pct: f64,
    /// Seconds the normal condition must persist before recovering.
    pub normal_recovery_sec: u32,
}

impl Default for ProbingSection {
    fn default() -> Self {
        use PathHealth::*;
        use Protocol::*;
        let rates = vec![
            ProbingRate {
                primary: Icmp,
                health: Normal,
                icmp_pps: 0.20,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Icmp,
                health: Degraded,
                icmp_pps: 1.00,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Icmp,
                health: Unreachable,
                icmp_pps: 1.00,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Normal,
                icmp_pps: 0.05,
                tcp_pps: 0.20,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Degraded,
                icmp_pps: 0.05,
                tcp_pps: 1.00,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Unreachable,
                icmp_pps: 0.05,
                tcp_pps: 1.00,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Udp,
                health: Normal,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 0.20,
            },
            ProbingRate {
                primary: Udp,
                health: Degraded,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.00,
            },
            ProbingRate {
                primary: Udp,
                health: Unreachable,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.00,
            },
        ];
        Self {
            enabled_protocols: vec![Icmp, Tcp, Udp],
            priority: vec![Icmp, Tcp, Udp],
            rates,
            icmp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.90,
                healthy_recovery_pct: 0.10,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            tcp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.50,
                healthy_recovery_pct: 0.05,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            udp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.90,
                healthy_recovery_pct: 0.10,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            windows: ProbingWindows {
                primary_sec: 300,
                diversity_sec: 900,
            },
            diff_detection: ProbingDiffDetection {
                new_ip_min_freq: 0.20,
                missing_ip_max_freq: 0.05,
                hop_count_change: 1,
                rtt_shift_frac: 0.50,
            },
            path_health_thresholds: PathHealthThresholds {
                degraded_trigger_pct: 0.05,
                degraded_trigger_sec: 120,
                degraded_min_samples: 30,
                normal_recovery_pct: 0.02,
                normal_recovery_sec: 300,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_priority_pair() {
        let cfg = ProbingSection::default();
        for primary in &cfg.priority {
            for health in [
                PathHealth::Normal,
                PathHealth::Degraded,
                PathHealth::Unreachable,
            ] {
                assert!(
                    cfg.rates
                        .iter()
                        .any(|r| r.primary == *primary && r.health == health),
                    "missing rate entry for {primary:?} / {health:?}"
                );
            }
        }
    }

    #[test]
    fn defaults_fractions_in_range() {
        let cfg = ProbingSection::default();
        for (name, v) in [
            ("icmp.unhealthy", cfg.icmp_thresholds.unhealthy_trigger_pct),
            ("icmp.recovery", cfg.icmp_thresholds.healthy_recovery_pct),
            ("tcp.unhealthy", cfg.tcp_thresholds.unhealthy_trigger_pct),
            ("tcp.recovery", cfg.tcp_thresholds.healthy_recovery_pct),
            ("udp.unhealthy", cfg.udp_thresholds.unhealthy_trigger_pct),
            ("udp.recovery", cfg.udp_thresholds.healthy_recovery_pct),
            ("diff.new_ip", cfg.diff_detection.new_ip_min_freq),
            ("diff.missing_ip", cfg.diff_detection.missing_ip_max_freq),
            ("diff.rtt_shift", cfg.diff_detection.rtt_shift_frac),
            (
                "path.degraded",
                cfg.path_health_thresholds.degraded_trigger_pct,
            ),
            (
                "path.normal",
                cfg.path_health_thresholds.normal_recovery_pct,
            ),
        ] {
            assert!((0.0..=1.0).contains(&v), "{name} = {v} out of range");
        }
    }
}
