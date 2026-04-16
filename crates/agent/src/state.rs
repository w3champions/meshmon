//! Per-target state machines: per-protocol health + derived path health.
//!
//! See spec 02 § "State machine". Evaluated every 10 s by the per-target
//! supervisor. Pure (no tokio / no async / no locking) so unit tests can
//! inject synthetic [`FastSummary`]s + [`tokio::time::Instant`]s and assert
//! transitions deterministically. Rate publishing is the supervisor's job,
//! not this module's — `evaluate` returns a [`StateChange`] describing what
//! changed and the supervisor translates it into `watch::Sender::send`
//! calls.

use tokio::time::Instant;

use crate::config::ProbeConfig;
use crate::stats::FastSummary;
use meshmon_protocol::{PathHealth as PbPathHealth, Protocol, ProtocolHealth as PbProtocolHealth};

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
