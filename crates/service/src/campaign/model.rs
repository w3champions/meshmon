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

/// Provenance of a `measurements` row.
///
/// Distinguishes rows an agent actively measured from rows the evaluator
/// archived out of VictoriaMetrics continuous-mesh data so the agent
/// mesh doesn't have to re-probe itself at evaluation time.
///
/// Mirrors the `measurement_source` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "measurement_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MeasurementSource {
    /// An agent actively probed the destination (campaign dispatch,
    /// detail ping, detail MTR). Default for historical rows.
    ActiveProbe,
    /// The evaluator pulled this baseline from VictoriaMetrics
    /// continuous-mesh metrics and archived the aggregated value.
    ArchivedVmContinuous,
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
