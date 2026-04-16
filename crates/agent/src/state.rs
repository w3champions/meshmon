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
    PathHealth as PbPathHealth, PathHealthThresholds, Protocol, ProtocolHealth as PbProtocolHealth,
    ProtocolThresholds,
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

    pub fn evaluate(
        &mut self,
        primary_stats: Option<&FastSummary>,
        t: &PathHealthThresholds,
        now: Instant,
    ) -> Option<(PathHealthState, PathHealthState)> {
        let from = self.state;
        let new_state = match (self.state, primary_stats) {
            (_, None) => {
                self.condition_since = None;
                PathHealthState::Unreachable
            }
            (PathHealthState::Unreachable, Some(_)) => {
                self.condition_since = None;
                PathHealthState::Normal
            }
            (PathHealthState::Normal, Some(stats)) => {
                if stats.sample_count < t.degraded_min_samples as u64
                    || stats.failure_rate < t.degraded_trigger_pct
                {
                    self.condition_since = None;
                    PathHealthState::Normal
                } else {
                    let since = *self.condition_since.get_or_insert(now);
                    let dwell = Duration::from_secs(t.degraded_trigger_sec as u64);
                    if now.duration_since(since) >= dwell {
                        self.condition_since = None;
                        PathHealthState::Degraded
                    } else {
                        PathHealthState::Normal
                    }
                }
            }
            (PathHealthState::Degraded, Some(stats)) => {
                if stats.failure_rate > t.normal_recovery_pct {
                    self.condition_since = None;
                    PathHealthState::Degraded
                } else {
                    let since = *self.condition_since.get_or_insert(now);
                    let dwell = Duration::from_secs(t.normal_recovery_sec as u64);
                    if now.duration_since(since) >= dwell {
                        self.condition_since = None;
                        PathHealthState::Normal
                    } else {
                        PathHealthState::Degraded
                    }
                }
            }
        };
        self.state = new_state;
        if new_state == from {
            None
        } else {
            Some((from, new_state))
        }
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

/// Resolve `(primary, path_health)` → `RateTriple`. Rules:
/// - `primary = Some(p)` → lookup `(p, path_health)`.
/// - `primary = None` → lookup `(priority[0], Unreachable)`.
/// - Lookup miss → `RateTriple::zero()` + WARN.
pub fn rates_for_mode(config: &ProbeConfig, mode: Mode) -> RateTriple {
    let priority = config.priority_list();
    let (lookup_primary, lookup_health) = match mode.primary {
        Some(p) => (p, path_to_proto(mode.path_health)),
        None => (
            priority.first().copied().unwrap_or(Protocol::Icmp),
            PbPathHealth::Unreachable,
        ),
    };
    match config.rates_for(lookup_primary, lookup_health) {
        Some(row) => RateTriple {
            icmp_pps: row.icmp_pps,
            tcp_pps: row.tcp_pps,
            udp_pps: row.udp_pps,
        },
        None => {
            tracing::warn!(
                primary = ?lookup_primary,
                health = ?lookup_health,
                "no RateEntry for mode; publishing zero rates",
            );
            RateTriple::zero()
        }
    }
}

fn path_to_proto(state: PathHealthState) -> PbPathHealth {
    match state {
        PathHealthState::Normal => PbPathHealth::Normal,
        PathHealthState::Degraded => PbPathHealth::Degraded,
        PathHealthState::Unreachable => PbPathHealth::Unreachable,
    }
}

pub fn select_primary(
    priority: &[Protocol],
    healths: &[(Protocol, ProtoHealth)],
) -> Option<Protocol> {
    for p in priority {
        if let Some((_, h)) = healths.iter().find(|(proto, _)| proto == p) {
            if *h == ProtoHealth::Healthy {
                return Some(*p);
            }
        }
    }
    None
}

pub fn trippy_pps_for(protocol: Protocol, rates: RateTriple) -> f64 {
    match protocol {
        Protocol::Icmp => rates.icmp_pps,
        Protocol::Tcp => rates.tcp_pps,
        Protocol::Udp => rates.udp_pps,
        Protocol::Unspecified => 0.0,
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
impl PathStateMachine {
    pub fn force_state_for_tests(&mut self, state: PathHealthState) {
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

    fn config_with_rates(rates: Vec<meshmon_protocol::RateEntry>) -> ProbeConfig {
        ProbeConfig::from_proto(meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            priority: vec![
                Protocol::Icmp as i32,
                Protocol::Tcp as i32,
                Protocol::Udp as i32,
            ],
            rates,
            ..Default::default()
        })
        .unwrap()
    }

    fn rate_row(
        primary: Protocol,
        health: meshmon_protocol::PathHealth,
        icmp: f64,
        tcp: f64,
        udp: f64,
    ) -> meshmon_protocol::RateEntry {
        meshmon_protocol::RateEntry {
            primary: primary as i32,
            health: health as i32,
            icmp_pps: icmp,
            tcp_pps: tcp,
            udp_pps: udp,
        }
    }

    #[test]
    fn rates_for_mode_picks_matching_row() {
        let cfg = config_with_rates(vec![
            rate_row(
                Protocol::Icmp,
                meshmon_protocol::PathHealth::Normal,
                0.2,
                0.05,
                0.05,
            ),
            rate_row(
                Protocol::Icmp,
                meshmon_protocol::PathHealth::Degraded,
                1.0,
                0.05,
                0.05,
            ),
        ]);
        let r = rates_for_mode(
            &cfg,
            Mode {
                primary: Some(Protocol::Icmp),
                path_health: PathHealthState::Degraded,
            },
        );
        assert_eq!(r.icmp_pps, 1.0);
    }

    #[test]
    fn rates_for_mode_falls_back_to_priority_zero_when_primary_none() {
        let cfg = config_with_rates(vec![rate_row(
            Protocol::Icmp,
            meshmon_protocol::PathHealth::Unreachable,
            1.0,
            0.05,
            0.05,
        )]);
        let r = rates_for_mode(
            &cfg,
            Mode {
                primary: None,
                path_health: PathHealthState::Unreachable,
            },
        );
        assert_eq!(r.icmp_pps, 1.0);
    }

    #[test]
    fn rates_for_mode_returns_zero_triple_when_row_missing() {
        let cfg = config_with_rates(vec![]);
        let r = rates_for_mode(
            &cfg,
            Mode {
                primary: Some(Protocol::Udp),
                path_health: PathHealthState::Normal,
            },
        );
        assert_eq!(r, RateTriple::zero());
    }

    #[test]
    fn select_primary_prefers_priority_order() {
        let prio = [Protocol::Icmp, Protocol::Tcp, Protocol::Udp];
        let healths = [
            (Protocol::Icmp, ProtoHealth::Unhealthy),
            (Protocol::Tcp, ProtoHealth::Healthy),
            (Protocol::Udp, ProtoHealth::Healthy),
        ];
        assert_eq!(select_primary(&prio, &healths), Some(Protocol::Tcp));
    }

    #[test]
    fn select_primary_returns_none_when_all_unhealthy() {
        let prio = [Protocol::Icmp, Protocol::Tcp, Protocol::Udp];
        let healths = [
            (Protocol::Icmp, ProtoHealth::Unhealthy),
            (Protocol::Tcp, ProtoHealth::Unhealthy),
            (Protocol::Udp, ProtoHealth::Unhealthy),
        ];
        assert_eq!(select_primary(&prio, &healths), None);
    }

    fn path_thresholds() -> PathHealthThresholds {
        PathHealthThresholds {
            degraded_trigger_pct: 0.05,
            degraded_trigger_sec: 120,
            degraded_min_samples: 30,
            normal_recovery_pct: 0.02,
            normal_recovery_sec: 300,
        }
    }

    #[test]
    fn path_stays_normal_when_primary_healthy() {
        let mut p = PathStateMachine::new();
        let stats = summary(100, 100);
        assert_eq!(
            p.evaluate(Some(&stats), &path_thresholds(), Instant::now()),
            None
        );
        assert_eq!(p.state(), PathHealthState::Normal);
    }

    #[test]
    fn path_degrades_after_dwell_with_enough_samples() {
        let mut p = PathStateMachine::new();
        let t0 = Instant::now();
        let th = path_thresholds();
        let stats = summary(30, 27); // failure_rate = 0.1 > 0.05 trigger, 30 >= min_samples

        assert_eq!(p.evaluate(Some(&stats), &th, t0), None);
        assert_eq!(
            p.evaluate(Some(&stats), &th, t0 + Duration::from_secs(119)),
            None
        );
        assert_eq!(
            p.evaluate(Some(&stats), &th, t0 + Duration::from_secs(120)),
            Some((PathHealthState::Normal, PathHealthState::Degraded)),
        );
    }

    #[test]
    fn path_does_not_degrade_below_min_samples() {
        let mut p = PathStateMachine::new();
        let t0 = Instant::now();
        let th = path_thresholds();
        for offset in [0u64, 60, 130, 200] {
            let r = p.evaluate(
                Some(&summary(10, 0)),
                &th,
                t0 + Duration::from_secs(offset),
            );
            assert_eq!(r, None);
        }
        assert_eq!(p.state(), PathHealthState::Normal);
    }

    #[test]
    fn path_recovers_after_normal_dwell() {
        let mut p = PathStateMachine::new();
        p.force_state_for_tests(PathHealthState::Degraded);
        let t0 = Instant::now();
        let th = path_thresholds();
        let stats = summary(100, 99); // failure_rate = 0.01 <= 0.02

        p.evaluate(Some(&stats), &th, t0);
        assert_eq!(
            p.evaluate(Some(&stats), &th, t0 + Duration::from_secs(299)),
            None
        );
        assert_eq!(
            p.evaluate(Some(&stats), &th, t0 + Duration::from_secs(300)),
            Some((PathHealthState::Degraded, PathHealthState::Normal)),
        );
    }

    #[test]
    fn path_is_unreachable_when_no_primary() {
        let mut p = PathStateMachine::new();
        assert_eq!(
            p.evaluate(None, &path_thresholds(), Instant::now()),
            Some((PathHealthState::Normal, PathHealthState::Unreachable)),
        );
        assert_eq!(p.state(), PathHealthState::Unreachable);
    }

    #[test]
    fn path_snaps_from_unreachable_back_to_normal_on_recovered_primary() {
        let mut p = PathStateMachine::new();
        p.force_state_for_tests(PathHealthState::Unreachable);
        let stats = summary(100, 100);
        assert_eq!(
            p.evaluate(Some(&stats), &path_thresholds(), Instant::now()),
            Some((PathHealthState::Unreachable, PathHealthState::Normal)),
        );
    }
}
