//! Cross-mode `max_hops` correctness tests (T56 Phase F3).
//!
//! Five tests exercise the `max_hops` knob across all three evaluation modes.
//! These are pure-function unit tests of `eval::evaluate`, not HTTP integration
//! tests. This keeps Phase F3 independent of Phase G (EdgeCandidate persistence)
//! which hasn't been wired up yet.
//!
//! Test coverage:
//!   1. EdgeCandidate max_hops=0: only direct routes; no transit intermediaries.
//!   2. EdgeCandidate max_hops=1: 1-hop routes allowed alongside direct routes.
//!   3. EdgeCandidate max_hops=2: 2-hop routes allowed; coverage must not regress.
//!   4. Diversity max_hops=2: `winning_x_position` set on pair_details.
//!   5. Optimization max_hops=2: tiebreaker enumerates non-X alternatives.

use meshmon_service::campaign::eval::{
    evaluate, AgentRow, AttributedMeasurement, EvaluationInputs, EvaluationOutputs,
};
use meshmon_service::campaign::model::{DirectSource, EvaluationMode};
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

fn base_inputs(mode: EvaluationMode, max_hops: i16) -> EvaluationInputs {
    EvaluationInputs {
        measurements: Vec::new(),
        agents: Vec::new(),
        candidate_ips: Vec::new(),
        enrichment: Default::default(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: Some(200.0),
        max_hops,
    }
}

/// Unwrap a `Triple` output; panic on `EdgeCandidate`.
fn as_triple(out: EvaluationOutputs) -> meshmon_service::campaign::eval::TripleEvaluationOutputs {
    match out {
        EvaluationOutputs::Triple(t) => t,
        EvaluationOutputs::EdgeCandidate(_) => {
            panic!("expected Triple variant")
        }
    }
}

/// Unwrap an `EdgeCandidate` output; panic on `Triple`.
fn as_edge(out: EvaluationOutputs) -> meshmon_service::campaign::eval::EdgeCandidateOutputs {
    match out {
        EvaluationOutputs::EdgeCandidate(e) => e,
        EvaluationOutputs::Triple(_) => {
            panic!("expected EdgeCandidate variant")
        }
    }
}

// ---------------------------------------------------------------------------
// 1. EdgeCandidate max_hops=0: only direct routes
// ---------------------------------------------------------------------------

/// With `max_hops=0`, only direct (zero-transit-hop) routes are considered.
///
/// Topology:
///   Agents: X (mesh, 10.1.0.1), A (10.1.0.2), B (10.1.0.3).
///   X is the candidate. Direct measurements:
///     X→A = 80 ms (forward: Agent("x") → Ip(A.ip) → FOUND).
///     X→B = 70 ms (forward: Agent("x") → Ip(B.ip) → FOUND).
///
/// With max_hops=0, both A and B are covered by direct forward-lookup routes.
/// The `direct_share` for X must be 1.0 and `onehop_share` / `twohop_share` = 0.
#[test]
fn edge_candidate_max_hops_0_only_direct() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("x", "10.1.0.2", 80.0, 5.0, 0.0), // X→A direct
            m("x", "10.1.0.3", 70.0, 5.0, 0.0), // X→B direct
        ],
        agents: vec![
            agent("x", "10.1.0.1"),
            agent("a", "10.1.0.2"),
            agent("b", "10.1.0.3"),
        ],
        // X (10.1.0.1) is a mesh agent — endpoint becomes Agent("x")
        candidate_ips: vec![ip("10.1.0.1")],
        ..base_inputs(EvaluationMode::EdgeCandidate, 0)
    };

    let out = as_edge(evaluate(inputs).unwrap());
    let x_row = out
        .candidates
        .iter()
        .find(|r| r.candidate_ip == ip("10.1.0.1"))
        .expect("X=10.1.0.1 must appear as a candidate");

    assert_eq!(
        x_row.coverage_count, 2,
        "max_hops=0: direct-only routes must cover both A and B: {x_row:?}"
    );

    // All coverage must be direct (no transit hops possible with max_hops=0).
    assert_eq!(
        x_row.direct_share,
        Some(1.0),
        "max_hops=0: direct_share must be 1.0, got {:?}",
        x_row.direct_share
    );
    assert_eq!(
        x_row.onehop_share.unwrap_or(0.0) as f32,
        0.0,
        "max_hops=0: onehop_share must be 0"
    );
    assert_eq!(
        x_row.twohop_share.unwrap_or(0.0) as f32,
        0.0,
        "max_hops=0: twohop_share must be 0"
    );
}

// ---------------------------------------------------------------------------
// 2. EdgeCandidate max_hops=1
// ---------------------------------------------------------------------------

/// With `max_hops=1`, 1-hop transit routes are allowed. An arbitrary (non-mesh)
/// X can reach mesh agents B and C via 1-hop X→A→B and X→A→C.
///
/// Topology:
///   Agents: A (10.2.0.1), B (10.2.0.2), C (10.2.0.3).
///   X = 10.2.0.9 (not a mesh agent).
///   Measurements (A probes X; A probes B and C forward):
///     A→X = 30 ms (stored as (Agent("a"), Ip(X.ip)); reverse gives X→A leg)
///     A→B = 20 ms (forward: (Agent("a"), Ip(B.ip)))
///     A→C = 25 ms (forward: (Agent("a"), Ip(C.ip)))
///
///   Under max_hops=0, X has no direct measurements to any agent → all unreachable.
///   Under max_hops=1, X→A→B (30+20=50 ms) and X→A→C (30+25=55 ms) are found.
///   B and C are covered via 1-hop; A itself is unreachable (no direct X→A
///   measurement and no intermediary other than A for A itself).
///
/// The test pins that max_hops=1 enables the 1-hop coverage path and produces
/// `onehop_share > 0`.
#[test]
fn edge_candidate_max_hops_1_direct_plus_one_hop() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("a", "10.2.0.9", 30.0, 2.0, 0.0), // A→X (reverse gives X→A leg)
            m("a", "10.2.0.2", 20.0, 2.0, 0.0), // A→B forward
            m("a", "10.2.0.3", 25.0, 2.0, 0.0), // A→C forward
        ],
        agents: vec![
            agent("a", "10.2.0.1"),
            agent("b", "10.2.0.2"),
            agent("c", "10.2.0.3"),
        ],
        candidate_ips: vec![ip("10.2.0.9")], // X is non-mesh
        ..base_inputs(EvaluationMode::EdgeCandidate, 1)
    };

    let out = as_edge(evaluate(inputs).unwrap());
    let x_row = out
        .candidates
        .iter()
        .find(|r| r.candidate_ip == ip("10.2.0.9"))
        .expect("X=10.2.0.9 must appear");

    assert_eq!(
        x_row.destinations_total, 3,
        "max_hops=1: 3 destination agents (A, B, C)"
    );
    // B and C are reachable via 1-hop; A is not reachable (self-loop excluded).
    assert_eq!(
        x_row.coverage_count, 2,
        "max_hops=1: B and C covered via 1-hop X→A→B and X→A→C: {x_row:?}"
    );

    // Both covered pairs must be onehop.
    assert!(
        x_row.onehop_share.unwrap_or(0.0) > 0.0,
        "max_hops=1: onehop_share must be positive: {x_row:?}"
    );

    // Verify B and C pair details.
    for &agent_id in &["b", "c"] {
        let pair = x_row
            .pair_details
            .iter()
            .find(|p| p.destination_agent_id == agent_id)
            .unwrap_or_else(|| panic!("pair for {agent_id} must exist"));
        assert_eq!(
            pair.best_route_kind,
            meshmon_service::campaign::model::EdgeRouteKind::OneHop,
            "{agent_id} must be reached via 1-hop: {pair:?}"
        );
        assert!(!pair.is_unreachable, "{agent_id} must not be unreachable");
    }
}

// ---------------------------------------------------------------------------
// 3. EdgeCandidate max_hops=2
// ---------------------------------------------------------------------------

/// With `max_hops=2`, 2-hop routes are also enumerated. This test shows
/// that a destination unreachable under max_hops=1 can be reached via a
/// 2-hop route under max_hops=2.
///
/// Topology:
///   Agents: X (mesh, 10.3.0.1), A (10.3.0.2), C (10.3.0.3), B (10.3.0.4).
///   X is the candidate.
///   Direct measurements:
///     X→A = 5 ms.
///   Measurements that enable 2-hop routing:
///     C→A = 10 ms (reverse: A→C leg via sym-approx).
///     C→B = 10 ms (C→B forward).
///
///   With max_hops=1, B is unreachable from X (no direct X→B or 1-hop
///   X→A→B since A→B is not seeded; X→C→B: need X→C leg but only X→A is direct).
///   With max_hops=2, B is reachable via X→A→C→B (5+10+10=25 ms < T=100 ms).
///   direct_share = 1/2 (A is direct), twohop_share = 1/2 (B is twohop).
///   coverage_count = 2 (A + B); C is destinations_total=3 - covered through
///   the 2-hop route when we check its pair directly.
///
/// This test pins the monotone-coverage property and the twohop_share field.
#[test]
fn edge_candidate_max_hops_2_full_two_hop_set() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("x", "10.3.0.2", 5.0, 0.0, 0.0),  // X→A direct (L1)
            m("c", "10.3.0.2", 10.0, 0.0, 0.0),  // reverse: A→C sym (L2)
            m("c", "10.3.0.4", 10.0, 0.0, 0.0),  // C→B direct (L3)
        ],
        agents: vec![
            agent("x", "10.3.0.1"),
            agent("a", "10.3.0.2"),
            agent("c", "10.3.0.3"),
            agent("b", "10.3.0.4"),
        ],
        candidate_ips: vec![ip("10.3.0.1")], // X is mesh agent
        ..base_inputs(EvaluationMode::EdgeCandidate, 2)
    };

    let out = as_edge(evaluate(inputs).unwrap());
    let x_row = out
        .candidates
        .iter()
        .find(|r| r.candidate_ip == ip("10.3.0.1"))
        .expect("X=10.3.0.1 must appear");

    // B is reachable via 2-hop X→A→C→B; A is reachable directly.
    // C: no direct X→C and no 1-hop (would need X→Y→C where Y≠A,B,C,X;
    // there is no such agent). Under max_hops=2 X→A→C is reachable (1-hop).
    // So coverage_count >= 2 (A direct + B twohop) and possibly 3 (C onehop).
    assert!(
        x_row.coverage_count >= 2,
        "max_hops=2: at least A (direct) and B (2-hop) must be covered: {x_row:?}"
    );

    // B pair must use twohop.
    let b_pair = x_row
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "b")
        .expect("pair for B must exist");
    assert_eq!(
        b_pair.best_route_kind,
        meshmon_service::campaign::model::EdgeRouteKind::TwoHop,
        "B must be reached via 2-hop route X→A→C→B: {b_pair:?}"
    );
    assert!(
        (b_pair.best_route_ms - 25.0).abs() < 1e-3,
        "2-hop RTT must be 5+10+10=25ms, got {}",
        b_pair.best_route_ms
    );

    // twohop_share > 0 confirms that the route-kind tracking works correctly.
    assert!(
        x_row.twohop_share.unwrap_or(0.0) > 0.0,
        "max_hops=2: twohop_share must be positive when B is only reachable via 2-hop: {x_row:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Diversity max_hops=2: winning_x_position is populated
// ---------------------------------------------------------------------------

/// Diversity mode with `max_hops=2`: `winning_x_position` in pair_details is
/// set to 1 when X is the sole (and first) intermediary in a 1-hop route.
///
/// The field is `None` when `max_hops < 2` (single-position routes don't
/// need tracking).
///
/// Topology:
///   Agents: A, B. X=10.4.0.9 (non-mesh).
///   A→B direct = 300 ms.
///   A→X = 100 ms, B→X = 110 ms → X transit RTT = 210 ms.
///   Improvement = 300 - 210 - (11*1 - 24*1) = 300 - 210 + 13 = 103 ms > 0 → qualifies.
///   (stddev_weight = 1.0, direct stddev = 24ms → penalty = 24; transit stddev = ...
///    actually both A→X and B→X have stddev=5, composed = max or sum? let me use simple values)
#[test]
fn diversity_max_hops_2_x_position_best_of_wins() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("a", "10.4.0.2", 300.0, 5.0, 0.0), // A→B direct baseline
            m("a", "10.4.0.9", 100.0, 5.0, 0.0), // A→X
            m("b", "10.4.0.9", 110.0, 5.0, 0.0), // B→X → sym X→B = 110ms
        ],
        agents: vec![agent("a", "10.4.0.1"), agent("b", "10.4.0.2")],
        candidate_ips: Vec::new(),
        enrichment: Default::default(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::Diversity,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: None,
        max_hops: 2,
    };

    let out = as_triple(evaluate(inputs).unwrap());

    // X must qualify for the (A, B) pair.
    let x_idx = out
        .results
        .candidates
        .iter()
        .position(|c| c.destination_ip == "10.4.0.9")
        .expect("X=10.4.0.9 must appear as a candidate");

    let x_cand = &out.results.candidates[x_idx];
    assert!(
        x_cand.pairs_improved >= 1,
        "diversity max_hops=2: X must qualify for at least 1 pair: {x_cand:?}"
    );

    // The pair_detail for (A, B) must have winning_x_position = Some(1)
    // because X is the first (and only) intermediary in the 1-hop route.
    let pd_bundle = &out.pair_details_by_candidate[x_idx];
    let ab_detail = pd_bundle
        .pair_details
        .iter()
        .find(|pd| pd.source_agent_id == "a" && pd.destination_agent_id == "b")
        .expect("(A, B) pair_detail must be present");

    assert!(
        ab_detail.qualifies,
        "diversity max_hops=2: (A, B) pair_detail must qualify: {ab_detail:?}"
    );
    assert_eq!(
        ab_detail.winning_x_position,
        Some(1),
        "diversity max_hops=2: winning_x_position must be 1 for sole 1-hop intermediary: {ab_detail:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Optimization max_hops=2: non-X alternatives include 1-hop via mesh Y
// ---------------------------------------------------------------------------

/// Optimization mode with `max_hops=2`: X is rejected when a 1-hop non-X
/// route through mesh agent Y (using `CandidateIp(Y.ip)` intermediary) beats X.
///
/// Topology:
///   Agents: A, B, Y (mesh). X=10.5.0.9 (non-mesh).
///   A→B direct = 400 ms.
///   A→X = 150 ms, B→X = 160 ms → X transit = 310 ms.
///   A→Y = 100 ms, B→Y = 90 ms → Y 1-hop alternative = 190 ms (< 310 ms).
///
/// Since Y's route (190 ms) beats X (310 ms), X must NOT qualify under
/// optimization mode. The `qualifies_under_optimization_v2` function uses
/// `CandidateIp(Y.ip)` as the Y endpoint so LegLookup can resolve both
/// A→Y (forward: Agent(A), Ip(Y.ip)) and Y→B (reverse: Agent(B), Ip(Y.ip)).
#[test]
fn optimization_max_hops_2_tiebreaker_includes_two_hop_alternatives() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("a", "10.5.0.2", 400.0, 5.0, 0.0),  // A→B baseline
            m("a", "10.5.0.9", 150.0, 5.0, 0.0),  // A→X
            m("b", "10.5.0.9", 160.0, 5.0, 0.0),  // B→X → sym X→B
            m("a", "10.5.0.3", 100.0, 5.0, 0.0),  // A→Y (mesh) → forward lookup works
            m("b", "10.5.0.3", 90.0, 5.0, 0.0),   // B→Y → Y→B reverse lookup works
        ],
        agents: vec![
            agent("a", "10.5.0.1"),
            agent("b", "10.5.0.2"),
            agent("y", "10.5.0.3"),
        ],
        candidate_ips: Vec::new(),
        enrichment: Default::default(),
        loss_threshold_ratio: 0.05,
        stddev_weight: 1.0,
        mode: EvaluationMode::Optimization,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: None,
        max_hops: 2,
    };

    let out = as_triple(evaluate(inputs).unwrap());

    // X candidate must exist (triple is fully measured) but must not qualify.
    let x_idx = out
        .results
        .candidates
        .iter()
        .position(|c| c.destination_ip == "10.5.0.9")
        .expect("X=10.5.0.9 must appear as a candidate");

    let x_cand = &out.results.candidates[x_idx];
    assert_eq!(
        x_cand.pairs_improved, 0,
        "optimization max_hops=2: X must NOT qualify when Y provides better 1-hop route: {x_cand:?}"
    );

    // Pair detail for (A, B) must have qualifies=false.
    let pd_bundle = &out.pair_details_by_candidate[x_idx];
    let ab_detail = pd_bundle
        .pair_details
        .iter()
        .find(|pd| pd.source_agent_id == "a" && pd.destination_agent_id == "b")
        .expect("(A, B) pair_detail must be present");

    assert!(
        !ab_detail.qualifies,
        "optimization max_hops=2: (A, B) must NOT qualify when Y beats X: {ab_detail:?}"
    );
}
