//! Edge case: EdgeCandidate evaluator with a single mesh source.
//!
//! Covers:
//! - destinations_total accounting when X is the sole agent vs. arbitrary IP.
//! - max_hops=0 admits only direct routes (including a real reachable pair to
//!   distinguish the sentinel `EdgeRouteKind::Direct` on unreachable rows from
//!   a genuine direct win on a reachable pair).
//! - max_hops=0 suppresses a 1-hop route that would exist under max_hops=1.
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

#[test]
fn edge_candidate_with_single_source_evaluates_only_direct_routes() {
    // Two mesh agents A and B, plus an arbitrary candidate IP.
    // Candidate A (10.0.0.1): has a real direct measurement to B
    //   → pair A→B is reachable, best_route_ms=50ms, is_unreachable=false.
    // Candidate arbitrary (203.0.113.7): no outgoing agent-keyed
    //   measurement → pair to A and B both unreachable, is_unreachable=true.
    //
    // With max_hops=0 only direct routes are enumerated. No OneHop or
    // TwoHop routes may appear regardless of which pair is reachable.
    // Crucially: the reachable A→B pair gives us a concrete, non-sentinel
    // `Direct` result, proving the sentinel `EdgeRouteKind::Direct` on
    // unreachable rows doesn't make the assertion vacuous.
    let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
    let candidate_arb = ip("203.0.113.7");
    let measurements = vec![
        // A→B direct at 50ms (qualifies under T=80ms).
        meas("a", "10.0.0.2", 50.0),
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

    // max_hops=0: no OneHop or TwoHop routes may appear anywhere.
    for cand in &out.candidates {
        for pair in &cand.pair_details {
            assert!(
                pair.best_route_kind == EdgeRouteKind::Direct,
                "max_hops=0 must keep only direct routes; got {:?} for candidate {} → {}",
                pair.best_route_kind,
                cand.candidate_ip,
                pair.destination_agent_id,
            );
        }
    }

    // Mesh-agent candidate A (10.0.0.1):
    //   destinations_total = 1 (only B; self-pair excluded).
    //   The A→B measurement is 50ms and qualifies under T=80ms.
    let mesh_row = out
        .candidates
        .iter()
        .find(|c| c.candidate_ip == ip("10.0.0.1"))
        .expect("mesh agent A candidate present");
    assert_eq!(
        mesh_row.destinations_total, 1,
        "two agents, X==A → only B remains"
    );
    assert!(mesh_row.is_mesh_member);
    // The A→B pair must be genuinely reachable (not the unreachable sentinel).
    let ab_pair = mesh_row
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "b")
        .expect("A→B pair present");
    assert!(
        !ab_pair.is_unreachable,
        "A→B must be reachable via direct route (meas present)"
    );
    assert_eq!(
        ab_pair.best_route_kind,
        EdgeRouteKind::Direct,
        "A→B is a direct route (max_hops=0)"
    );
    assert!(
        ab_pair.best_route_ms.is_finite(),
        "reachable pair must have finite best_route_ms, got {}",
        ab_pair.best_route_ms
    );
    assert!(
        (ab_pair.best_route_ms - 50.0).abs() < 1e-3,
        "A→B direct RTT must be 50ms, got {}",
        ab_pair.best_route_ms
    );
    assert!(ab_pair.qualifies_under_t, "50ms qualifies under T=80ms");

    // Arbitrary candidate (203.0.113.7):
    //   destinations_total = 2 (both A and B).
    //   No outgoing measurements from this IP → all pairs unreachable.
    let arb_row = out
        .candidates
        .iter()
        .find(|c| c.candidate_ip == candidate_arb)
        .expect("arbitrary candidate present");
    assert_eq!(arb_row.destinations_total, 2, "two agents reachable in theory");
    assert!(!arb_row.is_mesh_member);
    for pair in &arb_row.pair_details {
        assert!(
            pair.is_unreachable,
            "arbitrary candidate has no source measurements → all pairs unreachable"
        );
        assert!(!pair.qualifies_under_t);
    }
}

#[test]
fn max_hops_zero_suppresses_one_hop_routes_that_exist_at_max_hops_one() {
    // Agents: A (10.0.0.1), B (10.0.0.2).
    // Candidate X = arbitrary IP 203.0.113.55 (not in the agent roster).
    //
    // Seeded measurements:
    //   meas("a", X.ip, 5ms)  — enables X→A leg via symmetric reverse
    //   meas("a", B.ip, 5ms)  — enables A→B leg forward
    //
    // With max_hops=0: X→B only via direct probe. X has no agent-keyed
    //   outgoing measurements, so direct is unreachable.
    // With max_hops=1: X→A→B via 1-hop (5+5=10ms) is found.
    //
    // This proves max_hops=0 suppresses the 1-hop route that
    // max_hops=1 would admit.
    let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
    let x_ip = ip("203.0.113.55");
    let measurements = vec![
        meas("a", "203.0.113.55", 5.0), // A probes X; reversed → X→A leg
        meas("a", "10.0.0.2", 5.0),     // A→B forward
    ];

    // max_hops=0: only direct routes. X→B has no direct measurement → unreachable.
    let inputs_0 = EvaluationInputs {
        measurements: measurements.clone(),
        agents: agents.clone(),
        candidate_ips: vec![x_ip],
        enrichment: HashMap::new(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::EdgeCandidate,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: Some(100.0),
        max_hops: 0,
    };
    let out_0 = match evaluate(inputs_0).expect("evaluate max_hops=0") {
        EvaluationOutputs::EdgeCandidate(o) => o,
        EvaluationOutputs::Triple(_) => panic!("expected EdgeCandidate"),
    };
    let xb_0 = out_0.candidates[0]
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "b")
        .expect("pair for B present");
    assert!(
        xb_0.is_unreachable,
        "max_hops=0: no direct X→B measurement → unreachable"
    );

    // max_hops=1: 1-hop X→A→B is admissible and found (5+5=10ms).
    let inputs_1 = EvaluationInputs {
        measurements,
        agents,
        candidate_ips: vec![x_ip],
        enrichment: HashMap::new(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::EdgeCandidate,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: Some(100.0),
        max_hops: 1,
    };
    let out_1 = match evaluate(inputs_1).expect("evaluate max_hops=1") {
        EvaluationOutputs::EdgeCandidate(o) => o,
        EvaluationOutputs::Triple(_) => panic!("expected EdgeCandidate"),
    };
    let xb_1 = out_1.candidates[0]
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "b")
        .expect("pair for B present");
    assert!(
        !xb_1.is_unreachable,
        "max_hops=1: 1-hop X→A→B must be reachable"
    );
    assert_eq!(
        xb_1.best_route_kind,
        EdgeRouteKind::OneHop,
        "max_hops=1: X→B route must be OneHop (via A)"
    );
    assert!(
        (xb_1.best_route_ms - 10.0).abs() < 1e-3,
        "1-hop RTT must be 5+5=10ms, got {}",
        xb_1.best_route_ms
    );
}
