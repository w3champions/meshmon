//! T56 — API-layer validation for new knobs and dismissal on knob changes.
//!
//! # Coverage
//!
//! | Test                                                 | Agents          | IPs (TEST-NET-2) |
//! |------------------------------------------------------|-----------------|------------------|
//! | `create_campaign_edge_candidate_without_useful_latency_returns_400` | — | — |
//! | `create_campaign_useful_latency_zero_returns_400`    | —               | —                |
//! | `create_campaign_diversity_with_max_hops_zero_returns_400` | —         | —                |
//! | `create_campaign_max_hops_three_returns_400`         | —               | —                |
//! | `create_campaign_vm_lookback_zero_returns_400`       | —               | —                |
//! | `create_campaign_vm_lookback_too_large_returns_400`  | —               | —                |
//! | `patch_max_hops_dismisses_evaluation`                | `t56v-{a,b,c}`  | `198.51.100.{71,.72,.73,.79}` |
//! | `evaluate_edge_candidate_with_no_destinations_returns_422` | `t56v-nd-a` | `198.51.100.{81}` |
//! | `evaluate_edge_candidate_with_no_measurements_returns_422` | `t56v-nm-{a,b}` | `198.51.100.{91,92,99}` |
//! | `evaluate_edge_candidate_excludes_mesh_candidate_from_destinations` | `t56v-c2-{a,b,c}` | `198.51.100.{111,112,119}` |

mod common;
use common::HttpHarness;
use serde_json::json;

// ---------------------------------------------------------------------------
// B1 — CREATE validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_campaign_edge_candidate_without_useful_latency_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "no useful latency",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"]
                // useful_latency_ms intentionally omitted
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "useful_latency_required", "body = {body}");
}

#[tokio::test]
async fn create_campaign_useful_latency_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "zero useful latency",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "useful_latency_ms": 0.0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "useful_latency_invalid", "body = {body}");
}

#[tokio::test]
async fn create_campaign_diversity_with_max_hops_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "diversity zero hops",
                "evaluation_mode": "diversity",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "max_hops": 0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "max_hops_invalid_for_mode", "body = {body}");
}

#[tokio::test]
async fn create_campaign_max_hops_three_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "max hops three",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "useful_latency_ms": 80.0,
                "max_hops": 3,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "max_hops_out_of_range", "body = {body}");
}

#[tokio::test]
async fn create_campaign_vm_lookback_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "vm lookback zero",
                "evaluation_mode": "optimization",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "vm_lookback_minutes": 0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "vm_lookback_out_of_range", "body = {body}");
}

#[tokio::test]
async fn create_campaign_vm_lookback_too_large_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "vm lookback too large",
                "evaluation_mode": "optimization",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "vm_lookback_minutes": 1441,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "vm_lookback_out_of_range", "body = {body}");
}

// ---------------------------------------------------------------------------
// B2 — PATCH dismisses evaluation on knob changes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_max_hops_dismisses_evaluation() {
    let h = HttpHarness::start().await;
    let id = common::create_evaluated_campaign(&h, "diversity").await;

    // Change max_hops — the campaign must transition out of `evaluated`
    // and the PATCH response body must reflect the post-dismiss state.
    // `campaign_evaluations` is append-only history, so the prior
    // evaluation row stays queryable; the frontend gates on
    // `state === 'evaluated'` to know the result is current.
    let patch_response: serde_json::Value = h
        .patch_json(&format!("/api/campaigns/{id}"), &json!({"max_hops": 1}))
        .await;
    assert_eq!(
        patch_response["state"], "completed",
        "PATCH response must reflect post-dismiss state; body = {patch_response}"
    );
    assert!(
        patch_response["evaluated_at"].is_null(),
        "PATCH response must clear evaluated_at after dismissal; body = {patch_response}"
    );

    // Historical evaluation row remains queryable — `dismiss_evaluation`
    // no longer DELETEs `campaign_evaluations`. The state transition is
    // the source of truth; the frontend treats `state === 'completed'`
    // as "evaluation is stale".
    let _: serde_json::Value = h
        .get_expect_status(&format!("/api/campaigns/{id}/evaluation"), 200)
        .await;
}

/// Switching `evaluation_mode` reshapes the entire evaluation row
/// (Triple → EdgeCandidate sidecars are incompatible). The PATCH
/// dismissal check must include `evaluation_mode` so the campaign
/// transitions out of `evaluated` and the SPA forces a re-run before
/// it can interact with mode-specific surfaces.
#[tokio::test]
async fn patch_evaluation_mode_dismisses_evaluation() {
    let h = HttpHarness::start().await;
    let id = common::create_evaluated_campaign(&h, "diversity").await;

    // Switch the evaluation mode to edge_candidate. useful_latency_ms
    // must accompany the change since the validator now sees the
    // post-PATCH state and edge_candidate requires it.
    let patch_response: serde_json::Value = h
        .patch_json(
            &format!("/api/campaigns/{id}"),
            &json!({"evaluation_mode": "edge_candidate", "useful_latency_ms": 80.0}),
        )
        .await;
    assert_eq!(
        patch_response["state"], "completed",
        "PATCH response must reflect post-dismiss state; body = {patch_response}"
    );
    assert!(
        patch_response["evaluated_at"].is_null(),
        "PATCH response must clear evaluated_at after dismissal; body = {patch_response}"
    );
}

/// Dismissing an evaluation must NOT delete the historical
/// `campaign_evaluations` row or its child sidecar tables. Each
/// evaluation snapshots the knobs it was run under and stays valid
/// for that snapshot — knob changes only invalidate the campaign's
/// *current* `evaluated` state, not the historical record.
#[tokio::test]
async fn dismiss_preserves_historical_evaluation_rows() {
    let h = HttpHarness::start().await;
    let pool = &h.state.pool;
    let id = common::create_evaluated_campaign(&h, "diversity").await;
    let campaign_uuid: uuid::Uuid = id.parse().expect("campaign id parses as uuid");

    // Confirm the post-evaluate row was written.
    let pre_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1::uuid",
    )
    .bind(campaign_uuid)
    .fetch_one(pool)
    .await
    .expect("count evaluations pre-patch");
    assert!(
        pre_count >= 1,
        "fixture must produce at least one evaluation row, got {pre_count}"
    );

    // PATCH a knob to trigger dismissal.
    let _: serde_json::Value = h
        .patch_json(&format!("/api/campaigns/{id}"), &json!({"max_hops": 1}))
        .await;

    // Historical evaluation rows persist — dismissal only flips the
    // campaign state.
    let post_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1::uuid",
    )
    .bind(campaign_uuid)
    .fetch_one(pool)
    .await
    .expect("count evaluations post-patch");
    assert_eq!(
        post_count, pre_count,
        "dismiss must NOT delete historical evaluation rows: pre={pre_count} post={post_count}"
    );

    // Campaign row is back in `completed` with `evaluated_at` cleared.
    let camp: serde_json::Value = h.get_json(&format!("/api/campaigns/{id}")).await;
    assert_eq!(camp["state"], "completed", "campaign state = {camp}");
    assert!(camp["evaluated_at"].is_null(), "evaluated_at = {camp}");
}

// ---------------------------------------------------------------------------
// S3 — HTTP-level 422 for edge_candidate evaluate preconditions
// ---------------------------------------------------------------------------

/// Create a minimal edge_candidate campaign, delete all its
/// `campaign_pairs` rows (so `candidate_ips` is empty), then call
/// `/evaluate`. Expect 422 `no_destinations`.
///
/// Campaign creation requires at least one destination_ip so we create
/// with one destination then DELETE the pair row directly via SQL to
/// simulate an `/edit` that removed the last destination.
#[tokio::test]
async fn evaluate_edge_candidate_with_no_destinations_returns_422() {
    let h = HttpHarness::start().await;
    let pool = &h.state.pool;

    common::insert_agent_with_ip(pool, "t56v-nd-a", "198.51.100.81".parse().unwrap()).await;

    let campaign: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ec-no-destinations",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["t56v-nd-a"],
                "destination_ips": ["198.51.100.81"],
                "useful_latency_ms": 80.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    // Remove all campaign_pairs so candidate_ips resolves to empty.
    sqlx::query("DELETE FROM campaign_pairs WHERE campaign_id = $1::uuid")
        .bind(&campaign_id)
        .execute(pool)
        .await
        .expect("delete campaign_pairs");

    common::mark_completed(pool, &campaign_id).await;

    let body = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            422,
        )
        .await;
    assert_eq!(body["error"], "no_destinations", "body = {body}");
}

/// Create an edge_candidate campaign with destinations but no probe
/// measurements, then call `/evaluate`. Expect 422 `no_candidates_with_data`.
#[tokio::test]
async fn evaluate_edge_candidate_with_no_measurements_returns_422() {
    let h = HttpHarness::start().await;
    let pool = &h.state.pool;

    common::insert_agent_with_ip(pool, "t56v-nm-a", "198.51.100.91".parse().unwrap()).await;
    common::insert_agent_with_ip(pool, "t56v-nm-b", "198.51.100.92".parse().unwrap()).await;

    let campaign: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ec-no-measurements",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["t56v-nm-a", "t56v-nm-b"],
                "destination_ips": ["198.51.100.99"],
                "useful_latency_ms": 80.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    // Intentionally skip seed_measurements — no probe data exists.
    common::mark_completed(pool, &campaign_id).await;

    let body = h
        .post_expect_status(
            &format!("/api/campaigns/{campaign_id}/evaluate"),
            &json!({}),
            422,
        )
        .await;
    assert_eq!(body["error"], "no_candidates_with_data", "body = {body}");
}

// ---------------------------------------------------------------------------
// B3 — PATCH validates against effective post-PATCH values
// ---------------------------------------------------------------------------

/// A metadata-only PATCH against an existing edge_candidate campaign
/// must not trip `useful_latency_required` just because the body omits
/// `useful_latency_ms`. The validator runs against the row's effective
/// state after PATCH (`COALESCE(body, stored)`), so an absent field
/// keeps its stored value.
#[tokio::test]
async fn patch_metadata_only_preserves_useful_latency_for_edge_candidate() {
    let h = HttpHarness::start().await;

    // Create an edge_candidate campaign with useful_latency_ms set.
    let body = json!({
        "title": "ec-patch-effective",
        "evaluation_mode": "edge_candidate",
        "protocol": "icmp",
        "source_agent_ids": ["sa1"],
        "destination_ips": ["198.51.100.1"],
        "useful_latency_ms": 80.0,
    });
    let created: serde_json::Value = h.post_json("/api/campaigns", &body).await;
    let id = created["id"].as_str().expect("id").to_string();
    assert!(
        (created["useful_latency_ms"].as_f64().unwrap() - 80.0).abs() < 1e-3,
        "stored useful_latency_ms must round-trip on create: {created}"
    );

    // PATCH only the notes — the validator must validate the
    // post-PATCH state (useful_latency_ms = 80.0) rather than the body
    // (useful_latency_ms = absent → None).
    let patched: serde_json::Value = h
        .patch_json(&format!("/api/campaigns/{id}"), &json!({"notes": "edited"}))
        .await;
    assert_eq!(patched["notes"], "edited");
    assert!(
        (patched["useful_latency_ms"].as_f64().unwrap() - 80.0).abs() < 1e-3,
        "useful_latency_ms must be preserved across metadata-only PATCH: {patched}"
    );
}

// ---------------------------------------------------------------------------
// C2-3 — EdgeCandidate roster excludes mesh-candidate destinations
// ---------------------------------------------------------------------------

/// EdgeCandidate destination set B is the source-agent roster, NOT the
/// candidate-IP set. A registered mesh agent whose IP is selected as a
/// candidate (but not as a source) must NOT leak into the destination
/// set — otherwise it gets double-counted as both X (via candidate_ips)
/// and B (via the agents roster), inflating destinations_total and
/// producing phantom heatmap rows.
#[tokio::test]
async fn evaluate_edge_candidate_excludes_mesh_candidate_from_destinations() {
    let h = HttpHarness::start().await;
    let pool = &h.state.pool;

    // Two source agents and one extra mesh agent C whose IP is also a
    // candidate. C is intentionally absent from `source_agent_ids`.
    common::insert_agent_with_ip(pool, "t56v-c2-a", "198.51.100.111".parse().unwrap()).await;
    common::insert_agent_with_ip(pool, "t56v-c2-b", "198.51.100.112".parse().unwrap()).await;
    common::insert_agent_with_ip(pool, "t56v-c2-c", "198.51.100.119".parse().unwrap()).await;

    let campaign: serde_json::Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ec-mesh-candidate-leak-guard",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["t56v-c2-a", "t56v-c2-b"],
                // C's IP is a candidate; C itself is not a source.
                "destination_ips": ["198.51.100.119"],
                "useful_latency_ms": 80.0,
            }),
        )
        .await;
    let campaign_id = campaign["id"].as_str().expect("campaign id").to_string();

    // Baseline a↔b plus transit through X = C's IP.
    common::seed_measurements(
        pool,
        &campaign_id,
        &[
            ("t56v-c2-a", "198.51.100.112", 300.0, 5.0, 0.0),
            ("t56v-c2-b", "198.51.100.111", 300.0, 5.0, 0.0),
            ("t56v-c2-a", "198.51.100.119", 100.0, 5.0, 0.0),
            ("t56v-c2-b", "198.51.100.119", 101.0, 5.0, 0.0),
        ],
    )
    .await;
    common::mark_completed(pool, &campaign_id).await;
    let _: serde_json::Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id}/evaluate"))
        .await;

    // Read back the persisted candidate aggregates and edge-pair rows.
    // Destinations are A and B only. C must not appear as a destination,
    // and `destinations_total` must equal 2 (A and B), not 3.
    let campaign_uuid: uuid::Uuid = campaign_id.parse().expect("uuid");
    let dest_total: i32 = sqlx::query_scalar(
        "SELECT destinations_total \
           FROM campaign_evaluation_candidates c \
           JOIN campaign_evaluations e ON e.id = c.evaluation_id \
          WHERE e.campaign_id = $1 \
            AND c.destination_ip = '198.51.100.119'::inet",
    )
    .bind(campaign_uuid)
    .fetch_one(pool)
    .await
    .expect("candidate row exists for X = 198.51.100.119");
    assert_eq!(
        dest_total, 2,
        "destinations_total must count source agents only (A, B); got {dest_total} — \
         mesh-candidate C leaked into the destination roster"
    );

    let dest_agent_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT destination_agent_id \
           FROM campaign_evaluation_edge_pair_details epd \
           JOIN campaign_evaluations e ON e.id = epd.evaluation_id \
          WHERE e.campaign_id = $1",
    )
    .bind(campaign_uuid)
    .fetch_all(pool)
    .await
    .expect("query edge pair destinations");
    assert!(
        !dest_agent_ids.contains(&"t56v-c2-c".to_string()),
        "destination roster must not contain mesh-candidate C; got {dest_agent_ids:?}"
    );
}
