//! Pure-function core for the campaign evaluator. No DB, no IO.
//!
//! `evaluate()` is the single entry point. Its caller (repo.rs) builds
//! `EvaluationInputs` from DB queries and persists the returned
//! `EvaluationOutputs` into the `campaign_evaluations` table.

use crate::campaign::dto::{EvaluationCandidateDto, EvaluationPairDetailDto, EvaluationResultsDto};
use crate::campaign::model::EvaluationMode;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

/// One row from `measurements` attributed to the campaign. Only the
/// columns the evaluator reads are carried here — the DB layer filters
/// to `kind='campaign'` pairs before constructing this.
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
    /// Observed loss percentage on this leg (0.0–100.0).
    pub loss_pct: f32,
    /// FK into `measurements.id` for the MTR-bearing row, when available.
    pub mtr_measurement_id: Option<i64>,
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
    /// Loss ceiling (percent); triples exceeding this never qualify.
    pub loss_threshold_pct: f32,
    /// Weight applied to RTT stddev when computing the improvement score.
    pub stddev_weight: f32,
    /// Diversity vs. Optimization arbitration (spec §2.4).
    pub mode: EvaluationMode,
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
    /// Serialisable results payload, persisted verbatim into
    /// `campaign_evaluations.results`.
    pub results: EvaluationResultsDto,
}

/// Errors surfaced by [`evaluate`].
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// No agent→agent baseline pair was present in `measurements`; the
    /// evaluator has nothing to score against.
    #[error("no baseline routes; include at least one agent→agent pair")]
    NoBaseline,
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

    let mut by_pair: HashMap<(String, IpAddr), &AttributedMeasurement> = HashMap::new();
    for meas in &inputs.measurements {
        by_pair.insert((meas.source_agent_id.clone(), meas.destination_ip), meas);
    }

    // Baselines: (A, B) where both A and B are agents and A→B measurement exists.
    let mut baselines: Vec<(String, IpAddr, String)> = Vec::new();
    for a in &inputs.agents {
        for b in &inputs.agents {
            if a.agent_id == b.agent_id {
                continue;
            }
            if by_pair.contains_key(&(a.agent_id.clone(), b.ip)) {
                baselines.push((a.agent_id.clone(), b.ip, b.agent_id.clone()));
            }
        }
    }

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

    for x_ip in &candidates {
        let has_from_any = inputs
            .agents
            .iter()
            .any(|a| by_pair.contains_key(&(a.agent_id.clone(), *x_ip)));
        if !has_from_any {
            continue;
        }

        let mut pair_details: Vec<EvaluationPairDetailDto> = Vec::new();
        let mut pairs_improved = 0i32;
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
            let a_to_x = match by_pair.get(&(a_id.clone(), *x_ip)) {
                Some(m) => *m,
                None => continue,
            };
            let x_to_b = match by_pair.get(&(b_id.clone(), *x_ip)) {
                Some(m) => *m,
                None => continue,
            };

            let Some(direct_rtt) = direct.latency_avg_ms else {
                continue;
            };
            let Some(ax_rtt) = a_to_x.latency_avg_ms else {
                continue;
            };
            let Some(xb_rtt) = x_to_b.latency_avg_ms else {
                continue;
            };
            let direct_stddev = direct.latency_stddev_ms.unwrap_or(0.0);
            let ax_stddev = a_to_x.latency_stddev_ms.unwrap_or(0.0);
            let xb_stddev = x_to_b.latency_stddev_ms.unwrap_or(0.0);

            let direct_loss = direct.loss_pct;
            let compound_loss_frac =
                1.0 - (1.0 - a_to_x.loss_pct / 100.0) * (1.0 - x_to_b.loss_pct / 100.0);
            let compound_loss_pct = compound_loss_frac * 100.0;

            let transit_rtt = ax_rtt + xb_rtt;
            let transit_stddev = (ax_stddev * ax_stddev + xb_stddev * xb_stddev).sqrt();
            let direct_penalty = inputs.stddev_weight * direct_stddev;
            let transit_penalty = inputs.stddev_weight * transit_stddev;
            let improvement_ms = direct_rtt - transit_rtt - (transit_penalty - direct_penalty);

            let loss_ok = compound_loss_pct <= inputs.loss_threshold_pct
                && direct_loss <= inputs.loss_threshold_pct;
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
                        let mut beats_every_y = true;
                        for y in &inputs.agents {
                            if y.agent_id == *a_id || y.agent_id == *b_id {
                                continue;
                            }
                            let ay = by_pair.get(&(a_id.clone(), y.ip));
                            let yb = by_pair.get(&(y.agent_id.clone(), *b_ip));
                            if let (Some(ay), Some(yb)) = (ay, yb) {
                                let Some(ay_rtt) = ay.latency_avg_ms else {
                                    continue;
                                };
                                let Some(yb_rtt) = yb.latency_avg_ms else {
                                    continue;
                                };
                                let ay_stddev = ay.latency_stddev_ms.unwrap_or(0.0);
                                let yb_stddev = yb.latency_stddev_ms.unwrap_or(0.0);
                                let ty_rtt = ay_rtt + yb_rtt;
                                let ty_stddev_pen = inputs.stddev_weight
                                    * (ay_stddev * ay_stddev + yb_stddev * yb_stddev).sqrt();
                                // Tiebreaker: reject X when any Y ties X exactly (cf. spec §2.4).
                                if transit_rtt + transit_penalty >= ty_rtt + ty_stddev_pen {
                                    beats_every_y = false;
                                    break;
                                }
                            }
                        }
                        beats_every_y
                    }
                }
            };

            if qualifies {
                pairs_improved += 1;
                improvements.push(improvement_ms);
            }
            compound_losses.push(compound_loss_pct);

            pair_details.push(EvaluationPairDetailDto {
                source_agent_id: a_id.clone(),
                destination_agent_id: b_id.clone(),
                destination_ip: b_ip.to_string(),
                direct_rtt_ms: direct_rtt,
                direct_stddev_ms: direct_stddev,
                direct_loss_pct: direct_loss,
                transit_rtt_ms: transit_rtt,
                transit_stddev_ms: transit_stddev,
                transit_loss_pct: compound_loss_pct,
                improvement_ms,
                qualifies,
                mtr_measurement_id_ax: a_to_x.mtr_measurement_id,
                mtr_measurement_id_xb: x_to_b.mtr_measurement_id,
            });
        }

        if pair_details.is_empty() {
            if any_threshold_fail {
                unqualified_reasons.insert(
                    x_ip.to_string(),
                    "all triples exceeded loss_threshold_pct".into(),
                );
            }
            continue;
        }

        let avg_improvement_ms = if improvements.is_empty() {
            None
        } else {
            Some(improvements.iter().sum::<f32>() / improvements.len() as f32)
        };
        let avg_loss_pct = if compound_losses.is_empty() {
            None
        } else {
            Some(compound_losses.iter().sum::<f32>() / compound_losses.len() as f32)
        };
        let composite_score =
            (pairs_improved as f32 / total_baseline as f32) * avg_improvement_ms.unwrap_or(0.0);

        let enr = inputs.enrichment.get(x_ip).cloned().unwrap_or_default();
        candidate_rows.push(EvaluationCandidateDto {
            destination_ip: x_ip.to_string(),
            display_name: enr.display_name,
            city: enr.city,
            country_code: enr.country_code,
            asn: enr.asn,
            network_operator: enr.network_operator,
            is_mesh_member: agent_by_ip.contains_key(x_ip),
            pairs_improved,
            pairs_total_considered: pair_details.len() as i32,
            avg_improvement_ms,
            avg_loss_pct,
            composite_score,
            pair_details,
        });
    }

    candidate_rows.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Deterministic tiebreaker: parsed `IpAddr` ordering rather
            // than lexicographic string compare. Lexicographic compare
            // would rank `"10.0.0.2" < "9.9.9.9"`, which operator-facing
            // tools render as surprising output; parsed-IP compare
            // preserves numeric intuition. Fall back to string compare
            // when either side fails to parse so the sort stays total.
            .then_with(|| {
                let a_ip = a.destination_ip.parse::<IpAddr>().ok();
                let b_ip = b.destination_ip.parse::<IpAddr>().ok();
                match (a_ip, b_ip) {
                    (Some(a_ip), Some(b_ip)) => a_ip.cmp(&b_ip),
                    _ => a.destination_ip.cmp(&b.destination_ip),
                }
            })
    });

    let candidates_total = candidate_rows.len() as i32;
    let candidates_good = candidate_rows
        .iter()
        .filter(|c| c.pairs_improved >= 1)
        .count() as i32;
    let all_improvements: Vec<f32> = candidate_rows
        .iter()
        .flat_map(|c| {
            c.pair_details
                .iter()
                .filter(|p| p.qualifies)
                .map(|p| p.improvement_ms)
        })
        .collect();
    let avg_improvement_ms = if all_improvements.is_empty() {
        None
    } else {
        Some(all_improvements.iter().sum::<f32>() / all_improvements.len() as f32)
    };

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
            loss_pct: loss,
            mtr_measurement_id: None,
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
            loss_threshold_pct: 2.0,
            stddev_weight: 1.0,
            mode,
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
            loss_threshold_pct: 2.0,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
        };
        let err = evaluate(i).unwrap_err();
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
        i.measurements[1].loss_pct = 3.0;
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
        // Four agents: a, b, y (mesh), plus third-party 203.0.113.7 = X.
        // Baseline A→B = 318ms.
        // Transit via X: A→X(120) + B→X(121) = 241ms (beats direct).
        // Transit via Y (mesh): A→Y(100) + Y→B(80) = 180ms (beats X).
        // Under optimization mode, X must NOT qualify because Y is
        // already a better transit.
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),   // A→B baseline
                m("a", "203.0.113.7", 120.0, 8.0, 0.0), // A→X
                m("b", "203.0.113.7", 121.0, 8.0, 0.0), // B→X (symmetry-approx X→B)
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),    // A→Y
                m("y", "10.0.0.2", 80.0, 5.0, 0.0),     // Y→B
            ],
            agents: vec![
                agent("a", "10.0.0.1"),
                agent("b", "10.0.0.2"),
                agent("y", "10.0.0.3"),
            ],
            enrichment: Default::default(),
            loss_threshold_pct: 2.0,
            stddev_weight: 1.0,
            mode: EvaluationMode::Optimization,
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
            loss_threshold_pct: 2.0,
            stddev_weight: 1.0,
            mode: EvaluationMode::Diversity,
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
