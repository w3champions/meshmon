//! Regression tests for the diversity/optimization evaluator after the
//! T56 F2 refactor: routes are now enumerated via `eval::routes::enumerate_routes`
//! up to `max_hops` instead of the former scalar leg lookup.
//!
//! These tests pin the pre-existing (max_hops=1) behaviour so that any
//! future refactor that breaks the diversity/optimization contract is
//! caught immediately.
//!
//! | Test                                                           | Agent ids                            | IPs                           |
//! |----------------------------------------------------------------|--------------------------------------|-------------------------------|
//! | `diversity_at_max_hops_1_matches_pre_change`                   | `eval-f2r-a`, `eval-f2r-b`          | `192.0.2.201`, `.202`, `.209` |
//! | `optimization_max_hops_2_rejects_when_pure_mesh_y_beats_x`    | `eval-f2r-c`, `eval-f2r-d`, `eval-f2r-e` | `192.0.2.211`, `.212`, `.213`, `.219` |

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;

/// Diversity mode with max_hops=1 must produce the same result as before
/// the F2 refactor: a 1-hop X transit that beats A→B direct qualifies and
/// appears in the candidate list with `pairs_improved ≥ 1`.
///
/// Data shape:
///   A→B direct = 318 ms, stddev=24
///   A→X        = 120 ms, stddev=8
///   B→X        = 121 ms, stddev=8   (symmetry-approx X→B)
///
/// Transit RTT = (120+121)/2 is NOT how the evaluator computes it;
/// enumerate_routes picks A→X(120) + X→B(sym=121) = 241 ms composite.
/// Improvement = 318 - 241 - (8-24)*1.0 = 77 + 16 = 93 ms → qualifies.
#[tokio::test]
async fn diversity_at_max_hops_1_matches_pre_change() {
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "192.0.2.201".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.202".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-f2r-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-f2r-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "f2r-diversity-max-hops-1",
                "protocol": "icmp",
                "source_agent_ids": ["eval-f2r-a", "eval-f2r-b"],
                "destination_ips": ["192.0.2.202", "192.0.2.201", "192.0.2.209"],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
                // max_hops defaults to 1 when not supplied; the F2 refactor
                // must preserve this behaviour.
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-f2r-a", "192.0.2.202", 318.0, 24.0, 0.0), // A→B baseline
            ("eval-f2r-a", "192.0.2.209", 120.0, 8.0, 0.0),  // A→X
            ("eval-f2r-b", "192.0.2.209", 121.0, 8.0, 0.0),  // B→X (sym-approx X→B)
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // Baseline pair count must be exactly 1 (only A→B exists; B→A absent).
    assert_eq!(
        eval["baseline_pair_count"].as_i64().unwrap_or(0),
        1,
        "baseline_pair_count: {eval}"
    );

    let candidates = eval["results"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates missing: {eval}"));

    let x_cand = candidates
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.209")
        .unwrap_or_else(|| panic!("X candidate missing: {eval}"));

    assert!(
        x_cand["pairs_improved"].as_i64().unwrap_or(0) >= 1,
        "diversity mode: X must qualify when transit beats direct: {x_cand}"
    );
    assert!(
        x_cand["avg_improvement_ms"]
            .as_f64()
            .unwrap_or(0.0)
            > 0.0,
        "diversity mode: avg_improvement_ms must be positive: {x_cand}"
    );

    // Pair detail must carry qualifies=true for the (A,B) pair.
    let cand_ip = x_cand["destination_ip"].as_str().expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!(
                "/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"
            ),
            200,
        )
        .await;
    let pair_details = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details missing: {pair_page}"));
    let ab_detail = pair_details
        .iter()
        .find(|pd| {
            pd["source_agent_id"] == "eval-f2r-a"
                && pd["destination_agent_id"] == "eval-f2r-b"
        })
        .unwrap_or_else(|| panic!("(A,B) pair_detail missing: {pair_page}"));
    assert!(
        ab_detail["qualifies"].as_bool().unwrap_or(false),
        "diversity: (A,B) pair_detail must qualify: {ab_detail}"
    );
}

/// Optimization mode with max_hops=2: X must NOT qualify when the mesh
/// provides a non-X 1-hop alternative (A→Y→B) that is at least as good.
///
/// Data shape (three agents A, D, E plus X):
///   A→D direct = 318 ms    (A=eval-f2r-c, D=eval-f2r-d)
///   A→X(219)   = 120 ms, B→X(219) = 121 ms  → X transit = 241 ms
///   A→Y(213)   = 100 ms, D→Y(213) = 80  ms  → Y transit = 180 ms (better than X)
///
/// Y is a mesh agent (eval-f2r-e at 192.0.2.213). Under optimization,
/// X must be disqualified because Y already beats X.
///
/// With max_hops=2 the evaluator can also enumerate 2-hop routes, but
/// the 1-hop Y route is still better, so X should still fail.
#[tokio::test]
async fn optimization_max_hops_2_rejects_when_pure_mesh_y_beats_x() {
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "192.0.2.211".parse().unwrap();
    let d_ip: IpAddr = "192.0.2.212".parse().unwrap();
    let y_ip: IpAddr = "192.0.2.213".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-f2r-c", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-f2r-d", d_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-f2r-e", y_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "f2r-optim-max-hops-2-y-beats-x",
                "protocol": "icmp",
                "source_agent_ids": ["eval-f2r-c", "eval-f2r-d", "eval-f2r-e"],
                "destination_ips": [
                    "192.0.2.212", "192.0.2.211", "192.0.2.213", "192.0.2.219"
                ],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
                "max_hops": 2,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            // A→D baseline
            ("eval-f2r-c", "192.0.2.212", 318.0, 24.0, 0.0),
            // A→X, D→X (symmetry-approx X→D)
            ("eval-f2r-c", "192.0.2.219", 120.0, 8.0, 0.0),
            ("eval-f2r-d", "192.0.2.219", 121.0, 8.0, 0.0),
            // A→Y, D→Y (symmetry-approx Y→D) — Y is mesh agent at .213
            ("eval-f2r-c", "192.0.2.213", 100.0, 5.0, 0.0),
            ("eval-f2r-d", "192.0.2.213", 80.0, 5.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    let candidates = eval["results"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates missing: {eval}"));

    // X candidate must appear in the results (the triple is fully measured)
    // but must NOT qualify, because Y(mesh) provides a better transit.
    let x_cand = candidates
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.219")
        .unwrap_or_else(|| panic!("X=192.0.2.219 candidate missing: {eval}"));

    assert_eq!(
        x_cand["pairs_improved"].as_i64().unwrap_or(-1),
        0,
        "optimization mode: X must NOT be counted as improved when Y is better: {x_cand}"
    );

    // Drilldown: the (A,D) pair_detail must be present with qualifies=false.
    let cand_ip = x_cand["destination_ip"].as_str().expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!(
                "/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"
            ),
            200,
        )
        .await;
    let pair_details = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details missing: {pair_page}"));
    let ad_detail = pair_details
        .iter()
        .find(|pd| {
            pd["source_agent_id"] == "eval-f2r-c"
                && pd["destination_agent_id"] == "eval-f2r-d"
        })
        .unwrap_or_else(|| panic!("(A,D) pair_detail missing: {pair_page}"));
    assert!(
        !ad_detail["qualifies"].as_bool().unwrap_or(true),
        "optimization: (A,D) pair_detail must NOT qualify when Y mesh route beats X: {ad_detail}"
    );
}
