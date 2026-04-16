//! Integration tests for the user-facing agent and route-snapshot endpoints:
//!
//! - `GET /api/agents` — list all agents from the registry snapshot
//! - `GET /api/agents/{id}` — single agent detail
//! - `GET /api/paths/{src}/{tgt}/routes/latest` — latest route snapshot
//! - `GET /api/paths/{src}/{tgt}/routes/{id}` — snapshot by id
//! - `GET /api/paths/{src}/{tgt}/routes` — paginated route list
//! - `GET /api/metrics/query` — PromQL instant query proxy
//! - `GET /api/metrics/query_range` — PromQL range query proxy
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
//! | `.130` | `alerts_proxy_forwards_active_alerts_and_filters_unused_…`  |
//! | `.131` | `alerts_proxy_returns_502_when_upstream_fails`              |
//! | `.132` | `alerts_proxy_single_returns_404_when_fingerprint_missing`  |
//! | `.133` | `alerts_proxy_returns_503_when_alertmanager_not_configured` |
//! | `.140` | `metrics_proxy_rejects_non_meshmon_query`                   |
//! | `.141` | `metrics_proxy_forwards_meshmon_query_to_vm`                |
//! | `.142` | `metrics_proxy_range_forwards_all_params`                   |
//! | `.143` | `metrics_proxy_returns_503_when_vm_not_configured`          |
//! | `.144` | `alerts_proxy_forwards_query_params_to_upstream`            |

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
    let state = common::state_with_admin(pool).await;
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
    let state = common::state_with_admin(pool).await;
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
    let state = common::state_with_admin(pool).await;
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
        "INSERT INTO agents (id, display_name, location, ip, lat, lon, tcp_probe_port, udp_probe_port, agent_version)
         VALUES ('brazil-north', 'Fortaleza', 'BR', '170.80.110.90', -3.7, -38.5, 3555, 3552, 'v0.1.0')",
    )
    .execute(&pool)
    .await
    .expect("seed agent row");

    let state = common::state_with_admin(pool.clone()).await;

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
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ($1, $1, '10.0.0.1', 3555, 3552) ON CONFLICT (id) DO NOTHING",
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

    let state = common::state_with_admin(pool.clone()).await;
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

    let state = common::state_with_admin(pool.clone()).await;
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
    let state = common::state_with_admin(pool).await;
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

    let state = common::state_with_admin(pool.clone()).await;
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
    let state = common::state_with_admin(pool).await;
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
    let state = common::state_with_admin(pool).await;
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
    let state = common::state_with_admin(pool).await;
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

// ---------------------------------------------------------------------------
// Task 8: Alertmanager proxy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alerts_proxy_forwards_active_alerts_and_filters_unused_fields() {
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Alertmanager v2 JSON array with all fields (including generatorURL
    // which should be dropped by the proxy normalization).
    let am_response = serde_json::json!([
        {
            "fingerprint": "abc123",
            "labels": {
                "alertname": "HighLatency",
                "severity": "critical"
            },
            "annotations": {
                "summary": "Latency is high",
                "description": "p99 latency exceeds 500ms"
            },
            "status": {
                "state": "active",
                "silencedBy": [],
                "inhibitedBy": []
            },
            "startsAt": "2025-01-15T10:00:00Z",
            "endsAt": "0001-01-01T00:00:00Z",
            "generatorURL": "http://prometheus:9090/graph?g0.expr=high_latency",
            "receivers": [{"name": "default"}],
            "updatedAt": "2025-01-15T10:05:00Z"
        }
    ]);

    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v2/alerts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&am_response))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &mock_server.uri()).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.130").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/alerts")
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

    let alerts = body.as_array().expect("expected JSON array");
    assert_eq!(alerts.len(), 1);

    let alert = &alerts[0];
    // Normalized fields present.
    assert_eq!(alert["fingerprint"], "abc123");
    assert_eq!(alert["labels"]["alertname"], "HighLatency");
    assert_eq!(alert["labels"]["severity"], "critical");
    assert_eq!(alert["summary"], "Latency is high");
    assert_eq!(alert["description"], "p99 latency exceeds 500ms");
    assert_eq!(alert["state"], "active");
    assert_eq!(alert["starts_at"], "2025-01-15T10:00:00Z");
    assert_eq!(alert["ends_at"], "0001-01-01T00:00:00Z");

    // Upstream-only fields should NOT be present.
    assert!(
        alert.get("generatorURL").is_none(),
        "generatorURL should be dropped: {alert}"
    );
    assert!(
        alert.get("receivers").is_none(),
        "receivers should be dropped: {alert}"
    );
    assert!(
        alert.get("updatedAt").is_none(),
        "updatedAt should be dropped: {alert}"
    );
}

#[tokio::test]
async fn alerts_proxy_forwards_query_params_to_upstream() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Wiremock asserts every query_param matcher is satisfied, proving the
    // proxy forwards the user-supplied query string to the upstream
    // Alertmanager verbatim.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v2/alerts"))
        .and(query_param("active", "true"))
        .and(query_param("silenced", "false"))
        .and(query_param("filter", "alertname=\"Foo\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &mock_server.uri()).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.144").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/alerts?active=true&silenced=false&filter=alertname%3D%22Foo%22")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn alerts_proxy_returns_502_when_upstream_fails() {
    let pool = common::shared_migrated_pool().await.clone();
    // Point at a closed port — reqwest will fail to connect.
    let state = common::state_with_admin_and_alertmanager(pool, "http://127.0.0.1:1").await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.131").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/alerts")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["error"], "upstream request failed",
        "expected generic error, got: {body}"
    );
}

#[tokio::test]
async fn alerts_proxy_single_returns_404_when_fingerprint_missing() {
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Return empty array — no alerts match.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v2/alerts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_alertmanager(pool, &mock_server.uri()).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.132").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/alerts/nonexistent")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "alert not found");
}

#[tokio::test]
async fn alerts_proxy_returns_503_when_alertmanager_not_configured() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await; // no alertmanager_url
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.133").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/alerts")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ---------------------------------------------------------------------------
// Task 9: VM PromQL proxy with meshmon_ prefix whitelist
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_proxy_rejects_non_meshmon_query() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_vm(pool, "http://127.0.0.1:1").await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.140").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics/query?query=up")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["error"], "query must start with a meshmon_ metric name",
        "body = {body}"
    );
}

#[tokio::test]
async fn metrics_proxy_forwards_meshmon_query_to_vm() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    let vm_response = serde_json::json!({
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": []
        }
    });

    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query"))
        .and(query_param("query", "meshmon_path_rtt_avg_micros"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&vm_response))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_vm(pool, &mock_server.uri()).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.141").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics/query?query=meshmon_path_rtt_avg_micros")
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
    assert_eq!(body["status"], "success", "body = {body}");
    assert_eq!(body["data"]["resultType"], "vector", "body = {body}");
}

#[tokio::test]
async fn metrics_proxy_range_forwards_all_params() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    let vm_response = serde_json::json!({
        "status": "success",
        "data": {
            "resultType": "matrix",
            "result": []
        }
    });

    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param("query", "meshmon_path_rtt_avg_micros"))
        .and(query_param("start", "1700000000"))
        .and(query_param("end", "1700003600"))
        .and(query_param("step", "15s"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&vm_response))
        .expect(1)
        .mount(&mock_server)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin_and_vm(pool, &mock_server.uri()).await;
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.142").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics/query_range?query=meshmon_path_rtt_avg_micros&start=1700000000&end=1700003600&step=15s")
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
    assert_eq!(body["status"], "success", "body = {body}");
    assert_eq!(body["data"]["resultType"], "matrix", "body = {body}");
}

#[tokio::test]
async fn metrics_proxy_returns_503_when_vm_not_configured() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await; // no vm_url
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.143").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics/query?query=meshmon_foo")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
