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
use crate::campaign::eval::legs::{LegLookup, LegLookupResult, LegMeasurement};
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
    /// Agent roster used as the destination set ("B") in the
    /// EdgeCandidate evaluator and as the baseline pair set in the
    /// triple evaluator. EdgeCandidate restricts this to source agents
    /// of the campaign so the heatmap doesn't double-count IPs that are
    /// both candidates (X) and destinations (B). Diversity / Optimization
    /// pass the union of source and destination agents (the full pair
    /// surface they iterate). For both modes this is a *subset* of
    /// [`Self::roster`], which carries the full identity registry needed
    /// for IP↔agent resolution inside [`legs::LegLookup`].
    pub agents: Vec<AgentRow>,
    /// Full identity roster — every mesh agent that may appear in any
    /// position of any route or candidate IP, regardless of whether it
    /// participates as a destination. This powers
    /// [`legs::LegLookup`]'s `agent_by_ip` / `ip_by_agent` maps so a
    /// `CandidateIp { ip }` whose IP belongs to a registered mesh agent
    /// resolves to that agent's identity even when the agent is omitted
    /// from [`Self::agents`] (which happens in EdgeCandidate mode for
    /// agents that aren't a campaign source). For Diversity /
    /// Optimization this is identical to [`Self::agents`].
    pub roster: Vec<AgentRow>,
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
    /// Diversity / Optimization output.
    Triple(TripleEvaluationOutputs),
    /// EdgeCandidate output. See
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
/// sources LATER in `measurements`. The `/evaluate` handler relies on
/// this — it prepends VM-synthesized rows and appends active-probe rows
/// so an active-probe measurement always overwrites a VM baseline for
/// the same `(source_agent_id, destination_ip)` tuple. See
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
/// measurement. Returns
/// `(a_id, b_ip, b_id, direct_was_substituted, direct_measurement)`.
///
/// `direct_was_substituted` is `true` when the A→B RTT was resolved from the
/// reverse B→A measurement (per LegLookup §3.1 symmetry substitution). The
/// fifth tuple element is a borrow of the measurement that should be used as
/// the direct A→B baseline downstream — the forward row when forward is
/// usable, otherwise the reverse row that LegLookup substituted in. Callers
/// must read direct RTT / stddev / loss / source from this reference rather
/// than re-querying `by_pair`, otherwise symmetry-substituted baselines
/// silently fall back to forward (or vanish entirely when forward is absent).
///
/// A row with no RTT (e.g. a 100 %-loss "success") cannot participate
/// in scoring downstream, so counting it would inflate
/// `baseline_pair_count` and suppress the `NoBaseline → 422` error
/// when every observed baseline is RTT-less.
fn collect_baselines<'m>(
    agents: &[AgentRow],
    by_pair: &HashMap<(String, IpAddr), &'m AttributedMeasurement>,
    leg_lookup: &LegLookup<'_>,
) -> Vec<(String, IpAddr, String, bool, &'m AttributedMeasurement)> {
    let mut baselines: Vec<(String, IpAddr, String, bool, &'m AttributedMeasurement)> = Vec::new();
    for a in agents {
        for b in agents {
            if a.agent_id == b.agent_id {
                continue;
            }

            let forward = by_pair.get(&(a.agent_id.clone(), b.ip)).copied();
            let forward_usable =
                forward.is_some_and(|m| m.latency_avg_ms.is_some() && m.loss_ratio < 1.0);

            // Determine substitution status via the leg-lookup table — this
            // is the single source of truth for whether the direct A→B leg
            // resolved via forward or symmetry-substituted reverse.
            let a_endpoint = Endpoint::Agent {
                id: a.agent_id.clone(),
            };
            let b_endpoint = Endpoint::CandidateIp { ip: b.ip };
            let direct_was_substituted = matches!(
                leg_lookup.lookup(&a_endpoint, &b_endpoint),
                LegLookupResult::Found {
                    was_substituted: true,
                    ..
                }
            );

            // Pick the measurement that downstream code should treat as the
            // direct baseline:
            // * Forward usable + not substituted → forward.
            // * Substituted → reverse (B probed A.ip), looked up from `by_pair`.
            // * Neither → no usable baseline; skip.
            let direct_measurement = if direct_was_substituted {
                by_pair
                    .get(&(b.agent_id.clone(), a.ip))
                    .copied()
                    .filter(|m| m.latency_avg_ms.is_some() && m.loss_ratio < 1.0)
            } else if forward_usable {
                forward
            } else {
                None
            };

            let Some(direct_measurement) = direct_measurement else {
                continue;
            };

            baselines.push((
                a.agent_id.clone(),
                b.ip,
                b.agent_id.clone(),
                direct_was_substituted,
                direct_measurement,
            ));
        }
    }
    baselines
}

/// Decide whether a scored triple's pair-detail row is persisted.
///
/// Implements the OR semantics of architecture #3 across the
/// `min_improvement_ms` and `min_improvement_ratio` knobs:
///
/// * Both `None` ⇒ store (gate is open).
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

/// Resolve a single leg via [`LegLookup::lookup`], adapting a `Found`
/// result into the standalone [`LegMeasurement`] shape route composers
/// consume. Returns `None` on `Broken` / `Missing` — the caller drops
/// any route containing the unresolvable leg, matching
/// `routes::look_up_leg` exactly.
fn lookup_leg(lookup: &LegLookup<'_>, from: &Endpoint, to: &Endpoint) -> Option<LegMeasurement> {
    match lookup.lookup(from, to) {
        LegLookupResult::Found {
            rtt_ms,
            stddev_ms,
            loss_ratio,
            source,
            was_substituted,
            mtr_measurement_id,
        } => Some(LegMeasurement {
            from: from.clone(),
            to: to.clone(),
            rtt_ms,
            stddev_ms,
            loss_ratio,
            source,
            was_substituted,
            mtr_measurement_id,
        }),
        _ => None,
    }
}

/// Per-X leg cache. Built once per candidate IP X before the baseline
/// loop runs; reused for every (A, B) triple.
///
/// All four maps store legs resolved via [`LegLookup::lookup`] — entries
/// only exist when the lookup returns `Found`.
struct XLegCache {
    /// A→X leg, keyed by the source mesh agent id. Used for both 1-hop
    /// (A→X→B) and 2-hop "X first" (A→X→Y→B) compositions.
    ax_by_a: HashMap<String, LegMeasurement>,
    /// X→B leg, keyed by the destination IP. Used for 1-hop (A→X→B)
    /// and 2-hop "X second" (A→Y→X→B).
    xb_by_b: HashMap<IpAddr, LegMeasurement>,
    /// X→Y middle leg, keyed by Y's agent id. Used for 2-hop "X first"
    /// compositions.
    xy_by_y: HashMap<String, LegMeasurement>,
    /// Y→X middle leg, keyed by Y's agent id. Used for 2-hop "X second"
    /// compositions.
    yx_by_y: HashMap<String, LegMeasurement>,
}

/// Build the per-X leg cache used by [`best_x_containing_route`].
///
/// X is always `Endpoint::CandidateIp { ip: x_ip }`. The middle-leg
/// maps (`xy_by_y`, `yx_by_y`) are skipped when `max_hops < 2` — they
/// have no consumer in that regime.
fn build_x_leg_cache(
    lookup: &LegLookup<'_>,
    agents: &[AgentRow],
    x_ip: IpAddr,
    max_hops: u8,
) -> XLegCache {
    let x_endpoint = Endpoint::CandidateIp { ip: x_ip };
    let mut ax_by_a: HashMap<String, LegMeasurement> = HashMap::with_capacity(agents.len());
    let mut xb_by_b: HashMap<IpAddr, LegMeasurement> = HashMap::with_capacity(agents.len());
    let mut xy_by_y: HashMap<String, LegMeasurement> = HashMap::new();
    let mut yx_by_y: HashMap<String, LegMeasurement> = HashMap::new();
    for a in agents {
        let a_endpoint = Endpoint::Agent {
            id: a.agent_id.clone(),
        };
        let to_x = lookup_leg(lookup, &a_endpoint, &x_endpoint);
        let from_x = lookup_leg(lookup, &x_endpoint, &a_endpoint);
        if let Some(leg) = to_x.clone() {
            ax_by_a.insert(a.agent_id.clone(), leg);
        }
        if let Some(leg) = from_x.clone() {
            xb_by_b.insert(a.ip, leg);
        }
        if max_hops >= 2 {
            if let Some(leg) = from_x {
                xy_by_y.insert(a.agent_id.clone(), leg);
            }
            if let Some(leg) = to_x {
                yx_by_y.insert(a.agent_id.clone(), leg);
            }
        }
    }
    XLegCache {
        ax_by_a,
        xb_by_b,
        xy_by_y,
        yx_by_y,
    }
}

/// Composition arithmetic shared by the inline composer. Matches
/// `routes::compose_one_hop` exactly — using the same evaluation order
/// keeps the f32 result byte-identical to the original implementation.
fn compose_two_legs(l1: &LegMeasurement, l2: &LegMeasurement) -> (f32, f32, f32) {
    let rtt = l1.rtt_ms + l2.rtt_ms;
    let stddev = (l1.stddev_ms.powi(2) + l2.stddev_ms.powi(2)).sqrt();
    let loss = 1.0 - (1.0 - l1.loss_ratio) * (1.0 - l2.loss_ratio);
    (rtt, stddev, loss)
}

/// Composition arithmetic for 2-hop routes. Matches
/// `routes::compose_two_hop` exactly.
fn compose_three_legs(
    l1: &LegMeasurement,
    l2: &LegMeasurement,
    l3: &LegMeasurement,
) -> (f32, f32, f32) {
    let rtt = l1.rtt_ms + l2.rtt_ms + l3.rtt_ms;
    let stddev = (l1.stddev_ms.powi(2) + l2.stddev_ms.powi(2) + l3.stddev_ms.powi(2)).sqrt();
    let loss = 1.0 - (1.0 - l1.loss_ratio) * (1.0 - l2.loss_ratio) * (1.0 - l3.loss_ratio);
    (rtt, stddev, loss)
}

/// Cap predicate identical to `routes::exceeds_caps`. Returns `true`
/// when the route should be discarded.
fn exceeds_caps(
    rtt: f32,
    stddev: f32,
    stddev_weight: f32,
    max_rtt: Option<f64>,
    max_stddev: Option<f64>,
) -> bool {
    let with_penalty = rtt + stddev_weight * stddev;
    if let Some(cap) = max_rtt {
        if (with_penalty as f64) > cap {
            return true;
        }
    }
    if let Some(cap) = max_stddev {
        if (stddev as f64) > cap {
            return true;
        }
    }
    false
}

/// Compose only X-containing routes from the pre-built leg caches and
/// pick the best by penalised RTT. Mirrors the original
/// `routes::enumerate_routes(...).filter(with_x).min_by(score)` chain
/// without enumerating any non-X routes.
///
/// Returns `None` when no X-containing route resolves under the caps.
#[allow(clippy::too_many_arguments)]
fn best_x_containing_route(
    x_cache: &XLegCache,
    x_endpoint: &Endpoint,
    x_ip: IpAddr,
    a_id: &str,
    b_ip: IpAddr,
    b_id: &str,
    mesh_from: &HashMap<String, Vec<(Endpoint, LegMeasurement)>>,
    mesh_to: &HashMap<IpAddr, Vec<(Endpoint, LegMeasurement)>>,
    agent_by_id: &HashMap<String, IpAddr>,
    max_hops: u8,
    max_rtt_cap: Option<f64>,
    max_sd_cap: Option<f64>,
    stddev_weight: f32,
) -> Option<routes::ComposedRoute> {
    let mut best: Option<routes::ComposedRoute> = None;
    let mut best_score: f32 = f32::INFINITY;

    let consider = |route: routes::ComposedRoute,
                    best: &mut Option<routes::ComposedRoute>,
                    best_score: &mut f32| {
        let score = route.rtt_ms + stddev_weight * route.stddev_ms;
        if score < *best_score {
            *best_score = score;
            *best = Some(route);
        }
    };

    let ax = x_cache.ax_by_a.get(a_id);
    let xb = x_cache.xb_by_b.get(&b_ip);

    // 1-hop A→X→B (requires max_hops >= 1, validated upstream; the cap
    // matches `routes::enumerate_routes`'s `max_hops >= 1` guard).
    if max_hops >= 1 {
        if let (Some(ax), Some(xb)) = (ax, xb) {
            let (rtt, stddev, loss) = compose_two_legs(ax, xb);
            if !exceeds_caps(rtt, stddev, stddev_weight, max_rtt_cap, max_sd_cap) {
                consider(
                    routes::ComposedRoute {
                        legs: vec![ax.clone(), xb.clone()],
                        rtt_ms: rtt,
                        stddev_ms: stddev,
                        loss_ratio: loss,
                        kind: routes::RouteKind::OneHop,
                        intermediaries: vec![x_endpoint.clone()],
                    },
                    &mut best,
                    &mut best_score,
                );
            }
        }
    }

    if max_hops >= 2 {
        // 2-hop "X first": A → X → Y → B. Iterate Y over legs Y→B.
        if let Some(ax) = ax {
            // Short-circuit: if A→X already exceeds the RTT cap, every
            // A→X→Y→B route (rtt = ax.rtt + xy.rtt + yb.rtt, monotonic
            // in ax.rtt) is doomed regardless of Y.
            let ax_score = ax.rtt_ms + stddev_weight * ax.stddev_ms;
            let ax_alone_doomed = matches!(max_rtt_cap, Some(cap) if (ax_score as f64) > cap);
            if !ax_alone_doomed {
                if let Some(yb_list) = mesh_to.get(&b_ip) {
                    for (y_endpoint, yb_leg) in yb_list {
                        let y_id = match y_endpoint {
                            Endpoint::Agent { id } => id,
                            // mesh_to entries are always Agent-form by
                            // construction; defensive skip otherwise.
                            _ => continue,
                        };
                        if y_id == a_id || y_id == b_id {
                            continue;
                        }
                        // Skip Y when X is itself a mesh agent at Y.ip
                        // (the route degenerates into A→X→X→B).
                        if agent_by_id.get(y_id).copied() == Some(x_ip) {
                            continue;
                        }
                        let Some(xy_leg) = x_cache.xy_by_y.get(y_id) else {
                            continue;
                        };
                        let (rtt, stddev, loss) = compose_three_legs(ax, xy_leg, yb_leg);
                        if exceeds_caps(rtt, stddev, stddev_weight, max_rtt_cap, max_sd_cap) {
                            continue;
                        }
                        consider(
                            routes::ComposedRoute {
                                legs: vec![ax.clone(), xy_leg.clone(), yb_leg.clone()],
                                rtt_ms: rtt,
                                stddev_ms: stddev,
                                loss_ratio: loss,
                                kind: routes::RouteKind::TwoHop,
                                intermediaries: vec![x_endpoint.clone(), y_endpoint.clone()],
                            },
                            &mut best,
                            &mut best_score,
                        );
                    }
                }
            }
        }

        // 2-hop "X second": A → Y → X → B. Iterate Y over legs A→Y.
        if let Some(xb) = xb {
            let xb_score = xb.rtt_ms + stddev_weight * xb.stddev_ms;
            let xb_alone_doomed = matches!(max_rtt_cap, Some(cap) if (xb_score as f64) > cap);
            if !xb_alone_doomed {
                if let Some(ay_list) = mesh_from.get(a_id) {
                    for (y_endpoint, ay_leg) in ay_list {
                        let y_id = match y_endpoint {
                            Endpoint::Agent { id } => id,
                            _ => continue,
                        };
                        if y_id == a_id || y_id == b_id {
                            continue;
                        }
                        if agent_by_id.get(y_id).copied() == Some(x_ip) {
                            continue;
                        }
                        let Some(yx_leg) = x_cache.yx_by_y.get(y_id) else {
                            continue;
                        };
                        let (rtt, stddev, loss) = compose_three_legs(ay_leg, yx_leg, xb);
                        if exceeds_caps(rtt, stddev, stddev_weight, max_rtt_cap, max_sd_cap) {
                            continue;
                        }
                        consider(
                            routes::ComposedRoute {
                                legs: vec![ay_leg.clone(), yx_leg.clone(), xb.clone()],
                                rtt_ms: rtt,
                                stddev_ms: stddev,
                                loss_ratio: loss,
                                kind: routes::RouteKind::TwoHop,
                                intermediaries: vec![y_endpoint.clone(), x_endpoint.clone()],
                            },
                            &mut best,
                            &mut best_score,
                        );
                    }
                }
            }
        }
    }

    best
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

/// Build per-baseline alt-route caches for Optimization mode.
///
/// For each baseline `(A, B)` we enumerate every A→B route via mesh
/// intermediaries (expressed as `CandidateIp` endpoints so symmetry
/// substitution resolves legs like `B→Y` as `Y→B`). X is NOT excluded
/// from the pool at build time; per-X filtering is applied at the query
/// site by skipping routes whose intermediaries contain
/// `CandidateIp(x_ip)`.
///
/// Building these once per evaluator pass avoids re-enumerating the
/// non-X alternatives for every (X, A, B) inner triple — the dominant
/// cost in the original implementation when `max_hops == 2`.
fn build_alt_routes_per_baseline(
    leg_lookup: &LegLookup<'_>,
    agents: &[AgentRow],
    baselines: &[(String, IpAddr, String, bool, &AttributedMeasurement)],
    max_hops: u8,
    max_rtt_cap: Option<f64>,
    max_sd_cap: Option<f64>,
    stddev_weight: f32,
) -> HashMap<(String, IpAddr), Vec<routes::ComposedRoute>> {
    let mut out: HashMap<(String, IpAddr), Vec<routes::ComposedRoute>> =
        HashMap::with_capacity(baselines.len());
    for (a_id, b_ip, b_id, _direct_was_substituted, _direct) in baselines {
        let a_endpoint = Endpoint::Agent { id: a_id.clone() };
        let b_endpoint = Endpoint::Agent { id: b_id.clone() };
        let non_ab_pool: Vec<Endpoint> = agents
            .iter()
            .filter(|a| a.agent_id != *a_id && a.agent_id != *b_id)
            .map(|a| Endpoint::CandidateIp { ip: a.ip })
            .collect();
        let alts = routes::enumerate_routes(
            leg_lookup,
            &a_endpoint,
            &b_endpoint,
            &non_ab_pool,
            max_hops,
            max_rtt_cap,
            max_sd_cap,
            stddev_weight,
        );
        out.insert((a_id.clone(), *b_ip), alts);
    }
    out
}

/// Derive the headline `avg_improvement_ms` from per-candidate
/// aggregates rather than re-iterating `pair_details`.
///
/// With storage filters in play, `pair_details` is a *subset* of
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
///
/// # Performance model
///
/// The naive form ran `routes::enumerate_routes` once per `(X, A, B)`
/// triple — quadratic in the agent count and re-enumerating the same
/// legs ~1.3B times on a real ~50-agent roster with `max_hops == 2`.
/// Instead we pre-compute three layers of leg/route caches once per
/// evaluator pass and compose only X-containing routes inline:
///
/// 1. **Per-(A,B) mesh-neighbor leg caches** (`mesh_from`, `mesh_to`) —
///    every leg A→Y / Y→B where Y is another mesh agent, keyed off A
///    and B. Built once.
/// 2. **Per-X leg caches** (`XLegCache`) — A→X / X→B / X→Y / Y→X legs
///    for the current X. Built once per X candidate.
/// 3. **Per-(A,B) alt-route cache** (`alt_routes_per_baseline`) — only
///    built for Optimization mode; lets the qualify predicate check
///    cached A→B alt routes by removing the per-(X,A,B) re-enumeration.
///
/// Route composition in the hot loop is therefore reduced to a handful
/// of `HashMap::get` calls per (X, A, B) plus the arithmetic for
/// `compose_one_hop` / `compose_two_hop` re-implemented inline. The
/// composition math is identical to `routes::compose_*` — see those
/// functions for the source of truth.
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
    // Diversity / Optimization use the full roster identical to
    // `inputs.agents` today, but route the LegLookup build through the
    // explicit roster field so a future divergence (e.g. limiting the
    // baseline pair set) does not silently break IP↔agent identity.
    let leg_lookup = LegLookup::build(&inputs.measurements, &inputs.roster);
    let baselines = collect_baselines(&inputs.agents, &by_pair, &leg_lookup);
    if baselines.is_empty() {
        return Err(EvalError::NoBaseline);
    }

    let candidates: HashSet<IpAddr> = inputs
        .measurements
        .iter()
        .map(|m| m.destination_ip)
        .collect();

    let total_baseline = baselines.len() as i32;
    let mut candidate_rows: Vec<EvaluationCandidateDto> = Vec::new();
    let mut pair_details_per_candidate: Vec<PairDetailsForCandidate> = Vec::new();
    let mut unqualified_reasons: BTreeMap<String, String> = BTreeMap::new();

    // Eligibility / storage knobs. All four are [`Option<f64>`] — `None`
    // means "knob unset; gate is open".
    let max_rtt_cap = inputs.max_transit_rtt_ms;
    let max_sd_cap = inputs.max_transit_stddev_ms;
    let min_imp_ms = inputs.min_improvement_ms;
    let min_imp_ratio = inputs.min_improvement_ratio;

    // Clamp max_hops to [0, 2] (validated upstream, but defence-in-depth).
    let max_hops = inputs.max_hops.clamp(0, 2) as u8;
    let stddev_weight = inputs.stddev_weight;

    // ── Cache 1 & 2: mesh-neighbor legs keyed off A and B ─────────────
    //
    // `mesh_from[a_id]` lists every leg A→Y (for mesh agent Y ≠ A) that
    // `LegLookup` returns `Found` for. Same shape for `mesh_to[b_ip]`:
    // legs Y→B where Y is a mesh agent ≠ the agent at `b_ip`.
    //
    // Entries are pushed in `inputs.agents` iteration order so 2-hop
    // tie-breaks in the inline composer match the order
    // `routes::enumerate_routes` would have produced (which iterates
    // the pool linearly).
    let mut mesh_from: HashMap<String, Vec<(Endpoint, LegMeasurement)>> =
        HashMap::with_capacity(inputs.agents.len());
    let mut mesh_to: HashMap<IpAddr, Vec<(Endpoint, LegMeasurement)>> =
        HashMap::with_capacity(inputs.agents.len());
    for pivot in &inputs.agents {
        // Treat the pivot as A: collect every A→Y leg.
        let pivot_endpoint = Endpoint::Agent {
            id: pivot.agent_id.clone(),
        };
        let mut from_legs: Vec<(Endpoint, LegMeasurement)> =
            Vec::with_capacity(inputs.agents.len().saturating_sub(1));
        // Also treat the pivot as B: collect every Y→B leg.
        let mut to_legs: Vec<(Endpoint, LegMeasurement)> =
            Vec::with_capacity(inputs.agents.len().saturating_sub(1));
        for y in &inputs.agents {
            if y.agent_id == pivot.agent_id {
                continue;
            }
            let y_endpoint = Endpoint::Agent {
                id: y.agent_id.clone(),
            };
            if let Some(leg) = lookup_leg(&leg_lookup, &pivot_endpoint, &y_endpoint) {
                from_legs.push((y_endpoint.clone(), leg));
            }
            if let Some(leg) = lookup_leg(&leg_lookup, &y_endpoint, &pivot_endpoint) {
                to_legs.push((y_endpoint, leg));
            }
        }
        mesh_from.insert(pivot.agent_id.clone(), from_legs);
        mesh_to.insert(pivot.ip, to_legs);
    }

    // ── Cache 4: Optimization-mode alt routes per (A, B) ─────────────
    //
    // Only built for Optimization mode; Diversity mode never calls the
    // qualify predicate that consumes it.
    let alt_routes_per_baseline = if inputs.mode == EvaluationMode::Optimization {
        build_alt_routes_per_baseline(
            &leg_lookup,
            &inputs.agents,
            &baselines,
            max_hops,
            max_rtt_cap,
            max_sd_cap,
            stddev_weight,
        )
    } else {
        HashMap::new()
    };

    for x_ip in &candidates {
        let has_from_any = inputs
            .agents
            .iter()
            .any(|a| by_pair.contains_key(&(a.agent_id.clone(), *x_ip)));
        if !has_from_any {
            continue;
        }

        let x_endpoint = Endpoint::CandidateIp { ip: *x_ip };

        // ── Cache 3: per-X leg caches ────────────────────────────────
        let x_cache = build_x_leg_cache(&leg_lookup, &inputs.agents, *x_ip, max_hops);

        let mut pair_details: Vec<EvaluationPairDetailDto> = Vec::new();
        let mut qualifying_legs: Vec<(String, String)> = Vec::new();
        let mut pairs_improved = 0i32;
        let mut pairs_total_considered = 0i32;
        let mut improvements: Vec<f32> = Vec::new();
        let mut compound_losses: Vec<f32> = Vec::new();
        let mut any_threshold_fail = false;

        for (a_id, b_ip, b_id, direct_was_substituted, direct) in &baselines {
            // Exclude degenerate triples where X is A or B.
            if Some(*x_ip) == agent_by_id.get(a_id).copied() {
                continue;
            }
            if *x_ip == *b_ip {
                continue;
            }

            // `direct` is the measurement chosen by `collect_baselines` —
            // forward A→B when usable, otherwise the symmetry-substituted
            // reverse B→A row. Reading from this borrow keeps the persisted
            // direct_rtt / direct_stddev / direct_loss_ratio / direct_source
            // consistent with `direct_was_substituted`.
            let Some(direct_rtt) = direct.latency_avg_ms else {
                continue;
            };
            let direct_stddev = direct.latency_stddev_ms.unwrap_or(0.0);
            let direct_loss_ratio = direct.loss_ratio;

            // Compose only X-containing routes inline.
            // The original `routes::enumerate_routes` + `with_x` filter
            // explored every non-X intermediary too; here we only consider
            // shapes that put X somewhere in the chain.
            let best_x_route = best_x_containing_route(
                &x_cache,
                &x_endpoint,
                *x_ip,
                a_id,
                *b_ip,
                b_id,
                &mesh_from,
                &mesh_to,
                &agent_by_id,
                max_hops,
                max_rtt_cap,
                max_sd_cap,
                stddev_weight,
            );

            let Some(best_x_route) = best_x_route else {
                // No X-containing route is reachable.
                continue;
            };

            let transit_rtt = best_x_route.rtt_ms;
            let transit_stddev = best_x_route.stddev_ms;
            let transit_penalty = stddev_weight * transit_stddev;
            let compound_loss_ratio = best_x_route.loss_ratio;
            let direct_penalty = stddev_weight * direct_stddev;
            let improvement_ms = direct_rtt - transit_rtt - (transit_penalty - direct_penalty);

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
                extract_ax_xb_meta(&best_x_route, &x_endpoint);

            let loss_ok = compound_loss_ratio <= inputs.loss_threshold_ratio
                && direct_loss_ratio <= inputs.loss_threshold_ratio;
            if !loss_ok {
                any_threshold_fail = true;
                continue;
            }

            let qualifies = match inputs.mode {
                EvaluationMode::Diversity => improvement_ms > 0.0,
                EvaluationMode::Optimization => {
                    if improvement_ms <= 0.0 {
                        false
                    } else {
                        match alt_routes_per_baseline.get(&(a_id.clone(), *b_ip)) {
                            // No baseline entry — degenerate; treat as
                            // no alt available, so X qualifies.
                            None => true,
                            Some(alts) => {
                                let transit_score = transit_rtt + transit_penalty;
                                let x_endpoint_match = Endpoint::CandidateIp { ip: *x_ip };
                                // Reject X when any non-X alt route is
                                // at least as good as the X route
                                // (penalised RTT, `>=` collapses ties
                                // to "non-X preferred" per spec §2.4).
                                // "Non-X" excludes routes whose
                                // intermediaries contain
                                // `CandidateIp(x_ip)`.
                                !alts.iter().any(|r| {
                                    let alt_score = r.rtt_ms + stddev_weight * r.stddev_ms;
                                    transit_score >= alt_score
                                        && !r.intermediaries.iter().any(|e| e == &x_endpoint_match)
                                })
                            }
                        }
                    }
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
                // only this A→B row can be VM-sourced. The `/evaluate`
                // handler synthesizes a [`DirectSource::VmContinuous`]
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
        // results). With storage filters the distinction between
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
        let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
        EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),
                m("a", "203.0.113.7", 120.0, 8.0, 0.0),
                m("b", "203.0.113.7", 121.0, 8.0, 0.0),
            ],
            roster: agents.clone(),
            agents,
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
        let agents = vec![agent("a", "10.0.0.1")];
        let i = EvaluationInputs {
            measurements: vec![m("a", "203.0.113.7", 120.0, 8.0, 0.0)],
            roster: agents.clone(),
            agents,
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
        let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
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
            roster: agents.clone(),
            agents,
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

    /// Symmetry-substituted A→B baseline must drive a real triple — the
    /// loop must read direct RTT/loss/stddev from the substituted (reverse)
    /// measurement and persist `direct_was_substituted=true`. Regression
    /// for the bug where `evaluate_triple` re-queried `by_pair[(a_id, b_ip)]`
    /// and silently dropped the triple when only the reverse direction
    /// existed.
    #[test]
    fn diversity_uses_reverse_substituted_direct_baseline() {
        // A=10.0.0.1, B=10.0.0.2; X=203.0.113.7 (non-mesh candidate).
        // No forward A→B; only B→A reverse (15ms). LegLookup substitutes.
        // Transit: A→X (10ms) + X→B via B→X reverse (10ms) = 20ms < 15ms baseline?
        // Actually: direct via reverse (B→A) = 60ms; transit A→X→B = 40ms.
        // Improvement = 60 - 40 = 20ms (stddev 0 throughout) → qualifies.
        let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
        let i = EvaluationInputs {
            measurements: vec![
                // No forward A→B. Reverse B→A acts as the substituted direct.
                m("b", "10.0.0.1", 60.0, 0.0, 0.0),
                // Forward A→X for the A→X leg.
                m("a", "203.0.113.7", 18.0, 0.0, 0.0),
                // B→X serves both as a candidate signal and (via reverse) X→B.
                m("b", "203.0.113.7", 22.0, 0.0, 0.0),
            ],
            roster: agents.clone(),
            agents,
            candidate_ips: vec![ip("203.0.113.7")],
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
        let out = triple(evaluate(i).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("candidate X must appear when symmetry-substituted baseline drives a triple");

        assert!(
            cand.pairs_total_considered >= 1,
            "substituted baseline must reach the storage filter — instead got pairs_total_considered=0: {cand:?}"
        );

        // Locate the (A,B) pair_detail row and verify the substituted
        // baseline's RTT / loss flowed through to persistence.
        let ab_pair = out
            .pair_details_by_candidate
            .iter()
            .flat_map(|p| p.pair_details.iter())
            .find(|d| d.source_agent_id == "a" && d.destination_agent_id == "b")
            .expect("(A,B) pair_detail must exist for the X candidate");

        assert_eq!(
            ab_pair.direct_was_substituted,
            Some(true),
            "direct_was_substituted must be true when only B→A reverse exists: {ab_pair:?}"
        );
        assert!(
            (ab_pair.direct_rtt_ms - 60.0).abs() < 1e-3,
            "direct_rtt_ms must reflect the reverse measurement (60ms), not 0/forward: {ab_pair:?}"
        );
        assert!(
            ab_pair.direct_loss_ratio.abs() < 1e-3,
            "direct_loss_ratio must come from the reverse row (0.0): {ab_pair:?}"
        );
    }

    #[test]
    fn diversity_qualifies_transit_that_beats_direct() {
        // `inputs_basic` seeds A→B forward + A→X + B→X. The reverse direction
        // B→A is symmetry-substituted from A→B, producing a second baseline
        // (B, A); X transits beat both directions, so `pairs_improved == 2`.
        let out = triple(evaluate(inputs_basic(EvaluationMode::Diversity)).unwrap());
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("candidate present");
        assert_eq!(cand.pairs_improved, 2);
        assert!(
            cand.avg_improvement_ms.unwrap() > 0.0,
            "transit (120+121=241) beats direct (318) in both directions"
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
        let agents = vec![
            agent("a", "10.0.0.1"),
            agent("b", "10.0.0.2"),
            agent("y", "10.0.0.3"),
        ];
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),   // A→B baseline
                m("a", "203.0.113.7", 120.0, 8.0, 0.0), // A→X
                m("b", "203.0.113.7", 121.0, 8.0, 0.0), // B→X (sym-approx X→B)
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),    // A→Y
                m("b", "10.0.0.3", 80.0, 5.0, 0.0),     // B→Y (sym-approx Y→B)
            ],
            roster: agents.clone(),
            agents,
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
        let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
        let inputs = EvaluationInputs {
            measurements: vec![
                a_b_baseline,
                m("a", "203.0.113.7", 120.0, 8.0, 0.0),
                m("b", "203.0.113.7", 121.0, 8.0, 0.0),
            ],
            roster: agents.clone(),
            agents,
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
        let agents = vec![
            agent("a", "10.0.0.1"),
            agent("b", "10.0.0.2"),
            agent("c", "10.0.0.3"),
        ];
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),
                m("b", "10.0.0.3", 130.0, 5.0, 0.0),
            ],
            roster: agents.clone(),
            agents,
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
