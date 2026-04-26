//! Domain types + state-machine helper.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::ipnetwork::IpNetwork;
use utoipa::ToSchema;
use uuid::Uuid;

/// Lifecycle state of a measurement campaign. Mirrors the
/// `campaign_state` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "campaign_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CampaignState {
    /// Editable, not yet scheduling. Default for newly created campaigns.
    Draft,
    /// Scheduler is dispatching pairs and ingesting results.
    Running,
    /// All pairs have reached a terminal state; awaiting evaluation.
    Completed,
    /// Evaluation pass (spec 04) has produced the result set.
    Evaluated,
    /// Operator halted the campaign before completion; may be re-edited.
    Stopped,
}

/// Terminal or in-progress state of a single `(source_agent_id,
/// destination_ip)` pair within a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "pair_resolution_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PairResolutionState {
    /// Waiting to be picked up by the scheduler.
    Pending,
    /// Dispatched to an agent; awaiting result.
    Dispatched,
    /// Resolved by reusing a measurement from the 24 h window.
    Reused,
    /// Agent completed the measurement successfully.
    Succeeded,
    /// Destination is unreachable (terminal failure).
    Unreachable,
    /// Skipped because the source agent is unavailable or ineligible.
    Skipped,
}

impl PairResolutionState {
    /// True when the pair has reached a terminal state and should no
    /// longer be scheduled.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Reused | Self::Succeeded | Self::Unreachable | Self::Skipped
        )
    }

    /// Stable lowercase label matching the Postgres enum value. Used as
    /// the `state` label on `meshmon_campaign_pairs_total`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Reused => "reused",
            Self::Succeeded => "succeeded",
            Self::Unreachable => "unreachable",
            Self::Skipped => "skipped",
        }
    }

    /// Every variant. Used by metric samplers to reset labels whose
    /// count dropped to zero (Postgres `GROUP BY` omits zero rows).
    pub const ALL: &'static [Self] = &[
        Self::Pending,
        Self::Dispatched,
        Self::Reused,
        Self::Succeeded,
        Self::Unreachable,
        Self::Skipped,
    ];
}

impl CampaignState {
    /// Stable lowercase label matching the Postgres enum value. Used as
    /// the `state` label on `meshmon_campaigns_total`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Evaluated => "evaluated",
            Self::Stopped => "stopped",
        }
    }

    /// Every variant. Used by metric samplers to reset labels whose
    /// count dropped to zero (Postgres `GROUP BY` omits zero rows).
    pub const ALL: &'static [Self] = &[
        Self::Draft,
        Self::Running,
        Self::Completed,
        Self::Evaluated,
        Self::Stopped,
    ];
}

/// The probe protocol an individual pair uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "probe_protocol", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ProbeProtocol {
    /// ICMP echo (requires `CAP_NET_RAW` on the agent).
    Icmp,
    /// TCP handshake against the meshmon echo port.
    Tcp,
    /// UDP echo against the meshmon echo port (secret-gated).
    Udp,
}

/// Evaluation strategy for the campaign's result-aggregation pass
/// (spec 04). Storage here; consumed by T48.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "evaluation_mode", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    /// Maximize path diversity across the selected sources.
    Diversity,
    /// Optimize the aggregate result against the loss/stddev target.
    Optimization,
    /// Rank candidate IPs by their connectivity to the existing mesh
    /// (directly or transitively). See spec
    /// `docs/superpowers/specs/2026-04-26-campaigns-edge-candidate-evaluation-mode-design.md`.
    EdgeCandidate,
}

/// Where an evaluation pair-detail's "direct A→B" baseline came from.
///
/// Mirrors the `pair_detail_direct_source` Postgres enum. The `/evaluate`
/// handler layers VM continuous-mesh baselines on top of the active-probe
/// `measurements` join for agent→agent pairs the campaign didn't cover
/// itself, stamping [`Self::VmContinuous`] on the synthesized rows. When
/// both sources exist for the same pair, active-probe wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "pair_detail_direct_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DirectSource {
    /// Baseline derived from a per-campaign active probe measurement
    /// (i.e. the `measurements` row joined in via `campaign_pairs`).
    ActiveProbe,
    /// Baseline synthesized from VictoriaMetrics continuous-monitoring
    /// samples at `/evaluate` time — used when the campaign's own
    /// active-probe data left the agent→agent pair uncovered.
    VmContinuous,
}

/// Provenance of a leg within a composed evaluator route.
///
/// Distinct from `DirectSource` (baseline-only) because edge_candidate routes
/// can include legs derived from symmetric reuse (using an `agent → candidate`
/// probe to model `candidate → agent` direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LegSource {
    /// Leg data sourced from VictoriaMetrics continuous-mesh time series.
    VmContinuous,
    /// Leg data sourced from a campaign active-probe measurement.
    ActiveProbe,
    /// Leg data inferred from the reverse direction (A→X used as X→A).
    SymmetricReuse,
}

/// One end of a leg or route. Mixes agent IDs (for mesh agents) with
/// arbitrary IPs (for catalogue candidates).
///
/// `IpAddr` has no built-in `ToSchema` impl under utoipa 5; the field is
/// annotated `schema(value_type = String)` so the OpenAPI document renders
/// the IP as a plain string. Serde still uses the default `IpAddr` display
/// serializer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Endpoint {
    /// A mesh-member agent endpoint, identified by its agent id string.
    Agent {
        /// Agent id (matches `agents.agent_id` in the database).
        id: String,
    },
    /// A catalogue or arbitrary IP candidate endpoint.
    CandidateIp {
        /// IP address of the candidate endpoint.
        #[schema(value_type = String)]
        ip: std::net::IpAddr,
    },
}

/// Wire-friendly companion enum used by `EvaluationEdgePairDetailDto.best_route_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRouteKind {
    /// No transit hops — the candidate connects directly to the destination.
    Direct,
    /// One intermediate transit hop between candidate and destination.
    OneHop,
    /// Two intermediate transit hops between candidate and destination.
    TwoHop,
}

/// Discriminator that pairs with `LegDto.from_id` / `to_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// The endpoint is a mesh-member agent.
    Agent,
    /// The endpoint is a catalogue candidate IP.
    Candidate,
}

/// Kind of measurement row stored in `measurements`. `campaign` is the
/// default; T44 never writes anything else (T45/T48 do).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "measurement_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    /// Campaign-dispatched measurement (produced by the scheduler).
    Campaign,
    /// Operator-initiated detail ping (ad-hoc diagnostic).
    DetailPing,
    /// Operator-initiated detail MTR (ad-hoc diagnostic).
    DetailMtr,
}

/// Allowed lifecycle transitions per spec 02 §3.1.
///
/// Returns `true` when `from -> to` is permitted. Any DB UPDATE that
/// mutates `state` first gates on this check.
pub fn transition_allowed(from: CampaignState, to: CampaignState) -> bool {
    use CampaignState::*;
    match (from, to) {
        (Draft, Running) => true,
        (Running, Completed) => true,
        (Running, Stopped) => true,
        (Completed, Evaluated) => true,
        (Completed, Running) => true, // edit-delta re-run
        (Stopped, Running) => true,   // edit-delta re-run on stopped
        (Evaluated, Running) => true, // edit-delta re-run on evaluated
        _ => false,
    }
}

/// Full `measurement_campaigns` row. Mirrors the DDL one-to-one.
#[derive(Debug, Clone)]
pub struct CampaignRow {
    /// Primary key.
    pub id: Uuid,
    /// Operator-facing title.
    pub title: String,
    /// Free-form operator notes.
    pub notes: String,
    /// Lifecycle state (see [`CampaignState`]).
    pub state: CampaignState,
    /// Probe protocol shared by every pair in the campaign.
    pub protocol: ProbeProtocol,
    /// Probes per dispatched measurement (campaign rounds).
    pub probe_count: i16,
    /// Probes per detail measurement (ping/MTR re-runs from the UI).
    pub probe_count_detail: i16,
    /// Per-probe timeout in milliseconds.
    pub timeout_ms: i32,
    /// Inter-probe stagger in milliseconds.
    pub probe_stagger_ms: i32,
    /// When `true`, the scheduler forces a fresh measurement instead of
    /// reusing a matching row from the 24 h window.
    pub force_measurement: bool,
    /// Loss-rate threshold (fraction 0.0–1.0) used by the evaluator.
    pub loss_threshold_ratio: f32,
    /// Weight applied to RTT stddev by the evaluator.
    pub stddev_weight: f32,
    /// Evaluation strategy (see [`EvaluationMode`]).
    pub evaluation_mode: EvaluationMode,
    /// Optional eligibility cap on composed transit RTT (ms). When
    /// `Some`, the evaluator drops `(A, X, B)` triples whose
    /// `transit_rtt_ms` exceeds the cap before counter accumulation.
    pub max_transit_rtt_ms: Option<f64>,
    /// Optional eligibility cap on composed transit RTT stddev (ms).
    /// Same semantics as [`Self::max_transit_rtt_ms`] but on the
    /// variance-additive composed stddev.
    pub max_transit_stddev_ms: Option<f64>,
    /// Optional storage floor on absolute improvement (ms). When
    /// `Some`, the evaluator persists a `pair_details` row only when
    /// the triple's `improvement_ms` clears the floor (OR-combined
    /// with [`Self::min_improvement_ratio`]).
    pub min_improvement_ms: Option<f64>,
    /// Optional storage floor on relative improvement (fraction
    /// 0.0–1.0). When `Some`, the evaluator persists a `pair_details`
    /// row only when `improvement_ms / direct_rtt_ms` clears the floor
    /// (OR-combined with [`Self::min_improvement_ms`]).
    pub min_improvement_ratio: Option<f64>,
    /// Optional RTT threshold (ms) below which a route qualifies as
    /// "useful" in edge_candidate mode. `None` disables the filter.
    pub useful_latency_ms: Option<f32>,
    /// Maximum number of transit hops for edge_candidate route
    /// enumeration. Range [0, 2]; default 2.
    pub max_hops: i16,
    /// Look-back window (minutes) for VictoriaMetrics data in
    /// edge_candidate mode. Range [1, 1440]; default 15.
    pub vm_lookback_minutes: i32,
    /// Optional principal string (session username) that created the row.
    pub created_by: Option<String>,
    /// Row creation timestamp.
    pub created_at: DateTime<Utc>,
    /// When the campaign most recently transitioned to `Running`.
    pub started_at: Option<DateTime<Utc>>,
    /// When the operator last stopped the campaign, if ever.
    pub stopped_at: Option<DateTime<Utc>>,
    /// When all pairs reached a terminal state, if ever.
    pub completed_at: Option<DateTime<Utc>>,
    /// When the evaluation pass last produced results, if ever.
    pub evaluated_at: Option<DateTime<Utc>>,
}

/// Full `campaign_pairs` row.
#[derive(Debug, Clone)]
pub struct PairRow {
    /// Primary key.
    pub id: i64,
    /// Owning campaign.
    pub campaign_id: Uuid,
    /// Source agent (the prober).
    pub source_agent_id: String,
    /// Destination IP (host address, not a wider CIDR).
    pub destination_ip: IpNetwork,
    /// Current resolution state (see [`PairResolutionState`]).
    pub resolution_state: PairResolutionState,
    /// FK to the `measurements` row once the pair is dispatched or reused.
    pub measurement_id: Option<i64>,
    /// When the scheduler dispatched the pair to an agent.
    pub dispatched_at: Option<DateTime<Utc>>,
    /// When the pair reached a terminal state.
    pub settled_at: Option<DateTime<Utc>>,
    /// Number of dispatch attempts to date.
    pub attempt_count: i16,
    /// Last error observed on this pair, if any.
    pub last_error: Option<String>,
    /// Measurement discriminator: `Campaign` for baseline pairs,
    /// `DetailPing` / `DetailMtr` for operator-initiated detail runs.
    /// Drives the dispatcher's `MeasurementKind` selection and the
    /// scheduler's kind-specific `probe_count` override.
    pub kind: MeasurementKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_can_start() {
        assert!(transition_allowed(
            CampaignState::Draft,
            CampaignState::Running
        ));
        assert!(!transition_allowed(
            CampaignState::Draft,
            CampaignState::Completed
        ));
    }

    #[test]
    fn running_can_stop_or_complete() {
        assert!(transition_allowed(
            CampaignState::Running,
            CampaignState::Stopped
        ));
        assert!(transition_allowed(
            CampaignState::Running,
            CampaignState::Completed
        ));
    }

    #[test]
    fn completed_can_be_evaluated_or_re_edited() {
        assert!(transition_allowed(
            CampaignState::Completed,
            CampaignState::Evaluated
        ));
        assert!(transition_allowed(
            CampaignState::Completed,
            CampaignState::Running
        ));
    }

    #[test]
    fn stopped_or_evaluated_can_re_edit_to_running() {
        assert!(transition_allowed(
            CampaignState::Stopped,
            CampaignState::Running
        ));
        assert!(transition_allowed(
            CampaignState::Evaluated,
            CampaignState::Running
        ));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!transition_allowed(
            CampaignState::Draft,
            CampaignState::Stopped
        ));
        assert!(!transition_allowed(
            CampaignState::Running,
            CampaignState::Draft
        ));
        assert!(!transition_allowed(
            CampaignState::Evaluated,
            CampaignState::Stopped
        ));
    }

    #[test]
    fn campaign_state_as_str_matches_postgres_enum() {
        assert_eq!(CampaignState::Draft.as_str(), "draft");
        assert_eq!(CampaignState::Running.as_str(), "running");
        assert_eq!(CampaignState::Completed.as_str(), "completed");
        assert_eq!(CampaignState::Evaluated.as_str(), "evaluated");
        assert_eq!(CampaignState::Stopped.as_str(), "stopped");
    }

    #[test]
    fn pair_resolution_state_as_str_matches_postgres_enum() {
        assert_eq!(PairResolutionState::Pending.as_str(), "pending");
        assert_eq!(PairResolutionState::Dispatched.as_str(), "dispatched");
        assert_eq!(PairResolutionState::Reused.as_str(), "reused");
        assert_eq!(PairResolutionState::Succeeded.as_str(), "succeeded");
        assert_eq!(PairResolutionState::Unreachable.as_str(), "unreachable");
        assert_eq!(PairResolutionState::Skipped.as_str(), "skipped");
    }

    #[test]
    fn terminal_states_classified_correctly() {
        for s in [
            PairResolutionState::Reused,
            PairResolutionState::Succeeded,
            PairResolutionState::Unreachable,
            PairResolutionState::Skipped,
        ] {
            assert!(s.is_terminal(), "{s:?} should be terminal");
        }
        for s in [
            PairResolutionState::Pending,
            PairResolutionState::Dispatched,
        ] {
            assert!(!s.is_terminal(), "{s:?} should not be terminal");
        }
    }
}
