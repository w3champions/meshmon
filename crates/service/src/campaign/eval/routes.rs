//! Route enumeration up to `max_hops`. Spec §3.2.

use crate::campaign::eval::legs::{LegLookup, LegLookupResult, LegMeasurement};
use crate::campaign::model::Endpoint;

/// A fully-composed route from `source` to `destination`, with aggregated
/// RTT, stddev (orthogonal sum), loss (compound), and the full leg list.
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by Phase E (EdgeCandidate evaluator arm)
pub(crate) struct ComposedRoute {
    pub legs: Vec<LegMeasurement>,
    pub rtt_ms: f32,
    pub stddev_ms: f32,
    pub loss_ratio: f32,
    pub kind: RouteKind,
    /// Intermediary endpoints in chain order (empty for direct).
    pub intermediaries: Vec<Endpoint>,
}

/// The number of transit hops in a composed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // consumed by Phase E (EdgeCandidate evaluator arm)
pub(crate) enum RouteKind {
    Direct,
    OneHop,
    TwoHop,
}

/// Enumerate all routes from `source` to `destination` up to `max_hops`,
/// using agents from `intermediaries_pool` (source and destination are
/// automatically excluded) as transit points. Routes whose any leg is
/// missing or broken are discarded. Routes that exceed the cap thresholds
/// (`max_transit_rtt_ms` or `max_transit_stddev_ms`) are also discarded.
///
/// The cap check is applied against `rtt + stddev_weight * stddev` for the
/// RTT cap and raw `stddev` for the stddev cap.
///
/// Stddev is composed as the orthogonal (Pythagorean) sum:
///   `sqrt(s1² + s2² [+ s3²])`
///
/// Loss is composed as the complement of the product of survivals:
///   `1 - ∏(1 - l_i)`
#[allow(dead_code)] // consumed by Phase E (EdgeCandidate evaluator arm)
#[allow(clippy::too_many_arguments)] // 8 args are load-bearing; a params struct would add ceremony
pub(crate) fn enumerate_routes(
    lookup: &LegLookup<'_>,
    source: &Endpoint,
    destination: &Endpoint,
    intermediaries_pool: &[Endpoint],
    max_hops: u8,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    stddev_weight: f32,
) -> Vec<ComposedRoute> {
    let mut out = Vec::new();

    // 0-hop direct — guarded by source != destination only;
    // `max_hops >= 0` is a tautology for u8 and suppressed.
    if source != destination {
        if let Some(r) = compose_direct(
            lookup,
            source,
            destination,
            max_transit_rtt_ms,
            max_transit_stddev_ms,
            stddev_weight,
        ) {
            out.push(r);
        }
    }

    // 1-hop: source → Y → destination
    if max_hops >= 1 {
        for y in intermediaries_pool {
            if y == source || y == destination {
                continue;
            }
            if let Some(r) = compose_one_hop(
                lookup,
                source,
                y,
                destination,
                max_transit_rtt_ms,
                max_transit_stddev_ms,
                stddev_weight,
            ) {
                out.push(r);
            }
        }
    }

    // 2-hop: source → Y1 → Y2 → destination
    if max_hops >= 2 {
        for y1 in intermediaries_pool {
            if y1 == source || y1 == destination {
                continue;
            }
            for y2 in intermediaries_pool {
                if y2 == source || y2 == destination || y2 == y1 {
                    continue;
                }
                if let Some(r) = compose_two_hop(
                    lookup,
                    source,
                    y1,
                    y2,
                    destination,
                    max_transit_rtt_ms,
                    max_transit_stddev_ms,
                    stddev_weight,
                ) {
                    out.push(r);
                }
            }
        }
    }

    out
}

fn compose_direct(
    lookup: &LegLookup<'_>,
    source: &Endpoint,
    destination: &Endpoint,
    max_rtt: Option<f64>,
    max_stddev: Option<f64>,
    stddev_weight: f32,
) -> Option<ComposedRoute> {
    let leg = look_up_leg(lookup, source, destination)?;
    let rtt = leg.rtt_ms;
    let stddev = leg.stddev_ms;
    let loss = leg.loss_ratio;
    let with_penalty = rtt + stddev_weight * stddev;
    if exceeds_caps(with_penalty, stddev, max_rtt, max_stddev) {
        return None;
    }
    Some(ComposedRoute {
        legs: vec![leg],
        rtt_ms: rtt,
        stddev_ms: stddev,
        loss_ratio: loss,
        kind: RouteKind::Direct,
        intermediaries: vec![],
    })
}

fn compose_one_hop(
    lookup: &LegLookup<'_>,
    source: &Endpoint,
    intermediary: &Endpoint,
    destination: &Endpoint,
    max_rtt: Option<f64>,
    max_stddev: Option<f64>,
    stddev_weight: f32,
) -> Option<ComposedRoute> {
    let l1 = look_up_leg(lookup, source, intermediary)?;
    let l2 = look_up_leg(lookup, intermediary, destination)?;
    let rtt = l1.rtt_ms + l2.rtt_ms;
    let stddev = (l1.stddev_ms.powi(2) + l2.stddev_ms.powi(2)).sqrt();
    let loss = 1.0 - (1.0 - l1.loss_ratio) * (1.0 - l2.loss_ratio);
    let with_penalty = rtt + stddev_weight * stddev;
    if exceeds_caps(with_penalty, stddev, max_rtt, max_stddev) {
        return None;
    }
    Some(ComposedRoute {
        legs: vec![l1, l2],
        rtt_ms: rtt,
        stddev_ms: stddev,
        loss_ratio: loss,
        kind: RouteKind::OneHop,
        intermediaries: vec![intermediary.clone()],
    })
}

#[allow(clippy::too_many_arguments)] // 8 args mirror enumerate_routes's own signature
fn compose_two_hop(
    lookup: &LegLookup<'_>,
    source: &Endpoint,
    y1: &Endpoint,
    y2: &Endpoint,
    destination: &Endpoint,
    max_rtt: Option<f64>,
    max_stddev: Option<f64>,
    stddev_weight: f32,
) -> Option<ComposedRoute> {
    let l1 = look_up_leg(lookup, source, y1)?;
    let l2 = look_up_leg(lookup, y1, y2)?;
    let l3 = look_up_leg(lookup, y2, destination)?;
    let rtt = l1.rtt_ms + l2.rtt_ms + l3.rtt_ms;
    let stddev = (l1.stddev_ms.powi(2) + l2.stddev_ms.powi(2) + l3.stddev_ms.powi(2)).sqrt();
    let loss = 1.0 - (1.0 - l1.loss_ratio) * (1.0 - l2.loss_ratio) * (1.0 - l3.loss_ratio);
    let with_penalty = rtt + stddev_weight * stddev;
    if exceeds_caps(with_penalty, stddev, max_rtt, max_stddev) {
        return None;
    }
    Some(ComposedRoute {
        legs: vec![l1, l2, l3],
        rtt_ms: rtt,
        stddev_ms: stddev,
        loss_ratio: loss,
        kind: RouteKind::TwoHop,
        intermediaries: vec![y1.clone(), y2.clone()],
    })
}

fn look_up_leg(lookup: &LegLookup<'_>, from: &Endpoint, to: &Endpoint) -> Option<LegMeasurement> {
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
        _ => None, // Missing or Broken — discard route
    }
}

/// Returns `true` when the route's penalised RTT or raw stddev exceeds the
/// caller-supplied caps. Either cap may be `None` (meaning "no cap").
fn exceeds_caps(
    rtt_with_penalty: f32,
    stddev: f32,
    max_rtt: Option<f64>,
    max_stddev: Option<f64>,
) -> bool {
    if let Some(cap) = max_rtt {
        if (rtt_with_penalty as f64) > cap {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::eval::AttributedMeasurement;
    use crate::campaign::model::DirectSource;

    // ── test helpers ─────────────────────────────────────────────────────────

    fn agent(id: &str) -> Endpoint {
        Endpoint::Agent { id: id.into() }
    }

    fn candidate(s: &str) -> Endpoint {
        Endpoint::CandidateIp {
            ip: s.parse().unwrap(),
        }
    }

    /// Build an `AttributedMeasurement`. `src` is an agent id; `dst` is a
    /// destination IP string. `LegLookup` stores `(Agent(src), Ip(dst))` as
    /// the forward key, enabling both forward and symmetric-reverse lookups.
    fn meas(src: &str, dst: &str, rtt: f32, stddev: f32, loss: f32) -> AttributedMeasurement {
        AttributedMeasurement {
            source_agent_id: src.into(),
            destination_ip: dst.parse().unwrap(),
            latency_avg_ms: Some(rtt),
            latency_stddev_ms: Some(stddev),
            loss_ratio: loss,
            mtr_measurement_id: None,
            direct_source: DirectSource::ActiveProbe,
        }
    }

    fn lookup(measurements: &[AttributedMeasurement]) -> LegLookup<'_> {
        LegLookup::build(measurements)
    }

    // ── 0-hop (direct) tests ─────────────────────────────────────────────────

    // Fixture note: LegLookup stores `(Agent(src_id), Ip(dst_ip))` entries.
    // A direct `agent("a") → candidate("ip")` leg resolves via forward lookup.

    #[test]
    fn zero_hop_direct_returns_one_route_when_leg_exists() {
        let ms = vec![meas("a", "10.0.0.1", 20.0, 2.0, 0.0)];
        let lk = lookup(&ms);
        let routes = enumerate_routes(&lk, &agent("a"), &candidate("10.0.0.1"), &[], 0, None, None, 1.0);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, RouteKind::Direct);
        assert!((routes[0].rtt_ms - 20.0).abs() < 1e-4);
    }

    #[test]
    fn zero_hop_excluded_when_source_equals_destination() {
        let ms = vec![meas("a", "10.0.0.1", 20.0, 2.0, 0.0)];
        let lk = lookup(&ms);
        let ep = agent("a");
        let routes = enumerate_routes(&lk, &ep, &ep, &[], 0, None, None, 1.0);
        assert!(routes.is_empty());
    }

    // ── 1-hop tests ──────────────────────────────────────────────────────────

    // Fixture note: the EdgeCandidate 1-hop shape is
    //   candidate(X) → agent(Y) → candidate(B)
    // where:
    //   Leg X→Y: resolved via reverse of meas("y_id", X.ip, ...) (symmetric sub.)
    //   Leg Y→B: resolved via forward meas("y_id", B.ip, ...)

    #[test]
    fn one_hop_produces_n_routes_for_n_valid_intermediaries() {
        // X=10.0.0.200, B=10.0.0.99, intermediaries: y1, y2
        let ms = vec![
            meas("y1", "10.0.0.200", 10.0, 1.0, 0.0), // X→Y1 via symmetry
            meas("y1", "10.0.0.99", 12.0, 1.0, 0.0),  // Y1→B forward
            meas("y2", "10.0.0.200", 15.0, 1.0, 0.0), // X→Y2 via symmetry
            meas("y2", "10.0.0.99", 14.0, 1.0, 0.0),  // Y2→B forward
        ];
        let lk = lookup(&ms);
        let src = candidate("10.0.0.200");
        let dst = candidate("10.0.0.99");
        let pool = vec![agent("y1"), agent("y2")];
        let routes = enumerate_routes(&lk, &src, &dst, &pool, 1, None, None, 1.0);
        let n = routes.iter().filter(|r| r.kind == RouteKind::OneHop).count();
        assert_eq!(n, 2, "expected 2 one-hop routes (one per intermediary)");
    }

    #[test]
    fn one_hop_excludes_intermediary_equal_to_source_or_destination() {
        let ms = vec![
            meas("y1", "10.0.0.200", 10.0, 1.0, 0.0),
            meas("y1", "10.0.0.99", 12.0, 1.0, 0.0),
        ];
        let lk = lookup(&ms);
        let src = candidate("10.0.0.200");
        let dst = candidate("10.0.0.99");
        let y1 = agent("y1");
        // pool includes src and dst as well — they must be skipped
        let pool = vec![src.clone(), dst.clone(), y1.clone()];
        let routes = enumerate_routes(&lk, &src, &dst, &pool, 1, None, None, 1.0);
        let one_hop: Vec<_> = routes.iter().filter(|r| r.kind == RouteKind::OneHop).collect();
        assert_eq!(one_hop.len(), 1, "src and dst must be excluded from intermediary pool");
        assert_eq!(one_hop[0].intermediaries[0], y1);
    }

    // ── 2-hop tests ──────────────────────────────────────────────────────────

    // Fixture note: the resolvable 2-hop shape within LegLookup's Agent→Ip model is
    //   agent("src") → candidate(Y1.ip) → agent("y2") → candidate(B.ip)
    // where:
    //   Leg src→Y1:  forward meas("src", Y1.ip)  → (Agent("src"), Ip(Y1.ip)) ✓
    //   Leg Y1→Y2:   reverse of meas("y2", Y1.ip) → (Agent("y2"), Ip(Y1.ip)) ✓
    //   Leg Y2→dst:  forward meas("y2", B.ip)     → (Agent("y2"), Ip(B.ip)) ✓

    #[test]
    fn two_hop_route_resolves_and_has_correct_structure() {
        let ms = vec![
            meas("src", "10.0.0.1", 10.0, 1.0, 0.0), // src→C1 forward
            meas("y2", "10.0.0.1", 5.0, 0.5, 0.0),   // C1→Y2 via reverse
            meas("y2", "10.0.0.99", 8.0, 0.5, 0.0),  // Y2→dst forward
        ];
        let lk = lookup(&ms);
        let src = agent("src");
        let c1 = candidate("10.0.0.1");
        let y2 = agent("y2");
        let dst = candidate("10.0.0.99");
        // Pool = [c1, y2]; (c1,y2) resolves; (y2,c1) fails because src→y2 is unresolvable
        let pool = vec![c1, y2];
        let routes = enumerate_routes(&lk, &src, &dst, &pool, 2, None, None, 1.0);
        let two_hop_routes: Vec<_> = routes.iter().filter(|r| r.kind == RouteKind::TwoHop).collect();
        assert_eq!(two_hop_routes.len(), 1);
        assert_eq!(two_hop_routes[0].legs.len(), 3);
        assert_eq!(two_hop_routes[0].intermediaries.len(), 2);
    }

    #[test]
    fn two_hop_y1_equals_y2_excluded() {
        // Pool of 1 intermediary: the inner loop always skips (y==y1), so 0 two-hop routes.
        let ms = vec![
            meas("src", "10.0.0.1", 10.0, 1.0, 0.0),
            meas("y1", "10.0.0.1", 5.0, 0.5, 0.0),
            meas("y1", "10.0.0.99", 8.0, 0.5, 0.0),
        ];
        let lk = lookup(&ms);
        let src = agent("src");
        let pool = vec![candidate("10.0.0.1")];
        let routes = enumerate_routes(&lk, &src, &candidate("10.0.0.99"), &pool, 2, None, None, 1.0);
        let n = routes.iter().filter(|r| r.kind == RouteKind::TwoHop).count();
        assert_eq!(n, 0, "single-intermediary pool must produce 0 two-hop routes");
    }

    // ── missing/broken leg tests ──────────────────────────────────────────────

    #[test]
    fn route_discarded_when_any_leg_missing() {
        // Supply only leg src→intermediary; the intermediary→dst leg is absent.
        let ms = vec![meas("src", "10.0.0.200", 10.0, 1.0, 0.0)];
        let lk = lookup(&ms);
        let src = agent("src");
        let dst = candidate("10.0.0.99");
        let pool = vec![candidate("10.0.0.200")]; // no leg from 10.0.0.200 to dst
        let routes = enumerate_routes(&lk, &src, &dst, &pool, 1, None, None, 1.0);
        assert!(routes.is_empty(), "route with a missing leg must be discarded");
    }

    // ── cap tests ─────────────────────────────────────────────────────────────

    #[test]
    fn route_discarded_when_rtt_with_penalty_exceeds_max_transit_rtt() {
        // rtt=50, stddev=10, weight=1.0 → penalised=60; cap=55 → discard
        let ms = vec![meas("a", "10.0.0.1", 50.0, 10.0, 0.0)];
        let lk = lookup(&ms);
        let routes = enumerate_routes(&lk, &agent("a"), &candidate("10.0.0.1"), &[], 0, Some(55.0), None, 1.0);
        assert!(routes.is_empty(), "route exceeding RTT cap must be discarded");
    }

    #[test]
    fn route_passes_when_rtt_with_penalty_at_or_below_max_transit_rtt() {
        // rtt=50, stddev=5, weight=1.0 → penalised=55; cap=55 → keep (not strictly greater)
        let ms = vec![meas("a", "10.0.0.1", 50.0, 5.0, 0.0)];
        let lk = lookup(&ms);
        let routes = enumerate_routes(&lk, &agent("a"), &candidate("10.0.0.1"), &[], 0, Some(55.0), None, 1.0);
        assert_eq!(routes.len(), 1, "route at exactly the cap must be kept");
    }

    #[test]
    fn route_discarded_when_stddev_exceeds_max_transit_stddev() {
        // stddev=10.0, cap=9.0 → discard
        let ms = vec![meas("a", "10.0.0.1", 20.0, 10.0, 0.0)];
        let lk = lookup(&ms);
        let routes = enumerate_routes(&lk, &agent("a"), &candidate("10.0.0.1"), &[], 0, None, Some(9.0), 1.0);
        assert!(routes.is_empty(), "route exceeding stddev cap must be discarded");
    }

    // ── composition-correctness tests ─────────────────────────────────────────

    // Fixture uses candidate(X) → agent(Y) → candidate(B) shape:
    //   Leg X→Y: reverse of meas("y", X.ip)
    //   Leg Y→B: forward meas("y", B.ip)

    #[test]
    fn loss_composition_one_hop_correctness() {
        // l1=0.1, l2=0.1 → 1 − (0.9·0.9) = 0.19
        let ms = vec![
            meas("y", "10.0.0.200", 10.0, 1.0, 0.1), // X→Y via symmetry, loss=0.1
            meas("y", "10.0.0.99", 10.0, 1.0, 0.1),  // Y→B forward, loss=0.1
        ];
        let lk = lookup(&ms);
        let pool = vec![agent("y")];
        let routes = enumerate_routes(&lk, &candidate("10.0.0.200"), &candidate("10.0.0.99"), &pool, 1, None, None, 1.0);
        let r = routes.iter().find(|r| r.kind == RouteKind::OneHop).expect("one-hop must exist");
        let expected = 1.0 - 0.9_f32 * 0.9_f32;
        assert!((r.loss_ratio - expected).abs() < 1e-5, "got {}", r.loss_ratio);
    }

    #[test]
    fn stddev_composition_one_hop_correctness() {
        // s1=3.0, s2=4.0 → sqrt(9+16) = 5.0
        let ms = vec![
            meas("y", "10.0.0.200", 10.0, 3.0, 0.0), // X→Y via symmetry, stddev=3
            meas("y", "10.0.0.99", 10.0, 4.0, 0.0),  // Y→B forward, stddev=4
        ];
        let lk = lookup(&ms);
        let pool = vec![agent("y")];
        let routes = enumerate_routes(&lk, &candidate("10.0.0.200"), &candidate("10.0.0.99"), &pool, 1, None, None, 1.0);
        let r = routes.iter().find(|r| r.kind == RouteKind::OneHop).expect("one-hop must exist");
        assert!((r.stddev_ms - 5.0).abs() < 1e-4, "got {}", r.stddev_ms);
    }

    #[test]
    fn max_hops_zero_with_intermediaries_yields_only_direct() {
        // Fixture: source agent A, destination IP D, candidate intermediary Y.
        // Both A→D and A→Y are resolvable. With max_hops=0, only the direct
        // route should be enumerated; the 1-hop A→Y→D route must NOT appear.
        let ms = vec![
            meas("a", "10.0.0.1", 20.0, 2.0, 0.0), // A → D direct (the route we want)
            meas("a", "10.0.0.2", 15.0, 1.0, 0.0), // A → Y direct (would enable 1-hop, but ignored at max_hops=0)
            meas("y", "10.0.0.1", 18.0, 1.5, 0.0), // Y → D forward (completes 1-hop shape, but suppressed)
        ];
        let lk = lookup(&ms);

        let routes = enumerate_routes(
            &lk,
            &agent("a"),
            &candidate("10.0.0.1"),
            &[agent("y")],
            0, // max_hops = 0
            None,
            None,
            1.0,
        );

        assert_eq!(routes.len(), 1, "max_hops=0 must yield exactly one route");
        assert_eq!(
            routes[0].kind,
            RouteKind::Direct,
            "max_hops=0 must yield only direct route"
        );
    }
}
