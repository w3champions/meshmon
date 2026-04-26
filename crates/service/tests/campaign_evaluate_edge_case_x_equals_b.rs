//! Edge case: EdgeCandidate evaluator excludes the self-pair when a
//! mesh agent's IP is also one of the campaign's destination_ips.
//!
//! With three mesh agents and a candidate IP equal to one of those
//! agents, the candidate row's `destinations_total` must be `2` (not
//! `3`) — the agent at that IP cannot route to itself. See spec §5.5
//! (X == B edge case).
//!
//! The full-stack `/evaluate` flow lands in Phase H; this test covers
//! the pure evaluator function so the algorithm is verified without
//! waiting on Phase G persistence.

use meshmon_service::campaign::eval::{
    evaluate, AgentRow, AttributedMeasurement, EvaluationInputs, EvaluationOutputs,
};
use meshmon_service::campaign::model::{DirectSource, EvaluationMode};
use std::collections::HashMap;
use std::net::IpAddr;

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

fn meas(src: &str, dst: &str, rtt: f32) -> AttributedMeasurement {
    AttributedMeasurement {
        source_agent_id: src.into(),
        destination_ip: ip(dst),
        latency_avg_ms: Some(rtt),
        latency_stddev_ms: Some(0.0),
        loss_ratio: 0.0,
        mtr_measurement_id: None,
        direct_source: DirectSource::ActiveProbe,
    }
}

#[tokio::test]
async fn edge_candidate_x_equals_b_excludes_self_pair() {
    // 3 mesh agents; candidate IP = agent A's IP. destinations_total
    // for that candidate must be 2 (B and C), not 3 — A cannot route
    // to itself.
    let agents = vec![
        agent("a", "10.0.0.1"),
        agent("b", "10.0.0.2"),
        agent("c", "10.0.0.3"),
    ];
    let measurements = vec![
        meas("a", "10.0.0.2", 10.0),
        meas("a", "10.0.0.3", 20.0),
        meas("b", "10.0.0.3", 15.0),
    ];
    let inputs = EvaluationInputs {
        measurements,
        agents,
        // X == agent A's IP.
        candidate_ips: vec![ip("10.0.0.1")],
        enrichment: HashMap::new(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::EdgeCandidate,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: Some(50.0),
        max_hops: 1,
    };
    let out = match evaluate(inputs).expect("edge_candidate evaluate") {
        EvaluationOutputs::EdgeCandidate(o) => o,
        EvaluationOutputs::Triple(_) => panic!("expected EdgeCandidate variant"),
    };

    let row = out.candidates.first().expect("one candidate row produced");
    assert_eq!(
        row.destinations_total, 2,
        "X == B self-pair excluded: 3 agents, X==A → only B and C remain"
    );
    assert_eq!(
        row.pair_details.len(),
        2,
        "pair_details count matches destinations_total"
    );
    assert!(
        row.pair_details
            .iter()
            .all(|p| p.destination_agent_id != "a"),
        "no pair_details row for the self-pair (X == B)"
    );
    assert!(row.is_mesh_member);
    assert_eq!(row.agent_id.as_deref(), Some("a"));
}
