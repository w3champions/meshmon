//! Integration tests for the `/detail` HTTP surface — three scopes
//! (`all`, `good_candidates`, `pair`) plus one invariant test that
//! guards the evaluator's baseline against poisoning from detail-kind
//! rows.
//!
//! These tests share the process-wide migrated Postgres pool via
//! `common::HttpHarness::start()`. Each test picks disjoint
//! `(agent_id, ip)` ranges so parallel test binaries never collide on
//! the `agents.id` primary key, the shared `measurements` table, or
//! the `campaign_pairs` `(campaign_id, source_agent_id, destination_ip,
//! kind)` uniqueness constraint.
//!
//! | Test                                                             | Agent ids                  | IPs (TEST-NET)                   |
//! |------------------------------------------------------------------|----------------------------|----------------------------------|
//! | `detail_scope_all_enqueues_settled_pairs_with_both_kinds`        | `det-t1-a`, `det-t1-b`     | `192.0.2.21/.22`, `203.0.113.21` |
//! | `detail_scope_good_candidates_filters_to_qualifying_triples`     | `det-t2-a`, `det-t2-b`     | `192.0.2.31/.32`, `203.0.113.31` |
//! | `detail_scope_good_candidates_requires_prior_evaluation`         | `det-t3-a`                 | `192.0.2.41/.42`                 |
//! | `detail_scope_pair_inserts_two_kind_rows`                        | `det-t4-a`                 | `192.0.2.51/.52`                 |
//! | `detail_scope_pair_missing_identifier_returns_400`               | `det-t5-a`                 | `192.0.2.61/.62`                 |
//! | `detail_pairs_excluded_from_next_evaluate_baseline`              | `det-t6-a`, `det-t6-b`     | `192.0.2.71/.72`, `203.0.113.71` |
//! | `detail_pair_scope_rejects_malformed_destination_ip`             | `det-t7-a`                 | `192.0.2.91/.92`                 |

mod common;

use serde_json::{json, Value};

#[tokio::test]
async fn detail_scope_all_enqueues_settled_pairs_with_both_kinds() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t1-a", "192.0.2.21".parse().unwrap()).await;
    common::insert_agent_with_ip(&h.state.pool, "det-t1-b", "192.0.2.22".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail all",
                "protocol": "icmp",
                "source_agent_ids": ["det-t1-a", "det-t1-b"],
                "destination_ips": ["192.0.2.22", "192.0.2.21", "203.0.113.21"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Seed three settled measurements (the campaign was a 2-source × 3-dest fan-out
    // but we settle only the realistic transit triplet for clarity).
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("det-t1-a", "192.0.2.22", 318.0, 24.0, 0.0),
            ("det-t1-a", "203.0.113.21", 120.0, 8.0, 0.0),
            ("det-t1-b", "203.0.113.21", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res: Value = h
        .post_json(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "all" }),
        )
        .await;
    // 3 settled pairs × 2 detail kinds (detail_ping + detail_mtr) = 6 inserted rows.
    assert_eq!(res["pairs_enqueued"], 6, "body = {res}");
    assert_eq!(res["campaign_state"], "running", "body = {res}");

    let detail_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
           WHERE campaign_id = $1::uuid \
             AND kind IN ('detail_ping','detail_mtr')",
    )
    .bind(&campaign_id)
    .fetch_one(&h.state.pool)
    .await
    .expect("count detail rows");
    assert_eq!(detail_count, 6);
}

#[tokio::test]
async fn detail_scope_good_candidates_filters_to_qualifying_triples() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t2-a", "192.0.2.31".parse().unwrap()).await;
    common::insert_agent_with_ip(&h.state.pool, "det-t2-b", "192.0.2.32".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail good",
                "protocol": "icmp",
                "source_agent_ids": ["det-t2-a", "det-t2-b"],
                "destination_ips": ["192.0.2.32", "192.0.2.31", "203.0.113.31"],
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("det-t2-a", "192.0.2.32", 318.0, 24.0, 0.0),
            ("det-t2-a", "203.0.113.31", 120.0, 8.0, 0.0),
            ("det-t2-b", "203.0.113.31", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    // Run evaluate first so good_candidates has data to slice.
    let _eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    let res: Value = h
        .post_json(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "good_candidates" }),
        )
        .await;
    // 1 good candidate (203.0.113.31), 1 qualifying (A,B) triple → 2 dispatch
    // pairs (A→X, B→X) × 2 detail kinds = 4 rows.
    assert_eq!(res["pairs_enqueued"], 4, "body = {res}");
    assert_eq!(res["campaign_state"], "running", "body = {res}");
}

#[tokio::test]
async fn detail_scope_good_candidates_requires_prior_evaluation() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t3-a", "192.0.2.41".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "no eval",
                "protocol": "icmp",
                "source_agent_ids": ["det-t3-a"],
                "destination_ips": ["192.0.2.42"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "good_candidates" }),
            400,
        )
        .await;
    assert_eq!(res["error"], "no_evaluation", "body = {res}");
}

#[tokio::test]
async fn detail_scope_pair_inserts_two_kind_rows() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t4-a", "192.0.2.51".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail one",
                "protocol": "icmp",
                "source_agent_ids": ["det-t4-a"],
                "destination_ips": ["192.0.2.52"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res: Value = h
        .post_json(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({
                "scope": "pair",
                "pair": { "source_agent_id": "det-t4-a", "destination_ip": "192.0.2.52" }
            }),
        )
        .await;
    // 1 pair × 2 detail kinds = 2 inserted rows.
    assert_eq!(res["pairs_enqueued"], 2, "body = {res}");
    assert_eq!(res["campaign_state"], "running", "body = {res}");
}

#[tokio::test]
async fn detail_pair_scope_rejects_malformed_destination_ip() {
    // `scope=pair` must reject a non-parseable `destination_ip` up front
    // with a 400 and the shared `invalid_destination_ip` envelope — the
    // same stable error code the `/edit` and `/force_pair` handlers use
    // so the SPA branches once on the code rather than parsing prose.
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t7-a", "192.0.2.91".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail bad ip",
                "protocol": "icmp",
                "source_agent_ids": ["det-t7-a"],
                "destination_ips": ["192.0.2.92"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({
                "scope": "pair",
                "pair": {
                    "source_agent_id": "det-t7-a",
                    "destination_ip": "not-an-ip"
                }
            }),
            400,
        )
        .await;
    assert_eq!(res["error"], "invalid_destination_ip", "body = {res}");
}

#[tokio::test]
async fn detail_scope_pair_missing_identifier_returns_400() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t5-a", "192.0.2.61".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "no pair",
                "protocol": "icmp",
                "source_agent_ids": ["det-t5-a"],
                "destination_ips": ["192.0.2.62"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "pair" }),
            400,
        )
        .await;
    assert_eq!(res["error"], "missing_pair", "body = {res}");
}

#[tokio::test]
async fn detail_scope_good_candidates_rejects_stale_evaluation() {
    // Repro the stale-evaluation hazard:
    //   1. Evaluate a campaign (→ `evaluated`, row persisted).
    //   2. Force state back to `completed` (simulates a re-run /
    //      edit flow that preserves the historical evaluation row).
    //   3. /detail?scope=good_candidates must now refuse — the
    //      evaluation row is from an earlier run and expanding it
    //      into detail pairs would target a stale candidate set.
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-stale-a", "192.0.2.151".parse().unwrap())
        .await;
    common::insert_agent_with_ip(&h.state.pool, "det-stale-b", "192.0.2.152".parse().unwrap())
        .await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail stale",
                "protocol": "icmp",
                "source_agent_ids": ["det-stale-a", "det-stale-b"],
                "destination_ips": ["192.0.2.152", "192.0.2.151", "203.0.113.151"],
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("det-stale-a", "192.0.2.152", 318.0, 24.0, 0.0),
            ("det-stale-a", "203.0.113.151", 120.0, 8.0, 0.0),
            ("det-stale-b", "203.0.113.151", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;
    let _: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // Force the campaign back to `completed` — the evaluation row
    // stays (apply_edit / force_pair preserve it), but the state no
    // longer matches the evaluation.
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "good_candidates" }),
            400,
        )
        .await;
    assert_eq!(res["error"], "no_evaluation", "body = {res}");
}

#[tokio::test]
async fn detail_rejects_pair_payload_for_non_pair_scope() {
    // `DetailRequest::pair` is documented as "required iff scope == pair,
    // rejected on other scopes" — silently ignoring a stray payload for
    // scope=all / scope=good_candidates would mask client bugs that set
    // `pair` unconditionally.
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-stray-a", "192.0.2.101".parse().unwrap())
        .await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "stray pair payload",
                "protocol": "icmp",
                "source_agent_ids": ["det-stray-a"],
                "destination_ips": ["192.0.2.102"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    for scope in ["all", "good_candidates"] {
        let res = h
            .post_expect_status(
                &format!("/api/campaigns/{campaign_id}/detail"),
                &json!({
                    "scope": scope,
                    "pair": {
                        "source_agent_id": "det-stray-a",
                        "destination_ip": "192.0.2.102",
                    },
                }),
                400,
            )
            .await;
        assert_eq!(
            res["error"], "unexpected_pair_payload",
            "scope={scope} body = {res}"
        );
    }
}

/// T56 H3 — `/detail?scope=good_candidates` for EdgeCandidate campaigns.
///
/// Three candidates with `coverage_count` of 5 / 0 / 2 respectively.
/// Only the two with `coverage_count >= 1` qualify. One source agent
/// (`det-edge-a`) participated in the campaign.
///
/// Expected detail pairs: 2 candidates × 1 source agent = 2 unique
/// `(source_agent, candidate_ip)` pairs, each expanded into
/// `detail_ping` + `detail_mtr` = 4 rows total.
#[tokio::test]
async fn detail_scope_good_candidates_edge_candidate_uses_coverage_count() {
    let h = common::HttpHarness::start().await;

    // Agent at a TEST-NET-3 address disjoint from every other detail test.
    common::insert_agent_with_ip(
        &h.state.pool,
        "det-edge-a",
        "203.0.113.201".parse().unwrap(),
    )
    .await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "edge-good-candidates",
                "protocol": "icmp",
                "evaluation_mode": "edge_candidate",
                "useful_latency_ms": 80.0,
                "source_agent_ids": ["det-edge-a"],
                "destination_ips": [
                    "10.65.1.1",  // candidate A — coverage_count 5
                    "10.65.1.2",  // candidate B — coverage_count 0
                    "10.65.1.3",  // candidate C — coverage_count 2
                ],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string");

    // Seed one settled campaign-kind measurement so `campaign_pairs` has
    // a row with `source_agent_id='det-edge-a'` and `kind='campaign'`.
    // `good_candidates_for_edge_campaign` reads DISTINCT source_agent_id
    // from that table to build the (source, candidate) expansion.
    common::seed_measurements(
        &h.state.pool,
        campaign_id,
        &[("det-edge-a", "10.65.1.1", 60.0, 2.0, 0.0)],
    )
    .await;
    common::mark_completed(&h.state.pool, campaign_id).await;

    // Insert an EdgeCandidate evaluation row directly (bypassing /evaluate
    // because the test fixture measurements are intentionally minimal and
    // the full evaluator path is exercised by other tests).
    let campaign_uuid: uuid::Uuid = campaign_id.parse().unwrap();
    let evaluation_id: uuid::Uuid = sqlx::query_scalar(
        r#"INSERT INTO campaign_evaluations
               (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
                useful_latency_ms, max_hops, vm_lookback_minutes,
                baseline_pair_count, candidates_total, candidates_good, evaluated_at)
           VALUES ($1, 0.05, 1.0, 'edge_candidate'::evaluation_mode,
                   80.0, 1, 30, 1, 3, 2, now())
           RETURNING id"#,
    )
    .bind(campaign_uuid)
    .fetch_one(&h.state.pool)
    .await
    .expect("insert evaluation row");

    // Seed three candidates with distinct coverage_count values.
    // Only the two with coverage_count >= 1 should be picked by /detail.
    for (ip, coverage) in [("10.65.1.1", 5_i32), ("10.65.1.2", 0), ("10.65.1.3", 2)] {
        let ip_net: sqlx::types::ipnetwork::IpNetwork =
            ip.parse::<std::net::IpAddr>().unwrap().into();
        sqlx::query(
            r#"INSERT INTO campaign_evaluation_candidates
                   (evaluation_id, destination_ip, is_mesh_member,
                    pairs_improved, pairs_total_considered, coverage_count)
               VALUES ($1, $2::inet, false, 0, 0, $3)"#,
        )
        .bind(evaluation_id)
        .bind(ip_net)
        .bind(coverage)
        .execute(&h.state.pool)
        .await
        .unwrap_or_else(|e| panic!("seed candidate {ip}: {e}"));
    }

    // Promote the campaign state to `evaluated` so /detail's state gate passes.
    sqlx::query(
        "UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1",
    )
    .bind(campaign_uuid)
    .execute(&h.state.pool)
    .await
    .unwrap();

    let res: Value = h
        .post_json(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "good_candidates" }),
        )
        .await;

    // 2 qualifying candidates (coverage_count 5 and 2) × 1 source agent
    // × 2 detail kinds (detail_ping + detail_mtr) = 4 rows.
    assert_eq!(
        res["pairs_enqueued"], 4,
        "2 candidates × 1 source × 2 kinds = 4; body = {res}"
    );
    assert_eq!(res["campaign_state"], "running", "body = {res}");
}

/// Invariant: `/detail` inserts `detail_ping` + `detail_mtr` rows on the
/// same `campaign_pairs` table that the evaluator reads. If the
/// evaluator's WHERE clause isn't kind-gated, a second `/evaluate` call
/// would see a bloated baseline. This test calls `/evaluate` twice
/// around a `/detail?scope=all` and asserts `baseline_pair_count` is
/// stable.
#[tokio::test]
async fn detail_pairs_excluded_from_next_evaluate_baseline() {
    let h = common::HttpHarness::start().await;

    common::insert_agent_with_ip(&h.state.pool, "det-t6-a", "192.0.2.71".parse().unwrap()).await;
    common::insert_agent_with_ip(&h.state.pool, "det-t6-b", "192.0.2.72".parse().unwrap()).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "detail-ignored-by-eval",
                "protocol": "icmp",
                "source_agent_ids": ["det-t6-a", "det-t6-b"],
                "destination_ips": ["192.0.2.72", "192.0.2.71", "203.0.113.71"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("det-t6-a", "192.0.2.72", 318.0, 24.0, 0.0),
            ("det-t6-a", "203.0.113.71", 120.0, 8.0, 0.0),
            ("det-t6-b", "203.0.113.71", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval1: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let baseline1 = eval1["baseline_pair_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("eval1 missing baseline_pair_count: {eval1}"));
    assert!(baseline1 > 0, "eval1 baseline must be non-zero: {eval1}");

    // Trigger detail-all → inserts detail_ping + detail_mtr rows on the
    // same `campaign_pairs` table the evaluator reads.
    let detail: Value = h
        .post_json(
            &format!("/api/campaigns/{campaign_id}/detail"),
            &json!({ "scope": "all" }),
        )
        .await;
    assert!(
        detail["pairs_enqueued"].as_i64().unwrap() > 0,
        "detail must enqueue rows for this test to exercise the kind filter: {detail}"
    );

    // `insert_detail_pairs` flipped state to running; fast-forward back
    // via the test fixture so `/evaluate`'s state gate admits again.
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let baseline2 = eval2["baseline_pair_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("eval2 missing baseline_pair_count: {eval2}"));

    assert_eq!(
        baseline2, baseline1,
        "detail rows must not change the evaluator's baseline_pair_count \
         (eval1={eval1}, eval2={eval2})"
    );
}
