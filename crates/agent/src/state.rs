//! Per-target state machines: per-protocol health + derived path health.
//!
//! See spec 02 § "State machine". Evaluated every 10 s by the per-target
//! supervisor. Pure (no tokio / no async / no locking) so unit tests can
//! inject synthetic [`FastSummary`]s + [`tokio::time::Instant`]s and assert
//! transitions deterministically. Rate publishing is the supervisor's job,
//! not this module's — `evaluate` returns a [`StateChange`] describing what
//! changed and the supervisor translates it into `watch::Sender::send`
//! calls.
//!
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
pub(crate) const MIN_TRANSITION_SAMPLES: u64 = 3;

/// Per-protocol health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtoHealth {
    Healthy,
    Unhealthy,
}

impl ProtoHealth {
    /// Consumed by T16 emitter scaffolding.
    #[allow(dead_code)]
    pub(crate) fn to_proto(self) -> PbProtocolHealth {
        match self {
            Self::Healthy => PbProtocolHealth::Healthy,
            Self::Unhealthy => PbProtocolHealth::Unhealthy,
        }
    }
}

/// Path-level derived health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PathHealthState {
    #[default]
    Normal,
    Degraded,
    Unreachable,
}

impl PathHealthState {
    pub(crate) fn to_proto(self) -> PbPathHealth {
        match self {
            Self::Normal => PbPathHealth::Normal,
            Self::Degraded => PbPathHealth::Degraded,
            Self::Unreachable => PbPathHealth::Unreachable,
        }
    }
}

/// Inputs a `RateEntry` lookup needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mode {
    /// `None` when all protocols are Unhealthy.
    pub(crate) primary: Option<Protocol>,
    pub(crate) path_health: PathHealthState,
}

/// Rates published to the four prober watch channels on every eval tick.
/// Zero values are legal (spec 02 — "never probe this cell"); the probers
/// idle until a positive rate arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RateTriple {
    pub(crate) icmp_pps: f64,
    pub(crate) tcp_pps: f64,
    pub(crate) udp_pps: f64,
}

impl RateTriple {
    pub(crate) const fn zero() -> Self {
        Self {
            icmp_pps: 0.0,
            tcp_pps: 0.0,
            udp_pps: 0.0,
        }
    }
}

/// Per-protocol hysteresis state.
#[derive(Debug)]
pub(crate) struct ProtocolStateMachine {
    pub(crate) protocol: Protocol,
    state: ProtoHealth,
    condition_since: Option<Instant>,
}

impl ProtocolStateMachine {
    pub(crate) fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            state: ProtoHealth::Healthy,
            condition_since: None,
        }
    }

    pub(crate) fn state(&self) -> ProtoHealth {
        self.state
    }

    pub(crate) fn evaluate(
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
                let hysteresis = Duration::from_secs(hysteresis_sec(self.state, thresholds) as u64);
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
pub(crate) struct PathStateMachine {
    state: PathHealthState,
    condition_since: Option<Instant>,
}

impl PathStateMachine {
    pub(crate) fn new() -> Self {
        Self {
            state: PathHealthState::Normal,
            condition_since: None,
        }
    }

    pub(crate) fn state(&self) -> PathHealthState {
        self.state
    }

    pub(crate) fn evaluate(
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
                // Symmetric evidence floor with the Normal → Degraded
                // path above: when the primary's rolling window empties,
                // `failure_rate` collapses to 0.0 and would otherwise
                // satisfy the recovery predicate, flipping back to Normal
                // with zero evidence of recovery. Require at least
                // MIN_TRANSITION_SAMPLES before honouring the dwell timer.
                if stats.sample_count < MIN_TRANSITION_SAMPLES
                    || stats.failure_rate > t.normal_recovery_pct
                {
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
pub(crate) struct ProtocolTransition {
    pub(crate) protocol: Protocol,
    pub(crate) from: ProtoHealth,
    pub(crate) to: ProtoHealth,
}

/// Composite result of one `TargetStateMachine::evaluate` call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StateChange {
    /// Per-protocol transitions that fired this tick.
    pub(crate) protocol_transitions: Vec<ProtocolTransition>,
    pub(crate) path: PathHealthState,
    pub(crate) path_transition: Option<(PathHealthState, PathHealthState)>,
    pub(crate) primary: Option<Protocol>,
    pub(crate) primary_transition: Option<(Option<Protocol>, Option<Protocol>)>,
    pub(crate) rates: RateTriple,
    pub(crate) trippy_protocol: Protocol,
    pub(crate) trippy_pps: f64,
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
    pub(crate) fn new() -> Self {
        Self {
            icmp: ProtocolStateMachine::new(Protocol::Icmp),
            tcp: ProtocolStateMachine::new(Protocol::Tcp),
            udp: ProtocolStateMachine::new(Protocol::Udp),
            path: PathStateMachine::new(),
        }
    }

    pub(crate) fn health_snapshot(&self) -> [(Protocol, ProtoHealth); 3] {
        [
            (Protocol::Icmp, self.icmp.state()),
            (Protocol::Tcp, self.tcp.state()),
            (Protocol::Udp, self.udp.state()),
        ]
    }

    #[allow(dead_code)]
    pub(crate) fn path_state(&self) -> PathHealthState {
        self.path.state()
    }

    pub(crate) fn evaluate(
        &mut self,
        config: &ProbeConfig,
        stats_by_protocol: [&FastSummary; 3],
        now: Instant,
    ) -> StateChange {
        let priority = config.priority_list();
        let old_primary = select_primary(&priority, &self.health_snapshot(), stats_by_protocol);

        let mut protocol_transitions = Vec::with_capacity(3);
        if let Some(t) = self.icmp.evaluate(
            stats_by_protocol[0],
            &config.thresholds_for(Protocol::Icmp),
            now,
        ) {
            protocol_transitions.push(t);
        }
        if let Some(t) = self.tcp.evaluate(
            stats_by_protocol[1],
            &config.thresholds_for(Protocol::Tcp),
            now,
        ) {
            protocol_transitions.push(t);
        }
        if let Some(t) = self.udp.evaluate(
            stats_by_protocol[2],
            &config.thresholds_for(Protocol::Udp),
            now,
        ) {
            protocol_transitions.push(t);
        }

        let new_primary = select_primary(&priority, &self.health_snapshot(), stats_by_protocol);
        let primary_transition = if new_primary == old_primary {
            None
        } else {
            Some((old_primary, new_primary))
        };

        let primary_stats: Option<&FastSummary> = new_primary.and_then(|p| match p {
            Protocol::Icmp => Some(stats_by_protocol[0]),
            Protocol::Tcp => Some(stats_by_protocol[1]),
            Protocol::Udp => Some(stats_by_protocol[2]),
            Protocol::Unspecified => None,
        });
        let path_transition = self
            .path
            .evaluate(primary_stats, &config.path_thresholds(), now);
        let path_state = self.path.state();

        let rates = rates_for_mode(
            config,
            Mode {
                primary: new_primary,
                path_health: path_state,
            },
        );
        let trippy_protocol = new_primary
            .unwrap_or_else(|| priority.first().copied().unwrap_or(Protocol::Unspecified));
        let trippy_pps = trippy_pps_for(trippy_protocol, rates);

        StateChange {
            protocol_transitions,
            path: path_state,
            path_transition,
            primary: new_primary,
            primary_transition,
            rates,
            trippy_protocol,
            trippy_pps,
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
pub(crate) fn rates_for_mode(config: &ProbeConfig, mode: Mode) -> RateTriple {
    let priority = config.priority_list();
    let (lookup_primary, lookup_health) = match mode.primary {
        Some(p) => (p, mode.path_health.to_proto()),
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

/// Returns the first priority-ordered protocol whose state machine is
/// `Healthy` **and** whose rolling window carries at least
/// [`MIN_TRANSITION_SAMPLES`] samples.
///
/// A bare `Healthy` check is insufficient because every
/// `ProtocolStateMachine` starts in `Healthy` and `evaluate` early-returns
/// below the sample floor, leaving never-probed protocols in their initial
/// state. Selecting such a protocol would have the supervisor report
/// `Normal` with zero evidence. Gating on `sample_count` here keeps
/// `select_primary` symmetric with [`ProtocolStateMachine::evaluate`]'s
/// own floor — until evidence exists we have no healthy path.
pub(crate) fn select_primary(
    priority: &[Protocol],
    healths: &[(Protocol, ProtoHealth)],
    stats_by_protocol: [&FastSummary; 3],
) -> Option<Protocol> {
    for p in priority {
        let Some(&(_, h)) = healths.iter().find(|(proto, _)| proto == p) else {
            continue;
        };
        if h != ProtoHealth::Healthy {
            continue;
        }
        let sample_count = match p {
            Protocol::Icmp => stats_by_protocol[0].sample_count,
            Protocol::Tcp => stats_by_protocol[1].sample_count,
            Protocol::Udp => stats_by_protocol[2].sample_count,
            Protocol::Unspecified => continue,
        };
        if sample_count >= MIN_TRANSITION_SAMPLES {
            return Some(*p);
        }
    }
    None
}

pub(crate) fn trippy_pps_for(protocol: Protocol, rates: RateTriple) -> f64 {
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
    pub(crate) fn force_state_for_tests(&mut self, state: ProtoHealth) {
        self.state = state;
        self.condition_since = None;
    }
}

#[cfg(test)]
impl PathStateMachine {
    pub(crate) fn force_state_for_tests(&mut self, state: PathHealthState) {
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

    /// Mirrors `config::default_tcp_thresholds()`. Kept duplicated here
    /// so the state-machine tests remain independent of `config.rs`; if
    /// the defaults ever diverge, the mismatch surfaces as a deliberate
    /// code change in both places.
    fn tcp_thresholds() -> ProtocolThresholds {
        ProtocolThresholds {
            unhealthy_trigger_pct: 0.5,
            healthy_recovery_pct: 0.05,
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

    /// Covers TCP's stricter defaults (0.5 trigger / 0.05 recovery)
    /// instead of ICMP's (0.9 / 0.1). A failure_rate of 0.6 is above
    /// TCP's trigger but well below ICMP's, so if the SM ever regressed
    /// to a hardcoded threshold this test would catch it.
    #[test]
    fn tcp_transitions_at_tcp_thresholds() {
        let mut m = ProtocolStateMachine::new(Protocol::Tcp);
        let t0 = Instant::now();
        let th = tcp_thresholds();

        // Healthy -> Unhealthy: failure_rate 0.6 is above TCP's 0.5
        // trigger but below ICMP's 0.9 — proves the threshold is
        // parameterized per protocol.
        assert_eq!(m.evaluate(&summary(10, 4), &th, t0), None);
        assert_eq!(
            m.evaluate(&summary(20, 8), &th, t0 + Duration::from_secs(29)),
            None
        );
        assert_eq!(
            m.evaluate(&summary(30, 12), &th, t0 + Duration::from_secs(30)),
            Some(ProtocolTransition {
                protocol: Protocol::Tcp,
                from: ProtoHealth::Healthy,
                to: ProtoHealth::Unhealthy,
            }),
        );
        assert_eq!(m.state(), ProtoHealth::Unhealthy);

        // Unhealthy -> Healthy: failure_rate 0.0 is at/below TCP's 0.05
        // recovery ceiling. Requires 60s of recovery hysteresis.
        let t1 = t0 + Duration::from_secs(30);
        assert_eq!(m.evaluate(&summary(40, 40), &th, t1), None);
        assert_eq!(
            m.evaluate(&summary(50, 50), &th, t1 + Duration::from_secs(59)),
            None
        );
        assert_eq!(
            m.evaluate(&summary(60, 60), &th, t1 + Duration::from_secs(60)),
            Some(ProtocolTransition {
                protocol: Protocol::Tcp,
                from: ProtoHealth::Unhealthy,
                to: ProtoHealth::Healthy,
            }),
        );
        assert_eq!(m.state(), ProtoHealth::Healthy);
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
        // All three protocols have >= MIN_TRANSITION_SAMPLES so the
        // select_primary evidence floor is satisfied; priority order
        // then elects TCP.
        let healthy = summary(10, 10);
        assert_eq!(
            select_primary(&prio, &healths, [&healthy, &healthy, &healthy]),
            Some(Protocol::Tcp),
        );
    }

    #[test]
    fn select_primary_returns_none_when_all_unhealthy() {
        let prio = [Protocol::Icmp, Protocol::Tcp, Protocol::Udp];
        let healths = [
            (Protocol::Icmp, ProtoHealth::Unhealthy),
            (Protocol::Tcp, ProtoHealth::Unhealthy),
            (Protocol::Udp, ProtoHealth::Unhealthy),
        ];
        let healthy = summary(10, 10);
        assert_eq!(
            select_primary(&prio, &healths, [&healthy, &healthy, &healthy]),
            None,
        );
    }

    #[test]
    fn select_primary_rejects_protocols_with_insufficient_samples() {
        let prio = [Protocol::Icmp, Protocol::Tcp, Protocol::Udp];
        let healths = [
            (Protocol::Icmp, ProtoHealth::Healthy),
            (Protocol::Tcp, ProtoHealth::Healthy),
            (Protocol::Udp, ProtoHealth::Healthy),
        ];

        // All three protocols below the evidence floor → None, even though
        // every state machine reports Healthy. This pins the P2 contract:
        // never-probed protocols must not be elected primary on the basis
        // of their initial-state Healthy.
        let empty = summary(0, 0);
        let just_below = summary(MIN_TRANSITION_SAMPLES - 1, MIN_TRANSITION_SAMPLES - 1);
        assert_eq!(
            select_primary(&prio, &healths, [&empty, &empty, &empty]),
            None,
        );
        assert_eq!(
            select_primary(&prio, &healths, [&just_below, &just_below, &just_below]),
            None,
        );

        // ICMP starved, TCP has evidence → skip ICMP, elect TCP.
        let enough = summary(MIN_TRANSITION_SAMPLES, MIN_TRANSITION_SAMPLES);
        assert_eq!(
            select_primary(&prio, &healths, [&empty, &enough, &enough]),
            Some(Protocol::Tcp),
        );

        // Only ICMP has evidence — elected even though TCP/UDP are
        // unprobed: the evidence floor is per-protocol, not global.
        assert_eq!(
            select_primary(&prio, &healths, [&enough, &empty, &empty]),
            Some(Protocol::Icmp),
        );
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
            let r = p.evaluate(Some(&summary(10, 0)), &th, t0 + Duration::from_secs(offset));
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
    fn path_does_not_recover_from_degraded_on_empty_window() {
        // H1 regression: once the primary's window empties, FastSummary
        // reports failure_rate = 0.0 and sample_count = 0. Without the
        // evidence floor, `failure_rate <= normal_recovery_pct` would
        // hold, the dwell timer would start, and after normal_recovery_sec
        // the path would flip back to Normal with zero evidence of real
        // recovery — exactly the oscillation MIN_TRANSITION_SAMPLES
        // already prevents in the protocol SM.
        let mut p = PathStateMachine::new();
        p.force_state_for_tests(PathHealthState::Degraded);
        let t0 = Instant::now();
        let th = path_thresholds();
        let empty = summary(0, 0);

        // Feed the machine empty windows across 2× the recovery dwell.
        for offset in [0u64, 100, 299, 300, 301, 600] {
            let r = p.evaluate(Some(&empty), &th, t0 + Duration::from_secs(offset));
            assert_eq!(
                r, None,
                "empty-window recovery must not fire at offset {offset}",
            );
        }
        assert_eq!(p.state(), PathHealthState::Degraded);

        // Partial evidence (just below the floor) must also be rejected.
        let just_below = summary(MIN_TRANSITION_SAMPLES - 1, MIN_TRANSITION_SAMPLES - 1);
        for offset in [900u64, 1200] {
            let r = p.evaluate(Some(&just_below), &th, t0 + Duration::from_secs(offset));
            assert_eq!(
                r, None,
                "below-floor recovery must not fire at offset {offset}",
            );
        }
        assert_eq!(p.state(), PathHealthState::Degraded);

        // Once real evidence returns, recovery proceeds normally through
        // the dwell timer.
        let healthy = summary(100, 100);
        let t_real = t0 + Duration::from_secs(1500);
        assert_eq!(p.evaluate(Some(&healthy), &th, t_real), None);
        assert_eq!(
            p.evaluate(
                Some(&healthy),
                &th,
                t_real + Duration::from_secs(th.normal_recovery_sec as u64),
            ),
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

    fn full_config() -> ProbeConfig {
        use meshmon_protocol::PathHealth as H;
        let rates = vec![
            rate_row(Protocol::Icmp, H::Normal, 0.2, 0.05, 0.05),
            rate_row(Protocol::Icmp, H::Degraded, 1.0, 0.05, 0.05),
            rate_row(Protocol::Icmp, H::Unreachable, 1.0, 0.05, 0.05),
            rate_row(Protocol::Tcp, H::Normal, 0.05, 0.2, 0.05),
            rate_row(Protocol::Tcp, H::Degraded, 0.05, 1.0, 0.05),
            rate_row(Protocol::Tcp, H::Unreachable, 0.05, 1.0, 0.05),
            rate_row(Protocol::Udp, H::Normal, 0.05, 0.05, 0.2),
            rate_row(Protocol::Udp, H::Degraded, 0.05, 0.05, 1.0),
            rate_row(Protocol::Udp, H::Unreachable, 0.05, 0.05, 1.0),
        ];
        config_with_rates(rates)
    }

    #[test]
    fn fresh_machine_emits_normal_icmp_primary() {
        let cfg = full_config();
        let mut tsm = TargetStateMachine::new();
        let healthy = summary(100, 100);
        let change = tsm.evaluate(&cfg, [&healthy, &healthy, &healthy], Instant::now());
        assert_eq!(change.primary, Some(Protocol::Icmp));
        assert_eq!(change.path, PathHealthState::Normal);
        assert_eq!(change.rates.icmp_pps, 0.2);
        assert_eq!(change.trippy_protocol, Protocol::Icmp);
        assert_eq!(change.trippy_pps, 0.2);
        assert!(change.protocol_transitions.is_empty());
        assert_eq!(change.path_transition, None);
        assert_eq!(change.primary_transition, None);
    }

    #[test]
    fn primary_swings_to_tcp_when_icmp_goes_unhealthy() {
        let cfg = full_config();
        let mut tsm = TargetStateMachine::new();
        let t0 = Instant::now();
        let healthy = summary(100, 100);
        let bad_icmp = summary(100, 0);

        tsm.evaluate(&cfg, [&bad_icmp, &healthy, &healthy], t0);
        let change = tsm.evaluate(
            &cfg,
            [&bad_icmp, &healthy, &healthy],
            t0 + Duration::from_secs(30),
        );
        assert_eq!(change.primary, Some(Protocol::Tcp));
        assert_eq!(change.rates.tcp_pps, 0.2);
        assert_eq!(change.trippy_protocol, Protocol::Tcp);
        assert_eq!(change.trippy_pps, 0.2);
        assert_eq!(
            change.primary_transition,
            Some((Some(Protocol::Icmp), Some(Protocol::Tcp))),
        );
    }

    #[test]
    fn all_unhealthy_yields_unreachable_and_fallback_rates() {
        let cfg = full_config();
        let mut tsm = TargetStateMachine::new();
        let t0 = Instant::now();
        let bad = summary(100, 0);

        tsm.evaluate(&cfg, [&bad, &bad, &bad], t0);
        let change = tsm.evaluate(&cfg, [&bad, &bad, &bad], t0 + Duration::from_secs(30));
        assert_eq!(change.primary, None);
        assert_eq!(change.path, PathHealthState::Unreachable);
        assert_eq!(change.rates.icmp_pps, 1.0); // icmp-unreachable row
        assert_eq!(change.trippy_protocol, Protocol::Icmp);
        assert_eq!(change.trippy_pps, 1.0);
    }

    #[test]
    fn path_degrades_when_primary_loses_samples() {
        let cfg = full_config();
        let mut tsm = TargetStateMachine::new();
        let t0 = Instant::now();
        let noisy_icmp = summary(30, 27); // failure_rate = 0.1: healthy at protocol, degraded at path
        let healthy = summary(30, 30);

        tsm.evaluate(&cfg, [&noisy_icmp, &healthy, &healthy], t0);
        let change = tsm.evaluate(
            &cfg,
            [&noisy_icmp, &healthy, &healthy],
            t0 + Duration::from_secs(120),
        );
        assert_eq!(change.primary, Some(Protocol::Icmp));
        assert_eq!(change.path, PathHealthState::Degraded);
        assert_eq!(change.rates.icmp_pps, 1.0);
    }
}
