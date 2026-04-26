//! EdgeCandidate evaluator arm. See spec
//! `docs/superpowers/specs/2026-04-26-campaigns-edge-candidate-evaluation-mode-design.md`
//! §4 (algorithm) and §5.5 (edge cases).
//!
//! For each candidate IP X (mesh-agent or arbitrary), and for every mesh
//! agent B != X, this module enumerates routes via [`super::routes`] and
//! picks the best by `rtt + stddev_weight * stddev`. Aggregates per
//! candidate include coverage, mean ping under T, route-kind shares, and
//! a coverage-weighted ping metric used as the default sort key.

use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::campaign::eval::legs::{LegLookup, LegMeasurement};
use crate::campaign::eval::routes::{enumerate_routes, ComposedRoute, RouteKind};
use crate::campaign::eval::{AgentRow, EvaluationInputs, EvaluationOutputs};
use crate::campaign::model::{EdgeRouteKind, Endpoint, LegSource};

/// Output bundle of the `evaluate_edge_candidate` arm. Mirrors the
/// per-candidate / per-pair persistence shape (Phase G writes these into
/// `campaign_evaluation_candidates` + a new edge-pair-details table).
#[derive(Debug, Clone)]
pub struct EdgeCandidateOutputs {
    /// One row per candidate IP, sorted by `coverage_weighted_ping_ms`
    /// ASC (tiebreak on `coverage_count` DESC, then `mean_ms_under_t`
    /// ASC). Candidates with `coverage_count = 0` and `None` metrics
    /// sink to the bottom.
    pub candidates: Vec<EdgeCandidateRow>,
    /// Reason map for candidates that produced no qualifying pairs.
    pub unqualified_reasons: BTreeMap<String, String>,
}

/// One candidate IP with aggregates and per-pair detail rows.
#[derive(Debug, Clone)]
pub struct EdgeCandidateRow {
    /// Candidate IP X.
    pub candidate_ip: IpAddr,
    /// Catalogue display label, when present.
    pub display_name: Option<String>,
    /// Catalogue city, when present.
    pub city: Option<String>,
    /// Catalogue ISO country code, when present.
    pub country_code: Option<String>,
    /// Catalogue ASN, when present.
    pub asn: Option<i64>,
    /// Catalogue network operator, when present.
    pub network_operator: Option<String>,
    /// Catalogue website URL, when present.
    pub website: Option<String>,
    /// Catalogue free-text notes, when present.
    pub notes: Option<String>,
    /// True iff X resolves to a registered mesh agent's IP.
    pub is_mesh_member: bool,
    /// Mesh agent id when `is_mesh_member`, else `None`.
    pub agent_id: Option<String>,
    /// Number of destination agents B for which a route under T was
    /// found.
    pub coverage_count: i32,
    /// Total destination agents evaluated (= `agents.len()` minus self
    /// when X is a mesh agent).
    pub destinations_total: i32,
    /// Mean of `best_route_ms` across qualifying pairs, in ms. `None`
    /// when `coverage_count == 0`.
    pub mean_ms_under_t: Option<f32>,
    /// `mean_ms_under_t * (destinations_total / coverage_count)`. With
    /// full coverage equals `mean_ms_under_t`; with partial coverage
    /// applies an inverse-coverage penalty. `None` when
    /// `coverage_count == 0`.
    pub coverage_weighted_ping_ms: Option<f32>,
    /// Fraction of qualifying pairs where the winning route is direct.
    /// `None` when `coverage_count == 0`.
    pub direct_share: Option<f32>,
    /// Fraction of qualifying pairs where the winning route is one-hop.
    pub onehop_share: Option<f32>,
    /// Fraction of qualifying pairs where the winning route is two-hop.
    pub twohop_share: Option<f32>,
    /// True iff X is a mesh agent AND at least one X-touching leg of
    /// any winning route used real (non-substituted) VM-continuous data.
    pub has_real_x_source_data: bool,
    /// Per-(X, B) pair scoring rows. Persisted into the edge-pair detail
    /// table by [`crate::campaign::evaluation_repo::insert_evaluation`]
    /// (Phase G).
    pub pair_details: Vec<EdgePairRow>,
}

/// Per-(X, B) best-route row. Captures both the chosen score, route
/// shape, and the legs (for drilldown) — independent of the storage
/// filter (which doesn't apply in this mode).
#[derive(Debug, Clone)]
pub struct EdgePairRow {
    /// Destination mesh agent B's id.
    pub destination_agent_id: String,
    /// Destination mesh agent B's reverse-DNS hostname (handler stamps).
    pub destination_hostname: Option<String>,
    /// Best route's RTT including stddev penalty (ms).
    pub best_route_ms: f32,
    /// Best route's compound loss ratio (0.0–1.0).
    pub best_route_loss_ratio: f32,
    /// Best route's composed stddev (ms).
    pub best_route_stddev_ms: f32,
    /// Direct / OneHop / TwoHop classification of the chosen route.
    pub best_route_kind: EdgeRouteKind,
    /// Intermediary agent ids on the chosen route, in chain order.
    pub best_route_intermediaries: Vec<String>,
    /// Full leg list of the chosen route — for drilldown and the
    /// `has_real_x_source_data` check.
    pub best_route_legs: Vec<LegMeasurement>,
    /// True iff `best_route_ms <= useful_latency_ms` (T threshold).
    pub qualifies_under_t: bool,
    /// True iff no route survived the loss filter for this (X, B).
    /// Sentinel-valued metric fields when set.
    pub is_unreachable: bool,
}

/// EdgeCandidate evaluator entry point. Pure function — no DB, no IO.
///
/// Spec §4 algorithm:
/// 1. For each candidate IP X (mesh-agent or arbitrary):
///    1. Determine X's `Endpoint` (Agent vs. CandidateIp).
///    2. For each mesh agent B != X:
///       - Enumerate routes via [`enumerate_routes`].
///       - Filter by `loss_ratio <= loss_threshold_ratio`.
///       - Pick the best surviving route by
///         `rtt + stddev_weight * stddev`.
///       - Mark `qualifies_under_t` when score `<= useful_latency_ms`.
///    3. Aggregate: coverage_count, mean_ms_under_t,
///       coverage_weighted_ping_ms, direct/onehop/twohop shares,
///       has_real_x_source_data.
/// 2. Sort candidates by `coverage_weighted_ping_ms` ASC; tiebreak
///    `coverage_count` DESC; tiebreak `mean_ms_under_t` ASC.
pub fn evaluate(inputs: EvaluationInputs) -> EvaluationOutputs {
    let lookup = LegLookup::build(&inputs.measurements, &inputs.agents);
    // `LegLookup` resolves either endpoint form against the agent
    // roster, so a single representation per intermediary would suffice.
    // The pool keeps both `Agent { id }` and `CandidateIp { ip }`
    // entries as defence-in-depth — duplicates that fail to compose are
    // silently discarded by `enumerate_routes`.
    let intermediaries: Vec<Endpoint> = inputs
        .agents
        .iter()
        .flat_map(|a| {
            [
                Endpoint::Agent {
                    id: a.agent_id.clone(),
                },
                Endpoint::CandidateIp { ip: a.ip },
            ]
        })
        .collect();

    let max_hops_u8 = u8::try_from(inputs.max_hops.max(0)).unwrap_or(u8::MAX);

    let mut candidates: Vec<EdgeCandidateRow> = Vec::with_capacity(inputs.candidate_ips.len());
    for x_ip in &inputs.candidate_ips {
        let x_endpoint = endpoint_for_ip(*x_ip, &inputs.agents);
        let row = score_candidate(
            *x_ip,
            &x_endpoint,
            &inputs,
            &lookup,
            &intermediaries,
            max_hops_u8,
        );
        candidates.push(row);
    }

    sort_candidates(&mut candidates);

    EvaluationOutputs::EdgeCandidate(EdgeCandidateOutputs {
        candidates,
        unqualified_reasons: BTreeMap::new(),
    })
}

/// Resolve a candidate IP to its `Endpoint` form. When the IP matches a
/// mesh agent's IP, returns `Endpoint::Agent { id }`; otherwise
/// `Endpoint::CandidateIp { ip }`.
fn endpoint_for_ip(ip: IpAddr, agents: &[AgentRow]) -> Endpoint {
    match agents.iter().find(|a| a.ip == ip) {
        Some(agent) => Endpoint::Agent {
            id: agent.agent_id.clone(),
        },
        None => Endpoint::CandidateIp { ip },
    }
}

/// Score one candidate X against every mesh agent B != X.
fn score_candidate(
    x_ip: IpAddr,
    x_endpoint: &Endpoint,
    inputs: &EvaluationInputs,
    lookup: &LegLookup<'_>,
    intermediaries: &[Endpoint],
    max_hops: u8,
) -> EdgeCandidateRow {
    let mut pair_rows: Vec<EdgePairRow> = Vec::with_capacity(inputs.agents.len());
    let mut destinations_total: i32 = 0;

    // Defense-in-depth: drop both endpoint forms of X from the intermediary
    // pool so the per-(X, B) `pool_for_b` filter below never has to bridge
    // the `Agent` / `CandidateIp` discriminants when X is a mesh agent. The
    // `Agent` form would otherwise be caught only via Endpoint equality in
    // `enumerate_routes`' `y == source` skip while the `CandidateIp(x.ip)`
    // form would slip through and produce a phantom `(Agent("x"), Ip(x.ip))`
    // leg that is never measured.
    let (x_agent_id, x_ip_opt) = match x_endpoint {
        Endpoint::Agent { id } => {
            let ip = inputs
                .agents
                .iter()
                .find(|a| &a.agent_id == id)
                .map(|a| a.ip);
            (Some(id.as_str()), ip)
        }
        Endpoint::CandidateIp { ip } => (None, Some(*ip)),
    };

    for b in &inputs.agents {
        // X == B self-pair excluded (spec §5.5 edge case). Compare on
        // the canonical identity (agent id when X is mesh, IP
        // otherwise) rather than on the `Endpoint` form because B's
        // route-side endpoint is always `CandidateIp(b.ip)` for the
        // leg-lookup to hit (see comment below).
        let is_self = match x_endpoint {
            Endpoint::Agent { id } => id == &b.agent_id,
            Endpoint::CandidateIp { ip } => ip == &b.ip,
        };
        if is_self {
            continue;
        }
        destinations_total += 1;

        // Either endpoint form would resolve correctly against the
        // roster-aware [`LegLookup`]; `CandidateIp(b.ip)` is convenient
        // because `enumerate_routes` compares endpoints by variant when
        // skipping intermediaries that equal `source` / `destination`,
        // and the pool below filters both forms of B out explicitly so
        // the comparison never has to bridge variants.
        let b_endpoint = Endpoint::CandidateIp { ip: b.ip };

        // Exclude both X and B from the intermediary pool. The pool
        // carries both `Agent(agent_id)` and `CandidateIp(ip)` for every
        // mesh agent (see the pool-construction comment above). All four
        // forms must be removed: `enumerate_routes`' built-in
        // `y == source` / `y == destination` skip compares endpoint
        // variants, but B is passed as `CandidateIp(b.ip)` and X as
        // `Agent(x.id)` (when X is a mesh member) — each variant's twin
        // has a different discriminant and would slip through. Pre-
        // filtering here keeps the route enumerator's contract simple
        // and removes every form of X and B.
        let pool_for_b: Vec<Endpoint> = intermediaries
            .iter()
            .filter(|e| match e {
                Endpoint::Agent { id } => id != &b.agent_id && Some(id.as_str()) != x_agent_id,
                Endpoint::CandidateIp { ip } => ip != &b.ip && Some(*ip) != x_ip_opt,
            })
            .cloned()
            .collect();

        let routes = enumerate_routes(
            lookup,
            x_endpoint,
            &b_endpoint,
            &pool_for_b,
            max_hops,
            inputs.max_transit_rtt_ms,
            inputs.max_transit_stddev_ms,
            inputs.stddev_weight,
        );
        let best = pick_best_route(routes, inputs.loss_threshold_ratio, inputs.stddev_weight);
        let row = build_pair_row(b, best, inputs.useful_latency_ms, inputs.stddev_weight);
        pair_rows.push(row);
    }

    let aggregates = aggregate_pair_rows(&pair_rows, destinations_total);
    let has_real_x_source_data = compute_has_real_x_source_data(x_endpoint, &pair_rows);
    let enrichment = inputs.enrichment.get(&x_ip).cloned().unwrap_or_default();
    let (is_mesh_member, agent_id) = match x_endpoint {
        Endpoint::Agent { id } => (true, Some(id.clone())),
        Endpoint::CandidateIp { .. } => (false, None),
    };

    EdgeCandidateRow {
        candidate_ip: x_ip,
        display_name: enrichment.display_name,
        city: enrichment.city,
        country_code: enrichment.country_code,
        asn: enrichment.asn,
        network_operator: enrichment.network_operator,
        website: enrichment.website,
        notes: enrichment.notes,
        is_mesh_member,
        agent_id,
        coverage_count: aggregates.coverage_count,
        destinations_total,
        mean_ms_under_t: aggregates.mean_ms_under_t,
        coverage_weighted_ping_ms: aggregates.coverage_weighted_ping_ms,
        direct_share: aggregates.direct_share,
        onehop_share: aggregates.onehop_share,
        twohop_share: aggregates.twohop_share,
        has_real_x_source_data,
        pair_details: pair_rows,
    }
}

/// Pick the best surviving route by `rtt + stddev_weight * stddev`.
/// Routes whose `loss_ratio` exceeds the threshold are dropped first.
fn pick_best_route(
    routes: Vec<ComposedRoute>,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
) -> Option<ComposedRoute> {
    routes
        .into_iter()
        .filter(|r| r.loss_ratio <= loss_threshold_ratio)
        .min_by(|a, b| {
            let a_score = a.rtt_ms + stddev_weight * a.stddev_ms;
            let b_score = b.rtt_ms + stddev_weight * b.stddev_ms;
            a_score
                .partial_cmp(&b_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Build one [`EdgePairRow`] from a destination B and the optional best
/// route. When `best` is `None`, emits a sentinel-valued unreachable row
/// (per spec §4: `best_route_ms = INFINITY`, `loss = 1.0`).
fn build_pair_row(
    b: &AgentRow,
    best: Option<ComposedRoute>,
    useful_latency_ms: Option<f32>,
    stddev_weight: f32,
) -> EdgePairRow {
    match best {
        Some(r) => {
            // `best_route_ms` is the penalised score the picker
            // minimised — pre-compute once for both qualification and
            // persistence.
            let best_route_ms = r.rtt_ms + stddev_weight * r.stddev_ms;
            let qualifies_under_t = useful_latency_ms
                .map(|t| best_route_ms <= t)
                .unwrap_or(false);
            EdgePairRow {
                destination_agent_id: b.agent_id.clone(),
                destination_hostname: b.hostname.clone(),
                best_route_ms,
                best_route_loss_ratio: r.loss_ratio,
                best_route_stddev_ms: r.stddev_ms,
                best_route_kind: route_kind_to_dto(r.kind),
                best_route_intermediaries: r
                    .intermediaries
                    .iter()
                    .filter_map(|e| match e {
                        Endpoint::Agent { id } => Some(id.clone()),
                        Endpoint::CandidateIp { .. } => None,
                    })
                    .collect(),
                best_route_legs: r.legs,
                qualifies_under_t,
                is_unreachable: false,
            }
        }
        None => EdgePairRow {
            destination_agent_id: b.agent_id.clone(),
            destination_hostname: b.hostname.clone(),
            // Sentinel for unreachable: f32::INFINITY would never
            // satisfy the qualifies check while staying obviously
            // non-meaningful in dashboards/exports.
            best_route_ms: f32::INFINITY,
            best_route_loss_ratio: 1.0,
            best_route_stddev_ms: 0.0,
            best_route_kind: EdgeRouteKind::Direct,
            best_route_intermediaries: Vec::new(),
            best_route_legs: Vec::new(),
            qualifies_under_t: false,
            is_unreachable: true,
        },
    }
}

/// Aggregates derived from a candidate's pair rows.
struct CandidateAggregates {
    coverage_count: i32,
    mean_ms_under_t: Option<f32>,
    coverage_weighted_ping_ms: Option<f32>,
    direct_share: Option<f32>,
    onehop_share: Option<f32>,
    twohop_share: Option<f32>,
}

fn aggregate_pair_rows(rows: &[EdgePairRow], destinations_total: i32) -> CandidateAggregates {
    let qualifying: Vec<&EdgePairRow> = rows.iter().filter(|p| p.qualifies_under_t).collect();
    let coverage_count = qualifying.len() as i32;
    if coverage_count == 0 {
        return CandidateAggregates {
            coverage_count: 0,
            mean_ms_under_t: None,
            coverage_weighted_ping_ms: None,
            direct_share: None,
            onehop_share: None,
            twohop_share: None,
        };
    }
    let sum_ms: f32 = qualifying.iter().map(|p| p.best_route_ms).sum();
    let mean = sum_ms / coverage_count as f32;
    // Inverse-coverage penalty: with full coverage equals `mean`; with
    // partial coverage scales up by `destinations_total / coverage_count`.
    let weighted = mean * (destinations_total as f32 / coverage_count as f32);
    let direct_n = qualifying
        .iter()
        .filter(|p| p.best_route_kind == EdgeRouteKind::Direct)
        .count() as f32;
    let onehop_n = qualifying
        .iter()
        .filter(|p| p.best_route_kind == EdgeRouteKind::OneHop)
        .count() as f32;
    let twohop_n = qualifying
        .iter()
        .filter(|p| p.best_route_kind == EdgeRouteKind::TwoHop)
        .count() as f32;
    let denom = coverage_count as f32;
    CandidateAggregates {
        coverage_count,
        mean_ms_under_t: Some(mean),
        coverage_weighted_ping_ms: Some(weighted),
        direct_share: Some(direct_n / denom),
        onehop_share: Some(onehop_n / denom),
        twohop_share: Some(twohop_n / denom),
    }
}

/// `has_real_x_source_data`: true iff X is a mesh agent AND at least one
/// leg of any winning route, where the leg has X as an endpoint, has
/// `source = LegSource::VmContinuous` AND was NOT substituted from the
/// reverse direction (per Phase C, substitution is signalled via
/// `was_substituted: bool`, not `LegSource::SymmetricReuse`).
fn compute_has_real_x_source_data(x_endpoint: &Endpoint, pair_rows: &[EdgePairRow]) -> bool {
    let x_agent_id: &str = match x_endpoint {
        Endpoint::Agent { id } => id.as_str(),
        Endpoint::CandidateIp { .. } => return false,
    };
    pair_rows.iter().any(|p| {
        p.best_route_legs.iter().any(|l| {
            let touches_x = matches!(&l.from, Endpoint::Agent { id } if id == x_agent_id)
                || matches!(&l.to, Endpoint::Agent { id } if id == x_agent_id);
            touches_x && l.source == LegSource::VmContinuous && !l.was_substituted
        })
    })
}

fn route_kind_to_dto(k: RouteKind) -> EdgeRouteKind {
    match k {
        RouteKind::Direct => EdgeRouteKind::Direct,
        RouteKind::OneHop => EdgeRouteKind::OneHop,
        RouteKind::TwoHop => EdgeRouteKind::TwoHop,
    }
}

/// Default sort: `coverage_weighted_ping_ms` ASC; tiebreak
/// `coverage_count` DESC; tiebreak `mean_ms_under_t` ASC. Candidates
/// with `coverage_count = 0` (None metrics) sink to the bottom.
fn sort_candidates(candidates: &mut [EdgeCandidateRow]) {
    candidates.sort_by(|a, b| {
        cmp_optional_ms(a.coverage_weighted_ping_ms, b.coverage_weighted_ping_ms)
            .then_with(|| b.coverage_count.cmp(&a.coverage_count))
            .then_with(|| cmp_optional_ms(a.mean_ms_under_t, b.mean_ms_under_t))
    });
}

/// `Some` < `None` so candidates with metrics sort ahead of those
/// without; among `Some` values the smaller wins.
fn cmp_optional_ms(a: Option<f32>, b: Option<f32>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::eval::AttributedMeasurement;
    use crate::campaign::model::{DirectSource, EvaluationMode};
    use std::collections::HashMap;

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

    fn meas(src: &str, dst: &str, rtt: f32, stddev: f32, loss: f32) -> AttributedMeasurement {
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

    /// Build a fully-meshed agent set + transit candidate so every B is
    /// reachable from X via direct, one-hop, or two-hop. Used as the
    /// base fixture for aggregate tests.
    fn build_inputs(
        agents: Vec<AgentRow>,
        candidate_ips: Vec<IpAddr>,
        measurements: Vec<AttributedMeasurement>,
        useful_latency_ms: Option<f32>,
        max_hops: u8,
    ) -> EvaluationInputs {
        EvaluationInputs {
            measurements,
            agents,
            candidate_ips,
            enrichment: HashMap::new(),
            loss_threshold_ratio: 0.05,
            stddev_weight: 1.0,
            mode: EvaluationMode::EdgeCandidate,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms,
            max_hops: max_hops as i16,
        }
    }

    fn outputs_edge(out: EvaluationOutputs) -> EdgeCandidateOutputs {
        match out {
            EvaluationOutputs::EdgeCandidate(o) => o,
            EvaluationOutputs::Triple(_) => panic!("expected EdgeCandidate variant"),
        }
    }

    #[test]
    fn coverage_weighted_ping_equals_mean_under_full_coverage() {
        // 1 candidate (X, agent), 2 destinations B1/B2; every B reachable
        // direct under T. Mean = (10 + 20)/2 = 15; full coverage so
        // weighted = mean = 15.
        let agents = vec![
            agent("x", "10.0.0.1"),
            agent("b1", "10.0.0.2"),
            agent("b2", "10.0.0.3"),
        ];
        let measurements = vec![
            meas("x", "10.0.0.2", 10.0, 0.0, 0.0),
            meas("x", "10.0.0.3", 20.0, 0.0, 0.0),
        ];
        let inputs = build_inputs(
            agents,
            vec![ip("10.0.0.1")], // X is mesh agent x
            measurements,
            Some(100.0),
            2,
        );
        let out = outputs_edge(evaluate(inputs));
        let row = &out.candidates[0];
        assert_eq!(row.coverage_count, 2);
        assert_eq!(row.destinations_total, 2);
        assert!((row.mean_ms_under_t.unwrap() - 15.0).abs() < 1e-4);
        assert!((row.coverage_weighted_ping_ms.unwrap() - 15.0).abs() < 1e-4);
        assert_eq!(row.direct_share, Some(1.0));
    }

    #[test]
    fn coverage_weighted_ping_inflates_under_partial_coverage() {
        // 1 candidate (X, agent), 5 destinations B1-B5. 4 of 5 reachable
        // under T; the 5th is unreachable. Mean of qualifying = 10;
        // weighted = 10 * (5 / 4) = 12.5.
        let agents = vec![
            agent("x", "10.0.0.1"),
            agent("b1", "10.0.0.2"),
            agent("b2", "10.0.0.3"),
            agent("b3", "10.0.0.4"),
            agent("b4", "10.0.0.5"),
            agent("b5", "10.0.0.6"),
        ];
        // Direct legs from x to b1..b4 with rtt=10. b5 has no direct
        // route (no measurement) and no usable indirect path.
        let measurements = vec![
            meas("x", "10.0.0.2", 10.0, 0.0, 0.0),
            meas("x", "10.0.0.3", 10.0, 0.0, 0.0),
            meas("x", "10.0.0.4", 10.0, 0.0, 0.0),
            meas("x", "10.0.0.5", 10.0, 0.0, 0.0),
        ];
        let inputs = build_inputs(
            agents,
            vec![ip("10.0.0.1")],
            measurements,
            Some(50.0),
            0, // disable transit hops so b5 stays unreachable
        );
        let out = outputs_edge(evaluate(inputs));
        let row = &out.candidates[0];
        assert_eq!(row.coverage_count, 4);
        assert_eq!(row.destinations_total, 5);
        assert!((row.mean_ms_under_t.unwrap() - 10.0).abs() < 1e-4);
        assert!(
            (row.coverage_weighted_ping_ms.unwrap() - 12.5).abs() < 1e-4,
            "got {}",
            row.coverage_weighted_ping_ms.unwrap()
        );
    }

    #[test]
    fn all_unreachable_yields_none_aggregates() {
        let agents = vec![
            agent("x", "10.0.0.1"),
            agent("b1", "10.0.0.2"),
            agent("b2", "10.0.0.3"),
        ];
        // No measurements at all → every destination unreachable.
        let measurements: Vec<AttributedMeasurement> = vec![];
        let inputs = build_inputs(agents, vec![ip("10.0.0.1")], measurements, Some(100.0), 2);
        let out = outputs_edge(evaluate(inputs));
        let row = &out.candidates[0];
        assert_eq!(row.coverage_count, 0);
        assert_eq!(row.destinations_total, 2);
        assert!(row.mean_ms_under_t.is_none());
        assert!(row.coverage_weighted_ping_ms.is_none());
        assert!(row.direct_share.is_none());
        assert!(row.onehop_share.is_none());
        assert!(row.twohop_share.is_none());
        for pair in &row.pair_details {
            assert!(pair.is_unreachable);
            assert!(!pair.qualifies_under_t);
        }
    }

    #[test]
    fn default_sort_orders_by_coverage_weighted_ping_ascending() {
        // Two candidates: both mesh agents (x1 and x2), plus two plain
        // destinations (b1, b2). All four are in the agent roster, so
        // each candidate's `destinations_total` = 3 (the other three).
        //
        // X1 (10.0.0.1) with max_hops=0 and T=100ms:
        //   x1→b1 10ms, x1→b2 10ms, x1→x2 30ms → all qualify.
        //   coverage_count=3, mean=(10+10+30)/3≈16.67, weighted=16.67
        //   (full coverage → weighted = mean).
        //
        // X2 (10.0.0.4):
        //   x2→b1 20ms, x2→b2 20ms, x2→x1 30ms → all qualify.
        //   coverage_count=3, mean=(20+20+30)/3≈23.33, weighted=23.33.
        //
        // X1 (16.67) sorts before X2 (23.33) as expected.
        let agents = vec![
            agent("x1", "10.0.0.1"),
            agent("b1", "10.0.0.2"),
            agent("b2", "10.0.0.3"),
            agent("x2", "10.0.0.4"),
        ];
        let measurements = vec![
            meas("x1", "10.0.0.2", 10.0, 0.0, 0.0), // x1 → b1
            meas("x1", "10.0.0.3", 10.0, 0.0, 0.0), // x1 → b2
            meas("x1", "10.0.0.4", 30.0, 0.0, 0.0), // x1 → x2
            meas("x2", "10.0.0.2", 20.0, 0.0, 0.0), // x2 → b1
            meas("x2", "10.0.0.3", 20.0, 0.0, 0.0), // x2 → b2
            meas("x2", "10.0.0.1", 30.0, 0.0, 0.0), // x2 → x1
        ];
        let inputs = build_inputs(
            agents,
            vec![ip("10.0.0.1"), ip("10.0.0.4")],
            measurements,
            Some(100.0),
            0,
        );
        let out = outputs_edge(evaluate(inputs));
        assert_eq!(out.candidates.len(), 2);
        assert_eq!(out.candidates[0].candidate_ip, ip("10.0.0.1"));
        assert_eq!(out.candidates[1].candidate_ip, ip("10.0.0.4"));
    }

    #[test]
    fn x_equals_b_self_pair_is_excluded_from_destinations_total() {
        // 3 mesh agents; X = one of them. destinations_total = 2 (not 3).
        let agents = vec![
            agent("x", "10.0.0.1"),
            agent("b1", "10.0.0.2"),
            agent("b2", "10.0.0.3"),
        ];
        let measurements = vec![
            meas("x", "10.0.0.2", 10.0, 0.0, 0.0),
            meas("x", "10.0.0.3", 20.0, 0.0, 0.0),
        ];
        let inputs = build_inputs(agents, vec![ip("10.0.0.1")], measurements, Some(100.0), 0);
        let out = outputs_edge(evaluate(inputs));
        let row = &out.candidates[0];
        assert_eq!(row.destinations_total, 2, "self-pair X==B excluded");
        assert_eq!(row.pair_details.len(), 2);
        assert!(row
            .pair_details
            .iter()
            .all(|p| p.destination_agent_id != "x"));
    }

    #[test]
    fn arbitrary_ip_candidate_has_is_mesh_member_false() {
        // X is an arbitrary IP not in the agent roster.
        let agents = vec![agent("a", "10.0.0.1"), agent("b", "10.0.0.2")];
        // No measurements involving 198.51.100.5 — the arbitrary IP is
        // treated as a CandidateIp and routed via mesh-member transit.
        // Without source legs from X, all pairs are unreachable.
        let measurements = vec![meas("a", "10.0.0.2", 10.0, 0.0, 0.0)];
        let inputs = build_inputs(
            agents,
            vec![ip("198.51.100.5")],
            measurements,
            Some(100.0),
            2,
        );
        let out = outputs_edge(evaluate(inputs));
        let row = &out.candidates[0];
        assert!(!row.is_mesh_member);
        assert!(row.agent_id.is_none());
        assert!(!row.has_real_x_source_data);
    }

    // ── Multi-hop route tests (regression guard for the intermediary pool model) ──

    #[test]
    fn one_hop_route_via_agent_intermediary_for_arbitrary_x() {
        // Fixture: 3 agents A (10.0.0.1), B (10.0.0.2), C (10.0.0.3).
        // Candidate X = arbitrary IP 203.0.113.99 (not a mesh agent).
        //
        // Seeded measurements:
        //   meas("a", X.ip, 5ms)   → gives leg X→A via reverse resolution
        //   meas("a", B.ip, 10ms)  → gives leg A→B forward
        //   meas("a", C.ip, 10ms)  → gives leg A→C forward
        //
        // With max_hops=1 and T=100ms:
        //   X targeting A: direct (reverse-resolved 5ms) qualifies.
        //   X targeting B: 1-hop X→A→B (5+10=15ms) qualifies.
        //   X targeting C: 1-hop X→A→C (5+10=15ms) qualifies.
        //
        // The test guards that B and C still resolve via the 1-hop
        // intermediary path, and that A surfaces as a direct hit rather
        // than going via a stale "unreachable" sentinel.
        let agents = vec![
            agent("a", "10.0.0.1"),
            agent("b", "10.0.0.2"),
            agent("c", "10.0.0.3"),
        ];
        let x_ip = ip("203.0.113.99");
        let measurements = vec![
            meas("a", "203.0.113.99", 5.0, 0.0, 0.0), // X→A via reverse resolution
            meas("a", "10.0.0.2", 10.0, 0.0, 0.0),    // A→B forward
            meas("a", "10.0.0.3", 10.0, 0.0, 0.0),    // A→C forward
        ];
        let inputs = build_inputs(agents, vec![x_ip], measurements, Some(100.0), 1);
        let out = outputs_edge(evaluate(inputs));
        assert_eq!(out.candidates.len(), 1);
        let row = &out.candidates[0];
        assert_eq!(
            row.destinations_total, 3,
            "3 agents, none excluded (X ≠ any agent)"
        );
        // A direct + B/C via 1-hop = 3 covered.
        assert_eq!(
            row.coverage_count, 3,
            "A direct + B/C via 1-hop must all qualify"
        );
        for pair in &row.pair_details {
            if pair.destination_agent_id == "b" || pair.destination_agent_id == "c" {
                assert_eq!(
                    pair.best_route_kind,
                    EdgeRouteKind::OneHop,
                    "pair {} must use 1-hop route, got {:?}",
                    pair.destination_agent_id,
                    pair.best_route_kind
                );
                assert_eq!(
                    pair.best_route_intermediaries.len(),
                    1,
                    "1-hop must have exactly one intermediary"
                );
                assert_eq!(
                    pair.best_route_intermediaries[0], "a",
                    "intermediary must be agent a"
                );
                // penalised score = rtt + stddev_weight * stddev = 15 + 1.0*0 = 15
                assert!(
                    (pair.best_route_ms - 15.0).abs() < 1e-3,
                    "1-hop RTT must be 5+10=15ms, got {}",
                    pair.best_route_ms
                );
            }
            if pair.destination_agent_id == "a" {
                assert_eq!(
                    pair.best_route_kind,
                    EdgeRouteKind::Direct,
                    "A is reachable directly via reverse-resolved leg: {pair:?}"
                );
                assert!(!pair.is_unreachable);
            }
        }
    }

    #[test]
    fn two_hop_route_for_mesh_agent_x() {
        // Fixture: 4 agents X (10.0.0.1), A (10.0.0.2), C (10.0.0.3), B (10.0.0.4).
        // Candidate X is a mesh agent (so x_endpoint = Agent("x")).
        //
        // The only resolvable 2-hop shape in the LegLookup model is:
        //   Agent("x") → CandidateIp(A.ip) → Agent("c") → CandidateIp(B.ip)
        //
        // Legs resolve as follows:
        //   L1: lookup(Agent("x"), CandidateIp(A.ip))
        //         forward=(Agent("x"), Ip(A.ip)) → meas("x","10.0.0.2",5ms) ✓
        //   L2: lookup(CandidateIp(A.ip), Agent("c"))
        //         reverse=(Agent("c"), Ip(A.ip)) → meas("c","10.0.0.2",10ms) ✓
        //         (wait, that's meas("c","A.ip") — so C→A not C→something else)
        //
        // More precisely, seeded measurements:
        //   meas("x", A.ip, 5ms)   L1: X→A forward
        //   meas("c", A.ip, 10ms)  L2: A→C via reverse of this measurement
        //   meas("c", B.ip, 10ms)  L3: C→B forward
        //
        // With max_hops=2 and T=100ms, X→A→C→B: 5+10+10 = 25ms (2-hop).
        //
        // This guards that max_hops=2 is not a dead letter for mesh-agent X.
        // Pre-fix, the intermediary pool used `Agent { id }` exclusively, so:
        //   - L1: lookup(Agent("x"), Agent("a")) → (Agent("x"),Agent("a")) missing ✗
        //   - With fix (CandidateIp in pool), L1 → (Agent("x"),Ip(A.ip)) → FOUND ✓
        //   - L2: the intermediary for this leg is CandidateIp(A.ip) and the
        //     next is Agent("c") → reverse=(Agent("c"),Ip(A.ip)) → FOUND ✓
        let agents = vec![
            agent("x", "10.0.0.1"),
            agent("a", "10.0.0.2"),
            agent("c", "10.0.0.3"),
            agent("b", "10.0.0.4"),
        ];
        let measurements = vec![
            meas("x", "10.0.0.2", 5.0, 0.0, 0.0),  // X→A forward (L1)
            meas("c", "10.0.0.2", 10.0, 0.0, 0.0), // A→C via reverse meas (L2)
            meas("c", "10.0.0.4", 10.0, 0.0, 0.0), // C→B forward (L3)
        ];
        // X is the mesh-agent candidate; max_hops=2 to allow 2-hop routes.
        let inputs = build_inputs(
            agents,
            vec![ip("10.0.0.1")], // candidate = X
            measurements,
            Some(100.0),
            2,
        );
        let out = outputs_edge(evaluate(inputs));
        assert_eq!(out.candidates.len(), 1);
        let row = &out.candidates[0];

        // Find the pair for B — reachable via 2-hop X→A→C→B.
        let b_pair = row
            .pair_details
            .iter()
            .find(|p| p.destination_agent_id == "b")
            .expect("pair for B must exist");
        assert_eq!(
            b_pair.best_route_kind,
            EdgeRouteKind::TwoHop,
            "X→A→C→B must be a 2-hop route, got {:?}",
            b_pair.best_route_kind
        );
        // A 2-hop route has 3 legs.
        assert_eq!(
            b_pair.best_route_legs.len(),
            3,
            "2-hop route must have 3 legs"
        );
        // `best_route_intermediaries` contains only Agent-form entries
        // (CandidateIp intermediaries are stripped in `build_pair_row`).
        // In this fixture the route is X → CandidateIp(A.ip) → Agent("c") → B,
        // so only "c" survives the filter.
        assert!(
            b_pair.best_route_intermediaries.contains(&"c".to_string()),
            "intermediary agent c must appear in best_route_intermediaries"
        );
        // penalised score = 5+10+10 + 1.0*sqrt(0+0+0) = 25ms
        assert!(
            (b_pair.best_route_ms - 25.0).abs() < 1e-3,
            "2-hop RTT must be 5+10+10=25ms, got {}",
            b_pair.best_route_ms
        );
        assert!(b_pair.qualifies_under_t, "25ms must qualify under T=100ms");
        assert!(!b_pair.is_unreachable);
    }
}
