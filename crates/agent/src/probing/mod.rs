// crates/agent/src/probing/mod.rs
//! Probing shared types.
//!
//! Each prober task (T12: trippy, TCP, UDP) emits `ProbeObservation`s into
//! the per-target supervisor's mpsc channel. This module owns the shared
//! data types; prober implementations live in sibling files.

pub mod echo_tcp;
pub mod wire;

use std::net::IpAddr;

/// One probe outcome reported by a prober. The supervisor maps these onto
/// `RollingStats` counters; see spec 02 § Probe outcomes.
#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    /// Probe completed successfully.
    Success {
        /// Round-trip time in microseconds.
        rtt_micros: u32,
    },
    /// Probe sent but no response within the per-probe deadline.
    Timeout,
    /// Probe was actively refused. For TCP this is an RST (or equivalent
    /// "connection refused"). For UDP this is the peer's listener sending
    /// back the `0xFFFFFFFF` rejection marker (see spec 02 § UDP wire
    /// protocol).
    Refused,
    /// Local/socket-level error (bind failure, unreachable, etc.). Logged
    /// and counted as failure; the string is for operator diagnosis only.
    Error(String),
}

impl ProbeOutcome {
    /// Convenience: did this probe succeed?
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// RTT if present (only on success).
    pub fn rtt_micros(&self) -> Option<u32> {
        match self {
            Self::Success { rtt_micros } => Some(*rtt_micros),
            _ => None,
        }
    }
}

/// One observation emitted into the supervisor's mpsc channel.
#[derive(Debug, Clone)]
pub struct ProbeObservation {
    /// Protocol that produced this observation.
    pub protocol: meshmon_protocol::Protocol,
    /// Target agent ID this observation is for.
    pub target_id: String,
    /// Outcome.
    pub outcome: ProbeOutcome,
    /// Hop-level detail from trippy. `None` for TCP/UDP direct pings.
    pub hops: Option<Vec<HopObservation>>,
    /// Monotonic instant when the probe was sent. Used for window math
    /// (rolling stats, state-machine dwell timers). NOT wall-clock time.
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

/// Probe rate in probes per second. Zero means "do not probe."
///
/// A rate update is delivered via `tokio::sync::watch` to TCP and UDP
/// probers (trippy uses the richer `TrippyRate` because it also needs to
/// know the current primary protocol).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeRate(pub f64);

impl ProbeRate {
    /// Compute the per-probe interval with ±20% jitter, or `None` if the
    /// rate is zero / non-finite (caller should idle until the next rate
    /// update).
    pub fn next_interval(self, rng: &mut impl rand::Rng) -> Option<std::time::Duration> {
        let pps = self.0;
        if !pps.is_finite() || pps <= 0.0 {
            return None;
        }
        let mean = 1.0 / pps;
        let jitter: f64 = mean * rng.random_range(-0.2..=0.2);
        let secs = (mean + jitter).max(0.001);
        Some(std::time::Duration::from_secs_f64(secs))
    }
}

/// Trippy rate update: both the primary protocol (which determines
/// trippy's MTR mode) and the pps. When `protocol` changes in a `watch`
/// update the trippy driver tears down its cached tracer and builds a new
/// one; pps changes alone do not rebuild.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrippyRate {
    pub protocol: meshmon_protocol::Protocol,
    pub pps: f64,
}

impl TrippyRate {
    pub fn idle() -> Self {
        Self {
            protocol: meshmon_protocol::Protocol::Unspecified,
            pps: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_outcome_success_helpers() {
        let outcome = ProbeOutcome::Success { rtt_micros: 1_234 };
        assert!(outcome.is_success());
        assert_eq!(outcome.rtt_micros(), Some(1_234));
    }

    #[test]
    fn probe_outcome_failure_helpers() {
        for outcome in [
            ProbeOutcome::Timeout,
            ProbeOutcome::Refused,
            ProbeOutcome::Error("bind failed".to_string()),
        ] {
            assert!(!outcome.is_success());
            assert_eq!(outcome.rtt_micros(), None);
        }
    }

    #[test]
    fn probe_rate_zero_returns_none() {
        let mut rng = rand::rng();
        assert_eq!(ProbeRate(0.0).next_interval(&mut rng), None);
        assert_eq!(ProbeRate(-1.0).next_interval(&mut rng), None);
        assert_eq!(ProbeRate(f64::NAN).next_interval(&mut rng), None);
        assert_eq!(ProbeRate(f64::INFINITY).next_interval(&mut rng), None);
    }

    #[test]
    fn probe_rate_positive_stays_within_jitter_band() {
        let mut rng = rand::rng();
        // 10 pps → mean 100ms; ±20% jitter → [80ms, 120ms].
        let rate = ProbeRate(10.0);
        for _ in 0..256 {
            let interval = rate.next_interval(&mut rng).expect("positive rate");
            let micros = interval.as_micros();
            assert!(
                (80_000..=120_000).contains(&micros),
                "interval out of jitter band: {micros}us",
            );
        }
    }

    #[test]
    fn probe_rate_huge_rate_clamps_to_minimum() {
        let mut rng = rand::rng();
        // 1e9 pps would otherwise yield a nanosecond-scale interval; the
        // clamp floor is 1ms so the returned duration should be >= 1ms.
        let interval = ProbeRate(1e9).next_interval(&mut rng).expect("positive");
        assert!(interval >= std::time::Duration::from_millis(1));
    }

    #[test]
    fn trippy_rate_idle_is_zero_unspecified() {
        let idle = TrippyRate::idle();
        assert_eq!(idle.pps, 0.0);
        assert_eq!(idle.protocol, meshmon_protocol::Protocol::Unspecified);
    }
}
