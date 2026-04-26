//! Pure-function campaign evaluator core. No DB, no IO.
//!
//! # Entry point
//!
//! [`evaluate`] is the single public entry point. Callers build
//! [`EvaluationInputs`] from DB queries and pass the returned
//! [`EvaluationOutputs`] to
//! [`crate::campaign::evaluation_repo::persist_evaluation`], which fans the
//! structure across the relational child tables.
//!
//! # Three-arm dispatch
//!
//! [`evaluate`] dispatches on [`crate::campaign::model::EvaluationMode`]:
//!
//! - **`diversity`** / **`optimization`** — both enter the triple-scoring
//!   path (`evaluate_triple`) and return [`EvaluationOutputs::Triple`].
//!   The evaluator scores every candidate IP (X) as a transit between two
//!   mesh agents (A→X→B). Diversity qualifies X when it beats the direct
//!   A→B path; optimization additionally requires X to beat every
//!   alternative mesh transit.
//! - **`edge_candidate`** — dispatched to
//!   [`edge_candidate::evaluate`][crate::campaign::eval::edge_candidate]
//!   before the triple loop runs. Returns
//!   [`EvaluationOutputs::EdgeCandidate`]. The evaluator scores each
//!   candidate IP (X) by its best route to every source mesh agent (A),
//!   rather than as a transit between two agents.
//!
//! # Leg-construction priority order
//!
//! The [`legs::LegLookup`] resolver applies the following priority for every
//! directed leg (from → to):
//!
//! 1. **Forward measurement with loss < 1.0** — used directly;
//!    `was_substituted = false`.
//! 2. **Reverse measurement (symmetry fallback) with loss < 1.0** — used as
//!    a substitute; `was_substituted = true`. The `source` field records the
//!    underlying measurement's provenance (`ActiveProbe` or `VmContinuous`)
//!    regardless of direction.
//! 3. **Both directions present with loss == 1.0** — leg is **broken**;
//!    any route containing this leg is discarded.
//! 4. **Neither direction has data** — leg is **missing**; route discarded.
//!
//! See [`legs::LegLookup::lookup`] for the full truth table.
//!
//! # Sub-modules
//!
//! - [`edge_candidate`] — per-(X, A) evaluator for EdgeCandidate mode.
//! - [`legs`] — `LegLookup` + `LegMeasurement`; leg resolution with
//!   symmetry-fallback.
//! - [`routes`] — `enumerate_routes`; multi-hop route composition from an
//!   endpoint pool.

pub(crate) mod edge_candidate;
pub(crate) mod legs;
pub(crate) mod routes;

pub use edge_candidate::{EdgeCandidateOutputs, EdgeCandidateRow, EdgePairRow};

use crate::campaign::dto::{EvaluationCandidateDto, EvaluationPairDetailDto, EvaluationResultsDto};
use crate::campaign::eval::legs::{LegLookup, LegLookupResult};
use crate::campaign::model::{DirectSource, Endpoint, EvaluationMode};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

/// One row attributed to the campaign for evaluation purposes.
///
/// Two provenances share this shape:
///
/// * `DirectSource::ActiveProbe` — a `measurements` row settled by the
///   campaign dispatcher, joined in via `campaign_pairs.measurement_id`.
/// * `DirectSource::VmContinuous` — a synthetic row the `/evaluate`
///   handler built from VictoriaMetrics continuous-mesh baselines for
///   agent→agent pairs the active-probe data did not cover. Never
///   persisted to `measurements`.
#[derive(Debug, Clone)]
pub struct AttributedMeasurement {
    /// Agent id of the probing source (`measurements.source_agent_id`).
    pub source_agent_id: String,
    /// Probed destination IP (`measurements.destination_ip`).
    pub destination_ip: IpAddr,
    /// Mean RTT in milliseconds; `None` when no reply landed.
    pub latency_avg_ms: Option<f32>,
    /// RTT stddev in milliseconds; `None` when no reply landed.
    pub latency_stddev_ms: Option<f32>,
    /// Observed loss fraction on this leg (0.0–1.0).
    pub loss_ratio: f32,
    /// FK into `measurements.id` for the MTR-bearing row, when available.
    /// `None` for VM-synthesized rows — they have no `measurements.id`.
    pub mtr_measurement_id: Option<i64>,
    /// Provenance of this row. Stamped onto
    /// [`EvaluationPairDetailDto::direct_source`] whenever the evaluator
    /// uses this row as the direct A→B baseline.
    pub direct_source: DirectSource,
}

/// Agent identity row used to build the baseline pair list.
#[derive(Debug, Clone)]
pub struct AgentRow {
    /// Agent id (`agents.agent_id`).
    pub agent_id: String,
    /// Agent's canonical IP (`agents.ip`), the key both sides key into.
    pub ip: IpAddr,
    /// Optional reverse-DNS hostname for the agent. Stamped onto
    /// `EdgePairRow.destination_hostname` in the EdgeCandidate arm so
    /// the wire DTO can render a friendly destination label without a
    /// second round-trip through the hostname cache.
    ///
    /// Diversity / Optimization don't read this field today (their
    /// hostname stamping happens via the post-process
    /// `bulk_hostnames_and_enqueue` path inside `handlers.rs`).
    pub hostname: Option<String>,
}

/// Lookup side-table: IP → catalogue enrichment. Agents contribute
/// their entry too (via `agents_with_catalogue`), so a non-mesh IP is
/// one that is NOT in `agents_by_ip` but may still be in `enrichment`.
#[derive(Debug, Clone, Default)]
pub struct CatalogueLookup {
    /// Operator-facing label from `ip_catalogue.display_name`.
    pub display_name: Option<String>,
    /// Catalogue city, when known.
    pub city: Option<String>,
    /// Catalogue ISO country code, when known.
    pub country_code: Option<String>,
    /// Catalogue ASN, when known.
    pub asn: Option<i64>,
    /// Catalogue network operator, when known.
    pub network_operator: Option<String>,
    /// Catalogue website URL, when known. Populated by the
    /// EdgeCandidate handler from `ip_catalogue.website`. Diversity /
    /// Optimization don't surface this field today.
    pub website: Option<String>,
    /// Catalogue free-text notes, when present. Same provenance as
    /// `website`; consumed by the EdgeCandidate arm only.
    pub notes: Option<String>,
    /// Reverse-DNS hostname for the IP, when cached. Populated by the
    /// EdgeCandidate handler so the evaluator can stamp it onto
    /// non-mesh `EdgeCandidateRow`s without a second round-trip.
    pub hostname: Option<String>,
}

/// Full input bundle for one evaluator pass.
#[derive(Debug, Clone)]
pub struct EvaluationInputs {
    /// All campaign-kind measurements the evaluator should consider.
    pub measurements: Vec<AttributedMeasurement>,
    /// Agent roster the campaign pulled its sources from.
    pub agents: Vec<AgentRow>,
    /// Candidate destination IPs (the X set). For Diversity /
    /// Optimization this is derived from the measurement set; for
    /// EdgeCandidate it comes straight from
    /// `measurement_campaigns.destination_ips` so X-IPs without any
    /// outgoing measurement still appear as a candidate row (with
    /// unreachable pairs).
    pub candidate_ips: Vec<IpAddr>,
    /// IP → catalogue enrichment for candidate rendering.
    pub enrichment: HashMap<IpAddr, CatalogueLookup>,
    /// Loss ceiling (fraction); triples exceeding this never qualify.
    pub loss_threshold_ratio: f32,
    /// Weight applied to RTT stddev when computing the improvement score.
    pub stddev_weight: f32,
    /// Diversity vs. Optimization vs. EdgeCandidate arbitration (spec
    /// §2.4 + EdgeCandidate spec §4).
    pub mode: EvaluationMode,
    /// Optional eligibility cap on composed transit RTT (ms). The
    /// evaluator drops `(A, X, B)` triples whose `transit_rtt_ms`
    /// exceeds the cap before counter accumulation.
    pub max_transit_rtt_ms: Option<f64>,
    /// Optional eligibility cap on composed transit RTT stddev (ms).
    pub max_transit_stddev_ms: Option<f64>,
    /// Optional storage floor on absolute improvement (ms). Combined
    /// with [`Self::min_improvement_ratio`] under OR semantics.
    pub min_improvement_ms: Option<f64>,
    /// Optional storage floor on relative improvement (fraction
    /// 0.0–1.0). Combined with [`Self::min_improvement_ms`] under OR
    /// semantics.
    pub min_improvement_ratio: Option<f64>,
    /// Useful-latency threshold T (ms) for EdgeCandidate qualification.
    /// Required when `mode == EdgeCandidate` (the API validation layer
    /// rejects requests where it is `None`); ignored by other modes.
    pub useful_latency_ms: Option<f32>,
    /// Maximum transit hops for route enumeration in EdgeCandidate
    /// mode. Diversity / Optimization don't enumerate multi-hop routes
    /// — they pass the validation default (`1`) but never read this
    /// field. Validated upstream into `[0, 2]`.
    pub max_hops: i16,
}

/// Top-level evaluator output. Tagged on the evaluation mode that
/// produced it: Diversity / Optimization both lower into the same
/// triple-scoring shape ([`TripleEvaluationOutputs`]); EdgeCandidate
/// uses its own per-(X, B) shape ([`EdgeCandidateOutputs`], in
/// [`crate::campaign::eval::edge_candidate`]).
///
/// Splitting the wire shape per mode keeps each arm's persistence
/// path narrow (the EdgeCandidate writer doesn't need
/// triple-shaped fields, and vice versa) and lets the read-path
/// dispatch on the variant rather than juggling a giant
/// `Option`-saturated struct.
#[derive(Debug, Clone)]
pub enum EvaluationOutputs {
    /// Diversity / Optimization output (T44/T48 shape).
    Triple(TripleEvaluationOutputs),
    /// EdgeCandidate output (T56). See
    /// [`crate::campaign::eval::edge_candidate`].
    EdgeCandidate(EdgeCandidateOutputs),
}

/// Diversity / Optimization evaluator output: summary counters plus
/// the wire-bound DTO and the per-candidate pair-detail rows persisted
/// into `campaign_evaluation_pair_details`.
///
/// `pair_details_by_candidate` is intentionally NOT nested inside
/// [`EvaluationCandidateDto`]: that DTO is the wire shape served by
/// `GET /api/campaigns/{id}/evaluation`, and pair-detail rows are only
/// reachable on the wire via the paginated
/// `…/candidates/{ip}/pair_details` endpoint. Keeping pair-detail rows
/// in a sidecar `Vec` lets the evaluator hand them straight to
/// `insert_evaluation` for persistence without round-tripping them
/// through the wire DTO.
#[derive(Debug, Clone)]
pub struct TripleEvaluationOutputs {
    /// Total baseline (A, B) agent pairs the evaluator scored against.
    pub baseline_pair_count: i32,
    /// Count of candidate transit destinations considered.
    pub candidates_total: i32,
    /// Count of candidates with at least one qualifying baseline pair.
    pub candidates_good: i32,
    /// Mean improvement (ms) across all qualifying pair details.
    pub avg_improvement_ms: Option<f32>,
    /// Serialisable results payload. Persisted by
    /// [`crate::campaign::evaluation_repo::insert_evaluation`] into the
    /// `campaign_evaluation_candidates` and
    /// `campaign_evaluation_unqualified_reasons` child tables.
    pub results: EvaluationResultsDto,
    /// Per-candidate pair-detail rows in the same order as
    /// [`Self::results`].`candidates`. Each entry's
    /// [`PairDetailsForCandidate::destination_ip`] matches the
    /// candidate at the same index. Persisted by
    /// [`crate::campaign::evaluation_repo::insert_evaluation`] into
    /// `campaign_evaluation_pair_details`.
    pub pair_details_by_candidate: Vec<PairDetailsForCandidate>,
}

/// Sidecar bundle of per-candidate persistence rows. Threads the
/// evaluator's per-pair scoring through to `insert_evaluation` without
/// nesting it in the wire DTO. The candidate at the same index in
/// `EvaluationOutputs.results.candidates` owns `destination_ip` as a
/// string; this struct repeats the parsed IP so persistence can FK off
/// it directly.
#[derive(Debug, Clone)]
pub struct PairDetailsForCandidate {
    /// Transit destination IP — same value as the matching candidate's
    /// `destination_ip`, parsed.
    pub destination_ip: IpAddr,
    /// Pair-detail rows the storage filter let through for this
    /// candidate. Used for the paginated drilldown surface and the
    /// candidate-level loss aggregate.
    pub pair_details: Vec<EvaluationPairDetailDto>,
    /// Every `(source_agent_id, destination_agent_id)` pair whose
    /// triple qualified for this candidate, captured BEFORE the storage
    /// filter runs. Persisted to `campaign_evaluation_qualifying_legs`
    /// so `Detail: good candidates` expands the full qualifying set
    /// regardless of how aggressively the storage floors prune
    /// `pair_details`.
    pub qualifying_legs: Vec<(String, String)>,
}

/// Errors surfaced by [`evaluate`].
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// No agent→agent baseline pair was present in `measurements`; the
    /// evaluator has nothing to score against.
    #[error("no agent-to-agent baseline measurements available")]
    NoBaseline,
}

/// Build the `(source_agent_id, destination_ip) → AttributedMeasurement`
/// lookup the rest of the evaluator keys into.
///
/// Last-write-wins on the pair key: callers place higher-priority
/// sources LATER in `measurements`. The T54-03 `/evaluate` handler
/// relies on this — it prepends VM-synthesized rows and appends active-
/// probe rows so an active-probe measurement always overwrites a VM
/// baseline for the same `(source_agent_id, destination_ip)` tuple. See
/// `campaign::handlers::fetch_and_synthesize_vm_baselines` for the
/// caller-side contract; any refactor here (pre-sort, dedupe, filter)
/// must preserve this ordering invariant or move the tie-break logic to
/// the caller.
fn build_pair_lookup(
    measurements: &[AttributedMeasurement],
) -> HashMap<(String, IpAddr), &AttributedMeasurement> {
    let mut by_pair: HashMap<(String, IpAddr), &AttributedMeasurement> = HashMap::new();
    for meas in measurements {
        by_pair.insert((meas.source_agent_id.clone(), meas.destination_ip), meas);
    }
    by_pair
}

/// Enumerate baseline `(A, B)` agent pairs that have a usable A→B
/// measurement. Returns `(a_id, b_ip, b_id, direct_was_substituted)`.
///
/// `direct_was_substituted` is `true` when the A→B RTT was resolved from the
/// reverse B→A measurement (per LegLookup §3.1 symmetry substitution). This
/// flag is propagated onto pair-detail rows so the wire DTO can surface it.
///
/// A row with no RTT (e.g. a 100 %-loss "success") cannot participate
/// in scoring downstream, so counting it would inflate
/// `baseline_pair_count` and suppress the `NoBaseline → 422` error
/// when every observed baseline is RTT-less.
fn collect_baselines(
    agents: &[AgentRow],
    by_pair: &HashMap<(String, IpAddr), &AttributedMeasurement>,
    leg_lookup: &LegLookup<'_>,
) -> Vec<(String, IpAddr, String, bool)> {
    let mut baselines: Vec<(String, IpAddr, String, bool)> = Vec::new();
    for a in agents {
        for b in agents {
            if a.agent_id == b.agent_id {
                continue;
            }
            if by_pair
                .get(&(a.agent_id.clone(), b.ip))
                .is_some_and(|m| m.latency_avg_ms.is_some())
            {
                // Determine substitution flag for the direct A→B leg.
                let a_endpoint = Endpoint::Agent {
                    id: a.agent_id.clone(),
                };
                let b_endpoint = Endpoint::CandidateIp { ip: b.ip };
                let direct_was_substituted =
                    matches!(leg_lookup.lookup(&a_endpoint, &b_endpoint),
                        LegLookupResult::Found { was_substituted: true, .. });
                baselines.push((
                    a.agent_id.clone(),
                    b.ip,
                    b.agent_id.clone(),
                    direct_was_substituted,
                ));
            }
        }
    }
    baselines
}


/// Decide whether a scored triple's pair-detail row is persisted.
///
/// Implements the OR semantics of architecture #3 across the
/// `min_improvement_ms` and `min_improvement_ratio` knobs:
///
/// * Both `None` ⇒ store (gate is open; pre-T55 behaviour).
/// * At least one set ⇒ row is stored when at least one *set* knob's
///   predicate passes; an unset knob auto-fails so the OR collapses to
///   "either set knob passes". Negative thresholds round-trip
///   end-to-end (no clamping at 0).
/// * Ratio gate divides by `direct_rtt_ms`; on `direct_rtt_ms ≤ 0` the
///   gate auto-passes to avoid div-by-zero / negative-baseline
///   pathologies.
fn passes_storage_filter(
    improvement_ms: f32,
    direct_rtt_ms: f32,
    min_imp_ms: Option<f64>,
    min_imp_ratio: Option<f64>,
) -> bool {
    match (min_imp_ms, min_imp_ratio) {
        (None, None) => true,
        (ms_thresh, ratio_thresh) => {
            let ratio_pass = match ratio_thresh {
                None => false,
                Some(_) if (direct_rtt_ms as f64) <= 0.0 => true,
                Some(t) => (improvement_ms as f64 / direct_rtt_ms as f64) >= t,
            };
            let ms_pass = match ms_thresh {
                None => false,
                Some(t) => (improvement_ms as f64) >= t,
            };
            ms_pass || ratio_pass
        }
    }
}


/// Finalize a candidate's accumulated counters + per-triple lists into
/// an [`EvaluationCandidateDto`].
///
/// Computes the candidate-level `avg_improvement_ms`, `avg_loss_ratio`,
/// and `composite_score` from the pre-storage-filter accumulators (see
/// architecture #2), and stamps catalogue enrichment + the
/// `is_mesh_member` flag from the caller-provided lookups.
#[allow(clippy::too_many_arguments)]
fn build_candidate_row(
    x_ip: IpAddr,
    pairs_improved: i32,
    pairs_total_considered: i32,
    improvements: &[f32],
    compound_losses: &[f32],
    total_baseline: i32,
    enrichment: &HashMap<IpAddr, CatalogueLookup>,
    agent_by_ip: &HashMap<IpAddr, String>,
) -> EvaluationCandidateDto {
    let avg_improvement_ms = if improvements.is_empty() {
        None
    } else {
        Some(improvements.iter().sum::<f32>() / improvements.len() as f32)
    };
    let avg_loss_ratio = if compound_losses.is_empty() {
        None
    } else {
        Some(compound_losses.iter().sum::<f32>() / compound_losses.len() as f32)
    };
    let composite_score =
        (pairs_improved as f32 / total_baseline as f32) * avg_improvement_ms.unwrap_or(0.0);

    let enr = enrichment.get(&x_ip).cloned().unwrap_or_default();
    EvaluationCandidateDto {
        destination_ip: x_ip.to_string(),
        display_name: enr.display_name,
        city: enr.city,
        country_code: enr.country_code,
        asn: enr.asn,
        network_operator: enr.network_operator,
        is_mesh_member: agent_by_ip.contains_key(&x_ip),
        pairs_improved,
        // Counters reflect the post-eligibility set (after the
        // route-enumeration cap check (max_transit_rtt_ms /
        // max_transit_stddev_ms)) but include rows the storage filter
        // dropped — see architecture #2 (eligibility vs storage).
        pairs_total_considered,
        avg_improvement_ms,
        avg_loss_ratio,
        composite_score: Some(composite_score),
        hostname: None,
        website: None,
        notes: None,
        agent_id: None,
        coverage_count: None,
        destinations_total: None,
        mean_ms_under_t: None,
        coverage_weighted_ping_ms: None,
        direct_share: None,
        onehop_share: None,
        twohop_share: None,
        has_real_x_source_data: None,
    }
}

/// Sort comparator for the per-candidate result list.
///
/// Primary key: descending `composite_score` (higher = better).
/// Secondary key (tiebreaker): parsed [`IpAddr`] order — lexicographic
/// string compare would rank `"10.0.0.2" < "9.9.9.9"`, which operator-
/// facing tools render as surprising output; parsed-IP compare
/// preserves numeric intuition. Falls back to string compare when
/// either side fails to parse so the sort stays total.
fn candidate_order(a: &EvaluationCandidateDto, b: &EvaluationCandidateDto) -> std::cmp::Ordering {
    b.composite_score
        .unwrap_or(0.0)
        .partial_cmp(&a.composite_score.unwrap_or(0.0))
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            let a_ip = a.destination_ip.parse::<IpAddr>().ok();
            let b_ip = b.destination_ip.parse::<IpAddr>().ok();
            match (a_ip, b_ip) {
                (Some(a_ip), Some(b_ip)) => a_ip.cmp(&b_ip),
                _ => a.destination_ip.cmp(&b.destination_ip),
            }
        })
}

/// Extract AX / XB leg metadata from the winning X-containing route for
/// pair-detail stamping.
///
/// Returns `(ax_was_substituted, xb_was_substituted, mtr_id_ax, mtr_id_xb)`.
///
/// For a 1-hop route `A→X→B`: leg[0] is the AX leg, leg[1] is the XB leg.
/// For a 2-hop route `A→X→Y→B`: leg[0] = AX (A→X), leg[1] = X→Y, leg[2] = Y→B
///   — use leg[0] as the AX proxy and leg[2] as the XB proxy (best boundary legs).
/// For a 2-hop route `A→Y→X→B`: leg[0] = A→Y, leg[1] = Y→X, leg[2] = X→B
///   — use leg[1] (Y→X) as the AX proxy. Rationale: leg[1] is the closest
///   measured leg to X on the A side; using leg[0] (A→Y) would describe the
///   A→Y path which is unrelated to X's reachability characteristics.
///
/// Falls back to `(false, false, None, None)` when intermediary ordering
/// can't be determined (defensive: shouldn't happen on valid routes).
fn extract_ax_xb_meta(
    route: &routes::ComposedRoute,
    x_endpoint: &Endpoint,
) -> (bool, bool, Option<i64>, Option<i64>) {
    let legs = &route.legs;
    match route.intermediaries.as_slice() {
        // 1-hop: A→X→B — leg[0]=AX, leg[1]=XB
        [only] if only == x_endpoint => {
            let ax = legs.first();
            let xb = legs.get(1);
            (
                ax.map(|l| l.was_substituted).unwrap_or(false),
                xb.map(|l| l.was_substituted).unwrap_or(false),
                ax.and_then(|l| l.mtr_measurement_id),
                xb.and_then(|l| l.mtr_measurement_id),
            )
        }
        // 2-hop with X first: A→X→Y→B — leg[0]=AX, leg[1]=X→Y, leg[2]=Y→B
        [first, _second] if first == x_endpoint => {
            let ax = legs.first();
            let xb = legs.last();
            (
                ax.map(|l| l.was_substituted).unwrap_or(false),
                xb.map(|l| l.was_substituted).unwrap_or(false),
                ax.and_then(|l| l.mtr_measurement_id),
                xb.and_then(|l| l.mtr_measurement_id),
            )
        }
        // 2-hop with X second: A→Y→X→B — leg[0]=A→Y, leg[1]=Y→X, leg[2]=XB
        [_first, second] if second == x_endpoint => {
            let ax = legs.get(1); // Y→X leg as AX proxy
            let xb = legs.last();
            (
                ax.map(|l| l.was_substituted).unwrap_or(false),
                xb.map(|l| l.was_substituted).unwrap_or(false),
                ax.and_then(|l| l.mtr_measurement_id),
                xb.and_then(|l| l.mtr_measurement_id),
            )
        }
        // Direct or unexpected shape — defensive fallback.
        _ => (false, false, None, None),
    }
}

/// Optimization-mode qualify predicate using [`routes::enumerate_routes`].
///
/// X qualifies when no non-X route from A to B is at least as good as the
/// X route (measured by penalised RTT). "At least as good" uses `>=` so an
/// exact tie collapses to "non-X preferred", per spec §2.4.
///
/// Non-X routes use other mesh agents as transit intermediaries. Their IPs
/// are expressed as `CandidateIp` endpoints so the `LegLookup` can resolve
/// legs via its `(Agent(src_id), Ip(dst_ip))` index and symmetry substitution
/// (e.g. `B→Y` used as `Y→B`). This matches the symmetry convention the
/// original scalar optimization predicate relied on.
///
/// Returns `false` immediately when `improvement_ms ≤ 0`.
#[allow(clippy::too_many_arguments)]
fn qualifies_under_optimization_v2(
    a_endpoint: &Endpoint,
    b_endpoint: &Endpoint,
    x_ip: IpAddr,
    transit_score: f32,
    improvement_ms: f32,
    leg_lookup: &LegLookup<'_>,
    agents: &[AgentRow],
    a_id: &str,
    b_id: &str,
    max_hops: u8,
    max_rtt_cap: Option<f64>,
    max_sd_cap: Option<f64>,
    stddev_weight: f32,
) -> bool {
    if improvement_ms <= 0.0 {
        return false;
    }
    // Build non-X alternative pool: other mesh agents (not A, not B, not X)
    // expressed as CandidateIp endpoints so LegLookup can resolve their legs
    // via the `(Agent(src_id), Ip(agent_ip))` stored measurements.
    let non_x_pool: Vec<Endpoint> = agents
        .iter()
        .filter(|a| a.agent_id != a_id && a.agent_id != b_id && a.ip != x_ip)
        .map(|a| Endpoint::CandidateIp { ip: a.ip })
        .collect();

    let alt_routes = routes::enumerate_routes(
        leg_lookup,
        a_endpoint,
        b_endpoint,
        &non_x_pool,
        max_hops,
        max_rtt_cap,
        max_sd_cap,
        stddev_weight,
    );

    for r in &alt_routes {
        let alt_score = r.rtt_ms + stddev_weight * r.stddev_ms;
        // Tiebreaker: reject X when any non-X alternative ties or beats X.
        if transit_score >= alt_score {
            return false;
        }
    }
    true
}

/// Derive the headline `avg_improvement_ms` from per-candidate
/// aggregates rather than re-iterating `pair_details`.
///
/// With T55 storage filters in play, `pair_details` is a *subset* of
/// the qualifying triples (a row can pass eligibility + qualify but be
/// dropped by the storage gate); re-iterating it would silently exclude
/// those triples from the headline. The candidate-level
/// `(pairs_improved, avg_improvement_ms)` aggregates were computed
/// pre-storage-filter — see architecture #2 (eligibility vs storage)
/// and architecture #4 (monotonic composition) — and remain the source
/// of truth for "how good is this candidate?".
fn aggregate_headline_avg(candidate_rows: &[EvaluationCandidateDto]) -> Option<f32> {
    let mut sum_total: f64 = 0.0;
    let mut count_total: i64 = 0;
    for c in candidate_rows {
        if let Some(avg) = c.avg_improvement_ms {
            sum_total += avg as f64 * c.pairs_improved as f64;
            count_total += c.pairs_improved as i64;
        }
    }
    if count_total == 0 {
        None
    } else {
        Some((sum_total / count_total as f64) as f32)
    }
}

/// Score every candidate transit destination against the baseline
/// agent→agent pairs and produce a persistable evaluation payload.
///
/// Pure function — no DB, no IO. See the crate-level README and
/// `docs/superpowers/specs/2026-04-19-campaigns-04-evaluation-and-results.md`
/// §2.4 for the Diversity / Optimization formulae;
/// `docs/superpowers/specs/2026-04-26-campaigns-edge-candidate-evaluation-mode-design.md`
/// §4 for the EdgeCandidate algorithm.
///
/// Dispatch on `mode`: Diversity / Optimization both fall through to
/// the triple-scoring path that returns
/// [`EvaluationOutputs::Triple`]; EdgeCandidate hands off to
/// [`edge_candidate::evaluate`] which returns
/// [`EvaluationOutputs::EdgeCandidate`].
pub fn evaluate(inputs: EvaluationInputs) -> Result<EvaluationOutputs, EvalError> {
    if inputs.mode == EvaluationMode::EdgeCandidate {
        return Ok(edge_candidate::evaluate(inputs));
    }
    evaluate_triple(inputs).map(EvaluationOutputs::Triple)
}

/// Triple-scoring evaluator path (Diversity / Optimization). Pure
/// function; the `evaluate` dispatcher wraps the result in
/// [`EvaluationOutputs::Triple`].
fn evaluate_triple(inputs: EvaluationInputs) -> Result<TripleEvaluationOutputs, EvalError> {
    let agent_by_ip: HashMap<IpAddr, String> = inputs
        .agents
        .iter()
        .map(|a| (a.ip, a.agent_id.clone()))
        .collect();
    let agent_by_id: HashMap<String, IpAddr> = inputs
        .agents
        .iter()
        .map(|a| (a.agent_id.clone(), a.ip))
        .collect();

    let by_pair = build_pair_lookup(&inputs.measurements);
    let leg_lookup = LegLookup::build(&inputs.measurements);
    let baselines = collect_baselines(&inputs.agents, &by_pair, &leg_lookup);
    if baselines.is_empty() {
        return Err(EvalError::NoBaseline);
    }

    let candidates: HashSet<IpAddr> = inputs
        .measurements
        .iter()
        .map(|m| m.destination_ip)
        .collect();

    // Build the full pool of mesh-agent endpoints (used to compose multi-hop
    // routes). Each baseline pair's inner loop subtracts A and B before passing
    // the pool to `enumerate_routes`.
    //
    // Each mesh agent contributes BOTH its `Agent { id }` form and its
    // `CandidateIp { ip }` form to the pool. Both forms are needed because
    // `LegLookup` indexes measurements as `(Agent(src_id), Ip(dst_ip))`, and
    // different leg positions require different endpoint variants:
    //
    // * `CandidateIp { ip }` — required for legs where the intermediary is the
    //   destination of a stored measurement. For example, the A→Y first leg
    //   resolves via forward key `(Agent("a"), Ip(Y.ip))`. For the terminal
    //   Y→B leg (where B is `Agent("b")`), using `CandidateIp(Y.ip)` allows the
    //   reverse key `(Agent("b"), Ip(Y.ip))` to hit the map. Using `Agent("y")`
    //   instead would yield `(Agent("y"), Agent("b"))` which never exists.
    //
    // * `Agent { id }` — required for the middle leg of a 2-hop route where
    //   the agent is the SOURCE of a stored measurement. For example, in
    //   `A → CandidateIp(Y) → Agent("y") → B`, the middle leg resolves via
    //   reverse `(Agent("y"), Ip(Y.ip))` — the agent-ID form is the key.
    //
    // Including both forms doubles the pool size but has no correctness cost:
    // `enumerate_routes` discards routes whose any leg can't resolve, so
    // non-matching endpoint forms simply produce no route and are filtered out.
    // `qualifies_under_optimization_v2` uses `CandidateIp`-only for its non-X
    // pool (a simpler 1-hop-only case); this pool is the richer dual-form version
    // needed to compose both X-first and X-second 2-hop routes correctly.
    let agent_endpoints: Vec<Endpoint> = inputs
        .agents
        .iter()
        .flat_map(|a| {
            [
                Endpoint::CandidateIp { ip: a.ip },
                Endpoint::Agent { id: a.agent_id.clone() },
            ]
        })
        .collect();

    let total_baseline = baselines.len() as i32;
    let mut candidate_rows: Vec<EvaluationCandidateDto> = Vec::new();
    let mut pair_details_per_candidate: Vec<PairDetailsForCandidate> = Vec::new();
    let mut unqualified_reasons: BTreeMap<String, String> = BTreeMap::new();

    // T55 eligibility / storage knobs. All four are [`Option<f64>`] — `None`
    // means "knob unset; gate is open".
    let max_rtt_cap = inputs.max_transit_rtt_ms;
    let max_sd_cap = inputs.max_transit_stddev_ms;
    let min_imp_ms = inputs.min_improvement_ms;
    let min_imp_ratio = inputs.min_improvement_ratio;

    // Clamp max_hops to [0, 2] (validated upstream, but defence-in-depth).
    let max_hops = inputs.max_hops.clamp(0, 2) as u8;

    for x_ip in &candidates {
        let has_from_any = inputs
            .agents
            .iter()
            .any(|a| by_pair.contains_key(&(a.agent_id.clone(), *x_ip)));
        if !has_from_any {
            continue;
        }

        let x_endpoint = Endpoint::CandidateIp { ip: *x_ip };

        let mut pair_details: Vec<EvaluationPairDetailDto> = Vec::new();
        let mut qualifying_legs: Vec<(String, String)> = Vec::new();
        let mut pairs_improved = 0i32;
        let mut pairs_total_considered = 0i32;
        let mut improvements: Vec<f32> = Vec::new();
        let mut compound_losses: Vec<f32> = Vec::new();
        let mut any_threshold_fail = false;

        for (a_id, b_ip, b_id, direct_was_substituted) in &baselines {
            // Exclude degenerate triples where X is A or B.
            if Some(*x_ip) == agent_by_id.get(a_id).copied() {
                continue;
            }
            if *x_ip == *b_ip {
                continue;
            }

            let direct = match by_pair.get(&(a_id.clone(), *b_ip)) {
                Some(m) => *m,
                None => continue,
            };
            let Some(direct_rtt) = direct.latency_avg_ms else {
                continue;
            };
            let direct_stddev = direct.latency_stddev_ms.unwrap_or(0.0);
            let direct_loss_ratio = direct.loss_ratio;

            // Build the intermediary pool for route enumeration:
            // all agents except A and B, plus X as a CandidateIp endpoint.
            // This allows X to appear in any intermediary position up to max_hops.
            //
            // Both A (source) and B (destination) are mesh agents, so their
            // endpoints carry agent IDs. X is a non-mesh candidate IP.
            // The LegLookup stores measurements as (Agent(src_id), Ip(dst_ip)),
            // so using Agent endpoints for A and B lets the leg resolver find
            // both direct (A→X, A→B) and symmetry-substituted (B→X used as X→B)
            // measurements correctly.
            let a_endpoint = Endpoint::Agent { id: a_id.clone() };
            let b_endpoint = Endpoint::Agent { id: b_id.clone() };
            // `agent_endpoints` contains both `CandidateIp` and `Agent` forms of
            // each mesh agent (see construction comment above). Both forms of A
            // and B must be removed from the pool so that `enumerate_routes`
            // doesn't route through A or B as a transit intermediary.
            let pool: Vec<Endpoint> = agent_endpoints
                .iter()
                .filter(|e| match e {
                    Endpoint::Agent { id } => id != a_id && id != b_id,
                    Endpoint::CandidateIp { ip } => {
                        Some(*ip) != agent_by_id.get(a_id.as_str()).copied()
                            && *ip != *b_ip
                    }
                })
                .cloned()
                .chain(std::iter::once(x_endpoint.clone()))
                .collect();

            let all_routes = routes::enumerate_routes(
                &leg_lookup,
                &a_endpoint,
                &b_endpoint,
                &pool,
                max_hops,
                max_rtt_cap,
                max_sd_cap,
                inputs.stddev_weight,
            );

            // Filter to routes that include X in their intermediaries.
            // For 1-hop this is A→X→B; for 2-hop both A→X→Y→B and
            // A→Y→X→B are included; `min_by` picks the better of the two.
            let with_x: Vec<&routes::ComposedRoute> = all_routes
                .iter()
                .filter(|r| r.intermediaries.iter().any(|e| e == &x_endpoint))
                .collect();

            let Some(best_x_route) = with_x.iter().min_by(|a, b| {
                let score_a = a.rtt_ms + inputs.stddev_weight * a.stddev_ms;
                let score_b = b.rtt_ms + inputs.stddev_weight * b.stddev_ms;
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) else {
                // No X-containing route is reachable.
                continue;
            };

            let transit_rtt = best_x_route.rtt_ms;
            let transit_stddev = best_x_route.stddev_ms;
            let transit_penalty = inputs.stddev_weight * transit_stddev;
            let compound_loss_ratio = best_x_route.loss_ratio;
            let direct_penalty = inputs.stddev_weight * direct_stddev;
            let improvement_ms =
                direct_rtt - transit_rtt - (transit_penalty - direct_penalty);

            // Determine where X sits in the winning route intermediary order.
            // Position 1 = X is first intermediary (A→X→Y→B);
            // Position 2 = X is second intermediary (A→Y→X→B).
            // None for direct (0 intermediaries) and 1-hop (1 intermediary —
            // X is the sole transit stop so "first vs second" has no meaning).
            // This is a function of route topology, not of `max_hops` config.
            let winning_x_position: Option<u8> = if best_x_route.intermediaries.len() == 2 {
                best_x_route
                    .intermediaries
                    .iter()
                    .position(|e| matches!(e, Endpoint::CandidateIp { ip } if *ip == *x_ip))
                    .map(|pos| (pos + 1) as u8)
            } else {
                None // direct (0 intermediaries) or 1-hop (1 intermediary)
            };

            // Extract AX and XB leg metadata from the winning route.
            // For 1-hop: leg[0] = A→X, leg[1] = X→B.
            // For 2-hop with X first: leg[0] = A→X, leg[1] = X→Y, leg[2] = Y→B.
            // For 2-hop with X second: leg[0] = A→Y, leg[1] = Y→X, leg[2] = X→B.
            // mtr_measurement_id and was_substituted come from the legs.
            let (ax_was_substituted, xb_was_substituted, mtr_id_ax, mtr_id_xb) =
                extract_ax_xb_meta(best_x_route, &x_endpoint);

            let loss_ok = compound_loss_ratio <= inputs.loss_threshold_ratio
                && direct_loss_ratio <= inputs.loss_threshold_ratio;
            if !loss_ok {
                any_threshold_fail = true;
                continue;
            }

            let qualifies = match inputs.mode {
                EvaluationMode::Diversity => improvement_ms > 0.0,
                EvaluationMode::Optimization => {
                    // Optimization: X qualifies only when no non-X route
                    // is at least as good as the X route. Build all non-X
                    // routes (routes where X does NOT appear in intermediaries)
                    // from the full agent pool (excluding A, B, and X).
                    qualifies_under_optimization_v2(
                        &a_endpoint,
                        &b_endpoint,
                        *x_ip,
                        transit_rtt + transit_penalty,
                        improvement_ms,
                        &leg_lookup,
                        &inputs.agents,
                        a_id,
                        b_id,
                        max_hops,
                        max_rtt_cap,
                        max_sd_cap,
                        inputs.stddev_weight,
                    )
                }
                // EdgeCandidate is dispatched at the top of `evaluate`
                // and never reaches this match — the dispatcher
                // returns from `edge_candidate::evaluate` first.
                EvaluationMode::EdgeCandidate => unreachable!(
                    "EdgeCandidate must be dispatched by `evaluate` before reaching the triple loop"
                ),
            };

            // Counter accumulation runs between the eligibility gate
            // (above) and the storage-filter gate (below). A row that
            // fails the storage filter still contributes to the
            // candidate's headline counters — only its persisted detail
            // row is dropped. See architecture #2 (eligibility vs
            // storage) and architecture #3 (OR semantics).
            pairs_total_considered += 1;
            if qualifies {
                pairs_improved += 1;
                improvements.push(improvement_ms);
                // Capture the qualifying leg for `Detail: good candidates`
                // BEFORE the storage filter runs — `Detail: good candidates`
                // expands every qualifying triple, and a tight storage floor
                // would otherwise drop legs from the dispatch's view.
                qualifying_legs.push((a_id.clone(), b_id.clone()));
            }
            compound_losses.push(compound_loss_ratio);

            // Storage filter — OR semantics across
            // `min_improvement_ms` / `min_improvement_ratio`. See
            // architecture #3 (OR semantics) and the
            // [`passes_storage_filter`] helper for the truth table.
            if !passes_storage_filter(improvement_ms, direct_rtt, min_imp_ms, min_imp_ratio) {
                continue;
            }

            pair_details.push(EvaluationPairDetailDto {
                source_agent_id: a_id.clone(),
                destination_agent_id: b_id.clone(),
                // Transit IP (X), matching the candidate this pair
                // detail is nested under. The baseline destination
                // agent's IP is recoverable via `destination_agent_id`
                // through `agents_with_catalogue`; surfacing X here
                // makes the DTO self-contained.
                destination_ip: x_ip.to_string(),
                direct_rtt_ms: direct_rtt,
                direct_stddev_ms: direct_stddev,
                direct_loss_ratio,
                // Provenance of the direct A→B baseline row. The transit
                // legs (A→X, X→B) are always active-probe measurements;
                // only this A→B row can be VM-sourced, per the T54-03
                // handler that synthesizes a [`DirectSource::VmContinuous`]
                // `AttributedMeasurement` for agent→agent pairs missing
                // from the active-probe join.
                direct_source: direct.direct_source,
                transit_rtt_ms: transit_rtt,
                transit_stddev_ms: transit_stddev,
                transit_loss_ratio: compound_loss_ratio,
                improvement_ms,
                qualifies,
                mtr_measurement_id_ax: mtr_id_ax,
                mtr_measurement_id_xb: mtr_id_xb,
                destination_hostname: None,
                ax_was_substituted: Some(ax_was_substituted),
                xb_was_substituted: Some(xb_was_substituted),
                direct_was_substituted: Some(*direct_was_substituted),
                winning_x_position,
            });
        }

        // Skip candidates that scored zero eligible triples — matches
        // the L2 early-termination decision (a candidate whose composed
        // RTT/stddev never satisfies the cap should not appear in the
        // results). With T55 storage filters the distinction between
        // `pair_details` and counters matters: a candidate may have
        // `pairs_total_considered > 0` while every detail row was
        // dropped by the storage gate — that candidate's headline
        // counters are still meaningful and its row must remain.
        if pairs_total_considered == 0 {
            if any_threshold_fail {
                unqualified_reasons.insert(
                    x_ip.to_string(),
                    "all triples exceeded loss_threshold_ratio".into(),
                );
            }
            continue;
        }

        candidate_rows.push(build_candidate_row(
            *x_ip,
            pairs_improved,
            pairs_total_considered,
            &improvements,
            &compound_losses,
            total_baseline,
            &inputs.enrichment,
            &agent_by_ip,
        ));
        pair_details_per_candidate.push(PairDetailsForCandidate {
            destination_ip: *x_ip,
            pair_details,
            qualifying_legs,
        });
    }

    // Sort candidates by composite score, carrying the matching
    // pair-detail bundle along so the parallel `Vec`s stay aligned by
    // index for `insert_evaluation`'s persistence loop.
    let mut zipped: Vec<(EvaluationCandidateDto, PairDetailsForCandidate)> = candidate_rows
        .into_iter()
        .zip(pair_details_per_candidate)
        .collect();
    zipped.sort_by(|a, b| candidate_order(&a.0, &b.0));
    let (candidate_rows, pair_details_by_candidate): (
        Vec<EvaluationCandidateDto>,
        Vec<PairDetailsForCandidate>,
    ) = zipped.into_iter().unzip();

    let candidates_total = candidate_rows.len() as i32;
    let candidates_good = candidate_rows
        .iter()
        .filter(|c| c.pairs_improved >= 1)
        .count() as i32;
    // Derive the headline `avg_improvement_ms` from the per-candidate
    // aggregates — see [`aggregate_headline_avg`] for the rationale
    // (architecture #2 / architecture #4).
    let avg_improvement_ms = aggregate_headline_avg(&candidate_rows);

    Ok(TripleEvaluationOutputs {
        baseline_pair_count: total_baseline,
        candidates_total,
        candidates_good,
        avg_improvement_ms,
        results: EvaluationResultsDto {
            candidates: candidate_rows,
            unqualified_reasons,
        },
        pair_details_by_candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn agent(id: &str, addr: &str) -> AgentRow {
        AgentRow {
            agent_id: id.into(),
            ip: ip(addr),
            hostname: None,
        }
    }

    /// Helper: unwrap a `Triple` evaluator output for assertions in the
    /// diversity / optimization tests. Panics on `EdgeCandidate` so a
    /// future refactor that mis-dispatches the mode fails loudly.
    fn triple(out: EvaluationOutputs) -> TripleEvaluationOutputs {
        match out {
            EvaluationOutputs::Triple(t) => t,
            EvaluationOutputs::EdgeCandidate(_) => {
                panic!("expected Triple variant for diversity / optimization mode")
            }
        }
    }

    fn m(src: &str, dst: &str, rtt: f32, stddev: f32, loss: f32) -> AttributedMeasurement {
        AttributedMeasurement {
            source_agent_id: src.into(),
            destination_ip: ip(dst),
            latency_avg_ms: Some(rtt),
            latency_stddev_ms: Some(stddev),
            loss_ratio: loss,
            mtr_measurement_id: None,
            direct_source: DirectSource::ActiveProbe,
        }
    }

    fn inputs_basic(mode: EvaluationMode) -> EvaluationInputs {
        EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),
                m("a", "203.0.113.7", 120.0, 8.0, 0.0),
                m("b", "203.0.113.7", 121.0, 8.0, 0.0),
            ],
            agents: vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        }
    }

    #[test]
    fn triple_excludes_x_equals_a_or_b() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements.push(m("a", "10.0.0.2", 318.0, 24.0, 0.0));
        i.measurements.push(m("b", "10.0.0.2", 0.0, 0.0, 0.0));
        let out = triple(evaluate(i).unwrap());
        for cand in &out.results.candidates {
            assert_ne!(cand.destination_ip, "10.0.0.2");
            assert_ne!(cand.destination_ip, "10.0.0.1");
        }
    }

    #[test]
    fn no_baseline_returns_empty_with_error() {
        let i = EvaluationInputs {
            measurements: vec![m("a", "203.0.113.7", 120.0, 8.0, 0.0)],
            agents: vec![agent("a", "10.0.0.1")],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        };
        let err = evaluate(i).unwrap_err();
        assert!(matches!(err, EvalError::NoBaseline));
    }

    #[test]
    fn rtt_less_baseline_does_not_count_toward_baseline_set() {
        // An A→B measurement that exists but has `latency_avg_ms=None`
        // (e.g. a 100 %-loss success) cannot be scored against — the
        // inner loop skips it at the `Some(direct_rtt)` destructure.
        // If the baseline-detection loop ignored that, `baseline_pair_count`
        // would be inflated and the caller-visible `NoBaseline → 422`
        // error would never fire on an all-loss campaign.
        let i = EvaluationInputs {
            measurements: vec![AttributedMeasurement {
                source_agent_id: "a".into(),
                destination_ip: ip("10.0.0.2"),
                latency_avg_ms: None,
                latency_stddev_ms: None,
                loss_ratio: 1.0,
                mtr_measurement_id: None,
                direct_source: DirectSource::ActiveProbe,
            }],
            agents: vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        };
        let err = evaluate(i).expect_err("RTT-less baseline must not register");
        assert!(matches!(err, EvalError::NoBaseline));
    }

    #[test]
    fn diversity_qualifies_transit_that_beats_direct() {
        let out = triple(evaluate(inputs_basic(EvaluationMode::Diversity)).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("candidate present");
        assert_eq!(cand.pairs_improved, 1);
        assert!(
            cand.avg_improvement_ms.unwrap() > 0.0,
            "transit (120+121=241) beats direct (318)"
        );
    }

    #[test]
    fn loss_threshold_filters_unreliable_transit() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements[1].loss_ratio = 0.03;
        let out = triple(evaluate(i).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7");
        assert!(cand.is_none() || cand.unwrap().pairs_improved == 0);
    }

    #[test]
    fn stddev_penalty_applied() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements[1].latency_stddev_ms = Some(200.0);
        i.measurements[2].latency_stddev_ms = Some(200.0);
        let out = triple(evaluate(i).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7");
        if let Some(c) = cand {
            assert_eq!(c.pairs_improved, 0);
        }
    }

    #[test]
    fn optimization_filters_out_when_existing_mesh_y_already_better() {
        // Three agents a, b, y (mesh), plus third-party 203.0.113.7 = X.
        // Baseline A→B = 318ms.
        // Transit via X: A→X(120) + B→X(121) = 241ms (beats direct).
        // Transit via Y (mesh): A→Y(100) + B→Y(80) = 180ms (beats X).
        // The Y legs mirror the X legs' symmetry convention
        // (source-is-baseline-agent, destination-is-transit), so the
        // Y→B direction is modelled as B→Y under the same symmetry
        // approximation the X→B leg already uses.
        // Under optimization mode, X must NOT qualify because Y is
        // already a better transit.
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),   // A→B baseline
                m("a", "203.0.113.7", 120.0, 8.0, 0.0), // A→X
                m("b", "203.0.113.7", 121.0, 8.0, 0.0), // B→X (sym-approx X→B)
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),    // A→Y
                m("b", "10.0.0.3", 80.0, 5.0, 0.0),     // B→Y (sym-approx Y→B)
            ],
            agents: vec![
                agent("a", "10.0.0.1"),
                agent("b", "10.0.0.2"),
                agent("y", "10.0.0.3"),
            ],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        };
        let out = triple(evaluate(inputs).unwrap());
        let (x_idx, x_cand) = out
            .results
            .candidates
            .iter()
            .enumerate()
            .find(|(_, c)| c.destination_ip == "203.0.113.7")
            .expect("X must appear as a candidate (triple is fully measured)");
        // X's pair_details entry for (A,B) must be present (triple is
        // fully measured) but qualifies=false because Y beats X.
        let ab_detail = out.pair_details_by_candidate[x_idx]
            .pair_details
            .iter()
            .find(|p| p.source_agent_id == "a" && p.destination_agent_id == "b")
            .expect("(A,B) pair_details entry present — triple fully measured");
        assert!(
            !ab_detail.qualifies,
            "optimization predicate must reject X when Y provides a better transit"
        );
        assert_eq!(
            x_cand.pairs_improved, 0,
            "X must not be counted as an improvement under optimization mode"
        );
    }

    #[test]
    fn pair_detail_stamps_direct_source_from_baseline_row() {
        // When the A→B baseline row carries `VmContinuous` provenance,
        // the evaluator must propagate that onto every pair_detail it
        // emits using that baseline. Transit legs (A→X, X→B) being
        // active-probe is irrelevant here — only the direct A→B
        // baseline's provenance lands on the DTO.
        let a_b_baseline = AttributedMeasurement {
            source_agent_id: "a".into(),
            destination_ip: ip("10.0.0.2"),
            latency_avg_ms: Some(318.0),
            latency_stddev_ms: Some(24.0),
            loss_ratio: 0.0,
            mtr_measurement_id: None,
            direct_source: DirectSource::VmContinuous,
        };
        let inputs = EvaluationInputs {
            measurements: vec![
                a_b_baseline,
                m("a", "203.0.113.7", 120.0, 8.0, 0.0),
                m("b", "203.0.113.7", 121.0, 8.0, 0.0),
            ],
            agents: vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Diversity,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        };
        let out = triple(evaluate(inputs).unwrap());
        let (cand_idx, _cand) = out
            .results
            .candidates
            .iter()
            .enumerate()
            .find(|(_, c)| c.destination_ip == "203.0.113.7")
            .expect("candidate present");
        assert_eq!(
            out.pair_details_by_candidate[cand_idx].pair_details[0].direct_source,
            DirectSource::VmContinuous,
            "pair_detail must carry the baseline row's direct_source"
        );
    }

    #[test]
    fn is_mesh_member_flag_set_when_x_is_agent() {
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),
                m("b", "10.0.0.3", 130.0, 5.0, 0.0),
            ],
            agents: vec![
                agent("a", "10.0.0.1"),
                agent("b", "10.0.0.2"),
                agent("c", "10.0.0.3"),
            ],
            candidate_ips: Vec::new(),
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Diversity,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: 1,
        };
        let out = triple(evaluate(inputs).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "10.0.0.3")
            .expect("agent-as-candidate present");
        assert!(cand.is_mesh_member);
    }
}
