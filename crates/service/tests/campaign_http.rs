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
//! The campaign scheduler is not spawned in the test harness (its
//! cancel token stays at `None` on the `AppState`), so draft and
//! running campaigns never tick forward on their own — every
//! transition in these tests is explicitly driven by an HTTP call.

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
             (source_agent_id, destination_ip, protocol, probe_count, loss_pct) \
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
