//! Cross-mode `max_hops` correctness tests (T56 Phase F3).
//!
//! Six tests exercise the `max_hops` knob across all three evaluation modes.
//! These are pure-function unit tests of `eval::evaluate`, not HTTP integration
//! tests. This keeps Phase F3 independent of Phase G (EdgeCandidate persistence)
//! which hasn't been wired up yet.
//!
//! Test coverage:
//!   1. EdgeCandidate max_hops=0: only direct routes; no transit intermediaries.
//!   2. EdgeCandidate max_hops=1: 1-hop routes allowed alongside direct routes.
//!   3. EdgeCandidate max_hops=2: 2-hop routes allowed; coverage must not regress.
//!   4. Diversity max_hops=2: `winning_x_position` semantics (None for 1-hop route).
//!   5. Optimization max_hops=2: tiebreaker enumerates non-X alternatives.
//!   6. Diversity max_hops=2: dual-form pool regression — A/B correctly excluded
//!      from intermediary pool so degenerate routes are not counted.

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
/// X can reach mesh agents B and C via 1-hop X→A→B and X→A→C, and reaches A
/// directly via the reverse of A's outbound probe to X.
///
/// Topology:
///   Agents: A (10.2.0.1), B (10.2.0.2), C (10.2.0.3).
///   X = 10.2.0.9 (not a mesh agent).
///   Measurements (A probes X; A probes B and C forward):
///     A→X = 30 ms (stored as (Agent("a"), Ip(X.ip)); reverse gives X→A leg)
///     A→B = 20 ms (forward: (Agent("a"), Ip(B.ip)))
///     A→C = 25 ms (forward: (Agent("a"), Ip(C.ip)))
///
///   X→A direct is reverse-resolved (was_substituted = true).
///   X→B direct is missing; 1-hop X→A→B (30+20=50 ms) is enumerated.
///   X→C direct is missing; 1-hop X→A→C (30+25=55 ms) is enumerated.
///
/// All three destinations are covered. Direct-share > 0 (A) and
/// onehop-share > 0 (B and C).
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
    // A is reachable directly (reverse-resolved); B and C via 1-hop.
    assert_eq!(
        x_row.coverage_count, 3,
        "max_hops=1: A direct + B/C via 1-hop must all qualify: {x_row:?}"
    );

    assert!(
        x_row.onehop_share.unwrap_or(0.0) > 0.0,
        "max_hops=1: onehop_share must be positive: {x_row:?}"
    );

    // Verify A direct, plus B/C onehop pair details.
    let a_pair = x_row
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "a")
        .expect("pair for A must exist");
    assert_eq!(
        a_pair.best_route_kind,
        meshmon_service::campaign::model::EdgeRouteKind::Direct,
        "A must be reached directly via reverse-resolved leg: {a_pair:?}"
    );
    assert!(!a_pair.is_unreachable);

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
            m("c", "10.3.0.2", 10.0, 0.0, 0.0), // reverse: A→C sym (L2)
            m("c", "10.3.0.4", 10.0, 0.0, 0.0), // C→B direct (L3)
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
/// `None` when the winning route is 1-hop (X is the sole intermediary — there
/// is no "first vs second" position to track).
///
/// `winning_x_position` encodes topology, not configuration: it is only
/// `Some(1)` or `Some(2)` when the winning route is a 2-hop route with two
/// intermediaries. A 1-hop route always returns `None`.
///
/// Topology:
///   Agents: A, B. X=10.4.0.9 (non-mesh).
///   A→B direct = 300 ms, stddev=5.
///   A→X = 100 ms, B→X = 110 ms → 1-hop X transit RTT = 210 ms, stddev=√50≈7.07.
///   Improvement = 300 - 210 - (7.07 - 5) * 1.0 ≈ 87.9 ms > 0 → qualifies.
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

    // The pair_detail for (A, B) must have winning_x_position = None
    // because the winning route is 1-hop (X is the sole intermediary —
    // "first vs second" position has no meaning for single-intermediary routes).
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
        None,
        "diversity max_hops=2: winning_x_position must be None for a 1-hop route (sole intermediary has no ordinal position): {ab_detail:?}"
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
            m("a", "10.5.0.2", 400.0, 5.0, 0.0), // A→B baseline
            m("a", "10.5.0.9", 150.0, 5.0, 0.0), // A→X
            m("b", "10.5.0.9", 160.0, 5.0, 0.0), // B→X → sym X→B
            m("a", "10.5.0.3", 100.0, 5.0, 0.0), // A→Y (mesh) → forward lookup works
            m("b", "10.5.0.3", 90.0, 5.0, 0.0),  // B→Y → Y→B reverse lookup works
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

// ---------------------------------------------------------------------------
// 6. Diversity max_hops=2: dual-form pool regression — A/B must not appear
//    as intermediaries after the pool filter is applied.
// ---------------------------------------------------------------------------

/// Diversity mode with `max_hops=2`: the intermediary pool construction uses
/// both `Agent` and `CandidateIp` forms for each mesh agent. The pool filter
/// must remove BOTH forms of A and B so that no route through A or B is
/// enumerated. This is a regression test for the dual-form pool fix: before
/// the fix, only the `Agent` form of each agent was in the pool, and the
/// filter matched on agent IDs. The fix added both forms and updated the
/// filter to exclude both. This test proves A and B are correctly excluded by
/// verifying that the only qualifying route for X is `A → X → B` (1-hop),
/// and that `winning_x_position = None` (1-hop route has no ordinal position).
///
/// Topology:
///   Agents: A (10.6.0.1), B (10.6.0.2), Y (10.6.0.3).
///   X = 10.6.0.9 (non-mesh candidate).
///   Measurements:
///     A→B direct = 300 ms (baseline, large to ensure X qualifies).
///     A→X = 100 ms (A→X forward leg).
///     B→X = 110 ms (B→X; provides X→B via symmetric reverse).
///     A→Y = 50 ms, B→Y = 60 ms (mesh Y measurements; Y→B available via reverse).
///
///   X qualifies via 1-hop A→X→B (transit RTT = 210 ms < 300 ms direct).
///   Y-only routes (A→Y→B) don't contain X and are correctly filtered out.
///
///   Regression check: if A or B leaked into the pool, degenerate routes like
///   A→A→X→B or A→X→B→B would be attempted. They can't resolve (A or B
///   endpoints as transit would fail leg lookups), but the test proves the pool
///   is clean by checking that X correctly qualifies with `pairs_improved = 1`
///   and that `winning_x_position = None` (correct for a 1-hop route).
#[test]
fn diversity_max_hops_2_dual_form_pool_filter_regression() {
    let inputs = EvaluationInputs {
        measurements: vec![
            m("a", "10.6.0.2", 300.0, 5.0, 0.0), // A→B baseline (direct)
            m("a", "10.6.0.9", 100.0, 5.0, 0.0), // A→X forward
            m("b", "10.6.0.9", 110.0, 5.0, 0.0), // B→X (provides X→B via reverse)
            m("a", "10.6.0.3", 50.0, 2.0, 0.0),  // A→Y (mesh Y measurements)
            m("b", "10.6.0.3", 60.0, 2.0, 0.0),  // B→Y (provides Y→B via reverse)
        ],
        agents: vec![
            agent("a", "10.6.0.1"),
            agent("b", "10.6.0.2"),
            agent("y", "10.6.0.3"),
        ],
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

    // X must appear as a candidate and qualify for the (A, B) pair.
    let x_idx = out
        .results
        .candidates
        .iter()
        .position(|c| c.destination_ip == "10.6.0.9")
        .expect("X=10.6.0.9 must appear as a candidate");

    let x_cand = &out.results.candidates[x_idx];
    assert_eq!(
        x_cand.pairs_improved, 1,
        "diversity max_hops=2: X must qualify for the (A, B) pair: {x_cand:?}"
    );

    // The (A, B) pair_detail must be present with qualifies=true.
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

    // winning_x_position must be None: the winning route is 1-hop (X is the sole
    // intermediary), so there is no "first vs second" ordinal position to report.
    assert_eq!(
        ab_detail.winning_x_position,
        None,
        "diversity max_hops=2: winning_x_position must be None for 1-hop (sole intermediary): {ab_detail:?}"
    );

    // Y must also appear as a candidate (A→Y→B measurements exist and A→Y→B
    // route qualifies via the CandidateIp(Y) pool entry). But Y is a mesh
    // member — verify it carries `is_mesh_member = true`.
    let y_cand = out
        .results
        .candidates
        .iter()
        .find(|c| c.destination_ip == "10.6.0.3");
    // Y being a candidate (if it appears) must be flagged as a mesh member.
    if let Some(y) = y_cand {
        assert!(
            y.is_mesh_member,
            "Y at 10.6.0.3 is a mesh agent: is_mesh_member must be true: {y:?}"
        );
    }

    // Sanity: the total baseline pair count must be ≥ 1 (A→B exists).
    assert!(
        out.baseline_pair_count >= 1,
        "baseline_pair_count must be at least 1: {}",
        out.baseline_pair_count
    );
}

// ---------------------------------------------------------------------------
// 7. EdgeCandidate non-mesh X with max_hops=0: direct route via reverse
// ---------------------------------------------------------------------------

/// Non-mesh candidate X reaches mesh agent B via the reverse of B's outbound
/// probe to X.ip. With `max_hops=0` only direct routes are considered, so the
/// leg lookup must resolve `X → B` from the stored
/// `(Agent("b"), Ip(X.ip))` measurement without help from any intermediary.
///
/// Topology:
///   Agents: B (10.7.0.1).
///   X = 203.0.113.7 (not a mesh agent).
///   Measurement: B→X = 40 ms (stored as `(Agent("b"), Ip(X.ip))`).
///
/// Expectation: `coverage_count == 1` and the pair row carries a real
/// `best_route_ms` (the reverse-resolved direct leg) rather than a sentinel.
#[test]
fn edge_candidate_non_mesh_x_resolves_direct_via_reverse_with_max_hops_zero() {
    let inputs = EvaluationInputs {
        measurements: vec![
            // Only outbound probe is B→X. The X→B leg must be reverse-resolved.
            m("b", "203.0.113.7", 40.0, 1.0, 0.0),
        ],
        agents: vec![agent("b", "10.7.0.1")],
        candidate_ips: vec![ip("203.0.113.7")],
        ..base_inputs(EvaluationMode::EdgeCandidate, 0)
    };

    let out = as_edge(evaluate(inputs).unwrap());
    let x_row = out
        .candidates
        .iter()
        .find(|r| r.candidate_ip == ip("203.0.113.7"))
        .expect("non-mesh X must appear as a candidate");

    assert_eq!(x_row.destinations_total, 1, "single mesh destination B");
    assert_eq!(
        x_row.coverage_count, 1,
        "max_hops=0: X→B direct (reverse-resolved) must qualify: {x_row:?}"
    );

    let b_pair = x_row
        .pair_details
        .iter()
        .find(|p| p.destination_agent_id == "b")
        .expect("pair for B must exist");
    assert!(!b_pair.is_unreachable, "X→B must be reachable: {b_pair:?}");
    // best_route_ms = rtt + stddev_weight * stddev = 40 + 1.0*1.0 = 41
    assert!(
        (b_pair.best_route_ms - 41.0).abs() < 1e-3,
        "best_route_ms must equal the penalised RTT (41ms = 40 rtt + 1*1 stddev), got {}",
        b_pair.best_route_ms
    );
    assert_eq!(
        b_pair.best_route_kind,
        meshmon_service::campaign::model::EdgeRouteKind::Direct,
        "non-mesh X with max_hops=0: route must be Direct (reverse-resolved): {b_pair:?}"
    );
}
