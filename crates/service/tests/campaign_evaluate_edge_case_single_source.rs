//! Edge case: EdgeCandidate evaluator with a single mesh source.
//!
//! With one mesh agent and N candidate IPs the destinations_total per
//! candidate must equal `agents.len() - 1` for a candidate that *is*
//! the agent (excluding self), and `agents.len()` for an arbitrary
//! candidate IP. With `max_hops = 0` only direct routes are
//! enumerated.
//!
//! The full-stack `/evaluate` flow lands in Phase H; this test covers
//! the pure evaluator function so the algorithm is verified without
//! waiting on Phase G persistence.

use meshmon_service::campaign::eval::{
    evaluate, AgentRow, AttributedMeasurement, EvaluationInputs, EvaluationOutputs,
};
use meshmon_service::campaign::model::{DirectSource, EdgeRouteKind, EvaluationMode};
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
async fn edge_candidate_with_single_source_evaluates_only_direct_routes() {
    // Single mesh agent A; two candidate IPs (one of them mesh = A
    // itself, the other arbitrary). With max_hops = 0 only direct
    // routes are enumerated. With N=1 mesh agent, intermediaries pool
    // is empty, so even a higher max_hops would yield no transit
    // routes anyway — the test asserts the direct-only behaviour
    // explicitly so a future bug that lets one-hop sneak in trips it.
    let agents = vec![agent("a", "10.0.0.1")];
    let candidate_arb = ip("203.0.113.7");
    let measurements = vec![
        // A → arbitrary X: a usable direct route (will surface as
        // `qualifies_under_t = false` for the agent-as-X case
        // because the mesh agent's only "destination" is itself —
        // which we exclude — leaving 0 destinations).
        meas("a", "203.0.113.7", 50.0),
    ];
    let inputs = EvaluationInputs {
        measurements,
        agents,
        candidate_ips: vec![ip("10.0.0.1"), candidate_arb],
        enrichment: HashMap::new(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::EdgeCandidate,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: Some(80.0),
        max_hops: 0,
    };
    let out = match evaluate(inputs).expect("edge_candidate evaluate") {
        EvaluationOutputs::EdgeCandidate(o) => o,
        EvaluationOutputs::Triple(_) => panic!("expected EdgeCandidate variant"),
    };
    assert_eq!(out.candidates.len(), 2, "two candidate rows produced");
    for cand in &out.candidates {
        for pair in &cand.pair_details {
            // With max_hops = 0 the only winning route is direct
            // (when reachable); unreachable rows carry the sentinel
            // EdgeRouteKind::Direct + is_unreachable = true. Either
            // way, no OneHop / TwoHop is admissible.
            assert!(
                pair.best_route_kind == EdgeRouteKind::Direct,
                "max_hops=0 must keep only direct routes; got {:?}",
                pair.best_route_kind
            );
        }
    }
    // The mesh-agent candidate (10.0.0.1) has destinations_total = 0
    // (only mesh agent is itself, excluded as X==B). The arbitrary
    // candidate has destinations_total = 1 (the mesh agent A).
    let mesh_row = out
        .candidates
        .iter()
        .find(|c| c.candidate_ip == ip("10.0.0.1"))
        .expect("mesh agent candidate present");
    assert_eq!(
        mesh_row.destinations_total, 0,
        "single mesh agent + agent-as-X has zero destinations after self-exclusion"
    );
    assert!(mesh_row.is_mesh_member);

    let arb_row = out
        .candidates
        .iter()
        .find(|c| c.candidate_ip == candidate_arb)
        .expect("arbitrary candidate present");
    assert_eq!(arb_row.destinations_total, 1);
    assert!(!arb_row.is_mesh_member);
}
