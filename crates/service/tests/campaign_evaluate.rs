//! Integration tests for the evaluator HTTP surface — `/evaluate` and
//! `/evaluation`.
//!
//! These tests share the process-wide migrated Postgres pool via
//! `common::HttpHarness::start()`. Each test picks disjoint
//! `(agent_id, ip)` ranges so parallel test binaries never collide on
//! the `agents.id` primary key, the shared `measurements` table, or
//! the `campaign_pairs` `(campaign_id, source_agent_id, destination_ip,
//! kind)` uniqueness constraint.
//!
//! | Test                                                  | Agent ids                                | IPs (TEST-NET-1)                 |
//! |-------------------------------------------------------|------------------------------------------|----------------------------------|
//! | `evaluate_then_reevaluate_different_mode_no_redispatch` | `eval-t1-a`, `eval-t1-b`                | `192.0.2.11`, `.12`, `.99`       |
//! | `evaluate_running_campaign_409`                       | `eval-t2-a`                              | `192.0.2.21`                     |
//! | `evaluate_empty_baseline_422`                         | `eval-t3-a`                              | `192.0.2.33`                     |
//! | `get_evaluation_404_before_evaluate`                  | `eval-t4-a`                              | `192.0.2.41`                     |
//! | `reused_pair_surfaces_in_baseline`                    | `eval-t5-a`, `eval-t5-b`                | `192.0.2.51`, `.52`, `.59`       |
//! | `vm_fills_missing_baselines`                          | `eval-t7-a`, `eval-t7-b`                | `192.0.2.131`, `.132`, `.139`    |
//! | `vm_unreachable_returns_503_vm_upstream`              | `eval-t8-a`, `eval-t8-b`                | `192.0.2.141`, `.142`, `.149`    |
//! | `vm_not_configured_falls_back_to_active`              | `eval-t9-a`, `eval-t9-b`                | `192.0.2.151`, `.152`, `.159`    |
//! | `vm_not_configured_still_422_without_active`          | `eval-t10-a`                             | `192.0.2.161`                    |
//! | `active_probe_wins_over_vm`                           | `eval-t11-a`, `eval-t11-b`              | `192.0.2.171`, `.172`, `.179`    |
//! | `malformed_vm_response_returns_503`                   | `eval-t12-a`, `eval-t12-b`              | `192.0.2.181`, `.182`, `.189`    |
//!
//! The campaign scheduler is not spawned in the test harness, so
//! `AppState.campaign_cancel` is a no-op token; state transitions
//! happen only as a side effect of the explicit lifecycle endpoints
//! these tests call, and the evaluator is driven against a
//! `mark_completed`-forced row rather than a naturally completed one.

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;

#[tokio::test]
async fn evaluate_then_reevaluate_different_mode_no_redispatch() {
    let h = common::HttpHarness::start().await;

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
    // verify that re-evaluating does NOT dispatch new probes.
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
        "re-evaluate must not dispatch new measurements",
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
    let h = common::HttpHarness::start().await;
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
    let h = common::HttpHarness::start().await;
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
    // baseline can form and the evaluator returns `NoBaseline`.
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
    let h = common::HttpHarness::start().await;

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

    // Pair-detail rows live behind the paginated endpoint since T55;
    // they are no longer nested on the candidate's wire shape.
    assert!(
        candidate.get("pair_details").is_none(),
        "T55: pair_details must NOT appear on the candidate's wire shape: {candidate}"
    );
    let cand_ip = candidate["destination_ip"]
        .as_str()
        .expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"),
            200,
        )
        .await;
    let pair_details = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details endpoint missing entries: {pair_page}"));
    let has_reused_leg = pair_details.iter().any(|pd| {
        pd["source_agent_id"] == "eval-t5-a" && pd["destination_agent_id"] == "eval-t5-b"
    });
    assert!(
        has_reused_leg,
        "reused measurement's source/destination must appear in pair_details: {pair_page}"
    );
    // This test seeds only active-probe data and never contacts VM, so
    // every pair_detail should stamp `direct_source='active_probe'`.
    for pd in pair_details {
        assert_eq!(
            pd["direct_source"], "active_probe",
            "every pair_detail should carry direct_source=active_probe: {pd}"
        );
    }
}

#[tokio::test]
async fn get_evaluation_404_before_evaluate() {
    // A campaign that has never been evaluated must surface 404
    // `not_evaluated`, NOT a zero-filled placeholder DTO.
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
async fn second_evaluate_appends_new_row_without_mutating_first() {
    // T54-02 dropped the per-campaign UNIQUE constraint so every
    // `/evaluate` call appends a fresh `campaign_evaluations` row.
    // Guards: two rows exist after two calls, the first row's id +
    // evaluated_at + evaluation_mode + candidate count are all
    // untouched, and the read-path surfaces the second (newer) row.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "192.0.2.71".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.72".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t6-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t6-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t6-history",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t6-a", "eval-t6-b"],
                "destination_ips": ["192.0.2.72", "192.0.2.71", "192.0.2.79"],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();
    let campaign_uuid: uuid::Uuid = campaign_id.parse().unwrap();

    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-t6-a", "192.0.2.72", 300.0, 20.0, 0.0),
            ("eval-t6-a", "192.0.2.79", 120.0, 8.0, 0.0),
            ("eval-t6-b", "192.0.2.79", 120.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    // First /evaluate — `optimization` mode.
    let eval1: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let first_evaluated_at = eval1["evaluated_at"]
        .as_str()
        .expect("evaluated_at on first evaluate")
        .to_owned();
    let first_mode = eval1["evaluation_mode"].as_str().expect("mode").to_owned();
    assert_eq!(first_mode, "optimization");

    // Snapshot the first row's (id, evaluated_at) directly out of the
    // DB so we can assert it remains untouched after the second call.
    let (first_id, first_ts): (uuid::Uuid, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT id, evaluated_at FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(campaign_uuid)
            .fetch_one(&h.state.pool)
            .await
            .expect("read first evaluation row");

    // Force a measurable clock delta before the second call.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Switch evaluation mode and re-evaluate — a distinct row should
    // land in `campaign_evaluations` rather than overwriting the first.
    let _patch: Value = h
        .patch_json(
            &format!("/api/campaigns/{campaign_id}"),
            &json!({ "evaluation_mode": "diversity" }),
        )
        .await;
    let eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert_eq!(eval2["evaluation_mode"], "diversity", "body = {eval2}");

    // Two rows exist, the first is untouched, the second is newer.
    let row_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(campaign_uuid)
            .fetch_one(&h.state.pool)
            .await
            .expect("count evaluations");
    assert_eq!(
        row_count, 2,
        "every /evaluate must append a row (not UPSERT)"
    );

    let first_still: (uuid::Uuid, chrono::DateTime<chrono::Utc>, String) = sqlx::query_as(
        "SELECT id, evaluated_at, evaluation_mode::text \
           FROM campaign_evaluations WHERE id = $1",
    )
    .bind(first_id)
    .fetch_one(&h.state.pool)
    .await
    .expect("re-read first evaluation row");
    assert_eq!(first_still.0, first_id, "first row id unchanged");
    assert_eq!(first_still.1, first_ts, "first row evaluated_at unchanged");
    assert_eq!(
        first_still.2, "optimization",
        "first row evaluation_mode unchanged (no UPSERT leak)"
    );

    // Read-through on GET /evaluation must return the latest row.
    let second_evaluated_at = eval2["evaluated_at"]
        .as_str()
        .expect("evaluated_at on second evaluate");
    assert_ne!(
        first_evaluated_at, second_evaluated_at,
        "second evaluate must stamp a distinct evaluated_at"
    );
    let fetched: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/evaluation"))
        .await;
    assert_eq!(fetched["evaluation_mode"], "diversity");
    assert_eq!(
        fetched["evaluated_at"], second_evaluated_at,
        "GET /evaluation must surface the latest row: {fetched}"
    );
}

// ---------------------------------------------------------------------------
// T54-03: VM continuous-mesh baselines at /evaluate time
// ---------------------------------------------------------------------------

/// Assemble a VictoriaMetrics `resultType: "vector"` response body whose
/// `result` contains one sample per `(source, target, value)` tuple.
/// Shape matches the one `vm_query::run_instant_query` parses.
fn vm_vector_body(samples: &[(&str, &str, &str)]) -> serde_json::Value {
    let result: Vec<serde_json::Value> = samples
        .iter()
        .map(|(src, tgt, val)| {
            json!({
                "metric": {
                    "source": src,
                    "target": tgt,
                },
                "value": [1_700_000_000i64, *val],
            })
        })
        .collect();
    json!({
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": result,
        },
    })
}

/// Mount three mocks on `server` — one per VM query (rtt / stddev /
/// failure_rate) — each returning its own vector response. Keeps the
/// happy-path tests terse without inventing a per-test DSL.
async fn mount_vm_baselines(
    server: &wiremock::MockServer,
    rtt_samples: &[(&str, &str, &str)],
    stddev_samples: &[(&str, &str, &str)],
    loss_samples: &[(&str, &str, &str)],
) {
    use wiremock::matchers::{method, path, query_param_contains};
    use wiremock::{Mock, ResponseTemplate};

    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains("query", "meshmon_path_rtt_avg_micros"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(rtt_samples)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains(
            "query",
            "meshmon_path_rtt_stddev_micros",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(stddev_samples)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param_contains("query", "meshmon_path_failure_rate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_vector_body(loss_samples)))
        .mount(server)
        .await;
}

#[tokio::test]
async fn vm_fills_missing_baselines() {
    use wiremock::MockServer;

    // Campaign has no `measurements` rows for the A→B agent pair, but
    // VM exposes continuous-mesh baselines. The evaluator must pick up
    // the VM baseline and stamp `direct_source='vm_continuous'` on
    // every pair_detail using it (both in the response DTO and in the
    // persisted `campaign_evaluation_pair_details` row).
    let vm = MockServer::start().await;
    mount_vm_baselines(
        &vm,
        &[
            // A→B (and B→A, for completeness; the evaluator only reads
            // A→B given the outer baseline scan, but VM labels both
            // directions).
            ("eval-t7-a", "eval-t7-b", "318.0"),
            ("eval-t7-b", "eval-t7-a", "315.0"),
        ],
        &[
            ("eval-t7-a", "eval-t7-b", "24.0"),
            ("eval-t7-b", "eval-t7-a", "22.0"),
        ],
        &[
            ("eval-t7-a", "eval-t7-b", "0.0"),
            ("eval-t7-b", "eval-t7-a", "0.0"),
        ],
    )
    .await;

    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.131".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.132".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t7-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t7-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t7-vm-fill",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t7-a", "eval-t7-b"],
                "destination_ips": ["192.0.2.132", "192.0.2.131", "192.0.2.139"],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Only transit legs have active-probe data. The A→B direct
    // baseline is missing from `measurements` — VM must fill it.
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-t7-a", "192.0.2.139", 120.0, 8.0, 0.0),
            ("eval-t7-b", "192.0.2.139", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    assert!(
        eval["baseline_pair_count"].as_i64().unwrap_or(0) >= 1,
        "VM-sourced A→B baseline must register: {eval}"
    );

    let candidates = eval["results"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates missing: {eval}"));
    let cand = candidates
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.139")
        .unwrap_or_else(|| panic!("transit candidate missing: {eval}"));
    let cand_ip = cand["destination_ip"].as_str().expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"),
            200,
        )
        .await;
    let pair_details = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details endpoint missing entries: {pair_page}"));
    let vm_leg = pair_details
        .iter()
        .find(|pd| {
            pd["source_agent_id"] == "eval-t7-a" && pd["destination_agent_id"] == "eval-t7-b"
        })
        .unwrap_or_else(|| panic!("A→B VM-backed pair_detail missing: {pair_page}"));
    assert_eq!(
        vm_leg["direct_source"], "vm_continuous",
        "VM-filled A→B baseline must carry direct_source=vm_continuous"
    );

    // Persisted row carries the enum too.
    let campaign_uuid: uuid::Uuid = campaign_id.parse().unwrap();
    let persisted: Vec<String> = sqlx::query_scalar(
        "SELECT direct_source::text \
           FROM campaign_evaluation_pair_details pd \
           JOIN campaign_evaluations e ON e.id = pd.evaluation_id \
          WHERE e.campaign_id = $1 \
            AND pd.source_agent_id = 'eval-t7-a' \
            AND pd.destination_agent_id = 'eval-t7-b'",
    )
    .bind(campaign_uuid)
    .fetch_all(&h.state.pool)
    .await
    .expect("read persisted direct_source");
    assert!(
        !persisted.is_empty(),
        "at least one persisted pair_detail for (A,B)"
    );
    for row in &persisted {
        assert_eq!(row, "vm_continuous", "persisted enum must be vm_continuous");
    }
}

#[tokio::test]
async fn vm_unreachable_returns_503_vm_upstream() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // VM is configured but returns 5xx on every query. The evaluator
    // must abort with 503 `vm_upstream` rather than silently falling
    // back to the (potentially incomplete) active-probe set.
    let vm = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&vm)
        .await;

    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.141".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.142".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t8-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t8-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t8-vm-down",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t8-a", "eval-t8-b"],
                "destination_ips": ["192.0.2.142", "192.0.2.141", "192.0.2.149"],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let res = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            503,
        )
        .await;
    assert_eq!(res["error"], "vm_upstream", "body = {res}");

    // And no `campaign_evaluations` row landed — the 503 must rollback
    // before `persist_evaluation` fires.
    let campaign_uuid: uuid::Uuid = campaign_id.parse().unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(campaign_uuid)
            .fetch_one(&h.state.pool)
            .await
            .expect("count evaluations");
    assert_eq!(count, 0, "no evaluation row may persist on VM failure");
}

#[tokio::test]
async fn vm_not_configured_falls_back_to_active() {
    // When `upstream.vm_url` is unset, the evaluator silently degrades
    // to active-probe data. If active-probe data covers the agent→agent
    // pair, /evaluate succeeds with `direct_source='active_probe'` on
    // every pair_detail — no `vm_not_configured` surface.
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "192.0.2.151".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.152".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t9-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t9-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t9-no-vm",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t9-a", "eval-t9-b"],
                "destination_ips": ["192.0.2.152", "192.0.2.151", "192.0.2.159"],
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
            ("eval-t9-a", "192.0.2.152", 318.0, 24.0, 0.0),
            ("eval-t9-a", "192.0.2.159", 120.0, 8.0, 0.0),
            ("eval-t9-b", "192.0.2.159", 121.0, 8.0, 0.0),
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
    let cand = candidates
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.159")
        .unwrap_or_else(|| panic!("transit candidate missing: {eval}"));
    let cand_ip = cand["destination_ip"].as_str().expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"),
            200,
        )
        .await;
    for pd in pair_page["entries"].as_array().expect("pair_details") {
        assert_eq!(
            pd["direct_source"], "active_probe",
            "VM-unset path must stamp every pair_detail with active_probe: {pd}"
        );
    }
}

#[tokio::test]
async fn vm_not_configured_still_422_without_active() {
    // With VM unset AND no agent→agent active-probe row, /evaluate
    // surfaces 422 `no_baseline_pairs` — the same contract as before
    // T54-03, confirming the VM fallback doesn't mask the error.
    let h = common::HttpHarness::start().await;

    // Single agent → no agent→agent pair is even possible. /evaluate
    // cannot baseline and must 422.
    let a_ip: IpAddr = "192.0.2.161".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t10-a", a_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t10-no-vm-no-active",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t10-a"],
                "destination_ips": ["192.0.2.161"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[("eval-t10-a", "192.0.2.161", 120.0, 8.0, 0.0)],
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
async fn active_probe_wins_over_vm() {
    use wiremock::MockServer;

    // Both active-probe data AND a VM sample exist for the same A→B
    // pair. The evaluator must prefer active-probe (since it's per-
    // campaign, freshly measured) and stamp `direct_source=active_probe`.
    // Exists to pin the "last-write-wins" insertion order invariant in
    // `eval::evaluate`'s `by_pair` loop against regression.
    let vm = MockServer::start().await;
    mount_vm_baselines(
        &vm,
        &[
            // VM value clearly distinct (987 ms) from the active-probe
            // value (318 ms) so a tiebreaker mistake would surface.
            ("eval-t11-a", "eval-t11-b", "987.0"),
        ],
        &[("eval-t11-a", "eval-t11-b", "99.0")],
        &[("eval-t11-a", "eval-t11-b", "0.5")],
    )
    .await;

    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.171".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.172".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t11-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t11-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t11-active-wins",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t11-a", "eval-t11-b"],
                "destination_ips": ["192.0.2.172", "192.0.2.171", "192.0.2.179"],
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
            // Active-probe A→B row — should win over the VM sample.
            ("eval-t11-a", "192.0.2.172", 318.0, 24.0, 0.0),
            ("eval-t11-a", "192.0.2.179", 120.0, 8.0, 0.0),
            ("eval-t11-b", "192.0.2.179", 121.0, 8.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(&h.state.pool, &campaign_id).await;

    let eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    let cand = eval["results"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["destination_ip"] == "192.0.2.179")
        .expect("transit candidate present")
        .clone();
    let cand_ip = cand["destination_ip"].as_str().expect("destination_ip");
    let pair_page: Value = h
        .get_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluation/candidates/{cand_ip}/pair_details?limit=500"),
            200,
        )
        .await;
    let leg = pair_page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pd| {
            pd["source_agent_id"] == "eval-t11-a" && pd["destination_agent_id"] == "eval-t11-b"
        })
        .expect("A→B pair_detail present")
        .clone();
    assert_eq!(
        leg["direct_source"], "active_probe",
        "active-probe row must win when both sources have A→B data: {leg}"
    );
    let rtt = leg["direct_rtt_ms"].as_f64().expect("direct_rtt_ms");
    assert!(
        (rtt - 318.0).abs() < 1e-3,
        "direct_rtt_ms must match the active-probe value (318.0), got {rtt}"
    );
}

#[tokio::test]
async fn malformed_vm_response_returns_503() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // VM returns 2xx but the body isn't a valid instant-query envelope.
    // The evaluator must abort with 503 `vm_upstream` (routed through
    // `VmQueryError::MalformedResponse` / `VmQueryError::Request`), not
    // 500.
    let vm = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&vm)
        .await;

    let h = common::HttpHarness::start_with_vm(&vm.uri()).await;

    let a_ip: IpAddr = "192.0.2.181".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.182".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t12-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t12-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t12-vm-malformed",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t12-a", "eval-t12-b"],
                "destination_ips": ["192.0.2.182", "192.0.2.181", "192.0.2.189"],
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();
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
