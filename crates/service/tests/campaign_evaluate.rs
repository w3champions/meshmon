//! Integration tests for the evaluator HTTP surface — `/evaluate` and
//! `/evaluation`.
//!
//! These tests share the process-wide migrated Postgres pool via
//! `common::HttpHarness::start_with_vm()`. Each test picks disjoint
//! `(agent_id, ip)` ranges so parallel test binaries never collide on
//! the `agents.id` primary key, the shared `measurements` table, or
//! the `campaign_pairs` `(campaign_id, source_agent_id, destination_ip,
//! kind)` uniqueness constraint.
//!
//! Every `/evaluate` call also fetches agent-to-agent baselines from
//! VictoriaMetrics and archives them as `measurements` rows
//! (source=`archived_vm_continuous`). Tests that don't want archival
//! rows to interfere with the behaviour under test point the harness at
//! a `MockServer` that returns an empty vector result for every
//! instant query — the archival path runs and records zero rows.
//!
//! | Test                                                  | Agent ids                                | IPs (TEST-NET-1)                 |
//! |-------------------------------------------------------|------------------------------------------|----------------------------------|
//! | `evaluate_then_reevaluate_different_mode_no_redispatch` | `eval-t1-a`, `eval-t1-b`                | `192.0.2.11`, `.12`, `.99`       |
//! | `evaluate_running_campaign_409`                       | `eval-t2-a`                              | `192.0.2.21`                     |
//! | `evaluate_empty_baseline_422`                         | `eval-t3-a`                              | `192.0.2.33`                     |
//! | `get_evaluation_404_before_evaluate`                  | `eval-t4-a`                              | `192.0.2.41`                     |
//! | `reused_pair_surfaces_in_baseline`                    | `eval-t5-a`, `eval-t5-b`                | `192.0.2.51`, `.52`, `.59`       |
//! | `evaluate_archives_vm_baselines`                      | `eval-t6-a`, `eval-t6-b`                | `192.0.2.61`, `.62`              |
//! | `evaluate_vm_unreachable_returns_503`                 | `eval-t7-a`, `eval-t7-b`                | `192.0.2.71`, `.72`              |
//! | `evaluate_without_vm_configured_returns_503`          | `eval-t8-a`                              | `192.0.2.81`                     |
//!
//! The campaign scheduler is not spawned in the test harness, so
//! `AppState.campaign_cancel` is a no-op token; state transitions
//! happen only as a side effect of the explicit lifecycle endpoints
//! these tests call, and the evaluator is driven against a
//! `mark_completed`-forced row rather than a naturally completed one.

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;
use wiremock::matchers::{method as wm_method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Install a VM instant-query mock that returns an empty `vector` for
/// every `GET /api/v1/query` call. Lets tests exercise the evaluator
/// without the VM archival path writing any rows.
async fn mount_empty_vm_mock(server: &MockServer) {
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": { "resultType": "vector", "result": [] },
        })))
        .mount(server)
        .await;
}

/// Install a VM instant-query mock that surfaces one sample per
/// `(source, target)` pair, keyed by the request's `query=` parameter
/// so RTT / stddev / loss responses can diverge. `rtt_micros` populates
/// the `_avg_micros` response, `stddev_micros` the `_stddev_micros`
/// response, and `loss_ratio` the `_failure_rate` response. The
/// archival path divides the RTT metric by 1000 inside PromQL, so the
/// mock returns micros values matching the agent ingestion convention.
async fn mount_vm_baseline_mock(
    server: &MockServer,
    source: &str,
    target: &str,
    rtt_micros: f64,
    stddev_micros: f64,
    loss_ratio: f64,
) {
    let avg = json!({
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                {
                    "metric": {
                        "__name__": "meshmon_path_rtt_avg_micros",
                        "source": source,
                        "target": target,
                        "protocol": "icmp",
                    },
                    "value": [1_700_000_000.0, format!("{:.6}", rtt_micros / 1000.0)],
                }
            ],
        },
    });
    let stddev = json!({
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                {
                    "metric": {
                        "__name__": "meshmon_path_rtt_stddev_micros",
                        "source": source,
                        "target": target,
                        "protocol": "icmp",
                    },
                    "value": [1_700_000_000.0, format!("{:.6}", stddev_micros / 1000.0)],
                }
            ],
        },
    });
    let loss = json!({
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                {
                    "metric": {
                        "__name__": "meshmon_path_failure_rate",
                        "source": source,
                        "target": target,
                        "protocol": "icmp",
                    },
                    "value": [1_700_000_000.0, format!("{loss_ratio:.6}")],
                }
            ],
        },
    });
    // Route responses by query content: the RTT query mentions
    // `rtt_avg_micros`, the stddev query mentions `rtt_stddev_micros`,
    // the loss query mentions `failure_rate`. One mock per-metric lets
    // wiremock match the exact request.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query"))
        .and(wiremock::matchers::query_param_contains(
            "query",
            "rtt_avg_micros",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(avg))
        .mount(server)
        .await;
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query"))
        .and(wiremock::matchers::query_param_contains(
            "query",
            "rtt_stddev_micros",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(stddev))
        .mount(server)
        .await;
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query"))
        .and(wiremock::matchers::query_param_contains(
            "query",
            "failure_rate",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(loss))
        .mount(server)
        .await;
}

#[tokio::test]
async fn evaluate_then_reevaluate_different_mode_no_redispatch() {
    let vm = MockServer::start().await;
    mount_empty_vm_mock(&vm).await;
    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    // Seed two agents so the evaluator's baseline scan finds
    // agent→agent pairs. `eval-t1-a` at .11, `eval-t1-b` at .12 — both
    // IPs appear in the campaign's destination list below so
    // `agents_with_catalogue` resolves them via the destination filter
    // as well as the source filter.
    let a_ip: IpAddr = "192.0.2.11".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.12".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t1-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t1-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t1",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t1-a", "eval-t1-b"],
                "destination_ips": ["192.0.2.12", "192.0.2.11", "192.0.2.99"],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Seed one A→B baseline + two transit legs through X=192.0.2.99.
    // With this set the evaluator's baseline pair count is exactly 1
    // (only a→.12 exists; b→.11 is absent, so `(b, .11, a)` is not a
    // baseline).
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-t1-a", "192.0.2.12", 318.0, 24.0, 0.0),
            ("eval-t1-a", "192.0.2.99", 120.0, 8.0, 0.0),
            ("eval-t1-b", "192.0.2.99", 121.0, 8.0, 0.0),
        ],
    )
    .await;

    // Force completed so /evaluate's state gate (`completed |
    // evaluated`) opens without running the scheduler.
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval1: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert_eq!(eval1["evaluation_mode"], "optimization", "body = {eval1}");
    assert_eq!(eval1["baseline_pair_count"], 1, "body = {eval1}");

    // Snapshot the measurement count for this test's agents so we can
    // verify that re-evaluating does NOT dispatch new probes. The VM
    // mock returns zero samples, so the archival path writes nothing
    // and the count stays fixed.
    let m_count_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM measurements \
           WHERE source_agent_id IN ('eval-t1-a', 'eval-t1-b')",
    )
    .fetch_one(&h.state.pool)
    .await
    .expect("count measurements (before)");

    // Switch evaluation mode via PATCH and re-evaluate.
    let _patched: Value = h
        .patch_json(
            &format!("/api/campaigns/{campaign_id}"),
            &json!({ "evaluation_mode": "diversity" }),
        )
        .await;

    let eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert_eq!(eval2["evaluation_mode"], "diversity", "body = {eval2}");

    let m_count_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM measurements \
           WHERE source_agent_id IN ('eval-t1-a', 'eval-t1-b')",
    )
    .fetch_one(&h.state.pool)
    .await
    .expect("count measurements (after)");
    assert_eq!(
        m_count_after, m_count_before,
        "re-evaluate must not dispatch new measurements (VM mock returns empty, archival writes none)",
    );

    // Read-through on /evaluation must expose the most recent mode.
    let got: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/evaluation"))
        .await;
    assert_eq!(got["evaluation_mode"], "diversity", "body = {got}");

    // `measurement_campaigns.evaluated_at` must restamp on re-evaluate,
    // not just `campaign_evaluations.evaluated_at`. UI consumers reading
    // campaign metadata would otherwise see a stale timestamp.
    let first_eval_at = eval1["evaluated_at"].as_str().expect("evaluated_at first");
    let second_eval_at = eval2["evaluated_at"].as_str().expect("evaluated_at second");
    assert_ne!(
        first_eval_at, second_eval_at,
        "re-evaluate must restamp campaign_evaluations.evaluated_at"
    );
    let mc_eval_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT evaluated_at FROM measurement_campaigns WHERE id = $1::uuid")
            .bind(&campaign_id)
            .fetch_one(&h.state.pool)
            .await
            .expect("read measurement_campaigns.evaluated_at");
    let mc_eval_at = mc_eval_at.expect("evaluated_at populated after second /evaluate");
    let second_eval_ts: chrono::DateTime<chrono::Utc> =
        second_eval_at.parse().expect("parse second eval timestamp");
    // Equal to the second `/evaluate`'s timestamp (same `now()` tx-scope).
    assert_eq!(
        mc_eval_at, second_eval_ts,
        "measurement_campaigns.evaluated_at must track the latest /evaluate"
    );
}

#[tokio::test]
async fn evaluate_running_campaign_409() {
    // The 409 path short-circuits before the VM fetch, so the harness
    // doesn't need a mock. Still goes through start_with_vm so the
    // config looks realistic.
    let vm = MockServer::start().await;
    mount_empty_vm_mock(&vm).await;
    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;
    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t2",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t2-a"],
                "destination_ips": ["192.0.2.21"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // draft → running; the state gate on /evaluate admits only
    // `completed | evaluated`.
    let _: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/start"))
        .await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            409,
        )
        .await;
    assert_eq!(res["error"], "illegal_state_transition", "body = {res}");
}

#[tokio::test]
async fn evaluate_empty_baseline_422() {
    let vm = MockServer::start().await;
    mount_empty_vm_mock(&vm).await;
    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;
    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t3",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t3-a"],
                "destination_ips": ["192.0.2.33"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Seed a campaign measurement so there's at least one row to read,
    // but the destination is not an agent IP — so no agent→agent
    // baseline can form and the evaluator returns `NoBaseline`. With
    // the VM mock returning an empty result, the archival path
    // contributes nothing either.
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[("eval-t3-a", "192.0.2.33", 120.0, 8.0, 0.0)],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            422,
        )
        .await;
    assert_eq!(res["error"], "no_baseline_pairs", "body = {res}");
}

#[tokio::test]
async fn reused_pair_surfaces_in_baseline() {
    // A pair resolved via reuse (`resolution_state='reused'`,
    // `measurement_id` pointing at a prior measurement) must contribute
    // to the evaluator's baseline just like a freshly-settled pair.
    // Otherwise the scheduler's reuse optimisation would silently shrink
    // the baseline every time a campaign covered already-probed legs.
    let vm = MockServer::start().await;
    mount_empty_vm_mock(&vm).await;
    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.51".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.52".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t5-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t5-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-reuse",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t5-a", "eval-t5-b"],
                "destination_ips": ["192.0.2.52", "192.0.2.51", "192.0.2.59"],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Seed the reused (A→B) measurement with `kind='detail_ping'` to
    // exercise the realistic cross-kind reuse case: a prior `/detail`
    // run left a high-resolution measurement for this tuple, and
    // `resolve_reuse` (which does NOT filter `m.kind`) binds it to the
    // new campaign's baseline pair. The evaluator must count it — the
    // `cp.kind='campaign'` filter on `measurements_for_campaign` is the
    // load-bearing invariant; the measurement's own kind is irrelevant.
    let m_ab: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, \
              latency_avg_ms, latency_stddev_ms, loss_ratio, kind) \
         VALUES ('eval-t5-a', '192.0.2.52'::inet, 'icmp', 10, 300.0, 20.0, 0.0, 'detail_ping') \
         RETURNING id",
    )
    .fetch_one(&h.state.pool)
    .await
    .expect("insert reused measurement (A→B)");

    // Upsert the (A→B) baseline pair as `reused` with the pre-existing
    // measurement id. `seed_measurements` would use `succeeded`; the
    // difference is the whole point of this test.
    sqlx::query(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, \
              resolution_state, measurement_id, kind) \
         VALUES ($1::uuid, 'eval-t5-a', '192.0.2.52'::inet, 'reused', $2, 'campaign') \
         ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind) \
           DO UPDATE SET measurement_id = EXCLUDED.measurement_id, \
                         resolution_state = 'reused'",
    )
    .bind(&campaign_id)
    .bind(m_ab)
    .execute(&h.state.pool)
    .await
    .expect("upsert reused pair (A→B)");

    // Transit legs settle normally via `seed_measurements`.
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-t5-a", "192.0.2.59", 120.0, 8.0, 0.0),
            ("eval-t5-b", "192.0.2.59", 120.0, 8.0, 0.0),
        ],
    )
    .await;

    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    let baseline = eval["baseline_pair_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("eval missing baseline_pair_count: {eval}"));
    assert!(
        baseline >= 1,
        "reused baseline pair must be counted: body={eval}"
    );

    let results = &eval["results"];
    let candidates = results["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("results missing candidates array: {eval}"));
    let candidate = candidates
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.59")
        .unwrap_or_else(|| panic!("candidate for 192.0.2.59 missing: {eval}"));

    let pair_details = candidate["pair_details"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details missing on candidate: {candidate}"));
    let has_reused_leg = pair_details.iter().any(|pd| {
        pd["source_agent_id"] == "eval-t5-a" && pd["destination_agent_id"] == "eval-t5-b"
    });
    assert!(
        has_reused_leg,
        "reused measurement's source/destination must appear in pair_details: {candidate}"
    );
}

#[tokio::test]
async fn get_evaluation_404_before_evaluate() {
    // Read-only path — never hits VM; plain harness is fine.
    let h = common::HttpHarness::start().await;
    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t4",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t4-a"],
                "destination_ips": ["192.0.2.41"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    let res = h
        .get_expect_status(&format!("/api/campaigns/{campaign_id}/evaluation"), 404)
        .await;
    assert_eq!(res["error"], "not_evaluated", "body = {res}");
}

#[tokio::test]
async fn evaluate_archives_vm_baselines() {
    // End-to-end: VM returns a real A→B baseline, the evaluator
    // archives it as a `measurements` row with
    // `source='archived_vm_continuous'`, and upserts the matching
    // `campaign_pairs` row so the evaluator's existing
    // join-by-measurement-id surfaces the baseline.
    let vm = MockServer::start().await;
    mount_vm_baseline_mock(&vm, "eval-t6-a", "eval-t6-b", 150_000.0, 3_000.0, 0.01).await;
    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.61".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.62".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t6-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t6-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-archive",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t6-a", "eval-t6-b"],
                "destination_ips": ["192.0.2.62", "192.0.2.61"],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let _eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // The VM mock returned one (a→b) sample. The archival path should
    // have persisted one measurements row tagged
    // `archived_vm_continuous` for this pair.
    let archived: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM measurements \
           WHERE source = 'archived_vm_continuous' \
             AND source_agent_id = 'eval-t6-a' \
             AND destination_ip = '192.0.2.62'::inet",
    )
    .fetch_one(&h.state.pool)
    .await
    .expect("count archived rows");
    assert_eq!(
        archived, 1,
        "one archival row expected per (source, target) VM sample"
    );

    // And the `campaign_pairs` upsert should have bound it.
    let cp_measurement_id: Option<i64> = sqlx::query_scalar(
        "SELECT measurement_id FROM campaign_pairs \
           WHERE campaign_id = $1::uuid \
             AND source_agent_id = 'eval-t6-a' \
             AND destination_ip = '192.0.2.62'::inet \
             AND kind = 'campaign'",
    )
    .bind(&campaign_id)
    .fetch_one(&h.state.pool)
    .await
    .expect("read campaign_pairs.measurement_id");
    assert!(
        cp_measurement_id.is_some(),
        "campaign_pairs.measurement_id must point at the archival row"
    );

    // Idempotency: calling /evaluate again must not double-count
    // `campaign_pairs` rows for the pair (unique key: campaign, source,
    // destination, kind). A fresh archival measurement lands, and the
    // upsert replaces the pointer.
    let _eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cp_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
           WHERE campaign_id = $1::uuid \
             AND source_agent_id = 'eval-t6-a' \
             AND destination_ip = '192.0.2.62'::inet \
             AND kind = 'campaign'",
    )
    .bind(&campaign_id)
    .fetch_one(&h.state.pool)
    .await
    .expect("count pair rows after re-evaluate");
    assert_eq!(
        cp_count, 1,
        "re-evaluate must upsert, not insert, the archival pair"
    );
}

#[tokio::test]
async fn evaluate_vm_unreachable_returns_503() {
    // Point the handler at a URL that will refuse connection (port 1
    // on loopback is a well-known "always-rejected" destination). VM
    // is configured, but the upstream fetch itself fails — the
    // handler surfaces 503 `vm_upstream`.
    let h = common::HttpHarness::start_with_vm("http://127.0.0.1:1").await;

    let a_ip: IpAddr = "192.0.2.71".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.72".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t7-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t7-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-vm-down",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t7-a", "eval-t7-b"],
                "destination_ips": ["192.0.2.72", "192.0.2.71"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            503,
        )
        .await;
    assert_eq!(res["error"], "vm_upstream", "body = {res}");
}

#[tokio::test]
async fn evaluate_without_vm_configured_returns_503() {
    // Harness without `[upstream.vm_url]` set — /evaluate returns 503
    // `vm_not_configured` with an operator-actionable message instead
    // of falling back to the old behaviour.
    let h = common::HttpHarness::start().await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-no-vm",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t8-a"],
                "destination_ips": ["192.0.2.81"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            503,
        )
        .await;
    assert_eq!(res["error"], "vm_not_configured", "body = {res}");
}
