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
//! | `.150` | `recent_routes_returns_latest_across_pairs`                 |
//! | `.151` | `recent_routes_respects_limit_cap`                          |
//! | `.152` | `recent_routes_rejects_invalid_limit`                       |
//! | `.160` | `agents_list_carries_catalogue_joined_fields`               |
//! | `.161` | `agents_list_stamps_hostname_from_positive_cache`           |
//! | `.162` | `agents_list_omits_hostname_on_negative_cache`              |
//! | `.163` | `agents_list_cold_cache_miss_enqueues_resolver`             |
//! | `.164` | `agent_detail_stamps_hostname_from_positive_cache`          |
//! | `.165` | `routes_latest_stamps_positive_hop_hostname`                |
//! | `.166` | `routes_latest_omits_hop_hostname_on_negative_cache`        |
//! | `.167` | `routes_latest_cold_hop_miss_enqueues_resolver`             |
//! | `.168` | `routes_by_id_stamps_positive_hop_hostname`                 |
//! | `.169` | `routes_by_id_omits_hop_hostname_on_negative_cache`         |
//! | `.170` | `routes_by_id_cold_hop_miss_enqueues_resolver`              |

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

    // Seed an agent row directly in the DB. Geo lives on ip_catalogue; the
    // LEFT JOIN in `registry::refresh_once` picks it up by matching `ip`.
    sqlx::query(
        "INSERT INTO agents (id, display_name, location, ip, tcp_probe_port, udp_probe_port, agent_version)
         VALUES ('brazil-north', 'Fortaleza', 'BR', '170.80.110.90', 8002, 8005, 'v0.1.0')",
    )
    .execute(&pool)
    .await
    .expect("seed agent row");
    sqlx::query(
        "INSERT INTO ip_catalogue (ip, source, latitude, longitude, operator_edited_fields)
         VALUES ('170.80.110.90'::inet, 'agent_registration', -3.7, -38.5,
                 ARRAY['Latitude','Longitude']::text[])
         ON CONFLICT (ip) DO UPDATE SET latitude = EXCLUDED.latitude,
                                        longitude = EXCLUDED.longitude",
    )
    .execute(&pool)
    .await
    .expect("seed catalogue row");

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
    assert_eq!(
        body["catalogue_coordinates"]["latitude"], -3.7,
        "body = {body}"
    );
    assert_eq!(
        body["catalogue_coordinates"]["longitude"], -38.5,
        "body = {body}"
    );
    assert_eq!(body["agent_version"], "v0.1.0", "body = {body}");
    assert!(body["registered_at"].is_string(), "body = {body}");
    assert!(body["last_seen_at"].is_string(), "body = {body}");

    // Cleanup: remove the seeded rows so other tests aren't affected.
    sqlx::query("DELETE FROM agents WHERE id = 'brazil-north'")
        .execute(&pool)
        .await
        .expect("cleanup agent row");
    sqlx::query("DELETE FROM ip_catalogue WHERE ip = '170.80.110.90'::inet")
        .execute(&pool)
        .await
        .expect("cleanup catalogue row");
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
             VALUES ($1, $1, '10.0.0.1', 8002, 8005) ON CONFLICT (id) DO NOTHING",
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
        body["error"], "query must reference at least one meshmon_ metric",
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

// ---------------------------------------------------------------------------
// T18: GET /api/routes/recent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recent_routes_returns_latest_across_pairs() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.150").await;

    // Test-unique IDs so we can filter our rows out of the shared pool's
    // route_snapshots table (other integration tests also seed this table).
    let src_a = "t18recent-src-a";
    let tgt_a = "t18recent-tgt-a";
    let src_b = "t18recent-src-b";
    let tgt_b = "t18recent-tgt-b";

    // Two source/target pairs, three snapshots total.
    // Pair A: one old + one new. Pair B: one mid.
    let now = chrono::Utc::now();
    let rows: [(&str, &str, &str, chrono::DateTime<chrono::Utc>); 3] = [
        (src_a, tgt_a, "icmp", now - chrono::Duration::minutes(60)),
        (src_a, tgt_a, "icmp", now - chrono::Duration::minutes(1)),
        (src_b, tgt_b, "icmp", now - chrono::Duration::minutes(30)),
    ];
    for (src, tgt, proto, ts) in rows {
        // Register the agents first so the FK holds.
        common::insert_agent(&pool, src).await;
        common::insert_agent(&pool, tgt).await;
        sqlx::query(
            "INSERT INTO route_snapshots \
             (source_id, target_id, protocol, observed_at, hops, path_summary) \
             VALUES ($1, $2, $3, $4, '[]'::jsonb, NULL)",
        )
        .bind(src)
        .bind(tgt)
        .bind(proto)
        .bind(ts)
        .execute(&pool)
        .await
        .expect("insert snapshot");
    }

    // Request enough rows to survive shared-pool pollution — other tests may
    // insert their own "latest" pairs that rank ahead of ours.
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/routes/recent?limit=100")
                .header(axum::http::header::COOKIE, &cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let items = body.as_array().expect("array");

    // DISTINCT ON globally: no (source_id, target_id) pair repeats in the response,
    // regardless of which test inserted the row.
    let mut seen_pairs: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for item in items {
        let src = item["source_id"].as_str().unwrap().to_string();
        let tgt = item["target_id"].as_str().unwrap().to_string();
        assert!(
            seen_pairs.insert((src.clone(), tgt.clone())),
            "duplicate pair ({src}, {tgt}) — DISTINCT ON not working"
        );
    }

    // observed_at must globally descend across all items — the ORDER BY contract
    // must hold regardless of how many foreign rows sit between ours.
    for w in items.windows(2) {
        let a = w[0]["observed_at"].as_str().unwrap();
        let b = w[1]["observed_at"].as_str().unwrap();
        assert!(a >= b, "expected descending: {a} >= {b}");
    }

    // Everything below is about OUR seeded rows — filter the response so foreign
    // rows from other tests in the same binary can't flake the assertions.
    let mine: Vec<&serde_json::Value> = items
        .iter()
        .filter(|i| {
            let s = i["source_id"].as_str().unwrap_or("");
            s == src_a || s == src_b
        })
        .collect();

    // DISTINCT ON must collapse src-a's two snapshots to one row, plus one for src-b.
    assert_eq!(
        mine.len(),
        2,
        "expected exactly 2 rows for our pairs, got {}: {mine:#?}",
        mine.len()
    );

    // Within our rows: src-a (1m ago) is newer than src-b (30m ago).
    assert_eq!(
        mine[0]["source_id"], src_a,
        "src-a (1m ago) must rank first among our rows"
    );
    assert_eq!(mine[0]["target_id"], tgt_a);
    assert_eq!(mine[1]["source_id"], src_b);
    assert_eq!(mine[1]["target_id"], tgt_b);

    // Cleanup seeded rows.
    sqlx::query("DELETE FROM route_snapshots WHERE source_id LIKE 't18recent-%'")
        .execute(&pool)
        .await
        .expect("cleanup snapshots");
    sqlx::query("DELETE FROM agents WHERE id LIKE 't18recent-%'")
        .execute(&pool)
        .await
        .expect("cleanup agents");
}

#[tokio::test]
async fn recent_routes_respects_limit_cap() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.151").await;

    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/routes/recent?limit=500")
                .header(axum::http::header::COOKIE, &cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("limit must be between 1 and 100"),
        "expected error mentioning 'limit must be between 1 and 100', got: {body}"
    );
}

#[tokio::test]
async fn recent_routes_rejects_invalid_limit() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.152").await;

    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/routes/recent?limit=0")
                .header(axum::http::header::COOKIE, &cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("limit must be between 1 and 100"),
        "expected error mentioning 'limit must be between 1 and 100', got: {body}"
    );
}

#[tokio::test]
async fn recent_routes_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/routes/recent")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Catalogue-joined fields + hostname stamping on /api/agents[/:id]
// ---------------------------------------------------------------------------

/// Per-test seed fields. Grouped in a struct so the helper doesn't trip
/// clippy's `too_many_arguments` lint and each call site names the
/// fields it cares about.
struct AgentCatalogueSeed<'a> {
    agent_id: &'a str,
    ip: &'a str,
    city: &'a str,
    country_code: &'a str,
    country_name: &'a str,
    asn: i32,
    network_operator: &'a str,
}

/// Seed an agent row plus its `ip_catalogue` join row with the given
/// city / country / ASN / network_operator fields populated.
///
/// Shared DB isolation: the helper uses the caller-supplied id (unique
/// per test) and its IP (unique per test) so parallel tests do not
/// collide. Each test pairs this with [`cleanup_agent_with_catalogue`]
/// on its success path.
async fn seed_agent_with_catalogue(pool: &sqlx::PgPool, seed: &AgentCatalogueSeed<'_>) {
    sqlx::query(
        "INSERT INTO agents (id, display_name, location, ip, tcp_probe_port, udp_probe_port, agent_version) \
         VALUES ($1, $1, 'TEST', $2::inet, 8002, 8005, 'v0.1.0')",
    )
    .bind(seed.agent_id)
    .bind(seed.ip)
    .execute(pool)
    .await
    .expect("seed agent row");

    sqlx::query(
        "INSERT INTO ip_catalogue (ip, source, city, country_code, country_name, asn, network_operator, \
                                   operator_edited_fields) \
         VALUES ($1::inet, 'agent_registration', $2, $3, $4, $5, $6, \
                 ARRAY['City','CountryCode','CountryName','Asn','NetworkOperator']::text[]) \
         ON CONFLICT (ip) DO UPDATE SET city = EXCLUDED.city, \
                                        country_code = EXCLUDED.country_code, \
                                        country_name = EXCLUDED.country_name, \
                                        asn = EXCLUDED.asn, \
                                        network_operator = EXCLUDED.network_operator",
    )
    .bind(seed.ip)
    .bind(seed.city)
    .bind(seed.country_code)
    .bind(seed.country_name)
    .bind(seed.asn)
    .bind(seed.network_operator)
    .execute(pool)
    .await
    .expect("seed catalogue row");
}

/// Drop the rows created by [`seed_agent_with_catalogue`]. Called on
/// every test success path so leakage across the shared test DB stays
/// bounded.
async fn cleanup_agent_with_catalogue(pool: &sqlx::PgPool, agent_id: &str, ip: &str) {
    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .expect("cleanup agent row");
    sqlx::query("DELETE FROM ip_catalogue WHERE ip = $1::inet")
        .bind(ip)
        .execute(pool)
        .await
        .expect("cleanup catalogue row");
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(ip)
        .execute(pool)
        .await
        .expect("cleanup hostname cache");
}

/// Pull `/api/agents` through the real router, locate the row whose `id`
/// matches the per-test seed, and return the row as a `serde_json::Value`.
///
/// Panics if the row is missing from the list — every test that calls
/// this helper first calls `seed_agent_with_catalogue` and `force_refresh`
/// so the row must appear in the snapshot.
async fn fetch_agent_row(app: &axum::Router, cookie: &str, agent_id: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/agents")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let agents = list.as_array().expect("agents list is an array");
    agents
        .iter()
        .find(|a| a["id"].as_str() == Some(agent_id))
        .cloned()
        .unwrap_or_else(|| panic!("agent {agent_id} not found in /api/agents response: {list}"))
}

#[tokio::test]
async fn agents_list_carries_catalogue_joined_fields() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = "t53b-catfields";
    let ip = "170.80.160.90";

    seed_agent_with_catalogue(
        &pool,
        &AgentCatalogueSeed {
            agent_id,
            ip,
            city: "Fortaleza",
            country_code: "BR",
            country_name: "Brazil",
            asn: 64512,
            network_operator: "AS Example",
        },
    )
    .await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.160").await;
    let row = fetch_agent_row(&app, &cookie, agent_id).await;

    assert_eq!(row["city"], "Fortaleza", "row = {row}");
    assert_eq!(row["country_code"], "BR", "row = {row}");
    assert_eq!(row["country_name"], "Brazil", "row = {row}");
    assert_eq!(row["asn"], 64512, "row = {row}");
    assert_eq!(row["network_operator"], "AS Example", "row = {row}");

    cleanup_agent_with_catalogue(&pool, agent_id, ip).await;
}

#[tokio::test]
async fn agents_list_stamps_hostname_from_positive_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = "t53b-host-pos";
    let ip = "170.80.161.90";

    seed_agent_with_catalogue(
        &pool,
        &AgentCatalogueSeed {
            agent_id,
            ip,
            city: "Sao Paulo",
            country_code: "BR",
            country_name: "Brazil",
            asn: 64513,
            network_operator: "AS Example",
        },
    )
    .await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.161").await;

    let ip_addr: std::net::IpAddr = ip.parse().unwrap();
    common::seed_hostname_positive(&pool, ip_addr, "agent-161.example.com").await;

    let row = fetch_agent_row(&app, &cookie, agent_id).await;
    assert_eq!(
        row["hostname"].as_str(),
        Some("agent-161.example.com"),
        "list must stamp hostname from positive cache; row = {row}",
    );

    cleanup_agent_with_catalogue(&pool, agent_id, ip).await;
}

#[tokio::test]
async fn agents_list_omits_hostname_on_negative_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = "t53b-host-neg";
    let ip = "170.80.162.90";

    seed_agent_with_catalogue(
        &pool,
        &AgentCatalogueSeed {
            agent_id,
            ip,
            city: "Berlin",
            country_code: "DE",
            country_name: "Germany",
            asn: 64514,
            network_operator: "AS Example",
        },
    )
    .await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.162").await;

    let ip_addr: std::net::IpAddr = ip.parse().unwrap();
    common::seed_hostname_negative(&pool, ip_addr).await;

    let row = fetch_agent_row(&app, &cookie, agent_id).await;
    assert!(
        row.get("hostname").is_none(),
        "negative cache hit must omit hostname; row = {row}",
    );

    cleanup_agent_with_catalogue(&pool, agent_id, ip).await;
}

#[tokio::test]
async fn agents_list_cold_cache_miss_enqueues_resolver() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = "t53b-host-cold";
    let ip = "170.80.163.90";

    seed_agent_with_catalogue(
        &pool,
        &AgentCatalogueSeed {
            agent_id,
            ip,
            city: "Paris",
            country_code: "FR",
            country_name: "France",
            asn: 64515,
            network_operator: "AS Example",
        },
    )
    .await;

    // Drain any prior cache state for this IP so the handler sees a true
    // cold miss and issues an enqueue to the resolver.
    let ip_addr: std::net::IpAddr = ip.parse().unwrap();
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(ip)
        .execute(&pool)
        .await
        .expect("clear cache");

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.163").await;

    let row = fetch_agent_row(&app, &cookie, agent_id).await;
    assert!(
        row.get("hostname").is_none(),
        "cold miss must omit hostname; row = {row}",
    );

    // The stub backend answers every unseeded IP with `NegativeNxDomain`,
    // so a successful enqueue writes a negative row we can observe. The
    // stub is the `test_hostname_fixtures` default — see `StubHostnameBackend`.
    assert!(
        common::wait_for_cache_row(&pool, ip_addr).await,
        "resolver never wrote a cache row for {ip_addr} — enqueue was skipped",
    );

    cleanup_agent_with_catalogue(&pool, agent_id, ip).await;
}

#[tokio::test]
async fn agent_detail_stamps_hostname_from_positive_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = "t53b-detail-pos";
    let ip = "170.80.164.90";

    seed_agent_with_catalogue(
        &pool,
        &AgentCatalogueSeed {
            agent_id,
            ip,
            city: "Warsaw",
            country_code: "PL",
            country_name: "Poland",
            asn: 64516,
            network_operator: "AS Example",
        },
    )
    .await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.164").await;

    let ip_addr: std::net::IpAddr = ip.parse().unwrap();
    common::seed_hostname_positive(&pool, ip_addr, "agent-164.example.com").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/agents/{agent_id}"))
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

    assert_eq!(body["id"], agent_id, "body = {body}");
    assert_eq!(
        body["hostname"].as_str(),
        Some("agent-164.example.com"),
        "detail must stamp hostname from positive cache; body = {body}",
    );
    // Catalogue-joined fields also reach the detail path.
    assert_eq!(body["city"], "Warsaw", "body = {body}");
    assert_eq!(body["country_code"], "PL", "body = {body}");
    assert_eq!(body["asn"], 64516, "body = {body}");
    assert_eq!(body["network_operator"], "AS Example", "body = {body}");

    cleanup_agent_with_catalogue(&pool, agent_id, ip).await;
}

// ---------------------------------------------------------------------------
// T53c I1: hop-hostname stamping on route-snapshot detail endpoints
// ---------------------------------------------------------------------------

/// Three IPs used across the hop-hostname tests. Each IP plays a different
/// role in the three-state coverage:
///
/// - `HOP_IP_POS` — seeded in `ip_hostname_cache` as a positive hit before
///   the handler runs.
/// - `HOP_IP_NEG` — seeded in `ip_hostname_cache` as a negative hit before
///   the handler runs; the `hostname` field must be absent in the response.
/// - `HOP_IP_COLD` — not seeded; the handler must enqueue it for background
///   resolution and leave `hostname` absent in the response.
const HOP_IP_POS: &str = "10.53.1.1";
const HOP_IP_NEG: &str = "10.53.1.2";
const HOP_IP_COLD: &str = "10.53.1.3";
const HOP_HOSTNAME_POS: &str = "hop-pos.example.com";

/// Seed a single route-snapshot row with three hops, each carrying one of the
/// three test IPs above. Also ensures the required agent rows exist.
async fn seed_three_hop_snapshot(pool: &sqlx::PgPool, src: &str, tgt: &str, protocol: &str) {
    for id in [src, tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ($1, $1, '10.0.0.1', 8002, 8005) ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
    let hops = serde_json::json!([
        {
            "position": 1,
            "observed_ips": [{"ip": HOP_IP_POS, "freq": 1.0}],
            "avg_rtt_micros": 1000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 2,
            "observed_ips": [{"ip": HOP_IP_NEG, "freq": 1.0}],
            "avg_rtt_micros": 2000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 3,
            "observed_ips": [{"ip": HOP_IP_COLD, "freq": 1.0}],
            "avg_rtt_micros": 3000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        }
    ]);
    let summary = serde_json::json!({
        "avg_rtt_micros": 2000,
        "loss_pct": 0.0,
        "hop_count": 3
    });
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

/// Delete route-snapshots + agents seeded for hop-hostname tests.
async fn cleanup_hop_snapshots(pool: &sqlx::PgPool, src: &str, tgt: &str) {
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

/// Seed the hostname cache into the three-state configuration used by all
/// hop-hostname tests: positive for `HOP_IP_POS`, negative for `HOP_IP_NEG`,
/// nothing for `HOP_IP_COLD`.
async fn seed_hop_hostname_cache(pool: &sqlx::PgPool) {
    let pos_ip: std::net::IpAddr = HOP_IP_POS.parse().unwrap();
    let neg_ip: std::net::IpAddr = HOP_IP_NEG.parse().unwrap();
    common::seed_hostname_positive(pool, pos_ip, HOP_HOSTNAME_POS).await;
    common::seed_hostname_negative(pool, neg_ip).await;
    // HOP_IP_COLD: ensure no stale cache row exists so the handler sees a
    // genuine cold miss.
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(HOP_IP_COLD)
        .execute(pool)
        .await
        .expect("clear cold-miss cache");
}

// ---------------------------------------------------------------------------
// GET /api/paths/{src}/{tgt}/routes/latest — hop hostname three-state tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_latest_stamps_positive_hop_hostname() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-latest-pos-src", "t53c-hop-latest-pos-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.165").await;

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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Hop 0 (position 1) must carry a positive hostname.
    let hop0_ip0 = &body["hops"][0]["observed_ips"][0];
    assert_eq!(
        hop0_ip0["ip"].as_str(),
        Some(HOP_IP_POS),
        "hop 0 ip mismatch: {body}"
    );
    assert_eq!(
        hop0_ip0["hostname"].as_str(),
        Some(HOP_HOSTNAME_POS),
        "hop 0 must have positive hostname: {body}"
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_latest_omits_hop_hostname_on_negative_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-latest-neg-src", "t53c-hop-latest-neg-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.166").await;

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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Hop 1 (position 2) must NOT have a hostname key (negative cache hit).
    let hop1_ip0 = &body["hops"][1]["observed_ips"][0];
    assert_eq!(
        hop1_ip0["ip"].as_str(),
        Some(HOP_IP_NEG),
        "hop 1 ip mismatch: {body}"
    );
    assert!(
        hop1_ip0.get("hostname").is_none(),
        "negative cache hit must omit hostname: {body}"
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_latest_cold_hop_miss_enqueues_resolver() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-latest-cold-src", "t53c-hop-latest-cold-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let cold_ip: std::net::IpAddr = HOP_IP_COLD.parse().unwrap();

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.167").await;

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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Hop 2 (position 3) must NOT have a hostname key (cold miss).
    let hop2_ip0 = &body["hops"][2]["observed_ips"][0];
    assert_eq!(
        hop2_ip0["ip"].as_str(),
        Some(HOP_IP_COLD),
        "hop 2 ip mismatch: {body}"
    );
    assert!(
        hop2_ip0.get("hostname").is_none(),
        "cold miss must omit hostname: {body}"
    );

    // The stub backend answers every unseeded IP with NegativeNxDomain so a
    // processed enqueue writes a negative row we can observe.
    assert!(
        common::wait_for_cache_row(&pool, cold_ip).await,
        "resolver never wrote a cache row for {cold_ip} — enqueue was skipped",
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// GET /api/paths/{src}/{tgt}/routes/{id} — hop hostname three-state tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_by_id_stamps_positive_hop_hostname() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-byid-pos-src", "t53c-hop-byid-pos-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let snapshot_id: i64 = sqlx::query_scalar(
        "SELECT id FROM route_snapshots WHERE source_id = $1 AND target_id = $2 LIMIT 1",
    )
    .bind(src)
    .bind(tgt)
    .fetch_one(&pool)
    .await
    .expect("fetch snapshot id");

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.168").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes/{snapshot_id}");
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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let hop0_ip0 = &body["hops"][0]["observed_ips"][0];
    assert_eq!(
        hop0_ip0["ip"].as_str(),
        Some(HOP_IP_POS),
        "hop 0 ip mismatch: {body}"
    );
    assert_eq!(
        hop0_ip0["hostname"].as_str(),
        Some(HOP_HOSTNAME_POS),
        "hop 0 must have positive hostname: {body}"
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_by_id_omits_hop_hostname_on_negative_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-byid-neg-src", "t53c-hop-byid-neg-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let snapshot_id: i64 = sqlx::query_scalar(
        "SELECT id FROM route_snapshots WHERE source_id = $1 AND target_id = $2 LIMIT 1",
    )
    .bind(src)
    .bind(tgt)
    .fetch_one(&pool)
    .await
    .expect("fetch snapshot id");

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.169").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes/{snapshot_id}");
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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let hop1_ip0 = &body["hops"][1]["observed_ips"][0];
    assert_eq!(
        hop1_ip0["ip"].as_str(),
        Some(HOP_IP_NEG),
        "hop 1 ip mismatch: {body}"
    );
    assert!(
        hop1_ip0.get("hostname").is_none(),
        "negative cache hit must omit hostname: {body}"
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}

#[tokio::test]
async fn routes_by_id_cold_hop_miss_enqueues_resolver() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-hop-byid-cold-src", "t53c-hop-byid-cold-tgt");
    seed_three_hop_snapshot(&pool, src, tgt, "icmp").await;
    seed_hop_hostname_cache(&pool).await;

    let cold_ip: std::net::IpAddr = HOP_IP_COLD.parse().unwrap();

    let snapshot_id: i64 = sqlx::query_scalar(
        "SELECT id FROM route_snapshots WHERE source_id = $1 AND target_id = $2 LIMIT 1",
    )
    .bind(src)
    .bind(tgt)
    .fetch_one(&pool)
    .await
    .expect("fetch snapshot id");

    let state = common::state_with_admin(pool.clone()).await;
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.170").await;

    let uri = format!("/api/paths/{src}/{tgt}/routes/{snapshot_id}");
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
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let hop2_ip0 = &body["hops"][2]["observed_ips"][0];
    assert_eq!(
        hop2_ip0["ip"].as_str(),
        Some(HOP_IP_COLD),
        "hop 2 ip mismatch: {body}"
    );
    assert!(
        hop2_ip0.get("hostname").is_none(),
        "cold miss must omit hostname: {body}"
    );

    assert!(
        common::wait_for_cache_row(&pool, cold_ip).await,
        "resolver never wrote a cache row for {cold_ip} — enqueue was skipped",
    );

    cleanup_hop_snapshots(&pool, src, tgt).await;
}
