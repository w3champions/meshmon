//! Wire shapes for `/api/campaigns/*`.
//!
//! Uses the same snake_case envelope + [`ErrorEnvelope`] shape as the
//! catalogue surface so clients can reuse existing error handling.
//!
//! The DTOs mirror the domain [`CampaignRow`] / [`PairRow`] types one
//! for one; conversions fill operator-facing views without leaking
//! storage-specific fields (e.g. the raw `IpNetwork` of a pair is
//! rendered as a bare IP string).

use super::model::{
    CampaignRow, CampaignState, EvaluationMode, PairResolutionState, PairRow, ProbeProtocol,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// JSON error body returned by every non-2xx `/api/campaigns/*` response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// Snake-case error code; stable across versions.
    pub error: String,
}

/// Wire shape for a single campaign.
///
/// `pair_counts` is populated only by the single-row GET endpoint; list
/// responses leave it empty to avoid an N+1 COUNT fan-out.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CampaignDto {
    /// Primary key.
    pub id: Uuid,
    /// Operator-facing title.
    pub title: String,
    /// Free-form operator notes.
    pub notes: String,
    /// Lifecycle state.
    pub state: CampaignState,
    /// Probe protocol shared by every pair.
    pub protocol: ProbeProtocol,
    /// Probes per dispatched measurement (campaign rounds).
    pub probe_count: i16,
    /// Probes per detail measurement (UI re-runs).
    pub probe_count_detail: i16,
    /// Per-probe timeout in milliseconds.
    pub timeout_ms: i32,
    /// Inter-probe stagger in milliseconds.
    pub probe_stagger_ms: i32,
    /// When `true`, the scheduler forces a fresh measurement instead of
    /// reusing a matching row from the 24 h window.
    pub force_measurement: bool,
    /// Loss-rate threshold (percent) used by the evaluator.
    pub loss_threshold_pct: f32,
    /// Weight applied to RTT stddev by the evaluator.
    pub stddev_weight: f32,
    /// Evaluation strategy.
    pub evaluation_mode: EvaluationMode,
    /// Session principal that created the row; audit-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Row creation timestamp.
    pub created_at: DateTime<Utc>,
    /// When the campaign most recently transitioned to `running`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// When the operator last stopped the campaign, if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<DateTime<Utc>>,
    /// When all pairs reached a terminal state, if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// When the evaluation pass last produced results, if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_at: Option<DateTime<Utc>>,
    /// Per-state pair counts. Empty on list responses; populated on single-row GET.
    #[serde(default)]
    pub pair_counts: Vec<(PairResolutionState, i64)>,
}

impl From<CampaignRow> for CampaignDto {
    fn from(r: CampaignRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            notes: r.notes,
            state: r.state,
            protocol: r.protocol,
            probe_count: r.probe_count,
            probe_count_detail: r.probe_count_detail,
            timeout_ms: r.timeout_ms,
            probe_stagger_ms: r.probe_stagger_ms,
            force_measurement: r.force_measurement,
            loss_threshold_pct: r.loss_threshold_pct,
            stddev_weight: r.stddev_weight,
            evaluation_mode: r.evaluation_mode,
            created_by: r.created_by,
            created_at: r.created_at,
            started_at: r.started_at,
            stopped_at: r.stopped_at,
            completed_at: r.completed_at,
            evaluated_at: r.evaluated_at,
            pair_counts: Vec::new(),
        }
    }
}

/// POST body for `/api/campaigns`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCampaignRequest {
    /// Operator-facing title. Rejected when blank.
    pub title: String,
    /// Optional free-form notes.
    #[serde(default)]
    pub notes: Option<String>,
    /// Probe protocol shared by every pair.
    pub protocol: ProbeProtocol,
    /// Source agent ids that will probe.
    #[serde(default)]
    pub source_agent_ids: Vec<String>,
    /// Destination IPs as strings (e.g. `"10.0.0.1"`, `"2001:db8::1"`).
    #[serde(default)]
    pub destination_ips: Vec<String>,
    /// When `true`, the scheduler ignores the 24 h reuse cache.
    #[serde(default)]
    pub force_measurement: bool,
    /// Optional probe-count override (campaign rounds).
    #[serde(default)]
    pub probe_count: Option<i16>,
    /// Optional detail-probe count override (UI re-runs).
    #[serde(default)]
    pub probe_count_detail: Option<i16>,
    /// Optional per-probe timeout (ms).
    #[serde(default)]
    pub timeout_ms: Option<i32>,
    /// Optional inter-probe stagger (ms).
    #[serde(default)]
    pub probe_stagger_ms: Option<i32>,
    /// Optional loss-rate threshold for the evaluator.
    #[serde(default)]
    pub loss_threshold_pct: Option<f32>,
    /// Optional RTT-stddev weight for the evaluator.
    #[serde(default)]
    pub stddev_weight: Option<f32>,
    /// Optional evaluation strategy.
    #[serde(default)]
    pub evaluation_mode: Option<EvaluationMode>,
}

/// PATCH body for `/api/campaigns/{id}`.
///
/// Absent fields leave the existing column untouched. There is no
/// revert-to-auto surface here because campaigns only persist
/// operator-authored data.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchCampaignRequest {
    /// Replacement title (when present).
    #[serde(default)]
    pub title: Option<String>,
    /// Replacement notes (when present).
    #[serde(default)]
    pub notes: Option<String>,
    /// Replacement loss-rate threshold.
    #[serde(default)]
    pub loss_threshold_pct: Option<f32>,
    /// Replacement RTT-stddev weight.
    #[serde(default)]
    pub stddev_weight: Option<f32>,
    /// Replacement evaluation strategy.
    #[serde(default)]
    pub evaluation_mode: Option<EvaluationMode>,
}

/// A single `(source_agent, destination_ip)` pair identifier used by
/// the edit and force endpoints. `destination_ip` is a bare IP string.
#[derive(Debug, Deserialize, ToSchema)]
pub struct EditPairDto {
    /// Source agent id.
    pub source_agent_id: String,
    /// Destination IP as a bare host string.
    pub destination_ip: String,
}

/// Body for `POST /api/campaigns/{id}/edit`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct EditCampaignRequest {
    /// Pairs to add (or reset to `pending` if they already exist).
    #[serde(default)]
    pub add_pairs: Vec<EditPairDto>,
    /// Pairs to remove entirely.
    #[serde(default)]
    pub remove_pairs: Vec<EditPairDto>,
    /// When `Some(true)`, flips the sticky `force_measurement` flag and
    /// resets every non-delta pair so the whole campaign re-runs.
    #[serde(default)]
    pub force_measurement: Option<bool>,
}

/// Body for `POST /api/campaigns/{id}/force_pair`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ForcePairRequest {
    /// Source agent id of the pair to force.
    pub source_agent_id: String,
    /// Destination IP of the pair to force.
    pub destination_ip: String,
}

/// Response body for `POST /api/campaigns/preview`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewDispatchResponse {
    /// Total number of `(source, destination)` pairs that would be created.
    pub total: i64,
    /// Pairs resolvable from the 24 h reuse window.
    pub reusable: i64,
    /// Pairs the scheduler would dispatch fresh.
    pub fresh: i64,
}

/// Wire shape for a single pair in `GET /api/campaigns/{id}/pairs`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PairDto {
    /// Primary key.
    pub id: i64,
    /// Owning campaign.
    pub campaign_id: Uuid,
    /// Source agent (the prober).
    pub source_agent_id: String,
    /// Destination IP as a bare host string.
    pub destination_ip: String,
    /// Current resolution state.
    pub resolution_state: PairResolutionState,
    /// FK to the `measurements` row once dispatched or reused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_id: Option<i64>,
    /// When the scheduler dispatched the pair to an agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<DateTime<Utc>>,
    /// When the pair reached a terminal state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    /// Number of dispatch attempts to date.
    pub attempt_count: i16,
    /// Last error observed on this pair, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl From<PairRow> for PairDto {
    fn from(r: PairRow) -> Self {
        Self {
            id: r.id,
            campaign_id: r.campaign_id,
            source_agent_id: r.source_agent_id,
            // Render the canonical IP; drop the `/32`/`/128` suffix that
            // `IpNetwork` carries in its default `Display` impl.
            destination_ip: r.destination_ip.ip().to_string(),
            resolution_state: r.resolution_state,
            measurement_id: r.measurement_id,
            dispatched_at: r.dispatched_at,
            settled_at: r.settled_at,
            attempt_count: r.attempt_count,
            last_error: r.last_error,
        }
    }
}

/// Query parameters for `GET /api/campaigns`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CampaignListQuery {
    /// Optional substring match on `title` / `notes`.
    #[serde(default)]
    pub q: Option<String>,
    /// Optional state filter.
    #[serde(default)]
    pub state: Option<CampaignState>,
    /// Optional exact-match filter on `created_by`.
    #[serde(default)]
    pub created_by: Option<String>,
    /// Page size. Clamped to `1..=500` internally; default 100.
    #[serde(default = "default_list_limit")]
    pub limit: i64,
}

/// Default page size for [`CampaignListQuery::limit`].
fn default_list_limit() -> i64 {
    100
}

/// Query parameters for `GET /api/campaigns/{id}/pairs`.
///
/// `state` accepts a comma-separated list of `pair_resolution_state`
/// names (e.g. `?state=pending,dispatched`). Repeat-key form
/// (`?state=pending&state=dispatched`) is NOT supported — axum's
/// default `Query` extractor is `serde_urlencoded`, which does not
/// deserialize repeated keys into `Vec<T>`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PairListQuery {
    /// Comma-separated list of pair resolution states.
    #[serde(default, deserialize_with = "deserialize_csv_states")]
    #[param(style = Form, explode = false)]
    pub state: Vec<PairResolutionState>,
    /// Page size. Clamped to `1..=500` internally; default 500.
    #[serde(default = "default_pair_list_limit")]
    pub limit: i64,
}

/// Default page size for [`PairListQuery::limit`].
fn default_pair_list_limit() -> i64 {
    500
}

/// Parse a comma-separated list of `pair_resolution_state` names into
/// the typed enum vector. Unknown tokens surface as deserialization
/// errors so the caller sees a 400 rather than silently-dropped
/// filters.
fn deserialize_csv_states<'de, D>(de: D) -> Result<Vec<PairResolutionState>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(Vec::new()) };
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            PairResolutionState::deserialize(serde::de::value::StrDeserializer::<
                serde::de::value::Error,
            >::new(t))
            .map_err(serde::de::Error::custom)
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Wire shape for `GET /api/campaigns/{id}/evaluation`.
///
/// Also the exact JSON persisted into `campaign_evaluations.results` —
/// the evaluator serialises [`EvaluationResultsDto`] directly into the
/// JSONB column so the read handler can hand the stored document back
/// to the client without rehydrating through a domain model.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluationDto {
    /// Owning campaign.
    pub campaign_id: Uuid,
    /// When the evaluator produced this result set.
    pub evaluated_at: DateTime<Utc>,
    /// Loss-rate threshold (percent) that was applied.
    pub loss_threshold_pct: f32,
    /// Weight applied to RTT stddev during scoring.
    pub stddev_weight: f32,
    /// Evaluation strategy that produced this result.
    pub evaluation_mode: EvaluationMode,
    /// Number of `(source, destination)` baseline pairs considered.
    pub baseline_pair_count: i32,
    /// Total candidate transit destinations scored.
    pub candidates_total: i32,
    /// Candidate transit destinations that cleared the `qualifies` bar.
    pub candidates_good: i32,
    /// Average end-to-end improvement (ms) across qualifying candidates;
    /// negative means faster. `None` when no candidate qualified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_improvement_ms: Option<f32>,
    /// Full candidate breakdown + unqualified-reason map.
    pub results: EvaluationResultsDto,
}

/// Candidate breakdown persisted in `campaign_evaluations.results`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluationResultsDto {
    /// Per-candidate scoring rows, ordered by composite score.
    pub candidates: Vec<EvaluationCandidateDto>,
    /// Keyed by destination IP (string); value is an explanatory
    /// sentence the UI renders verbatim.
    #[serde(default)]
    pub unqualified_reasons: std::collections::BTreeMap<String, String>,
}

/// Per-candidate transit destination scoring row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluationCandidateDto {
    /// Transit destination IP as a bare host string.
    pub destination_ip: String,
    /// Operator-facing label from the catalogue, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Catalogue city, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Catalogue ISO country code, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    /// Catalogue ASN, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asn: Option<i64>,
    /// Catalogue network operator, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_operator: Option<String>,
    /// True when destination_ip appears in agents.ip. UI renders a
    /// "mesh member — no acquisition needed" badge.
    pub is_mesh_member: bool,
    /// Number of baseline pairs this candidate improved.
    pub pairs_improved: i32,
    /// Number of baseline pairs this candidate was scored against.
    pub pairs_total_considered: i32,
    /// Average improvement (ms) across considered pairs; negative means faster.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_improvement_ms: Option<f32>,
    /// Average compound loss (percent) across transit triples that
    /// cleared the loss gate during scoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_loss_pct: Option<f32>,
    /// Composite score; lower is better.
    pub composite_score: f32,
    /// Per-pair scoring detail for this candidate.
    pub pair_details: Vec<EvaluationPairDetailDto>,
}

/// Per-pair scoring row inside an [`EvaluationCandidateDto`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluationPairDetailDto {
    /// Source agent id of the baseline pair.
    pub source_agent_id: String,
    /// Destination agent id of the baseline pair.
    pub destination_agent_id: String,
    /// Transit destination IP (also a candidate key).
    pub destination_ip: String,
    /// Direct A→B RTT (ms).
    pub direct_rtt_ms: f32,
    /// Direct A→B RTT stddev (ms).
    pub direct_stddev_ms: f32,
    /// Direct A→B observed loss (percent).
    pub direct_loss_pct: f32,
    /// Composed A→X→B transit RTT (ms).
    pub transit_rtt_ms: f32,
    /// Composed A→X→B transit RTT stddev (ms).
    pub transit_stddev_ms: f32,
    /// Composed A→X→B transit observed loss (percent).
    pub transit_loss_pct: f32,
    /// Transit minus direct RTT (ms); negative means faster via transit.
    pub improvement_ms: f32,
    /// Whether this pair cleared the evaluator's qualify predicate.
    pub qualifies: bool,
    /// FK to the `measurements` row covering A→X, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtr_measurement_id_ax: Option<i64>,
    /// FK to the `measurements` row covering X→B, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtr_measurement_id_xb: Option<i64>,
}

/// Which slice of candidates the detail-trigger handler should re-measure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetailScope {
    /// Every candidate in the evaluation result.
    All,
    /// Only candidates the evaluator flagged as qualifying.
    GoodCandidates,
    /// A single pair identified by `DetailRequest::pair`.
    Pair,
}

/// Pair coordinates for [`DetailScope::Pair`] requests.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailPairIdentifier {
    /// Source agent id of the pair to re-measure.
    pub source_agent_id: String,
    /// Destination IP of the pair to re-measure.
    pub destination_ip: String,
}

/// Body for `POST /api/campaigns/{id}/detail`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailRequest {
    /// Which slice of candidates to enqueue for re-measurement.
    pub scope: DetailScope,
    /// Required iff scope == "pair". Rejected on other scopes.
    #[serde(default)]
    pub pair: Option<DetailPairIdentifier>,
}

/// Response body for `POST /api/campaigns/{id}/detail`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DetailResponse {
    /// Number of detail pairs enqueued by this request.
    pub pairs_enqueued: i32,
    /// Campaign state after enqueueing (typically `running`).
    pub campaign_state: CampaignState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_dto_round_trips() {
        let v = EvaluationDto {
            campaign_id: Uuid::nil(),
            evaluated_at: chrono::Utc::now(),
            loss_threshold_pct: 2.0,
            stddev_weight: 1.0,
            evaluation_mode: EvaluationMode::Optimization,
            baseline_pair_count: 24,
            candidates_total: 10,
            candidates_good: 3,
            avg_improvement_ms: Some(-58.0),
            results: EvaluationResultsDto {
                candidates: vec![],
                unqualified_reasons: Default::default(),
            },
        };
        let j = serde_json::to_string(&v).unwrap();
        let r: EvaluationDto = serde_json::from_str(&j).unwrap();
        assert_eq!(r.baseline_pair_count, 24);
    }

    #[test]
    fn detail_scope_snake_case_on_wire() {
        assert_eq!(
            serde_json::to_string(&DetailScope::GoodCandidates).unwrap(),
            "\"good_candidates\"",
        );
    }
}
