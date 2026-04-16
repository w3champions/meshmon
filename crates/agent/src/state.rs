//! Per-target state machines: per-protocol health + derived path health.
//!
//! See spec 02 § "State machine". Evaluated every 10 s by the per-target
//! supervisor. Pure (no tokio / no async / no locking) so unit tests can
//! inject synthetic [`FastSummary`]s + [`tokio::time::Instant`]s and assert
//! transitions deterministically. Rate publishing is the supervisor's job,
//! not this module's — `evaluate` returns a [`StateChange`] describing what
//! changed and the supervisor translates it into `watch::Sender::send`
//! calls.

use std::time::Duration;

use tokio::time::Instant;

use crate::config::ProbeConfig;
use crate::stats::FastSummary;
use meshmon_protocol::{
    PathHealth as PbPathHealth, Protocol, ProtocolHealth as PbProtocolHealth, ProtocolThresholds,
};

/// Floor on samples-per-window required to transition in either direction.
/// Prevents the empty-window → `failure_rate=0.0` → spurious
/// `Unhealthy→Healthy` oscillation. Hard-coded rather than config-driven
/// because it's a correctness floor, not a tunable.
pub const MIN_TRANSITION_SAMPLES: u64 = 3;

/// Per-protocol health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoHealth {
    Healthy,
    Unhealthy,
}

impl ProtoHealth {
    pub fn to_proto(self) -> PbProtocolHealth {
        match self {
            Self::Healthy => PbProtocolHealth::Healthy,
            Self::Unhealthy => PbProtocolHealth::Unhealthy,
        }
    }
}

/// Path-level derived health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathHealthState {
    #[default]
    Normal,
    Degraded,
    Unreachable,
}

impl PathHealthState {
    pub fn to_proto(self) -> PbPathHealth {
        match self {
            Self::Normal => PbPathHealth::Normal,
            Self::Degraded => PbPathHealth::Degraded,
            Self::Unreachable => PbPathHealth::Unreachable,
        }
    }
}

/// Inputs a `RateEntry` lookup needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// `None` when all protocols are Unhealthy.
    pub primary: Option<Protocol>,
    pub path_health: PathHealthState,
}

/// Rates published to the four prober watch channels on every eval tick.
/// Zero values are legal (spec 02 — "never probe this cell"); the probers
/// idle until a positive rate arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateTriple {
    pub icmp_pps: f64,
    pub tcp_pps: f64,
    pub udp_pps: f64,
}

impl RateTriple {
    pub const fn zero() -> Self {
        Self {
            icmp_pps: 0.0,
            tcp_pps: 0.0,
            udp_pps: 0.0,
        }
    }
}

/// Per-protocol hysteresis state.
#[derive(Debug)]
pub struct ProtocolStateMachine {
    pub protocol: Protocol,
    state: ProtoHealth,
    condition_since: Option<Instant>,
}

impl ProtocolStateMachine {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            state: ProtoHealth::Healthy,
            condition_since: None,
        }
    }

    pub fn state(&self) -> ProtoHealth {
        self.state
    }

    pub fn evaluate(
        &mut self,
        stats: &FastSummary,
        thresholds: &ProtocolThresholds,
        now: Instant,
    ) -> Option<ProtocolTransition> {
        if stats.sample_count < MIN_TRANSITION_SAMPLES {
            self.condition_since = None;
            return None;
        }
        let trigger = condition_for(self.state, stats.failure_rate, thresholds);
        match trigger {
            None => {
                self.condition_since = None;
                None
            }
            Some(target) => {
                let since = *self.condition_since.get_or_insert(now);
                let hysteresis =
                    Duration::from_secs(hysteresis_sec(self.state, thresholds) as u64);
                if now.duration_since(since) >= hysteresis {
                    let from = self.state;
                    self.state = target;
                    self.condition_since = None;
                    Some(ProtocolTransition {
                        protocol: self.protocol,
                        from,
                        to: target,
                    })
                } else {
                    None
                }
            }
        }
    }
}

fn condition_for(
    state: ProtoHealth,
    failure_rate: f64,
    t: &ProtocolThresholds,
) -> Option<ProtoHealth> {
    match state {
        ProtoHealth::Healthy if failure_rate >= t.unhealthy_trigger_pct => {
            Some(ProtoHealth::Unhealthy)
        }
        ProtoHealth::Unhealthy if failure_rate <= t.healthy_recovery_pct => {
            Some(ProtoHealth::Healthy)
        }
        _ => None,
    }
}

fn hysteresis_sec(state: ProtoHealth, t: &ProtocolThresholds) -> u32 {
    match state {
        ProtoHealth::Healthy => t.unhealthy_hysteresis_sec,
        ProtoHealth::Unhealthy => t.healthy_hysteresis_sec,
    }
}

/// Path-level state machine. Driven by the primary protocol's stats.
#[derive(Debug)]
pub struct PathStateMachine {
    state: PathHealthState,
    condition_since: Option<Instant>,
}

impl PathStateMachine {
    pub fn new() -> Self {
        Self {
            state: PathHealthState::Normal,
            condition_since: None,
        }
    }

    pub fn state(&self) -> PathHealthState {
        self.state
    }
}

impl Default for PathStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// One per-protocol transition event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolTransition {
    pub protocol: Protocol,
    pub from: ProtoHealth,
    pub to: ProtoHealth,
}

/// Composite result of one `TargetStateMachine::evaluate` call.
#[derive(Debug, Clone, PartialEq)]
pub struct StateChange {
    /// Per-protocol transitions that fired this tick.
    pub protocol_transitions: Vec<ProtocolTransition>,
    pub path: PathHealthState,
    pub path_transition: Option<(PathHealthState, PathHealthState)>,
    pub primary: Option<Protocol>,
    pub primary_transition: Option<(Option<Protocol>, Option<Protocol>)>,
    pub rates: RateTriple,
    pub trippy_protocol: Protocol,
    pub trippy_pps: f64,
}

/// Composed per-target state: three per-protocol machines + one path machine.
#[derive(Debug)]
pub struct TargetStateMachine {
    icmp: ProtocolStateMachine,
    tcp: ProtocolStateMachine,
    udp: ProtocolStateMachine,
    path: PathStateMachine,
}

impl TargetStateMachine {
    pub fn new() -> Self {
        Self {
            icmp: ProtocolStateMachine::new(Protocol::Icmp),
            tcp: ProtocolStateMachine::new(Protocol::Tcp),
            udp: ProtocolStateMachine::new(Protocol::Udp),
            path: PathStateMachine::new(),
        }
    }
}

impl Default for TargetStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ProtocolStateMachine {
    /// Test-only: seed the machine into a specific state, skipping
    /// hysteresis. Used to set up Unhealthy starting states for recovery
    /// tests without simulating the full inbound transition path.
    pub fn force_state_for_tests(&mut self, state: ProtoHealth) {
        self.state = state;
        self.condition_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icmp_thresholds() -> ProtocolThresholds {
        ProtocolThresholds {
            unhealthy_trigger_pct: 0.9,
            healthy_recovery_pct: 0.1,
            unhealthy_hysteresis_sec: 30,
            healthy_hysteresis_sec: 60,
        }
    }

    fn summary(sample_count: u64, successful: u64) -> FastSummary {
        let failure_rate = if sample_count == 0 {
            0.0
        } else {
            1.0 - (successful as f64 / sample_count as f64)
        };
        FastSummary {
            sample_count,
            successful,
            failure_rate,
            mean_rtt_micros: None,
            stddev_rtt_micros: None,
            min_rtt_micros: None,
            max_rtt_micros: None,
        }
    }

    #[test]
    fn protocol_stays_healthy_below_trigger() {
        let mut m = ProtocolStateMachine::new(Protocol::Icmp);
        let now = Instant::now();
        let t = m.evaluate(&summary(10, 9), &icmp_thresholds(), now);
        assert_eq!(t, None);
        assert_eq!(m.state(), ProtoHealth::Healthy);
    }

    #[test]
    fn unhealthy_fires_only_after_hysteresis() {
        let mut m = ProtocolStateMachine::new(Protocol::Icmp);
        let t0 = Instant::now();
        let th = icmp_thresholds();

        assert_eq!(m.evaluate(&summary(10, 0), &th, t0), None);
        assert_eq!(
            m.evaluate(&summary(20, 0), &th, t0 + Duration::from_secs(29)),
            None
        );
        assert_eq!(
            m.evaluate(&summary(30, 0), &th, t0 + Duration::from_secs(30)),
            Some(ProtocolTransition {
                protocol: Protocol::Icmp,
                from: ProtoHealth::Healthy,
                to: ProtoHealth::Unhealthy,
            }),
        );
        assert_eq!(m.state(), ProtoHealth::Unhealthy);
    }

    #[test]
    fn condition_interrupt_resets_timer() {
        let mut m = ProtocolStateMachine::new(Protocol::Icmp);
        let t0 = Instant::now();
        let th = icmp_thresholds();

        m.evaluate(&summary(10, 0), &th, t0);
        m.evaluate(&summary(10, 5), &th, t0 + Duration::from_secs(20));
        assert_eq!(
            m.evaluate(&summary(20, 0), &th, t0 + Duration::from_secs(45)),
            None,
            "timer should have reset at the 20s condition drop"
        );
    }

    #[test]
    fn min_samples_guard_blocks_recovery_on_empty_window() {
        let mut m = ProtocolStateMachine::new(Protocol::Icmp);
        m.force_state_for_tests(ProtoHealth::Unhealthy);
        let t0 = Instant::now();
        let th = icmp_thresholds();
        for offset in 0..120 {
            let r = m.evaluate(&summary(0, 0), &th, t0 + Duration::from_secs(offset));
            assert_eq!(r, None, "empty window must not drive recovery");
        }
        assert_eq!(m.state(), ProtoHealth::Unhealthy);
    }

    #[test]
    fn healthy_recovery_fires_after_hysteresis_with_samples() {
        let mut m = ProtocolStateMachine::new(Protocol::Icmp);
        m.force_state_for_tests(ProtoHealth::Unhealthy);
        let t0 = Instant::now();
        let th = icmp_thresholds();

        m.evaluate(&summary(50, 50), &th, t0);
        assert_eq!(
            m.evaluate(&summary(60, 60), &th, t0 + Duration::from_secs(59)),
            None
        );
        assert_eq!(
            m.evaluate(&summary(70, 70), &th, t0 + Duration::from_secs(60)),
            Some(ProtocolTransition {
                protocol: Protocol::Icmp,
                from: ProtoHealth::Unhealthy,
                to: ProtoHealth::Healthy,
            }),
        );
    }
}
