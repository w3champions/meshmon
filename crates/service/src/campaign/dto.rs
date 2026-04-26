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
    CampaignRow, CampaignState, DirectSource, EdgeRouteKind, EndpointKind, EvaluationMode,
    LegSource, PairResolutionState, PairRow, ProbeProtocol,
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
    /// Loss-rate threshold (fraction 0.0–1.0) used by the evaluator.
    pub loss_threshold_ratio: f32,
    /// Weight applied to RTT stddev by the evaluator.
    pub stddev_weight: f32,
    /// Evaluation strategy.
    pub evaluation_mode: EvaluationMode,
    /// Optional eligibility cap on composed transit RTT (ms). When
    /// set, the evaluator drops `(A, X, B)` triples whose
    /// `transit_rtt_ms` exceeds the cap before counter accumulation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_transit_rtt_ms: Option<f64>,
    /// Optional eligibility cap on composed transit RTT stddev (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_transit_stddev_ms: Option<f64>,
    /// Optional storage floor on absolute improvement (ms). Combined
    /// with [`Self::min_improvement_ratio`] under OR semantics; the
    /// evaluator persists a `pair_details` row only when at least one
    /// set knob's threshold is cleared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_improvement_ms: Option<f64>,
    /// Optional storage floor on relative improvement (fraction
    /// 0.0–1.0). See [`Self::min_improvement_ms`] for OR semantics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_improvement_ratio: Option<f64>,
    /// Optional RTT threshold (ms) for edge_candidate useful-route
    /// qualification. `None` means the filter is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub useful_latency_ms: Option<f32>,
    /// Maximum transit hops for edge_candidate mode. Range [0, 2].
    pub max_hops: i16,
    /// VictoriaMetrics look-back window (minutes) for edge_candidate mode.
    /// Range [1, 1440].
    pub vm_lookback_minutes: i32,
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
            loss_threshold_ratio: r.loss_threshold_ratio,
            stddev_weight: r.stddev_weight,
            evaluation_mode: r.evaluation_mode,
            max_transit_rtt_ms: r.max_transit_rtt_ms,
            max_transit_stddev_ms: r.max_transit_stddev_ms,
            min_improvement_ms: r.min_improvement_ms,
            min_improvement_ratio: r.min_improvement_ratio,
            useful_latency_ms: r.useful_latency_ms,
            max_hops: r.max_hops,
            vm_lookback_minutes: r.vm_lookback_minutes,
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
    /// Optional loss-rate threshold for the evaluator (fraction 0.0–1.0).
    #[serde(default)]
    pub loss_threshold_ratio: Option<f32>,
    /// Optional RTT-stddev weight for the evaluator.
    #[serde(default)]
    pub stddev_weight: Option<f32>,
    /// Optional evaluation strategy.
    #[serde(default)]
    pub evaluation_mode: Option<EvaluationMode>,
    /// Optional eligibility cap on composed transit RTT (ms).
    #[serde(default)]
    pub max_transit_rtt_ms: Option<f64>,
    /// Optional eligibility cap on composed transit RTT stddev (ms).
    #[serde(default)]
    pub max_transit_stddev_ms: Option<f64>,
    /// Optional storage floor on absolute improvement (ms).
    #[serde(default)]
    pub min_improvement_ms: Option<f64>,
    /// Optional storage floor on relative improvement (fraction 0.0–1.0).
    #[serde(default)]
    pub min_improvement_ratio: Option<f64>,
    /// Optional RTT threshold (ms) below which a route qualifies as
    /// "useful" in edge_candidate mode.
    #[serde(default)]
    #[schema(example = 80.0)]
    pub useful_latency_ms: Option<f32>,
    /// Maximum number of transit hops for edge_candidate route
    /// enumeration. Range [0, 2]; default 2 when omitted.
    #[serde(default)]
    #[schema(example = 2)]
    pub max_hops: Option<i16>,
    /// Look-back window (minutes) for VictoriaMetrics data in
    /// edge_candidate mode. Range [1, 1440]; default 15 when omitted.
    #[serde(default)]
    #[schema(example = 15)]
    pub vm_lookback_minutes: Option<i32>,
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
    /// Replacement loss-rate threshold (fraction 0.0–1.0).
    #[serde(default)]
    pub loss_threshold_ratio: Option<f32>,
    /// Replacement RTT-stddev weight.
    #[serde(default)]
    pub stddev_weight: Option<f32>,
    /// Replacement evaluation strategy.
    #[serde(default)]
    pub evaluation_mode: Option<EvaluationMode>,
    /// Replacement eligibility cap on composed transit RTT (ms).
    #[serde(default)]
    pub max_transit_rtt_ms: Option<f64>,
    /// Replacement eligibility cap on composed transit RTT stddev (ms).
    #[serde(default)]
    pub max_transit_stddev_ms: Option<f64>,
    /// Replacement storage floor on absolute improvement (ms).
    #[serde(default)]
    pub min_improvement_ms: Option<f64>,
    /// Replacement storage floor on relative improvement (fraction 0.0–1.0).
    #[serde(default)]
    pub min_improvement_ratio: Option<f64>,
    /// Replacement RTT threshold (ms) for edge_candidate useful-route
    /// qualification. `None` leaves the existing value unchanged.
    #[serde(default)]
    pub useful_latency_ms: Option<f32>,
    /// Replacement maximum transit hops for edge_candidate mode. `None`
    /// leaves the existing value unchanged.
    #[serde(default)]
    pub max_hops: Option<i16>,
    /// Replacement VictoriaMetrics look-back window (minutes). `None`
    /// leaves the existing value unchanged.
    #[serde(default)]
    pub vm_lookback_minutes: Option<i32>,
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
    /// Reverse-DNS hostname for the destination IP, when cached.
    /// Absent on cold miss and negative-cached IPs (skip-none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_hostname: Option<String>,
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
            destination_hostname: None,
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
    /// Page size. Clamped to `1..=5000` internally; default 500.
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
/// Assembled from the relational `campaign_evaluations` parent row
/// plus its child tables (`campaign_evaluation_candidates`,
/// `campaign_evaluation_pair_details`,
/// `campaign_evaluation_unqualified_reasons`) by
/// [`crate::campaign::evaluation_repo::latest_evaluation_for_campaign`].
/// The read-path joins in Rust to assemble the wire DTO, which carries
/// a `direct_source` field on every `pair_detail`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvaluationDto {
    /// Owning campaign.
    pub campaign_id: Uuid,
    /// When the evaluator produced this result set.
    pub evaluated_at: DateTime<Utc>,
    /// Loss-rate threshold (fraction 0.0–1.0) that was applied.
    pub loss_threshold_ratio: f32,
    /// Weight applied to RTT stddev during scoring.
    pub stddev_weight: f32,
    /// Evaluation strategy that produced this result.
    pub evaluation_mode: EvaluationMode,
    /// Snapshot of [`CampaignDto::max_transit_rtt_ms`] at `/evaluate`
    /// time. `None` means the eligibility cap was disabled for this
    /// evaluation pass. Persisted on `campaign_evaluations` so each
    /// historical evaluation row carries the guardrails that produced
    /// it, even after later PATCHes change the campaign-level value.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_transit_rtt_ms: Option<f64>,
    /// Snapshot of [`CampaignDto::max_transit_stddev_ms`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_transit_stddev_ms: Option<f64>,
    /// Snapshot of [`CampaignDto::min_improvement_ms`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min_improvement_ms: Option<f64>,
    /// Snapshot of [`CampaignDto::min_improvement_ratio`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min_improvement_ratio: Option<f64>,
    /// Snapshot of [`CampaignDto::useful_latency_ms`] at `/evaluate` time.
    /// `None` on legacy evaluation rows or when the knob was unset.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub useful_latency_ms: Option<f32>,
    /// Snapshot of [`CampaignDto::max_hops`]. `None` on legacy rows.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_hops: Option<i16>,
    /// Snapshot of [`CampaignDto::vm_lookback_minutes`]. `None` on legacy rows.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vm_lookback_minutes: Option<i32>,
    /// Number of `(source, destination)` baseline pairs considered.
    pub baseline_pair_count: i32,
    /// Total candidate transit destinations scored.
    pub candidates_total: i32,
    /// Candidate transit destinations that cleared the `qualifies` bar.
    pub candidates_good: i32,
    /// Average end-to-end improvement (ms) across qualifying candidates.
    /// Same sign convention as
    /// [`EvaluationCandidateDto::avg_improvement_ms`] — positive means
    /// the transit beats the direct A→B baseline. `None` when no
    /// candidate qualified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_improvement_ms: Option<f32>,
    /// Full candidate breakdown + unqualified-reason map.
    pub results: EvaluationResultsDto,
}

/// Candidate breakdown assembled from
/// `campaign_evaluation_candidates` and
/// `campaign_evaluation_unqualified_reasons`.
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
    /// Average improvement (ms) across considered pairs. Defined as
    /// `direct_rtt − transit_rtt − (transit_stddev_penalty − direct_stddev_penalty)`,
    /// so a positive value means the transit candidate is faster than
    /// the direct A→B baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_improvement_ms: Option<f32>,
    /// Average compound loss (fraction 0.0–1.0) across transit triples
    /// that cleared the loss gate during scoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_loss_ratio: Option<f32>,
    /// Composite score `(pairs_improved / baseline_pair_count) ×
    /// avg_improvement_ms`; higher is better. Candidates are returned
    /// in descending composite-score order. `None` for edge_candidate mode
    /// (which uses `coverage_count` ordering instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composite_score: Option<f32>,
    /// Reverse-DNS hostname for the transit destination IP, when cached.
    /// Absent on cold miss and negative-cached IPs (skip-none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Catalogue website URL for the candidate, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// Catalogue notes for the candidate, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Mesh agent id when `is_mesh_member = true`; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Edge_candidate: number of destination agents reachable under the
    /// useful-latency threshold via this candidate. `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_count: Option<i32>,
    /// Edge_candidate: total destination agents evaluated for this
    /// candidate. `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destinations_total: Option<i32>,
    /// Edge_candidate: mean RTT (ms) for routes that are under the
    /// useful-latency threshold. `None` when no route qualifies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_ms_under_t: Option<f32>,
    /// Edge_candidate: coverage-count-weighted mean ping (ms) across
    /// qualifying routes. `None` when coverage is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_weighted_ping_ms: Option<f32>,
    /// Edge_candidate: fraction of qualifying routes that are direct
    /// (zero hops). `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_share: Option<f32>,
    /// Edge_candidate: fraction of qualifying routes that are one-hop.
    /// `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onehop_share: Option<f32>,
    /// Edge_candidate: fraction of qualifying routes that are two-hop.
    /// `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twohop_share: Option<f32>,
    /// Edge_candidate: `true` when at least one leg in the winning route
    /// came from live VM or active-probe data (not symmetric-reuse).
    /// `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_real_x_source_data: Option<bool>,
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
    /// Direct A→B observed loss (fraction 0.0–1.0).
    pub direct_loss_ratio: f32,
    /// Provenance of the direct A→B baseline figures:
    /// [`DirectSource::ActiveProbe`] for rows that came from the
    /// campaign's own `measurements`, or [`DirectSource::VmContinuous`]
    /// when the evaluator pulled the A→B baseline from VictoriaMetrics
    /// continuous-mesh data at `/evaluate` time. Transit legs (A→X,
    /// X→B) are always active-probe; only the direct A→B baseline can
    /// be VM-sourced.
    pub direct_source: DirectSource,
    /// Composed A→X→B transit RTT (ms).
    pub transit_rtt_ms: f32,
    /// Composed A→X→B transit RTT stddev (ms).
    pub transit_stddev_ms: f32,
    /// Composed A→X→B transit observed loss (fraction 0.0–1.0).
    pub transit_loss_ratio: f32,
    /// `direct_rtt − transit_rtt − (transit_stddev_penalty −
    /// direct_stddev_penalty)`; positive means the transit beats the
    /// direct A→B baseline by that many ms after stddev-penalty
    /// adjustment.
    pub improvement_ms: f32,
    /// Whether this pair cleared the evaluator's qualify predicate.
    pub qualifies: bool,
    /// FK to the `measurements` row covering A→X, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtr_measurement_id_ax: Option<i64>,
    /// FK to the `measurements` row covering X→B, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtr_measurement_id_xb: Option<i64>,
    /// Reverse-DNS hostname for the transit destination IP, when cached.
    /// Absent on cold miss and negative-cached IPs (skip-none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_hostname: Option<String>,
    /// `true` when the A→X leg RTT was substituted from the symmetric
    /// X→A measurement (edge_candidate mode). `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ax_was_substituted: Option<bool>,
    /// `true` when the X→B leg RTT was substituted from the symmetric
    /// B→X measurement (edge_candidate mode). `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xb_was_substituted: Option<bool>,
    /// `true` when the direct A→B baseline was VM-sourced and substituted.
    /// `None` for other modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_was_substituted: Option<bool>,
    /// Position of the candidate X in the winning route: `1` = X is first
    /// hop (A→X→B), `2` = X is second hop (A→Y→X→B). `None` for direct
    /// routes or when not applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winning_x_position: Option<u8>,
}

/// Sortable columns for the paginated pair_details endpoint
/// (`GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`).
///
/// The set is closed: every variant maps to a hardcoded SQL fragment in
/// [`crate::campaign::evaluation_repo::latest_pair_details_for_candidate`]
/// so user input never reaches the SQL string. Adding a sort column
/// requires extending the enum, the SQL builder's `match`, and the
/// composite-index migration if a new index is justified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairDetailSortCol {
    /// `improvement_ms` — default sort. Composite-indexed.
    ImprovementMs,
    /// `direct_rtt_ms`.
    DirectRttMs,
    /// `direct_stddev_ms`.
    DirectStddevMs,
    /// `transit_rtt_ms`.
    TransitRttMs,
    /// `transit_stddev_ms`.
    TransitStddevMs,
    /// `direct_loss_ratio`.
    DirectLossRatio,
    /// `transit_loss_ratio`.
    TransitLossRatio,
    /// `source_agent_id`.
    SourceAgentId,
    /// `destination_agent_id`.
    DestinationAgentId,
    /// `qualifies` — boolean column.
    Qualifies,
}

/// Sort direction for [`PairDetailSortCol`]. The composite-PK tiebreak
/// `(source_agent_id, destination_agent_id)` is always ascending; only
/// the leading column flips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairDetailSortDir {
    /// Ascending (smallest values first).
    Asc,
    /// Descending (largest values first). Default for the endpoint.
    Desc,
}

/// Default page size for [`EvaluationPairDetailQuery::limit`].
fn default_pair_detail_limit() -> u32 {
    100
}

fn default_pair_detail_sort() -> PairDetailSortCol {
    PairDetailSortCol::ImprovementMs
}

fn default_pair_detail_dir() -> PairDetailSortDir {
    PairDetailSortDir::Desc
}

/// Query parameters for
/// `GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`.
///
/// Defaults: sort = `improvement_ms`, dir = `desc`, limit = 100.
/// `limit` > 500 surfaces as `400 invalid_filter`. Filter values that
/// are non-finite (`NaN` / `Infinity`) likewise — the handler validates
/// them up front so the SQL plan never sees a garbage threshold.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EvaluationPairDetailQuery {
    /// Sort column. See [`PairDetailSortCol`] for the closed list.
    #[serde(default = "default_pair_detail_sort")]
    pub sort: PairDetailSortCol,
    /// Sort direction. Default `desc`.
    #[serde(default = "default_pair_detail_dir")]
    pub dir: PairDetailSortDir,
    /// Opaque keyset cursor returned by the previous page's
    /// `next_cursor`. Absent on the first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Default 100; cap 500. Zero is allowed (returns an
    /// empty `entries` page, useful for "just give me the total").
    #[serde(default = "default_pair_detail_limit")]
    pub limit: u32,
    /// Runtime filter: minimum `improvement_ms` (inclusive).
    #[serde(default)]
    pub min_improvement_ms: Option<f64>,
    /// Runtime filter: minimum `improvement_ms / direct_rtt_ms` ratio
    /// (inclusive). Rows with `direct_rtt_ms <= 0` auto-pass — mirrors
    /// the I2 evaluator's storage-filter semantics.
    #[serde(default)]
    pub min_improvement_ratio: Option<f64>,
    /// Runtime filter: maximum `transit_rtt_ms` (inclusive).
    #[serde(default)]
    pub max_transit_rtt_ms: Option<f64>,
    /// Runtime filter: maximum `transit_stddev_ms` (inclusive).
    #[serde(default)]
    pub max_transit_stddev_ms: Option<f64>,
    /// Runtime filter: when `Some(true)`, restricts to rows where
    /// `qualifies = true`. `Some(false)` selects unqualifying rows;
    /// `None` (default) is unconstrained.
    #[serde(default)]
    pub qualifies_only: Option<bool>,
}

/// Wire response body for
/// `GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`.
///
/// `total` reflects the runtime filter set but ignores the cursor — it
/// is the size of the full filtered result set across pages, not the
/// remaining-after-cursor count, so a UI status bar can render
/// "showing N of TOTAL" with one number that doesn't drift mid-scroll.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvaluationPairDetailListResponse {
    /// Pair-detail rows for this page.
    pub entries: Vec<EvaluationPairDetailDto>,
    /// Total rows across the full filtered result set, ignoring the
    /// cursor. Renderable as the "of TOTAL" half of a status bar.
    pub total: u64,
    /// Opaque cursor for the next page, or `None` at end-of-result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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

/// Per-(X, B) result for the edge_candidate mode. Returned by
/// `GET /api/campaigns/:id/evaluation/edge_pairs`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct EvaluationEdgePairDetailDto {
    /// Candidate IP being evaluated as an edge (transit) node.
    pub candidate_ip: String,
    /// Destination agent id (the B endpoint).
    pub destination_agent_id: String,
    /// Reverse-DNS hostname for the destination agent IP, when cached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_hostname: Option<String>,
    /// Best-route composed RTT (ms). `None` when `is_unreachable` is
    /// `true` — the wire serializes the field as JSON `null` rather
    /// than an unrepresentable infinity sentinel.
    pub best_route_ms: Option<f32>,
    /// Best-route composed loss fraction (0.0–1.0).
    pub best_route_loss_ratio: f32,
    /// Best-route composed RTT stddev (ms).
    pub best_route_stddev_ms: f32,
    /// Topology of the best route (direct / one_hop / two_hop).
    pub best_route_kind: EdgeRouteKind,
    /// Intermediate hop IPs between candidate and destination (empty for direct).
    pub best_route_intermediaries: Vec<String>,
    /// Ordered per-leg breakdown of the best route.
    pub best_route_legs: Vec<LegDto>,
    /// `true` when the best-route RTT is below the campaign's
    /// `useful_latency_ms` threshold.
    pub qualifies_under_t: bool,
    /// `true` when every probed path to this destination timed out or
    /// had 100 % loss.
    pub is_unreachable: bool,
}

/// Per-leg detail inside an [`EvaluationEdgePairDetailDto`].
///
/// Mirrors the JSONB shape persisted in
/// `campaign_evaluation_edge_pair_details.best_route_legs`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct LegDto {
    /// Kind of the from-endpoint (agent or candidate).
    pub from_kind: EndpointKind,
    /// ID string for the from-endpoint (agent id or IP string).
    pub from_id: String,
    /// Kind of the to-endpoint (agent or candidate).
    pub to_kind: EndpointKind,
    /// ID string for the to-endpoint (agent id or IP string).
    pub to_id: String,
    /// Observed RTT for this leg (ms).
    pub rtt_ms: f32,
    /// Observed RTT stddev for this leg (ms).
    pub stddev_ms: f32,
    /// Observed loss fraction for this leg (0.0–1.0).
    pub loss_ratio: f32,
    /// Data source that produced this leg's metrics.
    pub source: LegSource,
    /// `true` when this leg's direction was inferred from the reverse
    /// measurement (symmetric-reuse substitution).
    pub was_substituted: bool,
    /// FK to the `measurements` row that produced this leg's data,
    /// when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtr_measurement_id: Option<i64>,
}

/// Sortable columns for the edge-pair paginated endpoint
/// (`GET /api/campaigns/{id}/evaluation/edge_pairs`).
///
/// The set is closed — every variant maps to a hardcoded SQL column name
/// in [`crate::campaign::evaluation_repo::latest_evaluation_edge_pairs`] so
/// user input never reaches the SQL string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgePairSortCol {
    /// `best_route_ms` — default sort.
    BestRouteMs,
    /// `best_route_loss_ratio`.
    BestRouteLossRatio,
    /// `best_route_stddev_ms`.
    BestRouteStddevMs,
    /// `best_route_kind` (text column: direct / 1hop / 2hop).
    BestRouteKind,
    /// `qualifies_under_t` — boolean column.
    QualifiesUnderT,
    /// `is_unreachable` — boolean column.
    IsUnreachable,
    /// `candidate_ip` — inet column; lexicographic text ordering.
    CandidateIp,
    /// `destination_agent_id` — text column.
    DestinationAgentId,
}

/// Sort direction for [`EdgePairSortCol`].
///
/// The composite tiebreak `(candidate_ip, destination_agent_id)` is always
/// ascending; only the leading column flips direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgePairSortDir {
    /// Ascending (smallest first). Default for the endpoint.
    Asc,
    /// Descending (largest values first).
    Desc,
}

/// Default page size for [`EdgePairsQuery::limit`].
fn default_edge_pairs_limit() -> u32 {
    100
}

fn default_edge_pair_sort() -> EdgePairSortCol {
    EdgePairSortCol::BestRouteMs
}

fn default_edge_pair_dir() -> EdgePairSortDir {
    EdgePairSortDir::Asc
}

/// Query parameters for `GET /api/campaigns/{id}/evaluation/edge_pairs`.
///
/// Defaults: sort = `best_route_ms`, dir = `asc`, limit = 100.
/// `limit` > 500 surfaces as `400 invalid_filter`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EdgePairsQuery {
    /// Candidate IP filter. When set, restricts results to rows where
    /// `candidate_ip` matches this address (exact match).
    #[serde(default)]
    pub candidate_ip: Option<String>,
    /// When `Some(true)`, restricts to rows where `qualifies_under_t = true`.
    /// `Some(false)` selects non-qualifying rows; `None` (default) is
    /// unconstrained.
    #[serde(default)]
    pub qualifies_only: Option<bool>,
    /// When `Some(true)`, restricts to rows where `is_unreachable = false`.
    /// Useful for displaying only reachable routes.
    #[serde(default)]
    pub reachable_only: Option<bool>,
    /// Sort column. Default `best_route_ms`.
    #[serde(default = "default_edge_pair_sort")]
    #[param(inline)]
    pub sort: EdgePairSortCol,
    /// Sort direction. Default `asc` (ascending RTT = best routes first).
    #[serde(default = "default_edge_pair_dir")]
    #[param(inline)]
    pub dir: EdgePairSortDir,
    /// Opaque keyset cursor returned by the previous page's `next_cursor`.
    /// Absent on the first page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Default 100; cap 500. Zero is allowed.
    #[serde(default = "default_edge_pairs_limit")]
    pub limit: u32,
}

/// Opaque keyset-pagination cursor for the edge-pairs endpoint.
///
/// Encoding is JSON inside base64 (URL-safe, no padding) — clients must
/// treat the byte stream as opaque.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgePairCursor {
    /// Sort column the page-1 caller requested.
    pub sort_col: EdgePairSortCol,
    /// Value of `sort_col` on the last row of the previous page.
    pub sort_value: crate::campaign::cursor::SortValue,
    /// Tiebreak: candidate IP (as text).
    pub candidate_ip: String,
    /// Tiebreak: destination agent id.
    pub destination_agent_id: String,
}

impl EdgePairCursor {
    /// Encode to the opaque `cursor` string.
    pub fn encode(&self) -> String {
        use base64::Engine as _;
        let json = serde_json::to_vec(self).expect("EdgePairCursor JSON serialization is total");
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    /// Decode and validate against the expected sort column.
    pub fn decode(
        raw: &str,
        expected: EdgePairSortCol,
    ) -> Result<Self, crate::campaign::cursor::CursorError> {
        use crate::campaign::cursor::CursorError;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|e| CursorError::Decode(format!("base64: {e}")))?;
        let cursor: EdgePairCursor = serde_json::from_slice(&bytes)
            .map_err(|e| CursorError::Decode(format!("json: {e}")))?;
        if cursor.sort_col != expected {
            return Err(CursorError::SortMismatch);
        }
        Ok(cursor)
    }
}

/// Paginated response for `GET /api/campaigns/:id/evaluation/edge_pairs`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EdgePairsListResponse {
    /// Entries for this page.
    pub entries: Vec<EvaluationEdgePairDetailDto>,
    /// Total matching rows across all pages (ignoring the cursor).
    pub total: i64,
    /// Opaque cursor for the next page, or `None` at end-of-result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_dto_round_trips() {
        let v = EvaluationDto {
            campaign_id: Uuid::nil(),
            evaluated_at: chrono::Utc::now(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            evaluation_mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: Some(200.0),
            max_transit_stddev_ms: None,
            min_improvement_ms: Some(5.0),
            min_improvement_ratio: Some(0.1),
            useful_latency_ms: None,
            max_hops: None,
            vm_lookback_minutes: None,
            baseline_pair_count: 24,
            candidates_total: 10,
            candidates_good: 3,
            avg_improvement_ms: Some(58.0),
            results: EvaluationResultsDto {
                candidates: vec![],
                unqualified_reasons: Default::default(),
            },
        };
        let j = serde_json::to_string(&v).unwrap();
        let r: EvaluationDto = serde_json::from_str(&j).unwrap();
        assert_eq!(r.baseline_pair_count, 24);
        assert_eq!(r.max_transit_rtt_ms, Some(200.0));
        assert_eq!(r.max_transit_stddev_ms, None);
        assert_eq!(r.min_improvement_ms, Some(5.0));
        assert_eq!(r.min_improvement_ratio, Some(0.1));
    }

    #[test]
    fn detail_scope_snake_case_on_wire() {
        assert_eq!(
            serde_json::to_string(&DetailScope::GoodCandidates).unwrap(),
            "\"good_candidates\"",
        );
    }
}
