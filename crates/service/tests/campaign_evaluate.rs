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
//! | `read_legacy_evaluation_with_null_snapshot_columns`   | `eval-t13-a`, `eval-t13-b`              | `192.0.2.191`, `.192`            |
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
    // Symmetry-substituted reverse counts as a baseline too, so A→B
    // forward + B→A reverse-from-A→B = 2 baseline pairs.
    assert_eq!(eval1["baseline_pair_count"], 2, "body = {eval1}");

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
    // evaluated_at + evaluation_mode are all untouched, and the
    // read-path surfaces the second (newer) row.
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

    // Re-evaluate without changing any knobs — a distinct row should
    // land in `campaign_evaluations` rather than overwriting the first.
    // Knob-changing PATCHes (mode, max_hops, useful_latency_ms, etc.)
    // dismiss the existing row by design so they don't appear here.
    let eval2: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;
    assert_eq!(eval2["evaluation_mode"], "optimization", "body = {eval2}");

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
    assert_eq!(fetched["evaluation_mode"], "optimization");
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

/// G4: The read path must tolerate pre-T56 `campaign_evaluations` rows that
/// have NULL in the three new snapshot columns (`useful_latency_ms`,
/// `max_hops`, `vm_lookback_minutes`). These NULLs arise from rows written
/// before the 20260426000000 migration added the columns; the migration
/// intentionally makes all three nullable so pre-existing data remains
/// valid and the evaluator's read path does not panic on NULL.
///
/// Strategy: bypass the normal `/evaluate` handler and INSERT a raw row
/// with NULL snapshot columns, then assert the GET endpoint returns the
/// row with those fields absent (skip_serializing_if = None) rather than
/// erroring.
#[tokio::test]
async fn read_legacy_evaluation_with_null_snapshot_columns() {
    let h = common::HttpHarness::start().await;

    let a_ip: std::net::IpAddr = "192.0.2.191".parse().unwrap();
    let b_ip: std::net::IpAddr = "192.0.2.192".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t13-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t13-b", b_ip).await;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t13-legacy-null-snapshot",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t13-a", "eval-t13-b"],
                "destination_ips": ["192.0.2.192", "192.0.2.191"],
                "loss_threshold_ratio": 0.02,
                "stddev_weight": 1.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id").to_string();

    // Force the campaign to `completed` so the state gate allows an
    // evaluation row. We bypass `/evaluate` and insert the row directly
    // with NULL snapshot columns — simulating a pre-T56 evaluation row.
    common::mark_completed(&h.state.pool, &campaign_id).await;

    // Insert a minimal `campaign_evaluations` row with NULL for all three
    // T56 snapshot columns. This is exactly the shape of rows that existed
    // before the 20260426000000_campaigns_edge_candidate migration.
    sqlx::query(
        "INSERT INTO campaign_evaluations
            (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
             baseline_pair_count, candidates_total, candidates_good,
             useful_latency_ms, max_hops, vm_lookback_minutes,
             evaluated_at)
         VALUES
            ($1::uuid, 0.02, 1.0, 'optimization',
             2, 0, 0,
             NULL, NULL, NULL,
             now())",
    )
    .bind(&campaign_id)
    .execute(&h.state.pool)
    .await
    .expect("raw insert of legacy evaluation row");

    // Flip campaign state to `evaluated` so the GET endpoint doesn't
    // require an additional state check.
    sqlx::query(
        "UPDATE measurement_campaigns SET state = 'evaluated', evaluated_at = now() WHERE id = $1::uuid",
    )
    .bind(&campaign_id)
    .execute(&h.state.pool)
    .await
    .expect("flip state to evaluated");

    // The read-path must return the row with NULL snapshot columns mapped
    // to absent JSON keys (skip_serializing_if = None).
    let got: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/evaluation"))
        .await;

    assert_eq!(
        got["baseline_pair_count"], 2,
        "baseline_pair_count must be readable: body = {got}"
    );
    assert_eq!(
        got["evaluation_mode"], "optimization",
        "evaluation_mode must be readable: body = {got}"
    );

    // The three T56 snapshot columns were NULL in the DB → must be absent
    // from the JSON (skip_serializing_if = Option::is_none).
    assert!(
        got.get("useful_latency_ms").is_none() || got["useful_latency_ms"].is_null(),
        "useful_latency_ms must be absent or null for legacy row: body = {got}"
    );
    assert!(
        got.get("max_hops").is_none() || got["max_hops"].is_null(),
        "max_hops must be absent or null for legacy row: body = {got}"
    );
    assert!(
        got.get("vm_lookback_minutes").is_none() || got["vm_lookback_minutes"].is_null(),
        "vm_lookback_minutes must be absent or null for legacy row: body = {got}"
    );
}

/// `latest_evaluation_for_campaign` must order edge_candidate candidates
/// by `coverage_weighted_ping_ms ASC` (lower is better) with tie-break
/// by `coverage_count DESC`. The shared SELECT previously ordered by
/// triple-mode `pairs_improved DESC, avg_improvement_ms DESC`, which
/// collapses to destination-IP order for edge_candidate (every row has
/// `pairs_improved=0` and `avg_improvement_ms=NULL`) and buries the
/// real ranking signal.
#[tokio::test]
async fn evaluation_orders_edge_candidates_by_coverage_weighted_ping_ms() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ec-rank-by-coverage-weighted",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["eval-rank-a"],
                "destination_ips": ["10.99.0.1"],
                "useful_latency_ms": 80.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();
    let campaign_uuid: uuid::Uuid = campaign_id.parse().expect("uuid");

    common::insert_agent_with_ip(pool, "eval-rank-a", "10.99.0.10".parse().unwrap()).await;

    // Seed an evaluation parent and three candidates with deliberately
    // permuted `coverage_weighted_ping_ms` values such that
    // destination-IP order disagrees with the spec ranking. After the
    // fix, the read should order by `coverage_weighted_ping_ms ASC`,
    // not by IP.
    let evaluation_id: uuid::Uuid = sqlx::query_scalar(
        r#"INSERT INTO campaign_evaluations
               (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
                useful_latency_ms, max_hops, vm_lookback_minutes,
                baseline_pair_count, candidates_total, candidates_good, evaluated_at)
           VALUES ($1, 0.05, 1.0, 'edge_candidate'::evaluation_mode,
                   80.0, 1, 30, 0, 3, 3, now())
           RETURNING id"#,
    )
    .bind(campaign_uuid)
    .fetch_one(pool)
    .await
    .expect("insert campaign_evaluations row");

    // Three rows: ascending IP, but the desired rank order is mid → low → high IP.
    // best:    coverage_weighted_ping_ms = 12.0  (IP 10.99.0.21)
    // middle:  coverage_weighted_ping_ms = 25.0  (IP 10.99.0.20)
    // worst:   coverage_weighted_ping_ms = 75.0  (IP 10.99.0.22)
    for (ip, cwp_ms, coverage) in &[
        ("10.99.0.20", 25.0_f32, 1_i32),
        ("10.99.0.21", 12.0_f32, 2_i32),
        ("10.99.0.22", 75.0_f32, 1_i32),
    ] {
        let ip_net =
            sqlx::types::ipnetwork::IpNetwork::from(ip.parse::<std::net::IpAddr>().unwrap());
        sqlx::query(
            r#"INSERT INTO campaign_evaluation_candidates
                   (evaluation_id, destination_ip, is_mesh_member,
                    pairs_improved, pairs_total_considered,
                    coverage_count, destinations_total, mean_ms_under_t,
                    coverage_weighted_ping_ms, direct_share, onehop_share, twohop_share,
                    has_real_x_source_data)
               VALUES ($1, $2::inet, false, 0, 0,
                       $3, 1, $4,
                       $4, 1.0, 0.0, 0.0,
                       true)"#,
        )
        .bind(evaluation_id)
        .bind(ip_net)
        .bind(coverage)
        .bind(cwp_ms)
        .execute(pool)
        .await
        .expect("insert candidate row");
    }

    let eval: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/evaluation"))
        .await;
    let candidates = eval["results"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates missing: {eval}"));

    let order: Vec<&str> = candidates
        .iter()
        .map(|c| c["destination_ip"].as_str().expect("destination_ip"))
        .collect();
    assert_eq!(
        order,
        vec!["10.99.0.21", "10.99.0.20", "10.99.0.22"],
        "edge_candidate read must rank by coverage_weighted_ping_ms ASC, not by destination_ip: {eval}"
    );
}

/// `composite_score` is the triple-mode `(pairs_improved /
/// baseline_pair_count) × avg_improvement_ms` ranking score; for
/// edge_candidate evaluations the rank metric is `coverage_count` /
/// `coverage_weighted_ping_ms` instead and `composite_score` must be
/// absent from the wire DTO. The DTO field uses
/// `serde(skip_serializing_if = "Option::is_none")`, so emitting `None`
/// drops the key entirely — distinguishable from a real triple-mode
/// candidate that scored exactly `0.0`.
#[tokio::test]
async fn evaluation_omits_composite_score_for_edge_candidate() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ec-composite-score-omitted",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["eval-cs-a"],
                "destination_ips": ["10.98.0.1"],
                "useful_latency_ms": 80.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();
    let campaign_uuid: uuid::Uuid = campaign_id.parse().expect("uuid");

    common::insert_agent_with_ip(pool, "eval-cs-a", "10.98.0.10".parse().unwrap()).await;

    // Seed a minimal edge_candidate evaluation parent + one candidate row.
    // `baseline_pair_count = 0` is the structural distinguisher of
    // edge_candidate parents — the triple-mode formula collapses to `0.0`
    // here, so the read-side guard must use the `evaluation_mode` column
    // (not just the `baseline_pair_count > 0` test) to elide the field.
    let evaluation_id: uuid::Uuid = sqlx::query_scalar(
        r#"INSERT INTO campaign_evaluations
               (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
                useful_latency_ms, max_hops, vm_lookback_minutes,
                baseline_pair_count, candidates_total, candidates_good, evaluated_at)
           VALUES ($1, 0.05, 1.0, 'edge_candidate'::evaluation_mode,
                   80.0, 1, 30, 0, 1, 1, now())
           RETURNING id"#,
    )
    .bind(campaign_uuid)
    .fetch_one(pool)
    .await
    .expect("insert campaign_evaluations row");

    let ip_net =
        sqlx::types::ipnetwork::IpNetwork::from("10.98.0.1".parse::<std::net::IpAddr>().unwrap());
    sqlx::query(
        r#"INSERT INTO campaign_evaluation_candidates
               (evaluation_id, destination_ip, is_mesh_member,
                pairs_improved, pairs_total_considered,
                coverage_count, destinations_total, mean_ms_under_t,
                coverage_weighted_ping_ms, direct_share, onehop_share, twohop_share,
                has_real_x_source_data)
           VALUES ($1, $2::inet, false, 0, 0,
                   1, 1, 12.0,
                   12.0, 1.0, 0.0, 0.0,
                   true)"#,
    )
    .bind(evaluation_id)
    .bind(ip_net)
    .execute(pool)
    .await
    .expect("insert candidate row");

    let eval: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/evaluation"))
        .await;
    let candidates = eval["results"]["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("candidates missing: {eval}"));
    assert_eq!(
        candidates.len(),
        1,
        "expected exactly one candidate: {eval}"
    );
    let cand = &candidates[0];

    // The DTO's `serde(skip_serializing_if = "Option::is_none")` drops the
    // key when None, so the field is absent (not `null`, not `0`). Either
    // a missing key or an explicit `null` is acceptable; a literal `0` is
    // a regression — operators reading a triple-mode dashboard would
    // mistake the absent score for a bottom-of-the-rank candidate.
    let score = cand.get("composite_score");
    assert!(
        score.is_none() || score == Some(&Value::Null),
        "composite_score must be absent or null for edge_candidate (got {score:?}): {cand}"
    );
}

/// C12-1 regression: when a campaign settles BOTH directions of a pair
/// (A→B and B→A), the reverse-direction fetch must not be allowed to
/// clobber the campaign-owned B→A row in `inputs.measurements`.
///
/// The reverse query (`reverse_direction_measurements_for_campaign`)
/// scans the global `measurements` table over a 24h window — it can
/// legitimately surface unrelated rows (detail-ping kind, another
/// campaign, a fresher reuse-bound sample) for the same
/// `(source_agent_id, destination_ip)` key the campaign already owns.
/// Pre-fix, those reverse rows were appended unconditionally and
/// `build_pair_lookup`'s last-write-wins replaced the campaign-owned
/// B→A baseline with whatever the global table held last. That broke
/// the per-pair direct RTT used by the evaluator's scoring. The fix
/// filters reverse rows whose `(source, destination_ip)` is already in
/// the campaign's active set; reverse rows still flow through for
/// pairs the campaign measured in only one direction, preserving the
/// `LegLookup` symmetry-fallback behavior.
#[tokio::test]
async fn reverse_does_not_clobber_campaign_owned_active_pair() {
    let h = common::HttpHarness::start().await;

    let a_ip: IpAddr = "192.0.2.211".parse().unwrap();
    let b_ip: IpAddr = "192.0.2.212".parse().unwrap();
    common::insert_agent_with_ip(&h.state.pool, "eval-t14-a", a_ip).await;
    common::insert_agent_with_ip(&h.state.pool, "eval-t14-b", b_ip).await;

    // Both A and B are sources; both .211 and .212 appear as
    // destinations so the campaign-owned set covers BOTH directions
    // (A→B and B→A) plus a transit candidate at .219.
    let campaign: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "evaluate-t14-reverse-no-clobber",
                "protocol": "icmp",
                "source_agent_ids": ["eval-t14-a", "eval-t14-b"],
                "destination_ips": ["192.0.2.212", "192.0.2.211", "192.0.2.219"],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "optimization",
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("id is string").to_string();

    // Campaign-owned baselines: A→B at 300 ms, B→A at 350 ms — both
    // distinct from the polluting value seeded below. The transit legs
    // go through .219 so a candidate row exists in the evaluation.
    common::seed_measurements(
        &h.state.pool,
        &campaign_id,
        &[
            ("eval-t14-a", "192.0.2.212", 300.0, 20.0, 0.0),
            ("eval-t14-b", "192.0.2.211", 350.0, 22.0, 0.0),
            ("eval-t14-a", "192.0.2.219", 120.0, 8.0, 0.0),
            ("eval-t14-b", "192.0.2.219", 121.0, 8.0, 0.0),
        ],
    )
    .await;

    // Polluting B→A row from outside the campaign (a `detail_ping` in
    // the same 24h window, written AFTER the campaign rows so its
    // `measured_at` is strictly newer). Without the C12-1 filter,
    // `reverse_direction_measurements_for_campaign` returns this row
    // (DISTINCT ON + ORDER BY measured_at DESC picks the newest), the
    // handler appends it to `inputs.measurements`, and
    // `build_pair_lookup`'s last-write-wins replaces the campaign-owned
    // 350.0 ms B→A baseline with this 999.0 ms pollutant.
    let dst_a = sqlx::types::ipnetwork::IpNetwork::from(a_ip);
    sqlx::query(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, latency_stddev_ms, loss_ratio, kind, measured_at) \
         VALUES ($1, $2, 'icmp', 10, 999.0, 50.0, 0.0, 'detail_ping', \
                 now() + interval '1 second')",
    )
    .bind("eval-t14-b")
    .bind(dst_a)
    .execute(&h.state.pool)
    .await
    .expect("insert polluting B→A detail_ping measurement");

    common::mark_completed(&h.state.pool, &campaign_id).await;

    let _eval: Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // Read the .219 candidate's pair_details and locate the (B→A) row.
    // The DTO's `direct_rtt_ms` for `(source=eval-t14-b,
    // destination=eval-t14-a)` is the B→A baseline RTT — must be the
    // campaign-owned 350.0, not the 999.0 pollutant.
    let pair_page: Value = h
        .get_expect_status(
            &format!(
                "/api/campaigns/{campaign_id}/evaluation/candidates/192.0.2.219/pair_details?limit=500"
            ),
            200,
        )
        .await;
    let entries = pair_page["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("pair_details endpoint missing entries: {pair_page}"));
    let ba_leg = entries
        .iter()
        .find(|pd| {
            pd["source_agent_id"] == "eval-t14-b" && pd["destination_agent_id"] == "eval-t14-a"
        })
        .unwrap_or_else(|| panic!("B→A pair_detail missing: {pair_page}"));
    let rtt = ba_leg["direct_rtt_ms"]
        .as_f64()
        .expect("direct_rtt_ms on B→A leg");
    assert!(
        (rtt - 350.0).abs() < 1e-3,
        "C12-1: campaign-owned B→A baseline (350.0) must win over the \
         unrelated reverse-fetched row (999.0); got direct_rtt_ms = {rtt}: \
         {ba_leg}"
    );
}
