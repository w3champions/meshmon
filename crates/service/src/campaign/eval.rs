//! Pure-function core for the campaign evaluator. No DB, no IO.
//!
//! `evaluate()` is the single entry point. Its caller (repo.rs) builds
//! `EvaluationInputs` from DB queries and persists the returned
//! `EvaluationOutputs` through
//! [`crate::campaign::evaluation_repo::persist_evaluation`], which
//! fans the structure out across `campaign_evaluations` +
//! `campaign_evaluation_{candidates, pair_details,
//! unqualified_reasons}`.

use crate::campaign::dto::{EvaluationCandidateDto, EvaluationPairDetailDto, EvaluationResultsDto};
use crate::campaign::model::{DirectSource, EvaluationMode};
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
}

/// Full input bundle for one evaluator pass.
#[derive(Debug, Clone)]
pub struct EvaluationInputs {
    /// All campaign-kind measurements the evaluator should consider.
    pub measurements: Vec<AttributedMeasurement>,
    /// Agent roster the campaign pulled its sources from.
    pub agents: Vec<AgentRow>,
    /// IP → catalogue enrichment for candidate rendering.
    pub enrichment: HashMap<IpAddr, CatalogueLookup>,
    /// Loss ceiling (fraction); triples exceeding this never qualify.
    pub loss_threshold_ratio: f32,
    /// Weight applied to RTT stddev when computing the improvement score.
    pub stddev_weight: f32,
    /// Diversity vs. Optimization arbitration (spec §2.4).
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
}

/// Full evaluator output: summary counters plus the JSONB-bound DTO.
#[derive(Debug, Clone)]
pub struct EvaluationOutputs {
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
    /// `campaign_evaluation_candidates`,
    /// `campaign_evaluation_pair_details`, and
    /// `campaign_evaluation_unqualified_reasons` child tables.
    pub results: EvaluationResultsDto,
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
/// measurement.
///
/// A row with no RTT (e.g. a 100 %-loss "success") cannot participate
/// in scoring downstream, so counting it would inflate
/// `baseline_pair_count` and suppress the `NoBaseline → 422` error
/// when every observed baseline is RTT-less.
fn collect_baselines(
    agents: &[AgentRow],
    by_pair: &HashMap<(String, IpAddr), &AttributedMeasurement>,
) -> Vec<(String, IpAddr, String)> {
    let mut baselines: Vec<(String, IpAddr, String)> = Vec::new();
    for a in agents {
        for b in agents {
            if a.agent_id == b.agent_id {
                continue;
            }
            if by_pair
                .get(&(a.agent_id.clone(), b.ip))
                .is_some_and(|m| m.latency_avg_ms.is_some())
            {
                baselines.push((a.agent_id.clone(), b.ip, b.agent_id.clone()));
            }
        }
    }
    baselines
}

/// One A→X transit leg surviving the L1/L2/L3 cartesian-product prune.
///
/// Built once per candidate from the baseline source agent set. The
/// inner triple loop indexes these by `a_id` to look up each baseline's
/// matching A→X measurement without re-scanning `by_pair`.
#[derive(Debug)]
struct AxLeg<'m> {
    /// Baseline source agent id.
    a_id: String,
    /// `latency_avg_ms` (extracted up-front so the L1/L2/L3 reductions
    /// stay in `f32` arithmetic without re-unwrapping `Option`).
    rtt_ms: f32,
    /// `latency_stddev_ms.unwrap_or(0.0)` — same rationale.
    stddev_ms: f32,
    /// Source `AttributedMeasurement`. Carried through so the inner
    /// loop can stamp `mtr_measurement_id_ax` and `loss_ratio`.
    meas: &'m AttributedMeasurement,
}

/// One X→B transit leg surviving the L1/L2/L3 cartesian-product prune.
///
/// Mirrors [`AxLeg`] for the destination side. Built using the
/// source-symmetric `by_pair[(b_id, x_ip)]` lookup — see the
/// `symmetry-approx` comment in the inner triple loop.
#[derive(Debug)]
struct XbLeg<'m> {
    /// Baseline destination agent id.
    b_id: String,
    /// `latency_avg_ms` (see [`AxLeg::rtt_ms`]).
    rtt_ms: f32,
    /// `latency_stddev_ms.unwrap_or(0.0)`.
    stddev_ms: f32,
    /// Source `AttributedMeasurement` — carried for
    /// `mtr_measurement_id_xb` + `loss_ratio` in the inner loop.
    meas: &'m AttributedMeasurement,
}

/// Build per-candidate AX/XB leg lists from the baseline set.
///
/// AX = "A→X" for some baseline source agent A; XB = "X→B" with B as a
/// baseline destination agent. The XB direction uses the source-
/// symmetric `by_pair[(b_id, x_ip)]` lookup — see the `symmetry-approx`
/// comment in [`evaluate`]'s inner triple loop. Pathological self-
/// transits (X equals A's IP, or X equals B's IP) are skipped to keep
/// degenerate `0.0`/`NaN` rows out of the L1 minima.
fn build_leg_sets<'m>(
    x_ip: IpAddr,
    baselines: &[(String, IpAddr, String)],
    by_pair: &HashMap<(String, IpAddr), &'m AttributedMeasurement>,
    agent_by_id: &HashMap<String, IpAddr>,
) -> (Vec<AxLeg<'m>>, Vec<XbLeg<'m>>) {
    let mut ax_legs: Vec<AxLeg<'m>> = Vec::new();
    let mut xb_legs: Vec<XbLeg<'m>> = Vec::new();
    let mut seen_ax: HashSet<&String> = HashSet::new();
    let mut seen_xb: HashSet<&String> = HashSet::new();
    for (a_id, b_ip, b_id) in baselines {
        if Some(x_ip) == agent_by_id.get(a_id).copied() || x_ip == *b_ip {
            continue;
        }
        if seen_ax.insert(a_id) {
            if let Some(meas) = by_pair.get(&(a_id.clone(), x_ip)).copied() {
                if let Some(rtt) = meas.latency_avg_ms {
                    ax_legs.push(AxLeg {
                        a_id: a_id.clone(),
                        rtt_ms: rtt,
                        stddev_ms: meas.latency_stddev_ms.unwrap_or(0.0),
                        meas,
                    });
                }
            }
        }
        if seen_xb.insert(b_id) {
            if let Some(meas) = by_pair.get(&(b_id.clone(), x_ip)).copied() {
                if let Some(rtt) = meas.latency_avg_ms {
                    xb_legs.push(XbLeg {
                        b_id: b_id.clone(),
                        rtt_ms: rtt,
                        stddev_ms: meas.latency_stddev_ms.unwrap_or(0.0),
                        meas,
                    });
                }
            }
        }
    }
    (ax_legs, xb_legs)
}

/// Apply the L1/L2/L3 cartesian-product prune for the eligibility caps.
///
/// Returns `None` when L2/L3 (or post-L1 emptiness) drops the candidate
/// entirely; otherwise returns the surviving leg sets. See architecture
/// #4 (monotonic composition + L1/L2/L3 pruning) for the reasoning.
///
/// * **L1** — single-leg cap: every leg's RTT (and stddev) must itself
///   be ≤ the cap; no composition `a + b ≤ cap` (RTT) or
///   `sqrt(a² + b²) ≤ cap` (stddev) can hold otherwise.
/// * **L2** — candidate-level early termination: if even the
///   `(min_ax, min_xb)` pairing exceeds the cap, no triple under this
///   candidate can satisfy it — drop the candidate.
/// * **L3** — tight bidirectional pre-filter: once L2 admits the
///   candidate, an A→X leg must satisfy `rtt_ax + min_xb ≤ cap` and
///   likewise for X→B. For stddev the algebra rearranges
///   `sqrt(l.stddev² + min_other²) ≤ cap` to
///   `l.stddev² ≤ cap² − min_other²`; a negative RHS means no leg can
///   satisfy and the candidate drops.
///
/// The in-loop guard inside the triple loop is belt-and-braces against
/// the residual case where L3 admits a leg that pairs with the
/// *minimum* on the opposite side but not with a non-minimum partner.
fn apply_l1_l2_l3_pruning<'m>(
    mut ax_legs: Vec<AxLeg<'m>>,
    mut xb_legs: Vec<XbLeg<'m>>,
    max_rtt_cap: Option<f64>,
    max_sd_cap: Option<f64>,
) -> Option<(Vec<AxLeg<'m>>, Vec<XbLeg<'m>>)> {
    // L1 — single-leg cap on RTT and stddev.
    if let Some(cap) = max_rtt_cap {
        let cap32 = cap as f32;
        ax_legs.retain(|l| l.rtt_ms <= cap32);
        xb_legs.retain(|l| l.rtt_ms <= cap32);
    }
    if let Some(cap) = max_sd_cap {
        let cap32 = cap as f32;
        ax_legs.retain(|l| l.stddev_ms <= cap32);
        xb_legs.retain(|l| l.stddev_ms <= cap32);
    }
    if ax_legs.is_empty() || xb_legs.is_empty() {
        return None;
    }

    // Compute the minima up-front; they feed L2 + L3.
    let min_ax_rtt = ax_legs
        .iter()
        .map(|l| l.rtt_ms)
        .reduce(f32::min)
        .expect("ax_legs non-empty");
    let min_xb_rtt = xb_legs
        .iter()
        .map(|l| l.rtt_ms)
        .reduce(f32::min)
        .expect("xb_legs non-empty");
    let min_ax_sd = ax_legs
        .iter()
        .map(|l| l.stddev_ms)
        .reduce(f32::min)
        .expect("ax_legs non-empty");
    let min_xb_sd = xb_legs
        .iter()
        .map(|l| l.stddev_ms)
        .reduce(f32::min)
        .expect("xb_legs non-empty");

    // L2 — candidate-level early termination.
    if let Some(cap) = max_rtt_cap {
        if (min_ax_rtt + min_xb_rtt) as f64 > cap {
            return None;
        }
    }
    if let Some(cap) = max_sd_cap {
        let composed = ((min_ax_sd * min_ax_sd + min_xb_sd * min_xb_sd) as f64).sqrt();
        if composed > cap {
            return None;
        }
    }

    // L3 — tight bidirectional pre-filter (RTT).
    if let Some(cap) = max_rtt_cap {
        let cap32 = cap as f32;
        ax_legs.retain(|l| l.rtt_ms + min_xb_rtt <= cap32);
        xb_legs.retain(|l| l.rtt_ms + min_ax_rtt <= cap32);
    }
    // L3 — tight bidirectional pre-filter (stddev). Solve
    // `sqrt(l.stddev² + min_other²) ≤ cap` ⇒
    // `l.stddev² ≤ cap² − min_other²`. When the RHS is negative, no
    // leg can satisfy — drop the candidate.
    if let Some(cap) = max_sd_cap {
        let cap_sq = cap * cap;
        let xb_min_sq = (min_xb_sd as f64).powi(2);
        let ax_min_sq = (min_ax_sd as f64).powi(2);
        let ax_budget = cap_sq - xb_min_sq;
        let xb_budget = cap_sq - ax_min_sq;
        if ax_budget < 0.0 || xb_budget < 0.0 {
            return None;
        }
        ax_legs.retain(|l| (l.stddev_ms as f64).powi(2) <= ax_budget);
        xb_legs.retain(|l| (l.stddev_ms as f64).powi(2) <= xb_budget);
    }
    if ax_legs.is_empty() || xb_legs.is_empty() {
        return None;
    }
    Some((ax_legs, xb_legs))
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

/// Scalar score for one `(A, X, B)` triple — composed transit RTT /
/// stddev / improvement plus the compound loss ratio.
///
/// Pure derivation; the caller still gates eligibility, qualification,
/// and storage on the returned [`TripleScore`]. Per spec §2.4
/// (`docs/superpowers/specs/2026-04-19-campaigns-04-evaluation-and-results.md`):
///
/// * `transit_rtt = rtt_ax + rtt_xb`
/// * `transit_stddev = sqrt(stddev_ax² + stddev_xb²)`
/// * `improvement = direct_rtt − transit_rtt − stddev_weight ×
///   (transit_stddev − direct_stddev)`
/// * `compound_loss = 1 − (1 − loss_ax) × (1 − loss_xb)`
struct TripleScore {
    transit_rtt: f32,
    transit_stddev: f32,
    transit_penalty: f32,
    improvement_ms: f32,
    compound_loss_ratio: f32,
    direct_loss_ratio: f32,
    direct_stddev: f32,
}

fn score_triple(
    direct: &AttributedMeasurement,
    direct_rtt: f32,
    a_to_x_leg: &AxLeg<'_>,
    x_to_b_leg: &XbLeg<'_>,
    stddev_weight: f32,
) -> TripleScore {
    let direct_stddev = direct.latency_stddev_ms.unwrap_or(0.0);
    let ax_stddev = a_to_x_leg.stddev_ms;
    let xb_stddev = x_to_b_leg.stddev_ms;

    let transit_rtt = a_to_x_leg.rtt_ms + x_to_b_leg.rtt_ms;
    let transit_stddev = (ax_stddev * ax_stddev + xb_stddev * xb_stddev).sqrt();
    let direct_penalty = stddev_weight * direct_stddev;
    let transit_penalty = stddev_weight * transit_stddev;
    let improvement_ms = direct_rtt - transit_rtt - (transit_penalty - direct_penalty);

    let compound_loss_ratio =
        1.0 - (1.0 - a_to_x_leg.meas.loss_ratio) * (1.0 - x_to_b_leg.meas.loss_ratio);

    TripleScore {
        transit_rtt,
        transit_stddev,
        transit_penalty,
        improvement_ms,
        compound_loss_ratio,
        direct_loss_ratio: direct.loss_ratio,
        direct_stddev,
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
    pair_details: Vec<EvaluationPairDetailDto>,
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
        // Counters reflect the post-eligibility set (after L1+L2+L3
        // and the in-loop cap-check) but include rows the storage
        // filter dropped — see architecture #2 (eligibility vs storage).
        pairs_total_considered,
        avg_improvement_ms,
        avg_loss_ratio,
        composite_score,
        pair_details,
        hostname: None,
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
        .partial_cmp(&a.composite_score)
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

/// Optimization-mode qualify predicate: does the X transit beat every
/// other mesh agent Y as an alternative transit for the same `(A, B)`?
///
/// Uses the same source-symmetric `(A→Y, B→Y)` legs convention the X
/// path uses, so Y must be reachable from both baseline endpoints
/// through the campaign's measurements. The tiebreaker rejects X when
/// any Y *ties or beats* X (`>=`) so an exact tie collapses to "Y
/// preferred", per spec §2.4.
///
/// Returns `false` immediately when `improvement_ms ≤ 0` (the candidate
/// can't improve a non-improving triple). The caller still passes
/// `transit_rtt + transit_penalty` pre-composed so this helper stays a
/// pure scalar comparison and avoids reaching back into the legs.
fn qualifies_under_optimization(
    a_id: &str,
    b_id: &str,
    transit_score: f32,
    improvement_ms: f32,
    agents: &[AgentRow],
    by_pair: &HashMap<(String, IpAddr), &AttributedMeasurement>,
    stddev_weight: f32,
) -> bool {
    if improvement_ms <= 0.0 {
        return false;
    }
    for y in agents {
        if y.agent_id == a_id || y.agent_id == b_id {
            continue;
        }
        // Same symmetry convention as the X-transit legs: source-is-
        // baseline-agent, destination-is-transit. `a→y` is "A pings
        // Y"; `b→y` is "B pings Y" — the symmetry-approx for Y→B.
        // Using `y→b` here would require Y to be a source agent in
        // the campaign, which is a stricter invariant than what the
        // baseline scan assumes.
        let ay = by_pair.get(&(a_id.to_owned(), y.ip));
        let by = by_pair.get(&(b_id.to_owned(), y.ip));
        if let (Some(ay), Some(by)) = (ay, by) {
            let Some(ay_rtt) = ay.latency_avg_ms else {
                continue;
            };
            let Some(by_rtt) = by.latency_avg_ms else {
                continue;
            };
            let ay_stddev = ay.latency_stddev_ms.unwrap_or(0.0);
            let by_stddev = by.latency_stddev_ms.unwrap_or(0.0);
            let ty_rtt = ay_rtt + by_rtt;
            let ty_stddev_pen =
                stddev_weight * (ay_stddev * ay_stddev + by_stddev * by_stddev).sqrt();
            // Tiebreaker: reject X when any Y ties X exactly (cf. spec §2.4).
            if transit_score >= ty_rtt + ty_stddev_pen {
                return false;
            }
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
/// §2.4 for the formula.
pub fn evaluate(inputs: EvaluationInputs) -> Result<EvaluationOutputs, EvalError> {
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
    let baselines = collect_baselines(&inputs.agents, &by_pair);
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
    let mut unqualified_reasons: BTreeMap<String, String> = BTreeMap::new();

    // T55 eligibility / storage knobs (snake-cased locally for the
    // L1/L2/L3 pruning + inner-loop guards below). All four are
    // [`Option<f64>`] — `None` means "knob unset; gate is open".
    let max_rtt_cap = inputs.max_transit_rtt_ms;
    let max_sd_cap = inputs.max_transit_stddev_ms;
    let min_imp_ms = inputs.min_improvement_ms;
    let min_imp_ratio = inputs.min_improvement_ratio;

    for x_ip in &candidates {
        let has_from_any = inputs
            .agents
            .iter()
            .any(|a| by_pair.contains_key(&(a.agent_id.clone(), *x_ip)));
        if !has_from_any {
            continue;
        }

        // T55 cartesian-product pruning (architecture #4): build the
        // per-candidate AX/XB leg sets, then fold L1/L2/L3 over them.
        // `apply_l1_l2_l3_pruning` returns `None` when L2/L3 (or post-
        // L1 emptiness) drops the candidate entirely — at which point
        // we skip without writing a row.
        let (ax_legs, xb_legs) = build_leg_sets(*x_ip, &baselines, &by_pair, &agent_by_id);
        let Some((ax_legs, xb_legs)) =
            apply_l1_l2_l3_pruning(ax_legs, xb_legs, max_rtt_cap, max_sd_cap)
        else {
            continue;
        };

        // Index the surviving legs back by id so the inner triple
        // loop can look them up cheaply.
        let ax_by_id: HashMap<&str, &AxLeg<'_>> =
            ax_legs.iter().map(|l| (l.a_id.as_str(), l)).collect();
        let xb_by_id: HashMap<&str, &XbLeg<'_>> =
            xb_legs.iter().map(|l| (l.b_id.as_str(), l)).collect();

        let mut pair_details: Vec<EvaluationPairDetailDto> = Vec::new();
        let mut pairs_improved = 0i32;
        let mut pairs_total_considered = 0i32;
        let mut improvements: Vec<f32> = Vec::new();
        let mut compound_losses: Vec<f32> = Vec::new();
        let mut any_threshold_fail = false;

        for (a_id, b_ip, b_id) in &baselines {
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
            let Some(a_to_x_leg) = ax_by_id.get(a_id.as_str()) else {
                continue;
            };
            let Some(x_to_b_leg) = xb_by_id.get(b_id.as_str()) else {
                continue;
            };
            let a_to_x = a_to_x_leg.meas;
            let x_to_b = x_to_b_leg.meas;

            let Some(direct_rtt) = direct.latency_avg_ms else {
                continue;
            };
            let TripleScore {
                transit_rtt,
                transit_stddev,
                transit_penalty,
                improvement_ms,
                compound_loss_ratio,
                direct_loss_ratio,
                direct_stddev,
            } = score_triple(
                direct,
                direct_rtt,
                a_to_x_leg,
                x_to_b_leg,
                inputs.stddev_weight,
            );

            let loss_ok = compound_loss_ratio <= inputs.loss_threshold_ratio
                && direct_loss_ratio <= inputs.loss_threshold_ratio;
            if !loss_ok {
                any_threshold_fail = true;
                continue;
            }

            // Eligibility — last line of defence after L1+L2+L3.
            // L3 retains legs that pair with the *minimum* on the
            // opposite side; a non-minimum partner can still violate
            // the combined cap, so this in-loop guard is load-bearing.
            // NaN/Inf transit values would not be caught by this `>`
            // check (NaN comparisons return false in IEEE 754); they
            // cannot reach this point in practice because
            // `build_leg_sets` requires `latency_avg_ms.is_some()` from
            // the DB before pushing a leg, and the inner loop already
            // short-circuits on `direct.latency_avg_ms.is_none()`.
            if let Some(cap) = max_rtt_cap {
                if (transit_rtt as f64) > cap {
                    continue;
                }
            }
            if let Some(cap) = max_sd_cap {
                if (transit_stddev as f64) > cap {
                    continue;
                }
            }

            let qualifies = match inputs.mode {
                EvaluationMode::Diversity => improvement_ms > 0.0,
                EvaluationMode::Optimization => qualifies_under_optimization(
                    a_id,
                    b_id,
                    transit_rtt + transit_penalty,
                    improvement_ms,
                    &inputs.agents,
                    &by_pair,
                    inputs.stddev_weight,
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
                mtr_measurement_id_ax: a_to_x.mtr_measurement_id,
                mtr_measurement_id_xb: x_to_b.mtr_measurement_id,
                destination_hostname: None,
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
            pair_details,
            total_baseline,
            &inputs.enrichment,
            &agent_by_ip,
        ));
    }

    candidate_rows.sort_by(candidate_order);

    let candidates_total = candidate_rows.len() as i32;
    let candidates_good = candidate_rows
        .iter()
        .filter(|c| c.pairs_improved >= 1)
        .count() as i32;
    // Derive the headline `avg_improvement_ms` from the per-candidate
    // aggregates — see [`aggregate_headline_avg`] for the rationale
    // (architecture #2 / architecture #4).
    let avg_improvement_ms = aggregate_headline_avg(&candidate_rows);

    Ok(EvaluationOutputs {
        baseline_pair_count: total_baseline,
        candidates_total,
        candidates_good,
        avg_improvement_ms,
        results: EvaluationResultsDto {
            candidates: candidate_rows,
            unqualified_reasons,
        },
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
        }
    }

    #[test]
    fn triple_excludes_x_equals_a_or_b() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements.push(m("a", "10.0.0.2", 318.0, 24.0, 0.0));
        i.measurements.push(m("b", "10.0.0.2", 0.0, 0.0, 0.0));
        let out = evaluate(i).unwrap();
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
        };
        let err = evaluate(i).expect_err("RTT-less baseline must not register");
        assert!(matches!(err, EvalError::NoBaseline));
    }

    #[test]
    fn diversity_qualifies_transit_that_beats_direct() {
        let out = evaluate(inputs_basic(EvaluationMode::Diversity)).unwrap();
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
        let out = evaluate(i).unwrap();
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
        let out = evaluate(i).unwrap();
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
        };
        let out = evaluate(inputs).unwrap();
        let x_cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("X must appear as a candidate (triple is fully measured)");
        // X's pair_details entry for (A,B) must be present (triple is
        // fully measured) but qualifies=false because Y beats X.
        let ab_detail = x_cand
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Diversity,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
        };
        let out = evaluate(inputs).unwrap();
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("candidate present");
        assert_eq!(
            cand.pair_details[0].direct_source,
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
            enrichment: Default::default(),
            loss_threshold_ratio: 0.02,
            stddev_weight: 1.0,
            mode: EvaluationMode::Diversity,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
        };
        let out = evaluate(inputs).unwrap();
        let cand = out
            .results
            .candidates
            .iter()
            .find(|c| c.destination_ip == "10.0.0.3")
            .expect("agent-as-candidate present");
        assert!(cand.is_mesh_member);
    }
}
