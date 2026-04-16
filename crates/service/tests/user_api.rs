//! Integration tests for the user-facing agent and route-snapshot endpoints:
//!
//! - `GET /api/agents` — list all agents from the registry snapshot
//! - `GET /api/agents/{id}` — single agent detail
//! - `GET /api/paths/{src}/{tgt}/routes/latest` — latest route snapshot
//! - `GET /api/paths/{src}/{tgt}/routes/{id}` — snapshot by id
//! - `GET /api/paths/{src}/{tgt}/routes` — paginated route list
//!
//! All tests drive the real axum router via `tower::ServiceExt::oneshot`.
//!
//! `X-Forwarded-For` IP allocations for this binary (`.110`–`.149`):
//!
//! | Octet  | Test                                                        |
//! |--------|-------------------------------------------------------------|
//! | `.110` | `agents_list_returns_empty_array_when_registry_is_empty`    |
//! | `.111` | `agents_list_requires_session`                              |
//! | `.112` | `agent_detail_returns_404_for_unknown_id`                   |
//! | `.113` | `agent_detail_returns_registry_snapshot_fields`             |
//! | `.114` | `routes_latest_returns_the_most_recent_snapshot`            |
//! | `.115` | `routes_by_id_returns_the_exact_snapshot`                   |
//! | `.116` | `routes_by_id_404_when_id_unknown`                         |
//! | `.117` | `routes_list_returns_recent_snapshots_descending`           |
//! | `.118` | `routes_list_rejects_limit_over_cap`                       |
//! | `.121` | `routes_latest_returns_404_when_no_snapshot_exists`         |
//! | `.122` | `routes_latest_rejects_invalid_protocol`                    |

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::util::ServiceExt;

mod common;

// ---------------------------------------------------------------------------
// Task 3: GET /api/agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agents_list_returns_empty_array_when_registry_is_empty() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.110").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.is_array(), "expected JSON array, got: {body}");
    assert_eq!(body.as_array().unwrap().len(), 0, "body = {body}");
}

#[tokio::test]
async fn agents_list_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Task 4: GET /api/agents/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_detail_returns_404_for_unknown_id() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.112").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents/does-not-exist")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agent_detail_returns_registry_snapshot_fields() {
    let pool = common::shared_migrated_pool().await.clone();

    // Seed an agent row directly in the DB.
    sqlx::query(
        "INSERT INTO agents (id, display_name, location, ip, lat, lon, agent_version)
         VALUES ('brazil-north', 'Fortaleza', 'BR', '170.80.110.90', -3.7, -38.5, 'v0.1.0')",
    )
    .execute(&pool)
    .await
    .expect("seed agent row");

    let state = common::state_with_admin(pool.clone());

    // Force the registry to pick up the seeded row.
    state
        .registry
        .force_refresh()
        .await
        .expect("registry refresh");

    let app = meshmon_service::http::router(state);

    // Login (build request manually so we can reuse `app` for the detail call).
    let cookie = common::login_as_admin(&app, "203.0.113.113").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents/brazil-north")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["id"], "brazil-north", "body = {body}");
    assert_eq!(body["display_name"], "Fortaleza", "body = {body}");
    assert_eq!(body["location"], "BR", "body = {body}");
    assert_eq!(body["ip"], "170.80.110.90", "body = {body}");
    assert_eq!(body["lat"], -3.7, "body = {body}");
    assert_eq!(body["lon"], -38.5, "body = {body}");
    assert_eq!(body["agent_version"], "v0.1.0", "body = {body}");
    assert!(body["registered_at"].is_string(), "body = {body}");
    assert!(body["last_seen_at"].is_string(), "body = {body}");

    // Cleanup: remove the seeded row so other tests aren't affected.
    sqlx::query("DELETE FROM agents WHERE id = 'brazil-north'")
        .execute(&pool)
        .await
        .expect("cleanup agent row");
}

// ---------------------------------------------------------------------------
// Shared helpers for route-snapshot tests (Tasks 5–7)
// ---------------------------------------------------------------------------

// NOTE: These tests seed data directly into the shared pool and clean up
// via explicit DELETE rather than transaction rollback. The handlers under
// test read from the PgPool (not from a test transaction), so committed
// rows are required for the HTTP layer to see them. Unique per-test
// agent/snapshot IDs (t5-src-*, t6-src-*, t7-src-*) prevent cross-test
// interference. If a test panics before cleanup, leaked rows are benign
// within a single test-binary run (the next invocation gets a fresh DB).

/// Seed two route-snapshot rows for the given source/target/protocol.
/// The first is observed at NOW()-1h, the second at NOW()-1min (the "latest").
/// Also ensures the required agent rows exist for FK constraints.
async fn seed_two_snapshots(pool: &sqlx::PgPool, src: &str, tgt: &str, protocol: &str) {
    for id in [src, tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip) \
             VALUES ($1, $1, '10.0.0.1') ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
    let hops = serde_json::json!([{
        "position": 1,
        "observed_ips": [{"ip": "10.0.0.1", "freq": 1.0}],
        "avg_rtt_micros": 1000,
        "stddev_rtt_micros": 100,
        "loss_pct": 0.0
    }]);
    let summary = serde_json::json!({
        "avg_rtt_micros": 1000,
        "loss_pct": 0.0,
        "hop_count": 1
    });
    // First snapshot at t-1h.
    sqlx::query(
        "INSERT INTO route_snapshots \
         (source_id, target_id, protocol, observed_at, hops, path_summary) \
         VALUES ($1, $2, $3, NOW() - INTERVAL '1 hour', $4::jsonb, $5::jsonb)",
    )
    .bind(src)
    .bind(tgt)
    .bind(protocol)
    .bind(&hops)
    .bind(&summary)
    .execute(pool)
    .await
    .unwrap();
    // Second snapshot at t-1min (the "latest").
    sqlx::query(
        "INSERT INTO route_snapshots \
         (source_id, target_id, protocol, observed_at, hops, path_summary) \
         VALUES ($1, $2, $3, NOW() - INTERVAL '1 minute', $4::jsonb, $5::jsonb)",
    )
    .bind(src)
    .bind(tgt)
    .bind(protocol)
    .bind(&hops)
    .bind(&summary)
    .execute(pool)
    .await
    .unwrap();
}

/// Delete seeded route-snapshot and agent rows for a given source/target pair.
async fn cleanup_snapshots(pool: &sqlx::PgPool, src: &str, tgt: &str) {
    sqlx::query("DELETE FROM route_snapshots WHERE source_id = $1 AND target_id = $2")
        .bind(src)
        .bind(tgt)
        .execute(pool)
        .await
        .expect("cleanup route_snapshots");
    sqlx::query("DELETE FROM agents WHERE id IN ($1, $2)")
        .bind(src)
        .bind(tgt)
        .execute(pool)
        .await
        .expect("cleanup agents");
}

// ---------------------------------------------------------------------------
// Task 5: GET /api/paths/{src}/{tgt}/routes/latest
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_latest_returns_the_most_recent_snapshot() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t5-src-latest", "t5-tgt-latest");
    seed_two_snapshots(&pool, src, tgt, "icmp").await;

    let state = common::state_with_admin(pool.clone());
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.114").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes/latest?protocol=icmp");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Should return the most-recent snapshot (t-1min, not t-1h).
    assert_eq!(body["source_id"], src, "body = {body}");
    assert_eq!(body["target_id"], tgt, "body = {body}");
    assert_eq!(body["protocol"], "icmp", "body = {body}");
    assert!(body["id"].is_i64(), "id should be i64: {body}");
    assert!(
        body["observed_at"].is_string(),
        "observed_at should be string: {body}"
    );
    assert!(body["hops"].is_array(), "hops should be array: {body}");
    assert!(
        body["path_summary"].is_object(),
        "path_summary should be object: {body}"
    );

    cleanup_snapshots(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// Task 6: GET /api/paths/{src}/{tgt}/routes/{snapshot_id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_by_id_returns_the_exact_snapshot() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t6-src-byid", "t6-tgt-byid");
    seed_two_snapshots(&pool, src, tgt, "icmp").await;

    // Find the oldest snapshot's id.
    let oldest_id: i64 = sqlx::query_scalar(
        "SELECT id FROM route_snapshots \
         WHERE source_id = $1 AND target_id = $2 \
         ORDER BY observed_at ASC LIMIT 1",
    )
    .bind(src)
    .bind(tgt)
    .fetch_one(&pool)
    .await
    .expect("fetch oldest id");

    let state = common::state_with_admin(pool.clone());
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.115").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes/{oldest_id}");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["id"], oldest_id, "body = {body}");
    assert_eq!(body["source_id"], src, "body = {body}");
    assert!(body["hops"].is_array(), "hops should be array: {body}");

    cleanup_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_by_id_404_when_id_unknown() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.116").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/paths/a/b/routes/999999999")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Task 7: GET /api/paths/{src}/{tgt}/routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_list_returns_recent_snapshots_descending() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t7-src-list", "t7-tgt-list");
    seed_two_snapshots(&pool, src, tgt, "icmp").await;

    let state = common::state_with_admin(pool.clone());
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.117").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes?protocol=icmp");
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&uri)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 128 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let items = body["items"].as_array().expect("items should be array");
    assert_eq!(items.len(), 2, "expected 2 snapshots, got: {body}");

    // Descending order: first item is newer than second.
    let ts0 = items[0]["observed_at"].as_str().unwrap();
    let ts1 = items[1]["observed_at"].as_str().unwrap();
    assert!(ts0 > ts1, "expected descending order: {ts0} > {ts1}");

    // Items should NOT have hops (summary only).
    assert!(
        items[0].get("hops").is_none(),
        "list items should not have hops: {body}"
    );

    assert_eq!(body["limit"], 100, "default limit = {body}");
    assert_eq!(body["offset"], 0, "default offset = {body}");

    cleanup_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_list_rejects_limit_over_cap() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.118").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/paths/a/b/routes?limit=501")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Additional coverage: 404 and validation edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_latest_returns_404_when_no_snapshot_exists() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.121").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/paths/nonexistent/nonexistent/routes/latest")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn routes_latest_rejects_invalid_protocol() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool);
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.122").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/paths/a/b/routes/latest?protocol=xyz")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
