//! Integration tests for the T55 evaluator guardrails — eligibility caps
//! (`max_transit_rtt_ms`, `max_transit_stddev_ms`) and storage filters
//! (`min_improvement_ms`, `min_improvement_ratio`).
//!
//! Each test seeds a campaign + agents + measurements, optionally PATCHes
//! a knob value, then drives `/evaluate` and asserts on the response
//! shape (counters and pair_details). Tests use disjoint agent ids and
//! TEST-NET-2 (`198.51.100.0/24`) IP ranges so parallel test binaries
//! never collide on the shared `agents`/`measurements`/`campaign_pairs`
//! tables.
//!
//! | Test                                | Agents              | IPs (TEST-NET-2)                          |
//! |-------------------------------------|---------------------|-------------------------------------------|
//! | `default_knobs_baseline`            | t55-1-{a,b,c}       | `.11`, `.12`, `.13`, `.99`                |
//! | `max_rtt_drops_candidate_via_l2`    | t55-2-{a,b}         | `.21`, `.22`, `.79`                       |
//! | `max_rtt_filters_triples_in_loop`   | t55-3-{a,b,c}       | `.31`, `.32`, `.33`, `.99`                |
//! | `max_stddev_filters_triples`        | t55-4-{a,b,c}       | `.41`, `.42`, `.43`, `.99`                |
//! | `min_improvement_ms_filters_rows`   | t55-5-{a,b,c}       | `.51`, `.52`, `.53`, `.99`                |
//! | `or_semantics_storage_filter`       | t55-6-{a,b,c}       | `.61`, `.62`, `.63`, `.91`, `.92`, `.93`  |
//! | `direct_rtt_zero_ratio_auto_passes` | t55-7-{a,b,c}       | `.71`, `.72`, `.73`, `.94`, `.95`, `.96`  |
//! | `negative_min_improvement_ms`       | t55-8-{a,b,c}       | `.81`, `.82`, `.83`, `.99`                |
//! | `negative_min_improvement_ratio`    | t55-9-{a,b,c}       | `.91`, `.92`, `.93`, `.99`                |
//! | `recovery_on_re_evaluate`           | t55-10-{a,b,c}      | `.101`, `.102`, `.103`, `.199`            |

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;

/// Find the candidate row with the given destination IP, panic with a
/// helpful body if it's missing.
fn find_candidate<'a>(eval: &'a Value, ip: &str) -> Option<&'a Value> {
    eval["results"]["candidates"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["destination_ip"] == ip))
}

/// Fetch the full `pair_details` page for a candidate (limit=500
/// covers every test fixture in this file). Returns the raw entries
/// array as a [`Value`] to keep the call sites' assertion patterns
/// stable across the wire-shape cutover (T55).
async fn fetch_pair_details(
    h: &common::HttpHarness,
    campaign_id: &str,
    candidate_ip: &str,
) -> Value {
    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/evaluation/candidates/{candidate_ip}/pair_details?limit=500"
        ))
        .await;
    body["entries"].clone()
}

#[tokio::test]
async fn default_knobs_baseline() {
    // All four guardrails NULL ⇒ output behaves like the pre-T55
    // baseline. Pins the "no regression when knobs unset" contract.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.11".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.12".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.13".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-1-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-1-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-1-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-default-knobs",
                "protocol": "icmp",
                "source_agent_ids": ["t55-1-a", "t55-1-b", "t55-1-c"],
                "destination_ips": [
                    "198.51.100.12", "198.51.100.13", "198.51.100.11", "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // Baselines (a↔b, a↔c, b↔c).
            ("t55-1-a", "198.51.100.12", 300.0, 5.0, 0.0),
            ("t55-1-b", "198.51.100.11", 300.0, 5.0, 0.0),
            ("t55-1-a", "198.51.100.13", 300.0, 5.0, 0.0),
            ("t55-1-c", "198.51.100.11", 300.0, 5.0, 0.0),
            ("t55-1-b", "198.51.100.13", 300.0, 5.0, 0.0),
            ("t55-1-c", "198.51.100.12", 300.0, 5.0, 0.0),
            // Transit legs through X = 198.51.100.99.
            ("t55-1-a", "198.51.100.99", 100.0, 5.0, 0.0),
            ("t55-1-b", "198.51.100.99", 50.0, 5.0, 0.0),
            ("t55-1-c", "198.51.100.99", 60.0, 5.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert!(
        eval["max_transit_rtt_ms"].is_null(),
        "knob unset must surface as null: {eval}"
    );
    assert!(
        eval["min_improvement_ms"].is_null(),
        "knob unset must surface as null: {eval}"
    );

    let cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate present: {eval}"));
    let total = cand["pairs_total_considered"]
        .as_i64()
        .expect("pairs_total_considered i64");
    // Six baselines (a↔b, a↔c, b↔c — both directions), all triples
    // measurable through X. Pre-T55 behaviour: all six counted.
    assert_eq!(total, 6, "default-knobs path counts every triple: {cand}");
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    assert_eq!(
        pair_details.len(),
        6,
        "default-knobs path stores every pair_detail: {entries}"
    );
}

#[tokio::test]
async fn max_rtt_drops_candidate_via_l2() {
    // Cap = 200 ms; the only candidate's `min(rtt_ax) + min(rtt_xb)`
    // exceeds the cap. L2 must drop the candidate entirely so it never
    // appears in `results.candidates`.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.21".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.22".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-2-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-2-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-l2-drop",
                "protocol": "icmp",
                "source_agent_ids": ["t55-2-a", "t55-2-b"],
                "destination_ips": [
                    "198.51.100.22", "198.51.100.21", "198.51.100.79",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                // Cap so tight no triple can survive (a→X = b→X = 150;
                // composed = 300 > 200).
                "max_transit_rtt_ms": 200.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // a↔b baseline.
            ("t55-2-a", "198.51.100.22", 500.0, 5.0, 0.0),
            ("t55-2-b", "198.51.100.21", 500.0, 5.0, 0.0),
            // X = .79 — both legs cost 150 ms ⇒ composed 300 > 200.
            ("t55-2-a", "198.51.100.79", 150.0, 5.0, 0.0),
            ("t55-2-b", "198.51.100.79", 150.0, 5.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert!(
        find_candidate(&eval, "198.51.100.79").is_none(),
        "L2 must drop the candidate when min_ax+min_xb exceeds cap: {eval}"
    );
}

#[tokio::test]
async fn max_rtt_filters_triples_in_loop() {
    // Cap = 200 ms; some triples pass and some fail. Surviving
    // candidate's `pairs_total_considered` excludes the cap-failing
    // triples — verifies the L1+L3 pre-filter and the in-loop guard.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.31".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.32".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.33".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-3-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-3-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-3-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-l1-l3-prefilter",
                "protocol": "icmp",
                "source_agent_ids": ["t55-3-a", "t55-3-b", "t55-3-c"],
                "destination_ips": [
                    "198.51.100.32", "198.51.100.33", "198.51.100.31",
                    "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "max_transit_rtt_ms": 200.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // 6 baselines.
            ("t55-3-a", "198.51.100.32", 500.0, 5.0, 0.0),
            ("t55-3-b", "198.51.100.31", 500.0, 5.0, 0.0),
            ("t55-3-a", "198.51.100.33", 500.0, 5.0, 0.0),
            ("t55-3-c", "198.51.100.31", 500.0, 5.0, 0.0),
            ("t55-3-b", "198.51.100.33", 500.0, 5.0, 0.0),
            ("t55-3-c", "198.51.100.32", 500.0, 5.0, 0.0),
            // Transit legs to X = .99 — c is too slow at 250 (L1 drop).
            ("t55-3-a", "198.51.100.99", 100.0, 5.0, 0.0),
            ("t55-3-b", "198.51.100.99", 50.0, 5.0, 0.0),
            ("t55-3-c", "198.51.100.99", 250.0, 5.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate must survive: {eval}"));
    let total = cand["pairs_total_considered"]
        .as_i64()
        .expect("pairs_total_considered");
    // L1 drops c at 250 > 200. Surviving legs = {a(100), b(50)}.
    // Triples (a,b,X) and (b,a,X) compose to 150 ≤ 200 — counted.
    // Triples involving c (in either ax or xb position) are dropped.
    assert_eq!(
        total, 2,
        "only the (a,b) and (b,a) triples through X must count: {cand}"
    );
    // Every stored row must respect the cap.
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    for pd in pair_details {
        let composed = pd["transit_rtt_ms"].as_f64().expect("transit_rtt_ms");
        assert!(
            composed <= 200.0,
            "every stored row's composed RTT ≤ cap: {pd}"
        );
    }
}

#[tokio::test]
async fn max_stddev_filters_triples() {
    // Stddev composes by sqrt(a² + b²). Cap = 15: legs with stddev > 15
    // drop at L1; the composed pair must be ≤ 15.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.41".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.42".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.43".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-4-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-4-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-4-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-stddev-cap",
                "protocol": "icmp",
                "source_agent_ids": ["t55-4-a", "t55-4-b", "t55-4-c"],
                "destination_ips": [
                    "198.51.100.42", "198.51.100.43", "198.51.100.41",
                    "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "max_transit_stddev_ms": 15.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // 6 baselines.
            ("t55-4-a", "198.51.100.42", 500.0, 1.0, 0.0),
            ("t55-4-b", "198.51.100.41", 500.0, 1.0, 0.0),
            ("t55-4-a", "198.51.100.43", 500.0, 1.0, 0.0),
            ("t55-4-c", "198.51.100.41", 500.0, 1.0, 0.0),
            ("t55-4-b", "198.51.100.43", 500.0, 1.0, 0.0),
            ("t55-4-c", "198.51.100.42", 500.0, 1.0, 0.0),
            // Transit legs — c has stddev 20 > 15, dropped at L1.
            ("t55-4-a", "198.51.100.99", 100.0, 5.0, 0.0),
            ("t55-4-b", "198.51.100.99", 100.0, 8.0, 0.0),
            ("t55-4-c", "198.51.100.99", 100.0, 20.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate must survive: {eval}"));
    let total = cand["pairs_total_considered"]
        .as_i64()
        .expect("pairs_total_considered");
    // L1 drops c. Surviving (a,b) compositions: stddev =
    // sqrt(25 + 64) ≈ 9.43 ≤ 15. Two triples count.
    assert_eq!(total, 2, "stddev cap must drop c-bearing triples: {cand}");
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    for pd in pair_details {
        let composed = pd["transit_stddev_ms"].as_f64().expect("transit_stddev_ms");
        assert!(
            composed <= 15.0,
            "every stored row's composed stddev ≤ cap: {pd}"
        );
    }
}

#[tokio::test]
async fn min_improvement_ms_filters_rows() {
    // `min_improvement_ms = 5` ⇒ pair_detail rows with improvement < 5
    // are NOT persisted, but they DO count toward the candidate's
    // `pairs_total_considered`. Triples that qualify still bump
    // `pairs_improved` regardless of the storage gate.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.51".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.52".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.53".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-5-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-5-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-5-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-min-improvement-ms",
                "protocol": "icmp",
                "source_agent_ids": ["t55-5-a", "t55-5-b", "t55-5-c"],
                "destination_ips": [
                    "198.51.100.52", "198.51.100.53", "198.51.100.51",
                    "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "min_improvement_ms": 5.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    // Two distinct improvement levels — symmetric transit legs so every
    // triple's composed RTT is the same (200 ms):
    //   * a↔b direct = 250 ⇒ improvement = 50, above the 5 ms gate.
    //   * a↔c / b↔c directs = 204 ⇒ improvement = 4, below the gate.
    // Stddev set to 0 to keep the penalty term out of the math.
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("t55-5-a", "198.51.100.52", 250.0, 0.0, 0.0),
            ("t55-5-b", "198.51.100.51", 250.0, 0.0, 0.0),
            ("t55-5-a", "198.51.100.53", 204.0, 0.0, 0.0),
            ("t55-5-c", "198.51.100.51", 204.0, 0.0, 0.0),
            ("t55-5-b", "198.51.100.53", 204.0, 0.0, 0.0),
            ("t55-5-c", "198.51.100.52", 204.0, 0.0, 0.0),
            // Transit legs — every pair composes to exactly 200 ms.
            ("t55-5-a", "198.51.100.99", 100.0, 0.0, 0.0),
            ("t55-5-b", "198.51.100.99", 100.0, 0.0, 0.0),
            ("t55-5-c", "198.51.100.99", 100.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate must survive: {eval}"));
    let total = cand["pairs_total_considered"].as_i64().expect("total");
    let improved = cand["pairs_improved"].as_i64().expect("improved");
    // All 6 triples are eligible (no RTT/stddev cap), so all 6 count.
    assert_eq!(total, 6, "every eligible triple still counts: {cand}");
    // All 6 yield positive improvement (50 ms or 4 ms), so all 6 are
    // marked qualifies — counter reflects the full set.
    assert_eq!(
        improved, 6,
        "every model-improved triple counts toward pairs_improved: {cand}"
    );

    // Storage filter: only the a↔b pair (improvement = 50) passes the
    // 5 ms gate. The four a↔c / b↔c rows (improvement = 4) are dropped.
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    assert_eq!(
        pair_details.len(),
        2,
        "only large-improvement rows persist: {entries}"
    );
    for pd in pair_details {
        let imp = pd["improvement_ms"].as_f64().expect("improvement_ms");
        assert!(imp >= 5.0, "every persisted row clears the gate: {pd}");
    }
}

#[tokio::test]
async fn or_semantics_storage_filter() {
    // `min_improvement_ms = 100` AND `min_improvement_ratio = 0.5`.
    // Three triples cover the truth table:
    //   * X1 / (a→b): imp=50, direct=80   → ratio=0.625 ✓ ms ✗ ⇒ store (OR)
    //   * X2 / (b→c): imp=110, direct=1000 → ratio=0.11  ✗ ms ✓ ⇒ store
    //   * X3 / (c→a): imp=50, direct=1000 → ratio=0.05  ✗ ms ✗ ⇒ drop
    //
    // Each row is isolated under its own candidate so the
    // (a→X, b→X, c→X) leg system stays solvable for all three direct
    // baselines simultaneously — see the inline math.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.61".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.62".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.63".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-6-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-6-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-6-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-or-semantics",
                "protocol": "icmp",
                "source_agent_ids": ["t55-6-a", "t55-6-b", "t55-6-c"],
                "destination_ips": [
                    "198.51.100.62", "198.51.100.63", "198.51.100.61",
                    "198.51.100.91", "198.51.100.92", "198.51.100.93",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "min_improvement_ms": 100.0,
                "min_improvement_ratio": 0.5,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // Direct baselines — only one direction per pair so each
            // candidate's relevant baseline is unambiguous.
            ("t55-6-a", "198.51.100.62", 80.0, 0.0, 0.0), // a→b
            ("t55-6-b", "198.51.100.63", 1000.0, 0.0, 0.0), // b→c
            ("t55-6-c", "198.51.100.61", 1000.0, 0.0, 0.0), // c→a
            // X1 = .91: only a→X1 and b→X1 ⇒ only the (a,b) triple is
            // measurable. Composed = 30, imp = 50.
            ("t55-6-a", "198.51.100.91", 15.0, 0.0, 0.0),
            ("t55-6-b", "198.51.100.91", 15.0, 0.0, 0.0),
            // X2 = .92: only b→X2 and c→X2 ⇒ only the (b,c) triple is
            // measurable. Composed = 890, imp = 110.
            ("t55-6-b", "198.51.100.92", 15.0, 0.0, 0.0),
            ("t55-6-c", "198.51.100.92", 875.0, 0.0, 0.0),
            // X3 = .93: only c→X3 and a→X3 ⇒ only the (c,a) triple is
            // measurable. Composed = 950, imp = 50.
            ("t55-6-c", "198.51.100.93", 935.0, 0.0, 0.0),
            ("t55-6-a", "198.51.100.93", 15.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // X1: ratio passes ⇒ row stored.
    let x1 = find_candidate(&eval, "198.51.100.91")
        .unwrap_or_else(|| panic!("X1 candidate present: {eval}"));
    assert_eq!(x1["pairs_total_considered"], 1, "X1 has one triple: {x1}");
    let x1_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.91").await;
    assert_eq!(
        x1_entries.as_array().unwrap().len(),
        1,
        "X1: ratio-passing row stored under OR semantics: {x1_entries}"
    );

    // X2: ms passes ⇒ row stored.
    let x2 = find_candidate(&eval, "198.51.100.92")
        .unwrap_or_else(|| panic!("X2 candidate present: {eval}"));
    assert_eq!(x2["pairs_total_considered"], 1, "X2 has one triple: {x2}");
    let x2_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.92").await;
    assert_eq!(
        x2_entries.as_array().unwrap().len(),
        1,
        "X2: ms-passing row stored under OR semantics: {x2_entries}"
    );

    // X3: both gates fail ⇒ row dropped (but candidate row still present
    // since pairs_total_considered = 1).
    let x3 = find_candidate(&eval, "198.51.100.93")
        .unwrap_or_else(|| panic!("X3 candidate present even with empty pair_details: {eval}"));
    assert_eq!(x3["pairs_total_considered"], 1, "X3 has one triple: {x3}");
    let x3_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.93").await;
    assert_eq!(
        x3_entries.as_array().unwrap().len(),
        0,
        "X3: both-fail row dropped, leaving empty pair_details for this candidate: {x3_entries}"
    );
    // pairs_improved still reflects the model-improved triple — the
    // storage gate only suppresses the persisted row, not the counter.
    assert_eq!(
        x3["pairs_improved"], 1,
        "pairs_improved counts every model-improved triple, including \
         storage-dropped ones: {x3}"
    );
}

#[tokio::test]
async fn direct_rtt_zero_ratio_auto_passes() {
    // When `direct_rtt_ms ≤ 0`, the ratio gate auto-passes. With a
    // ratio-only knob set, that row is stored regardless of how
    // unfavourable the composed transit is. Each row is isolated under
    // its own candidate (same trick as `or_semantics_storage_filter`)
    // so the leg system stays solvable across distinct direct values.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.71".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.72".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.73".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-7-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-7-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-7-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-ratio-zero-baseline",
                "protocol": "icmp",
                "source_agent_ids": ["t55-7-a", "t55-7-b", "t55-7-c"],
                "destination_ips": [
                    "198.51.100.72", "198.51.100.73", "198.51.100.71",
                    "198.51.100.94", "198.51.100.95", "198.51.100.96",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "min_improvement_ratio": 0.5,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // Direct baselines — one per pair direction.
            ("t55-7-a", "198.51.100.72", 0.0, 0.0, 0.0), // a→b direct = 0
            ("t55-7-b", "198.51.100.73", 100.0, 0.0, 0.0), // b→c
            ("t55-7-c", "198.51.100.71", 100.0, 0.0, 0.0), // c→a
            // X1 = .94: triple (a, b, X1). composed = 20 — irrelevant
            // because direct=0 makes the ratio gate auto-pass.
            ("t55-7-a", "198.51.100.94", 10.0, 0.0, 0.0),
            ("t55-7-b", "198.51.100.94", 10.0, 0.0, 0.0),
            // X2 = .95: triple (b, c, X2). direct=100, composed=30
            // ⇒ ratio = 0.7 passes.
            ("t55-7-b", "198.51.100.95", 15.0, 0.0, 0.0),
            ("t55-7-c", "198.51.100.95", 15.0, 0.0, 0.0),
            // X3 = .96: triple (c, a, X3). direct=100, composed=90
            // ⇒ ratio = 0.1 fails.
            ("t55-7-c", "198.51.100.96", 45.0, 0.0, 0.0),
            ("t55-7-a", "198.51.100.96", 45.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    let x1 = find_candidate(&eval, "198.51.100.94")
        .unwrap_or_else(|| panic!("X1 candidate present: {eval}"));
    let x1_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.94").await;
    assert_eq!(
        x1_entries.as_array().unwrap().len(),
        1,
        "X1: direct_rtt_ms = 0 ⇒ ratio auto-pass ⇒ row stored: {x1_entries}"
    );
    // X1's improvement is `0 − 20 = −20`, so it does NOT qualify under
    // diversity mode (`improvement_ms > 0` is false). The storage gate
    // is independent of qualification — a stored row with
    // qualifies=false must NOT bump pairs_improved.
    assert_eq!(
        x1["pairs_improved"], 0,
        "X1: stored-but-non-qualifying row must not count as improved: {x1}"
    );

    let _x2 = find_candidate(&eval, "198.51.100.95")
        .unwrap_or_else(|| panic!("X2 candidate present: {eval}"));
    let x2_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.95").await;
    assert_eq!(
        x2_entries.as_array().unwrap().len(),
        1,
        "X2: ratio = 0.7 ⇒ stored: {x2_entries}"
    );

    let x3 = find_candidate(&eval, "198.51.100.96")
        .unwrap_or_else(|| panic!("X3 candidate present (counter > 0): {eval}"));
    let x3_entries = fetch_pair_details(&h, &campaign_id, "198.51.100.96").await;
    assert_eq!(
        x3_entries.as_array().unwrap().len(),
        0,
        "X3: ratio = 0.1 ⇒ dropped: {x3_entries}"
    );
    assert_eq!(
        x3["pairs_total_considered"], 1,
        "X3 still has one considered triple: {x3}"
    );
}

#[tokio::test]
async fn negative_min_improvement_ms() {
    // `min_improvement_ms = -10`. A row whose `improvement_ms = -5`
    // (slower transit but stable) is stored; one at -15 is dropped.
    // Confirms signed thresholds round-trip end-to-end with no
    // accidental clamp at 0.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.81".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.82".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.83".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-8-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-8-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-8-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-negative-ms",
                "protocol": "icmp",
                "source_agent_ids": ["t55-8-a", "t55-8-b", "t55-8-c"],
                "destination_ips": [
                    "198.51.100.82", "198.51.100.83", "198.51.100.81",
                    "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "min_improvement_ms": -10.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // a→b: direct=25, composed=30 ⇒ improvement = -5 ≥ -10 stored.
            ("t55-8-a", "198.51.100.82", 25.0, 0.0, 0.0),
            // b→c: direct=15, composed=30 ⇒ improvement = -15 < -10 dropped.
            ("t55-8-b", "198.51.100.83", 15.0, 0.0, 0.0),
            // c→a: direct=100, composed=30 ⇒ improvement = +70 stored.
            ("t55-8-c", "198.51.100.81", 100.0, 0.0, 0.0),
            ("t55-8-a", "198.51.100.99", 15.0, 0.0, 0.0),
            ("t55-8-b", "198.51.100.99", 15.0, 0.0, 0.0),
            ("t55-8-c", "198.51.100.99", 15.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let _cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate must survive: {eval}"));
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    let stored_pairs: Vec<(String, String)> = pair_details
        .iter()
        .map(|pd| {
            (
                pd["source_agent_id"].as_str().unwrap().to_owned(),
                pd["destination_agent_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        stored_pairs.contains(&("t55-8-a".into(), "t55-8-b".into())),
        "imp=-5 ≥ -10 ⇒ stored (no clamp at 0): {pair_details:?}"
    );
    assert!(
        !stored_pairs.contains(&("t55-8-b".into(), "t55-8-c".into())),
        "imp=-15 < -10 ⇒ dropped: {pair_details:?}"
    );
    assert!(
        stored_pairs.contains(&("t55-8-c".into(), "t55-8-a".into())),
        "imp=+70 ⇒ stored: {pair_details:?}"
    );
}

#[tokio::test]
async fn negative_min_improvement_ratio() {
    // `min_improvement_ratio = -0.05`. Row with ratio = -0.02 stored;
    // ratio = -0.10 dropped. Mirrors the negative-ms test for the
    // ratio knob.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.91".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.92".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.93".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-9-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-9-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-9-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-negative-ratio",
                "protocol": "icmp",
                "source_agent_ids": ["t55-9-a", "t55-9-b", "t55-9-c"],
                "destination_ips": [
                    "198.51.100.92", "198.51.100.93", "198.51.100.91",
                    "198.51.100.99",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                "min_improvement_ratio": -0.05,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // a→b: direct=100, composed=102 ⇒ imp=-2 ⇒ ratio=-0.02 ≥ -0.05 stored.
            ("t55-9-a", "198.51.100.92", 100.0, 0.0, 0.0),
            // b→c: direct=100, composed=110 ⇒ imp=-10 ⇒ ratio=-0.10 < -0.05 dropped.
            ("t55-9-b", "198.51.100.93", 100.0, 0.0, 0.0),
            // c→a: direct=100, composed=80 ⇒ imp=+20 ⇒ ratio=+0.2 stored.
            ("t55-9-c", "198.51.100.91", 100.0, 0.0, 0.0),
            // Three transit legs to make composed RTTs land where we want.
            // Pair sums per (A→B): A→X + B→X must give the composed value.
            // For row1 (a→b) composed=102 and row2 (b→c) composed=110,
            // we need a→X, b→X, c→X such that:
            //   a→X + b→X = 102
            //   b→X + c→X = 110
            //   c→X + a→X = 80
            // Sum: 2(a+b+c) = 292 ⇒ a+b+c = 146; so c = 146-102 = 44,
            // a = 146-110 = 36, b = 146-80 = 66. Check: a+b=102 ✓,
            // b+c=110 ✓, c+a=80 ✓.
            ("t55-9-a", "198.51.100.99", 36.0, 0.0, 0.0),
            ("t55-9-b", "198.51.100.99", 66.0, 0.0, 0.0),
            ("t55-9-c", "198.51.100.99", 44.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let _cand = find_candidate(&eval, "198.51.100.99")
        .unwrap_or_else(|| panic!("X candidate must survive: {eval}"));
    let entries = fetch_pair_details(&h, &campaign_id, "198.51.100.99").await;
    let pair_details = entries.as_array().expect("pair_details entries");
    let stored_pairs: Vec<(String, String)> = pair_details
        .iter()
        .map(|pd| {
            (
                pd["source_agent_id"].as_str().unwrap().to_owned(),
                pd["destination_agent_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        stored_pairs.contains(&("t55-9-a".into(), "t55-9-b".into())),
        "ratio=-0.02 ≥ -0.05 ⇒ stored: {pair_details:?}"
    );
    assert!(
        !stored_pairs.contains(&("t55-9-b".into(), "t55-9-c".into())),
        "ratio=-0.10 < -0.05 ⇒ dropped: {pair_details:?}"
    );
    assert!(
        stored_pairs.contains(&("t55-9-c".into(), "t55-9-a".into())),
        "ratio=+0.2 ⇒ stored: {pair_details:?}"
    );
}

#[tokio::test]
async fn recovery_on_re_evaluate() {
    // Tight knobs ⇒ candidate dropped. PATCH looser knobs, re-evaluate
    // ⇒ candidate reappears. The inputs (measurements) are durable; the
    // evaluator just recomputes from inputs each time.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "198.51.100.101".parse().unwrap();
    let b_ip: IpAddr = "198.51.100.102".parse().unwrap();
    let c_ip: IpAddr = "198.51.100.103".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "t55-10-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-10-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "t55-10-c", c_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "t55-recovery",
                "protocol": "icmp",
                "source_agent_ids": ["t55-10-a", "t55-10-b", "t55-10-c"],
                "destination_ips": [
                    "198.51.100.102", "198.51.100.103", "198.51.100.101",
                    "198.51.100.199",
                ],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                // Start tight: composed RTT through X is 30 ms but cap = 10 ms.
                "max_transit_rtt_ms": 10.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("t55-10-a", "198.51.100.102", 500.0, 0.0, 0.0),
            ("t55-10-b", "198.51.100.101", 500.0, 0.0, 0.0),
            ("t55-10-a", "198.51.100.103", 500.0, 0.0, 0.0),
            ("t55-10-c", "198.51.100.101", 500.0, 0.0, 0.0),
            ("t55-10-b", "198.51.100.103", 500.0, 0.0, 0.0),
            ("t55-10-c", "198.51.100.102", 500.0, 0.0, 0.0),
            ("t55-10-a", "198.51.100.199", 15.0, 0.0, 0.0),
            ("t55-10-b", "198.51.100.199", 15.0, 0.0, 0.0),
            ("t55-10-c", "198.51.100.199", 15.0, 0.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    // First evaluate — tight cap drops the candidate.
    let eval1: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert!(
        find_candidate(&eval1, "198.51.100.199").is_none(),
        "tight cap drops the only candidate: {eval1}"
    );

    // Loosen via PATCH and re-evaluate.
    let _patched: Value = h
        .patch_json(
            &format!("/api/campaigns/{campaign_id}"),
            &json!({ "max_transit_rtt_ms": 500.0 }),
        )
        .await;
    let eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cand = find_candidate(&eval2, "198.51.100.199")
        .unwrap_or_else(|| panic!("X reappears with loose knob: {eval2}"));
    let total = cand["pairs_total_considered"].as_i64().expect("total");
    // 6 baselines all eligible under cap = 500.
    assert_eq!(
        total, 6,
        "loose knob restores the full triple count: {cand}"
    );
}
