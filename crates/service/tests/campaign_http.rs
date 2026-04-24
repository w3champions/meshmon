//! Integration tests for `/api/campaigns/*` — lifecycle, preview, and pairs.
//!
//! This binary shares the process-wide migrated Postgres pool via
//! `common::HttpHarness::start()`. Tests pick disjoint `(agent_id,
//! destination_ip)` ranges so parallel runs never collide on
//! `campaign_pairs`'s `(campaign_id, source_agent_id, destination_ip)`
//! uniqueness constraint or on the `measurements` reuse-index lookup.
//!
//! | Test                                             | Agent id   | Destination IPs            |
//! |--------------------------------------------------|------------|----------------------------|
//! | `create_start_stop_lifecycle`                    | `agent-h1` | `198.51.100.201, .202`     |
//! | `preview_dispatch_count_returns_total_reusable…` | `agent-h2` | `198.51.100.210`           |
//! | `pairs_endpoint_filters_by_state`                | `agent-h3` | `198.51.100.220, .221`     |
//!
//! The campaign scheduler is not spawned in the test harness, so
//! `AppState.campaign_cancel` is a no-op token; lifecycle transitions
//! are driven explicitly by HTTP calls and draft / running campaigns
//! never tick forward on their own.

mod common;

use axum::http::StatusCode;

#[tokio::test]
async fn create_start_stop_lifecycle() {
    let h = common::HttpHarness::start().await;

    let body = serde_json::json!({
        "title": "http-create",
        "protocol": "icmp",
        "source_agent_ids": ["agent-h1"],
        "destination_ips": ["198.51.100.201", "198.51.100.202"],
    });
    let created: serde_json::Value = h.post_json("/api/campaigns", &body).await;
    let id = created["id"].as_str().expect("id is string");
    assert_eq!(created["state"], "draft", "body = {created}");

    // Draft -> Running.
    let started: serde_json::Value = h
        .post_json_empty(&format!("/api/campaigns/{id}/start"))
        .await;
    assert_eq!(started["state"], "running", "body = {started}");

    // Second start must 409 (illegal_state_transition).
    let (status, body) = h.post_empty(&format!("/api/campaigns/{id}/start")).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "second /start must 409; body = {body}"
    );

    // Running -> Stopped.
    let stopped: serde_json::Value = h
        .post_json_empty(&format!("/api/campaigns/{id}/stop"))
        .await;
    assert_eq!(stopped["state"], "stopped", "body = {stopped}");

    // DELETE -> 204 No Content (idempotent).
    let (status, body) = h.delete(&format!("/api/campaigns/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body = {body}");
    assert!(body.is_empty(), "204 body must be empty, got {body:?}");
}

#[tokio::test]
async fn preview_dispatch_count_returns_total_reusable_fresh() {
    let h = common::HttpHarness::start().await;

    let body = serde_json::json!({
        "title": "http-preview",
        "protocol": "icmp",
        "source_agent_ids": ["agent-h2"],
        "destination_ips": ["198.51.100.210"],
    });
    let created: serde_json::Value = h.post_json("/api/campaigns", &body).await;
    let id = created["id"].as_str().expect("id is string");

    // Seed a reusable measurement for the pair. Uses `sqlx::query(...)`
    // (dynamic, not the macro) so this test does not write into the
    // committed `.sqlx/` offline cache.
    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, loss_ratio) \
         VALUES ('agent-h2', '198.51.100.210', 'icmp', 10, 0.0)",
    )
    .execute(&h.state.pool)
    .await
    .expect("seed measurement");

    let preview: serde_json::Value = h
        .get_json(&format!("/api/campaigns/{id}/preview-dispatch-count"))
        .await;

    let total = preview["total"].as_i64().expect("total is i64");
    let reusable = preview["reusable"].as_i64().expect("reusable is i64");
    let fresh = preview["fresh"].as_i64().expect("fresh is i64");

    // 1 source × 1 destination = 1 pair.
    assert_eq!(total, 1, "body = {preview}");
    // At least the one we just seeded; the `DISTINCT ON (source, dest)`
    // in the preview query caps reusable at `total`, so the assertion
    // below is tight even when prior runs left stale rows in the shared
    // pool's `measurements` table.
    assert!(reusable >= 1, "reusable >= 1 expected; body = {preview}");
    assert_eq!(fresh, total - reusable, "body = {preview}");
}

#[tokio::test]
async fn pairs_endpoint_filters_by_state() {
    let h = common::HttpHarness::start().await;

    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "http-pairs",
                "protocol": "icmp",
                "source_agent_ids": ["agent-h3"],
                "destination_ips": ["198.51.100.220", "198.51.100.221"],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    // No scheduler is running in this harness, so all pairs stay in
    // their seeded `pending` state — 1 source × 2 destinations = 2
    // pending pairs.
    let pending: Vec<serde_json::Value> = h
        .get_json(&format!("/api/campaigns/{id}/pairs?state=pending"))
        .await;
    assert_eq!(pending.len(), 2, "pending pairs = {pending:?}");

    let succeeded: Vec<serde_json::Value> = h
        .get_json(&format!("/api/campaigns/{id}/pairs?state=succeeded"))
        .await;
    assert_eq!(succeeded.len(), 0, "succeeded pairs = {succeeded:?}");
}

#[tokio::test]
async fn get_one_pair_counts_excludes_detail_rows() {
    // `GET /api/campaigns/:id` returns baseline `pair_counts`. Detail
    // rows have independent state and must not inflate baseline
    // pending/dispatched/settled counters that operators read to track
    // campaign completion.
    let h = common::HttpHarness::start().await;

    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "http-pair-counts-detail",
                "protocol": "icmp",
                "source_agent_ids": ["agent-pc1"],
                "destination_ips": ["198.51.100.240"],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    // Seed a detail_mtr row on a different destination so it is not
    // collapsed onto the baseline row under the 4-col UNIQUE.
    let uuid_id: uuid::Uuid = id.parse().expect("id is uuid");
    sqlx::query(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, \
              resolution_state, kind) \
         VALUES ($1::uuid, 'agent-pc1', '198.51.100.241'::inet, \
                 'pending', 'detail_mtr')",
    )
    .bind(uuid_id)
    .execute(&h.state.pool)
    .await
    .expect("seed detail_mtr row");

    let got: serde_json::Value = h.get_json(&format!("/api/campaigns/{id}")).await;
    let counts = got["pair_counts"]
        .as_array()
        .expect("pair_counts array present");
    // Exactly one baseline pair seeded, all `pending`; the detail_mtr
    // row must NOT appear in the aggregate.
    let pending_total: i64 = counts
        .iter()
        .filter(|c| c[0] == "pending")
        .map(|c| c[1].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(
        pending_total, 1,
        "only baseline pair counted; got: {counts:?}"
    );
}

#[tokio::test]
async fn patch_rejects_blank_title() {
    // `create` refuses a blank title (handler check at create-time).
    // PATCH must mirror that invariant so an existing campaign can't
    // have its title edited to whitespace through a side door.
    let h = common::HttpHarness::start().await;
    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "http-patch-title",
                "protocol": "icmp",
                "source_agent_ids": ["agent-h4"],
                "destination_ips": ["198.51.100.230"],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    // A blank title must produce 400 with `title_required`.
    let (status, body) = h
        .patch_raw(
            &format!("/api/campaigns/{id}"),
            &serde_json::json!({ "title": "   " }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert!(body.contains("title_required"), "body = {body}");

    // A missing title (no change) still succeeds.
    let patched: serde_json::Value = h
        .patch_json(
            &format!("/api/campaigns/{id}"),
            &serde_json::json!({ "notes": "updated" }),
        )
        .await;
    assert_eq!(patched["notes"], "updated");
    assert_eq!(patched["title"], "http-patch-title", "body = {patched}");
}

// ---------------------------------------------------------------------------
// T53c: hostname stamping on campaign pair endpoints (three-state)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pairs_endpoint_stamps_destination_hostname_from_positive_cache() {
    use meshmon_service::hostname::record_positive;

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let dest_str = "198.51.100.250";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    // Create a campaign with one pair at the destination.
    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "hn-pairs-pos",
                "protocol": "icmp",
                "source_agent_ids": ["agent-hn-pairs"],
                "destination_ips": [dest_str],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    // Seed positive hostname cache.
    record_positive(pool, dest_ip, "pairs-dest.example.com")
        .await
        .expect("seed pairs dest hostname");

    let pairs: Vec<serde_json::Value> = h.get_json(&format!("/api/campaigns/{id}/pairs")).await;
    assert!(!pairs.is_empty(), "expected at least one pair");

    let pair = pairs
        .iter()
        .find(|p| p["destination_ip"].as_str() == Some(dest_str))
        .expect("pair with destination 198.51.100.250 not found");
    assert_eq!(
        pair["destination_hostname"], "pairs-dest.example.com",
        "positive-cached destination_hostname missing in pairs: {pair}"
    );
}

#[tokio::test]
async fn pairs_endpoint_omits_destination_hostname_on_negative_cache() {
    use meshmon_service::hostname::record_negative;

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let dest_str = "198.51.100.251";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "hn-pairs-neg",
                "protocol": "icmp",
                "source_agent_ids": ["agent-hn-pairs-neg"],
                "destination_ips": [dest_str],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    // Seed negative hostname cache.
    record_negative(pool, dest_ip)
        .await
        .expect("seed pairs dest negative hostname");

    let pairs: Vec<serde_json::Value> = h.get_json(&format!("/api/campaigns/{id}/pairs")).await;
    assert!(!pairs.is_empty(), "expected at least one pair");

    let pair = pairs
        .iter()
        .find(|p| p["destination_ip"].as_str() == Some(dest_str))
        .expect("pair with destination 198.51.100.251 not found");
    assert!(
        pair.get("destination_hostname").is_none(),
        "negative-cached pair must omit destination_hostname: {pair}"
    );
}

#[tokio::test]
async fn pairs_endpoint_cold_miss_omits_destination_hostname_and_enqueues_resolver() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let dest_str = "198.51.100.252";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "hn-pairs-cold",
                "protocol": "icmp",
                "source_agent_ids": ["agent-hn-pairs-cold"],
                "destination_ips": [dest_str],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");
    // No cache seed → cold miss.

    let pairs: Vec<serde_json::Value> = h.get_json(&format!("/api/campaigns/{id}/pairs")).await;
    assert!(!pairs.is_empty(), "expected at least one pair");

    let pair = pairs
        .iter()
        .find(|p| p["destination_ip"].as_str() == Some(dest_str))
        .expect("pair with destination 198.51.100.252 not found");
    assert!(
        pair.get("destination_hostname").is_none(),
        "cold-miss pair must omit destination_hostname: {pair}"
    );

    // The resolver should have been enqueued.
    assert!(
        common::wait_for_cache_row(pool, dest_ip).await,
        "resolver never wrote a cache row for {dest_ip} — enqueue was skipped"
    );
}

#[tokio::test]
async fn get_evaluation_stamps_candidate_and_pair_detail_hostnames() {
    // Verifies both nested stamp paths on `/api/campaigns/{id}/evaluation`:
    //   - `results.candidates[*].hostname`            (candidate IP)
    //   - `results.candidates[*].pair_details[*].destination_hostname`
    //     (pair-detail destination IP)
    //
    // Also proves the stamp is response-time only: after the response
    // comes back, the stored JSONB (`campaign_evaluations.results`) is
    // re-read directly and asserted to carry NO hostname fields at all.
    use meshmon_service::hostname::record_positive;

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // Create a campaign so the FK from `campaign_evaluations.campaign_id`
    // resolves, and transition it to `completed` so `state IN
    // ('completed','evaluated')` — the only states `/evaluation` can be
    // read from. (The handler surfaces 404 `not_evaluated` for other
    // states, but `get_evaluation` reads the row directly regardless of
    // state, so the transition is only strictly required for
    // `POST /evaluate`. Left here for symmetry with the end-to-end flow.)
    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "hn-get-eval",
                "protocol": "icmp",
                "source_agent_ids": ["agent-hn-eval"],
                "destination_ips": ["198.51.100.253"],
            }),
        )
        .await;
    let campaign_id_str = created["id"].as_str().expect("id is string").to_owned();
    let campaign_id: uuid::Uuid = campaign_id_str.parse().expect("parse campaign id");

    // Candidate IP and pair-detail destination IP. Distinct so the two
    // stamp paths can be asserted independently.
    let cand_ip_str = "198.51.100.254";
    let cand_ip: std::net::IpAddr = cand_ip_str.parse().unwrap();
    let pd_ip_str = "198.51.100.255";
    let pd_ip: std::net::IpAddr = pd_ip_str.parse().unwrap();

    // Craft a raw `EvaluationResultsDto` JSONB with one candidate + one
    // nested pair_detail. No hostname fields are included — they are
    // skip-none, so the stored JSON must never carry them. The handler
    // populates both via response-time stamping.
    let results = serde_json::json!({
        "candidates": [{
            "destination_ip": cand_ip_str,
            "is_mesh_member": false,
            "pairs_improved": 1,
            "pairs_total_considered": 1,
            "avg_improvement_ms": 2.0,
            "avg_loss_ratio": 0.0,
            "composite_score": 1.5,
            "pair_details": [{
                "source_agent_id": "agent-a",
                "destination_agent_id": "agent-b",
                "destination_ip": pd_ip_str,
                "direct_rtt_ms": 10.0,
                "direct_stddev_ms": 1.0,
                "direct_loss_ratio": 0.0,
                "transit_rtt_ms": 8.0,
                "transit_stddev_ms": 1.0,
                "transit_loss_ratio": 0.0,
                "improvement_ms": 2.0,
                "qualifies": true
            }]
        }],
        "unqualified_reasons": {}
    });
    sqlx::query(
        "INSERT INTO campaign_evaluations \
             (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode, \
              baseline_pair_count, candidates_total, candidates_good, \
              avg_improvement_ms, results, evaluated_at) \
         VALUES ($1, 0.10, 1.0, 'optimization', 1, 1, 1, 2.0, $2::jsonb, now())",
    )
    .bind(campaign_id)
    .bind(&results)
    .execute(pool)
    .await
    .expect("seed campaign_evaluations row");

    // Seed positive-cached hostnames for both the candidate and the
    // pair-detail destination — covers both nested stamp paths.
    record_positive(pool, cand_ip, "candidate.example.com")
        .await
        .expect("seed candidate hostname");
    record_positive(pool, pd_ip, "pair-detail.example.com")
        .await
        .expect("seed pair_detail hostname");

    let body: serde_json::Value = h
        .get_json(&format!("/api/campaigns/{campaign_id_str}/evaluation"))
        .await;

    let cand = &body["results"]["candidates"][0];
    assert_eq!(
        cand["destination_ip"], cand_ip_str,
        "candidate ip mismatch: {body}"
    );
    assert_eq!(
        cand["hostname"], "candidate.example.com",
        "candidate hostname must be stamped from positive cache: {body}"
    );
    let pd = &cand["pair_details"][0];
    assert_eq!(
        pd["destination_ip"], pd_ip_str,
        "pair_detail ip mismatch: {body}"
    );
    assert_eq!(
        pd["destination_hostname"], "pair-detail.example.com",
        "pair_detail destination_hostname must be stamped from positive cache: {body}"
    );

    // Stamp-invariant: the stored JSONB must NEVER carry hostname fields
    // — the stamp is a response-time join only. Re-read the row and
    // assert both keys are absent from the serialised document.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT results FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(campaign_id)
            .fetch_one(pool)
            .await
            .expect("read back campaign_evaluations row");
    let stored_cand = &stored["candidates"][0];
    assert!(
        stored_cand.get("hostname").is_none(),
        "stored JSONB must NOT carry candidates[*].hostname: {stored}"
    );
    let stored_pd = &stored_cand["pair_details"][0];
    assert!(
        stored_pd.get("destination_hostname").is_none(),
        "stored JSONB must NOT carry pair_details[*].destination_hostname: {stored}"
    );
}
