//! Regression: VM continuous-mesh baseline synthesis must use the full
//! identity roster, not the source-only agent subset, when the
//! evaluator runs in EdgeCandidate mode.
//!
//! Setup: three mesh agents A, B, C. The campaign's source list is
//! `[B]` (so `inputs.agents == [B]`) and its candidate IP is A's IP
//! (so X resolves to mesh agent A, but A is *not* a campaign source).
//!
//! Active-probe data covers only the campaign-dispatched B→A leg.
//! VictoriaMetrics carries the continuous-mesh A→B baseline that the
//! evaluator needs to render the X→B (= A→B) direct leg with forward
//! provenance. The pre-fix code passed `inputs.agents` (= `[B]`) to
//! `fetch_and_synthesize_vm_baselines`, so the missing-pair set
//! computed there was empty (a single-agent roster has no agent→agent
//! pairs), VM was never queried for A's outgoing baselines, and the
//! direct leg fell back to a reverse-direction substitution off the
//! campaign-probed B→A row. The fix passes `inputs.roster` instead so
//! all three agents drive the missing-pair calculation and A's
//! outgoing baselines are fetched.

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;

use wiremock::matchers::{method, path, query_param_contains};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a VictoriaMetrics `resultType: "vector"` body. Mirrors the
/// helper in `campaign_evaluate.rs`; duplicated here so this test file
/// stays self-contained.
fn vm_vector_body(samples: &[(&str, &str, &str)]) -> Value {
    let result: Vec<Value> = samples
        .iter()
        .map(|(src, tgt, val)| {
            json!({
                "metric": { "source": src, "target": tgt },
                "value": [1_700_000_000_i64, *val],
            })
        })
        .collect();
    json!({
        "status": "success",
        "data": { "resultType": "vector", "result": result },
    })
}

async fn mount_vm_baselines(
    server: &MockServer,
    rtt: &[(&str, &str, &str)],
    stddev: &[(&str, &str, &str)],
    loss: &[(&str, &str, &str)],
) {
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains("query", "meshmon_path_rtt_avg_micros"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(rtt)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains(
            "query",
            "meshmon_path_rtt_stddev_micros",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(stddev)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains("query", "meshmon_path_failure_rate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(loss)))
        .mount(server)
        .await;
}

#[tokio::test]
async fn edge_candidate_vm_synthesis_uses_full_roster_for_non_source_candidate() {
    // Three mesh agents; only B is a campaign source. The candidate IP
    // is A's IP, so X is a mesh agent that is NOT a campaign source —
    // exactly the C3-8 separation between `inputs.agents` (source-only
    // for edge_candidate) and `inputs.roster` (full identity).
    let vm = MockServer::start().await;

    // VM exposes the continuous-mesh A→B baseline at a clearly
    // distinct value (180 ms) so a regression that substituted from
    // the reverse B→A active-probe row (50 ms) would surface as a
    // ratio mismatch on the direct-leg RTT and as `was_substituted=true`
    // on the leg's provenance flag.
    mount_vm_baselines(
        &vm,
        // VM samples cover both A's outgoing legs so the fixture
        // exercises both A→B and A→C — the C3-8 fix split agents/roster
        // exactly so candidates that are mesh agents would have their
        // outgoing legs fetched.
        &[
            ("eval-vmrx-a", "eval-vmrx-b", "180.0"),
            ("eval-vmrx-a", "eval-vmrx-c", "200.0"),
            ("eval-vmrx-b", "eval-vmrx-a", "175.0"),
            ("eval-vmrx-c", "eval-vmrx-a", "195.0"),
        ],
        &[
            ("eval-vmrx-a", "eval-vmrx-b", "5.0"),
            ("eval-vmrx-a", "eval-vmrx-c", "6.0"),
            ("eval-vmrx-b", "eval-vmrx-a", "5.5"),
            ("eval-vmrx-c", "eval-vmrx-a", "6.5"),
        ],
        &[
            ("eval-vmrx-a", "eval-vmrx-b", "0.0"),
            ("eval-vmrx-a", "eval-vmrx-c", "0.0"),
            ("eval-vmrx-b", "eval-vmrx-a", "0.0"),
            ("eval-vmrx-c", "eval-vmrx-a", "0.0"),
        ],
    )
    .await;

    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.81".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.82".parse().unwrap();
    let c_ip: IpAddr = "192.0.2.83".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-vmrx-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-vmrx-b", b_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-vmrx-c", c_ip).await;

    // Source list is intentionally NOT including agent A. The
    // candidate IP is A's IP — A is therefore present in the full
    // `inputs.roster` (full mesh registry) but absent from
    // `inputs.agents` (source-only). The pre-fix code passed
    // `inputs.agents` here, masking A's outgoing baselines.
    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-edge-candidate-non-source-roster",
                "protocol": "icmp",
                "evaluation_mode": "edge_candidate",
                "useful_latency_ms": 250.0,
                "source_agent_ids": ["eval-vmrx-b"],
                "destination_ips": ["192.0.2.81"],
                "max_hops": 0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    // Seed the active-probe row the campaign would have produced —
    // only B→A is active (the campaign's only source is B, and X==A).
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[("eval-vmrx-b", "192.0.2.81", 50.0, 1.0, 0.0)],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let _eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // Inspect the persisted edge_pair detail row for X=A → B. The
    // direct leg's `source` must be `vm_continuous` (the VM-fetched
    // forward baseline) and `was_substituted=false` (forward, not
    // symmetry-fallback). The `best_route_ms` must match the VM
    // sample (180 ms), not the reverse-direction active-probe value
    // (50 ms) — the smoking gun for the bug.
    let pair_page: Value = h
        .get_expect_status(
            &format!(
                "/api/campaigns/{campaign_id}/evaluation/edge_pairs?candidate_ip=192.0.2.81&limit=10"
            ),
            200,
        )
        .await;
    let entries = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("edge_pairs entries missing: {pair_page}"));
    let row = entries
        .iter()
        .find(|r| r["destination_agent_id"] == "eval-vmrx-b")
        .unwrap_or_else(|| panic!("X=A → B row missing: {pair_page}"));

    // The persisted RTT must be the VM forward value, not the
    // reverse-substituted active-probe value. Allow a tiny epsilon
    // for the f32→f64 round-trip through JSON.
    let best = row["best_route_ms"]
        .as_f64()
        .unwrap_or_else(|| panic!("best_route_ms missing: {row}"));
    assert!(
        (best - 180.0).abs() < 1.0,
        "X→B best_route_ms must be the VM forward baseline (180 ms), \
         got {best}: {row}"
    );

    // Direct route — single leg, `vm_continuous` source, not substituted.
    assert_eq!(row["best_route_kind"], "direct", "row = {row}");
    let legs = row["best_route_legs"]
        .as_array()
        .unwrap_or_else(|| panic!("best_route_legs missing: {row}"));
    assert_eq!(legs.len(), 1, "direct route has exactly one leg: {row}");
    let leg = &legs[0];
    assert_eq!(
        leg["source"], "vm_continuous",
        "direct leg must use the VM-fetched A→B forward baseline: {leg}"
    );
    assert_eq!(
        leg["was_substituted"], false,
        "direct leg must be the forward measurement, not a reverse fallback: {leg}"
    );
}
