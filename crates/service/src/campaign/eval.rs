//! Pure-function core for the campaign evaluator. No DB, no IO.
//!
//! `evaluate()` is the single entry point. Its caller (repo.rs) builds
//! `EvaluationInputs` from DB queries and persists the returned
//! `EvaluationOutputs` into the `campaign_evaluations` table.

use crate::campaign::dto::{
    EvaluationCandidateDto, EvaluationPairDetailDto, EvaluationResultsDto,
};
use crate::campaign::model::EvaluationMode;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

/// One row from `measurements` attributed to the campaign. Only the
/// columns the evaluator reads are carried here — the DB layer filters
/// to `kind='campaign'` pairs before constructing this.
#[derive(Debug, Clone)]
pub struct AttributedMeasurement {
    pub source_agent_id: String,
    pub destination_ip: IpAddr,
    pub latency_avg_ms: Option<f32>,
    pub latency_stddev_ms: Option<f32>,
    pub loss_pct: f32,
    pub mtr_measurement_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct AgentRow {
    pub agent_id: String,
    pub ip: IpAddr,
}

/// Lookup side-table: IP → catalogue enrichment. Agents contribute
/// their entry too (via `agents_with_catalogue`), so a non-mesh IP is
/// one that is NOT in `agents_by_ip` but may still be in `enrichment`.
#[derive(Debug, Clone, Default)]
pub struct CatalogueLookup {
    pub display_name: Option<String>,
    pub city: Option<String>,
    pub country_code: Option<String>,
    pub asn: Option<i64>,
    pub network_operator: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvaluationInputs {
    pub measurements: Vec<AttributedMeasurement>,
    pub agents: Vec<AgentRow>,
    pub enrichment: HashMap<IpAddr, CatalogueLookup>,
    pub loss_threshold_pct: f32,
    pub stddev_weight: f32,
    pub mode: EvaluationMode,
}

#[derive(Debug, Clone)]
pub struct EvaluationOutputs {
    pub baseline_pair_count: i32,
    pub candidates_total: i32,
    pub candidates_good: i32,
    pub avg_improvement_ms: Option<f32>,
    pub results: EvaluationResultsDto,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("no baseline routes; include at least one agent→agent pair")]
    NoBaseline,
}

pub fn evaluate(_inputs: EvaluationInputs) -> Result<EvaluationOutputs, EvalError> {
    todo!("implemented in Task 4")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

    fn agent(id: &str, addr: &str) -> AgentRow {
        AgentRow { agent_id: id.into(), ip: ip(addr) }
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
            agents: vec![
                agent("a", "10.0.0.1"),
                agent("b", "10.0.0.2"),
            ],
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
        let cand = out.results.candidates.iter()
            .find(|c| c.destination_ip == "203.0.113.7")
            .expect("candidate present");
        assert_eq!(cand.pairs_improved, 1);
        assert!(cand.avg_improvement_ms.unwrap() > 0.0,
            "transit (120+121=241) beats direct (318)");
    }

    #[test]
    fn loss_threshold_filters_unreliable_transit() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements[1].loss_pct = 3.0;
        let out = evaluate(i).unwrap();
        let cand = out.results.candidates.iter()
            .find(|c| c.destination_ip == "203.0.113.7");
        assert!(cand.is_none() || cand.unwrap().pairs_improved == 0);
    }

    #[test]
    fn stddev_penalty_applied() {
        let mut i = inputs_basic(EvaluationMode::Diversity);
        i.measurements[1].latency_stddev_ms = Some(200.0);
        i.measurements[2].latency_stddev_ms = Some(200.0);
        let out = evaluate(i).unwrap();
        let cand = out.results.candidates.iter()
            .find(|c| c.destination_ip == "203.0.113.7");
        if let Some(c) = cand {
            assert_eq!(c.pairs_improved, 0);
        }
    }

    #[test]
    fn optimization_filters_out_when_existing_mesh_y_already_better() {
        let inputs = EvaluationInputs {
            measurements: vec![
                m("a", "10.0.0.2", 318.0, 24.0, 0.0),
                m("a", "203.0.113.7", 120.0, 8.0, 0.0),
                m("a", "10.0.0.3", 100.0, 5.0, 0.0),
                m("y", "10.0.0.2", 130.0, 5.0, 0.0),
                m("b-observer", "203.0.113.7", 121.0, 8.0, 0.0),
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
        let cand = out.results.candidates.iter()
            .find(|c| c.destination_ip == "203.0.113.7");
        assert!(cand.is_none() || cand.unwrap().pairs_improved == 0,
            "X should not qualify when Y already provides a better transit");
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
        let cand = out.results.candidates.iter()
            .find(|c| c.destination_ip == "10.0.0.3")
            .expect("agent-as-candidate present");
        assert!(cand.is_mesh_member);
    }
}
