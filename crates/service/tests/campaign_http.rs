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

/// `GET /api/campaigns/:id` populates `source_agent_ids` with the
/// DISTINCT set of source agents from `campaign_pairs`, ascending. The
/// SPA's CompareTab picker reads this field directly; without it the
/// picker rendered the empty-state card for every real campaign.
#[tokio::test]
async fn get_one_returns_source_agent_ids() {
    let h = common::HttpHarness::start().await;

    let created: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "http-source-agents",
                "protocol": "icmp",
                "source_agent_ids": ["agent-sa-c", "agent-sa-a", "agent-sa-b"],
                "destination_ips": ["198.51.100.250"],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string");

    let got: serde_json::Value = h.get_json(&format!("/api/campaigns/{id}")).await;
    let agents: Vec<String> = got["source_agent_ids"]
        .as_array()
        .expect("source_agent_ids array present")
        .iter()
        .map(|v| v.as_str().expect("agent id is string").to_string())
        .collect();
    // DISTINCT-ordered ascending; each source surfaces exactly once.
    assert_eq!(
        agents,
        vec![
            "agent-sa-a".to_string(),
            "agent-sa-b".to_string(),
            "agent-sa-c".to_string(),
        ],
        "source_agent_ids must be DISTINCT and sorted; got {agents:?}"
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
    // Verifies both stamp paths now that pair-detail rows live behind
    // the paginated endpoint instead of nested on the candidate DTO:
    //   - `/evaluation`                               → candidate hostname
    //   - `/evaluation/.../pair_details`              → pair-detail
    //                                                   destination_hostname
    //
    // The candidate transit IP is identical to the pair_detail's
    // destination_ip, so one positive-cache record covers both stamps.
    //
    // Also proves the stamp is response-time only: the child tables
    // carry no hostname columns, so the response-time join is the
    // only path hostnames can ever reach the wire.
    use meshmon_service::hostname::record_positive;

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

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

    // Candidate transit IP — the evaluator surfaces this same value on
    // both `candidates[*].destination_ip` and
    // `candidates[*].pair_details[*].destination_ip`, so one hostname
    // stamp lights up both nested paths.
    let cand_ip_str = "198.51.100.254";
    let cand_ip: std::net::IpAddr = cand_ip_str.parse().unwrap();

    // Seed the parent + child rows directly so the test focuses on
    // the stamp paths rather than driving the full evaluator. The
    // evaluator today stamps every pair_detail with `active_probe`;
    // seed the same here.
    let evaluation_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO campaign_evaluations \
             (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode, \
              baseline_pair_count, candidates_total, candidates_good, \
              avg_improvement_ms, evaluated_at) \
         VALUES ($1, 0.10, 1.0, 'optimization', 1, 1, 1, 2.0, now()) \
         RETURNING id",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await
    .expect("seed campaign_evaluations row");

    sqlx::query(
        "INSERT INTO campaign_evaluation_candidates \
             (evaluation_id, destination_ip, is_mesh_member, \
              pairs_improved, pairs_total_considered, avg_improvement_ms) \
         VALUES ($1, $2::inet, false, 1, 1, 2.0)",
    )
    .bind(evaluation_id)
    .bind(cand_ip_str)
    .execute(pool)
    .await
    .expect("seed candidate row");

    sqlx::query(
        "INSERT INTO campaign_evaluation_pair_details \
             (evaluation_id, candidate_destination_ip, source_agent_id, destination_agent_id, \
              direct_rtt_ms, direct_stddev_ms, direct_loss_ratio, direct_source, \
              transit_rtt_ms, transit_stddev_ms, transit_loss_ratio, \
              improvement_ms, qualifies) \
         VALUES ($1, $2::inet, 'agent-a', 'agent-b', \
                 10.0, 1.0, 0.0, 'active_probe', \
                 8.0, 1.0, 0.0, 2.0, true)",
    )
    .bind(evaluation_id)
    .bind(cand_ip_str)
    .execute(pool)
    .await
    .expect("seed pair_detail row");

    // Seed a positive-cached hostname for the candidate IP — both
    // stamp paths (`candidates[*].hostname` and
    // `candidates[*].pair_details[*].destination_hostname`) key on
    // the same IP in this fixture, so one record covers both.
    record_positive(pool, cand_ip, "candidate.example.com")
        .await
        .expect("seed candidate hostname");

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
    assert!(
        cand.get("pair_details").is_none(),
        "T55: pair_details must NOT appear on the candidate's wire shape: {body}"
    );

    // Pair-detail hostname stamp now happens on the paginated
    // endpoint. The candidate transit IP doubles as the
    // pair_detail.destination_ip, so the same positive-cache record
    // covers both stamps.
    let page: serde_json::Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id_str}/evaluation/candidates/{cand_ip_str}/pair_details?limit=10"
        ))
        .await;
    let pd = &page["entries"][0];
    assert_eq!(
        pd["destination_ip"], cand_ip_str,
        "pair_detail's destination_ip mirrors the candidate transit IP: {page}"
    );
    assert_eq!(
        pd["destination_hostname"], "candidate.example.com",
        "pair_detail destination_hostname must be stamped from positive cache: {page}"
    );
    assert_eq!(
        pd["direct_source"], "active_probe",
        "pair_detail direct_source stamped by the evaluator defaults to active_probe: {page}"
    );

    // Stamp-invariant: the relational child tables carry no hostname
    // columns, so the only way any hostname reaches the wire is via
    // the response-time join. Assert the schema stays hostname-free.
    let cand_cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
          WHERE table_name = 'campaign_evaluation_candidates'",
    )
    .fetch_all(pool)
    .await
    .expect("list candidate cols");
    assert!(
        !cand_cols.iter().any(|c| c == "hostname"),
        "campaign_evaluation_candidates must not carry a hostname column: {cand_cols:?}"
    );
    let pd_cols: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
          WHERE table_name = 'campaign_evaluation_pair_details'",
    )
    .fetch_all(pool)
    .await
    .expect("list pair_detail cols");
    assert!(
        !pd_cols.iter().any(|c| c == "destination_hostname"),
        "campaign_evaluation_pair_details must not carry a destination_hostname column: {pd_cols:?}"
    );
}
