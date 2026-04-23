//! Integration tests for `GET /api/paths/{src}/{tgt}/overview` (T19 Task 2).
//!
//! The overview endpoint aggregates what the Path Detail page needs:
//! - source/target agent metadata,
//! - the latest route snapshot per protocol in the window,
//! - a recent-snapshots list (capped at 100),
//! - VM-sourced RTT avg + failure-rate series at a server-chosen step,
//! - a server-picked primary protocol (auto: `icmp > udp > tcp`, or the
//!   client-supplied `?protocol=` override).
//!
//! `X-Forwarded-For` IP allocations for this binary (`.160`–`.176`):
//!
//! | Octet  | Test                                                        |
//! |--------|-------------------------------------------------------------|
//! | `.160` | `overview_happy_path_returns_latest_recent_and_metrics`     |
//! | `.161` | `overview_returns_404_when_source_missing`                  |
//! | `.162` | `overview_returns_404_when_target_missing`                  |
//! | `.163` | `overview_rejects_invalid_protocol_override`                |
//! | `.164` | `overview_returns_metrics_null_when_vm_unreachable`         |
//! | `.165` | `overview_honours_protocol_override`                        |
//! | `.166` | `overview_auto_picks_udp_over_tcp_when_icmp_absent`         |
//! | `.167` | `overview_auto_picks_tcp_when_only_tcp_present`             |
//! | `.168` | `overview_step_is_1m_for_24h_window`                        |
//! | `.169` | `overview_step_is_5m_for_7d_window`                         |
//! | `.170` | `overview_requires_session`                                 |
//! | `.171` | `overview_sets_truncated_flag_when_recent_hits_limit`       |
//! | `.172` | `overview_truncated_flag_is_false_when_below_limit`         |
//! | `.173` | `overview_primary_protocol_is_null_when_no_snapshots`       |
//! | `.174` | `overview_recent_filtered_by_auto_primary`                  |
//! | `.175` | `overview_truncated_false_when_filtered_pool_below_limit`   |
//! | `.176` | `overview_truncated_true_when_filtered_pool_exceeds_limit`  |
//! | `.177` | `overview_stamps_positive_hop_hostname`                     |
//! | `.178` | `overview_omits_hop_hostname_on_negative_cache`             |
//! | `.179` | `overview_cold_hop_miss_enqueues_resolver`                  |

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use sqlx::PgPool;
use tower::util::ServiceExt;

mod common;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert an agents row with rich metadata (display name, version) plus a
/// matching `ip_catalogue` entry carrying the lat/lon (catalogue is the
/// single authority for geo; the `agents` table no longer stores it).
async fn insert_agent_detailed(
    pool: &PgPool,
    id: &str,
    display_name: &str,
    location: &str,
    ip: &str,
    lat: f64,
    lon: f64,
) {
    sqlx::query(
        "INSERT INTO agents (id, display_name, location, ip, tcp_probe_port, udp_probe_port, agent_version) \
         VALUES ($1, $2, $3, $4::inet, 8002, 8005, 'v0.1.0') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(display_name)
    .bind(location)
    .bind(ip)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("insert_agent_detailed({id}) failed: {e}"));

    sqlx::query(
        "INSERT INTO ip_catalogue (ip, source, latitude, longitude, operator_edited_fields) \
         VALUES ($1::inet, 'agent_registration', $2, $3, ARRAY['Latitude','Longitude']::text[]) \
         ON CONFLICT (ip) DO UPDATE SET latitude = EXCLUDED.latitude, \
                                        longitude = EXCLUDED.longitude",
    )
    .bind(ip)
    .bind(lat)
    .bind(lon)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("insert_agent_detailed catalogue({id}) failed: {e}"));
}

/// Insert a single route_snapshot row at the supplied timestamp.
async fn insert_snapshot(
    pool: &PgPool,
    src: &str,
    tgt: &str,
    protocol: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) {
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
    sqlx::query(
        "INSERT INTO route_snapshots \
         (source_id, target_id, protocol, observed_at, hops, path_summary) \
         VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb)",
    )
    .bind(src)
    .bind(tgt)
    .bind(protocol)
    .bind(observed_at)
    .bind(&hops)
    .bind(&summary)
    .execute(pool)
    .await
    .unwrap();
}

/// DELETE rows for the seeded (src, tgt) pair so shared-pool tests stay clean.
async fn cleanup_pair(pool: &PgPool, src: &str, tgt: &str) {
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

/// Two-series matrix response shaped like VictoriaMetrics returns.
fn vm_matrix_body(values: &[(i64, &str)]) -> serde_json::Value {
    serde_json::json!({
        "status": "success",
        "data": {
            "resultType": "matrix",
            "result": [{
                "metric": {},
                "values": values
                    .iter()
                    .map(|(ts, v)| serde_json::json!([*ts, *v]))
                    .collect::<Vec<_>>()
            }]
        }
    })
}

/// Parse a response body as JSON. Tiny shim to keep the assertions below
/// readable.
fn parse_body(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("parse JSON body")
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_happy_path_returns_latest_recent_and_metrics() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;

    // Two distinct mocks so RTT and loss use semantically-correct sample
    // shapes: RTT returns millisecond-scale values (already divided by
    // 1000 in MetricsQL), loss returns [0, 1] fractions. The mocks are
    // keyed on the metric name in the `query` param so a bug in either
    // wiring would mean the wrong mock fires (and `.expect(1)` blows up).
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param_contains("query", "rtt_avg_micros"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_matrix_body(&[
            (1_700_000_000, "12.5"),
            (1_700_000_060, "13.75"),
        ])))
        .expect(1)
        .mount(&mock)
        .await;
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param_contains("query", "failure_rate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_matrix_body(&[
            (1_700_000_000, "0.02"),
            (1_700_000_060, "0.05"),
        ])))
        .expect(1)
        .mount(&mock)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-src", "t19-ov-tgt");

    insert_agent_detailed(&pool, src, "Source Node", "US", "10.0.0.1", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target Node", "DE", "10.0.0.2", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    // Two ICMP snapshots (t-1h older, t-1min newer) and one TCP (t-5min).
    insert_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::hours(1)).await;
    insert_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(1)).await;
    insert_snapshot(&pool, src, tgt, "tcp", now - chrono::Duration::minutes(5)).await;

    let state = common::state_with_admin_and_vm(pool.clone(), &mock.uri()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.160").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);

    // Source + target metadata
    assert_eq!(body["source"]["id"], src, "body = {body}");
    assert_eq!(body["source"]["display_name"], "Source Node");
    assert_eq!(body["target"]["id"], tgt, "body = {body}");
    assert_eq!(body["target"]["display_name"], "Target Node");

    // Primary protocol: ICMP beats TCP when both present.
    assert_eq!(body["primary_protocol"], "icmp", "body = {body}");

    // latest_by_protocol must expose only ICMP and TCP; UDP is absent.
    assert!(body["latest_by_protocol"]["icmp"].is_object());
    assert!(body["latest_by_protocol"]["tcp"].is_object());
    assert!(body["latest_by_protocol"]["udp"].is_null());

    // Latest ICMP row must be the t-1min one (newer), not the t-1h one.
    let icmp_latest_ts = body["latest_by_protocol"]["icmp"]["observed_at"]
        .as_str()
        .expect("observed_at");
    let one_hour_ago = now - chrono::Duration::minutes(10); // well after t-1h
    assert!(
        icmp_latest_ts > one_hour_ago.to_rfc3339().as_str(),
        "latest ICMP should be newer than t-10min ({icmp_latest_ts})"
    );

    // recent_snapshots: only the two ICMP rows in descending time order
    // (ICMP is primary, TCP is filtered out post-T32). Cross-protocol
    // leakage would have kept the TCP row in the list.
    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 2, "body = {body}");
    for row in recent {
        assert_eq!(row["protocol"], "icmp", "row = {row}");
    }
    for w in recent.windows(2) {
        let a = w[0]["observed_at"].as_str().unwrap();
        let b = w[1]["observed_at"].as_str().unwrap();
        assert!(a >= b, "expected descending: {a} >= {b}");
    }
    // List items must NOT include hops (summary shape).
    assert!(recent[0].get("hops").is_none(), "items = {recent:?}");

    // metrics: 2 samples in each series, current values populated.
    // RTT series uses ms-scale values (already divided by 1000 in MetricsQL);
    // loss series uses [0, 1] fractions.
    let metrics = &body["metrics"];
    assert!(metrics.is_object(), "metrics = {metrics}");
    let rtt = metrics["rtt_series"].as_array().expect("rtt_series");
    let loss = metrics["loss_series"].as_array().expect("loss_series");
    assert_eq!(rtt.len(), 2);
    assert_eq!(loss.len(), 2);
    // Values are [epoch_ms, value] — first entry: ts * 1000, second: parsed float.
    assert_eq!(rtt[0][0].as_f64().unwrap(), 1_700_000_000_000.0);
    assert_eq!(rtt[0][1].as_f64().unwrap(), 12.5);
    assert_eq!(rtt[1][1].as_f64().unwrap(), 13.75);
    assert_eq!(metrics["rtt_current"].as_f64().unwrap(), 13.75);
    // Loss is a fraction — last sample of the loss mock is 0.05 (5%).
    assert_eq!(loss[0][1].as_f64().unwrap(), 0.02);
    assert_eq!(loss[1][1].as_f64().unwrap(), 0.05);
    assert_eq!(metrics["loss_current"].as_f64().unwrap(), 0.05);

    // window / step echoes
    assert!(body["window"]["from"].is_string(), "body = {body}");
    assert!(body["window"]["to"].is_string(), "body = {body}");
    assert_eq!(body["step"], "1m", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// 404 on missing agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_returns_404_when_source_missing() {
    let pool = common::shared_migrated_pool().await.clone();

    // Seed only the target.
    let tgt = "t19-ov-tgt-only";
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.9", 0.0, 0.0).await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.161").await;

    let uri = format!("/api/paths/does-not-exist/{tgt}/overview");
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
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(tgt)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn overview_returns_404_when_target_missing() {
    let pool = common::shared_migrated_pool().await.clone();

    let src = "t19-ov-src-only";
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.8", 0.0, 0.0).await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.162").await;

    let uri = format!("/api/paths/{src}/does-not-exist/overview");
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
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(src)
        .execute(&pool)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// 400 on invalid protocol override
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_rejects_invalid_protocol_override() {
    let pool = common::shared_migrated_pool().await.clone();

    let (src, tgt) = ("t19-ov-bogus-src", "t19-ov-bogus-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.10", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.11", 0.0, 0.0).await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.163").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview?protocol=bogus");
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
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// 200 + metrics: null when VM unreachable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_returns_metrics_null_when_vm_unreachable() {
    let pool = common::shared_migrated_pool().await.clone();

    let (src, tgt) = ("t19-ov-unreach-src", "t19-ov-unreach-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.12", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.13", 0.0, 0.0).await;
    // Seed one snapshot so a primary protocol can be picked.
    insert_snapshot(
        &pool,
        src,
        tgt,
        "icmp",
        chrono::Utc::now() - chrono::Duration::minutes(2),
    )
    .await;

    // vm_url points at a closed port — the proxy client will fail fast.
    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.164").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);
    assert!(body["metrics"].is_null(), "body = {body}");
    // But the rest of the response still fills in.
    assert_eq!(body["primary_protocol"], "icmp");
    assert!(body["source"].is_object());
    assert!(body["target"].is_object());

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// Primary protocol: override + auto-pick
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_honours_protocol_override() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;

    // Assert the ?protocol=tcp override makes it through to the VM query
    // by matching the substring `protocol="tcp"` in the query param.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param_contains("query", "protocol=\"tcp\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_matrix_body(&[])))
        .expect(2)
        .mount(&mock)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-over-src", "t19-ov-over-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.14", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.15", 0.0, 0.0).await;
    // Both icmp and tcp snapshots exist — default would pick icmp.
    insert_snapshot(
        &pool,
        src,
        tgt,
        "icmp",
        chrono::Utc::now() - chrono::Duration::minutes(2),
    )
    .await;
    insert_snapshot(
        &pool,
        src,
        tgt,
        "tcp",
        chrono::Utc::now() - chrono::Duration::minutes(3),
    )
    .await;

    let state = common::state_with_admin_and_vm(pool.clone(), &mock.uri()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.165").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview?protocol=tcp");
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
    let body = parse_body(&bytes);
    assert_eq!(body["primary_protocol"], "tcp", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_auto_picks_udp_over_tcp_when_icmp_absent() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-udp-src", "t19-ov-udp-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.16", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.17", 0.0, 0.0).await;

    // Seed both UDP and TCP (and no ICMP): the fixed priority
    // `icmp > udp > tcp` should make UDP win. Newer TCP observed_at must
    // NOT tip the pick toward TCP.
    insert_snapshot(
        &pool,
        src,
        tgt,
        "udp",
        chrono::Utc::now() - chrono::Duration::minutes(3),
    )
    .await;
    insert_snapshot(
        &pool,
        src,
        tgt,
        "tcp",
        chrono::Utc::now() - chrono::Duration::minutes(2),
    )
    .await;

    // No VM needed; we only care about primary pick.
    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.166").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);
    // icmp absent; UDP beats TCP.
    assert_eq!(body["primary_protocol"], "udp", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_auto_picks_tcp_when_only_tcp_present() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-tcp-src", "t19-ov-tcp-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.18", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.19", 0.0, 0.0).await;

    insert_snapshot(
        &pool,
        src,
        tgt,
        "tcp",
        chrono::Utc::now() - chrono::Duration::minutes(2),
    )
    .await;

    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.167").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);
    assert_eq!(body["primary_protocol"], "tcp", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// Step selection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_step_is_1m_for_24h_window() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    // `step=1m` matcher catches a regression where the echoed `step`
    // differs from what actually went upstream to VM.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param("step", "1m"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_matrix_body(&[])))
        .expect(2)
        .mount(&mock)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-24h-src", "t19-ov-24h-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.20", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.21", 0.0, 0.0).await;
    insert_snapshot(
        &pool,
        src,
        tgt,
        "icmp",
        chrono::Utc::now() - chrono::Duration::minutes(2),
    )
    .await;

    let state = common::state_with_admin_and_vm(pool.clone(), &mock.uri()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.168").await;

    // Window = 24 h exactly. RFC3339 uses `+00:00` for UTC offsets, which
    // the `?` query parser would decode as ` ` — percent-encode the `+`.
    let to = chrono::Utc::now();
    let from = to - chrono::Duration::hours(24);
    let uri = format!(
        "/api/paths/{src}/{tgt}/overview?from={}&to={}",
        from.to_rfc3339().replace('+', "%2B"),
        to.to_rfc3339().replace('+', "%2B")
    );
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
    let body = parse_body(&bytes);
    assert_eq!(body["step"], "1m", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_step_is_5m_for_7d_window() {
    use wiremock::matchers::{method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    // `step=5m` matcher — same rationale as the 24 h test.
    Mock::given(wm_method("GET"))
        .and(wm_path("/api/v1/query_range"))
        .and(query_param("step", "5m"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vm_matrix_body(&[])))
        .expect(2)
        .mount(&mock)
        .await;

    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-7d-src", "t19-ov-7d-tgt");
    insert_agent_detailed(&pool, src, "S", "US", "10.0.0.22", 0.0, 0.0).await;
    insert_agent_detailed(&pool, tgt, "T", "DE", "10.0.0.23", 0.0, 0.0).await;
    insert_snapshot(
        &pool,
        src,
        tgt,
        "icmp",
        chrono::Utc::now() - chrono::Duration::hours(3),
    )
    .await;

    let state = common::state_with_admin_and_vm(pool.clone(), &mock.uri()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.169").await;

    let to = chrono::Utc::now();
    let from = to - chrono::Duration::days(7);
    let uri = format!(
        "/api/paths/{src}/{tgt}/overview?from={}&to={}",
        from.to_rfc3339().replace('+', "%2B"),
        to.to_rfc3339().replace('+', "%2B")
    );
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
    let body = parse_body(&bytes);
    assert_eq!(body["step"], "5m", "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// 401 when no session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_requires_session() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;
    let app = meshmon_service::http::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/paths/a/b/overview")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// recent_snapshots truncation signal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overview_sets_truncated_flag_when_recent_hits_limit() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-trunc-src", "t19-ov-trunc-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.11", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.12", 52.0, 13.0).await;

    // Seed 101 snapshots within the default 24h window. The 101st row tells
    // the handler there's more to see than fits in RECENT_LIMIT.
    let now = chrono::Utc::now();
    for i in 0..101 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "icmp",
            now - chrono::Duration::seconds(i * 10),
        )
        .await;
    }

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.171").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview"))
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
    let body = parse_body(&bytes);

    // Capped at 100 and flagged as truncated. The 101st row must not leak.
    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 100, "body = {body}");
    assert_eq!(body["recent_snapshots_truncated"], true, "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_truncated_flag_is_false_when_below_limit() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-nontrunc-src", "t19-ov-nontrunc-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.13", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.14", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    // Exactly RECENT_LIMIT rows — must not be flagged truncated, since no
    // 101st row exists.
    for i in 0..100 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "icmp",
            now - chrono::Duration::seconds(i * 10),
        )
        .await;
    }

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.172").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview"))
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
    let body = parse_body(&bytes);
    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 100, "body = {body}");
    assert_eq!(body["recent_snapshots_truncated"], false, "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

/// When the window has no snapshots for any protocol, `primary_protocol`
/// is serialized as JSON `null` (per spec §meshmon-03) so consumers don't
/// have to distinguish between "field absent" and "no data" — they both
/// mean the same thing, and null is less ambiguous than an omitted key.
#[tokio::test]
async fn overview_primary_protocol_is_null_when_no_snapshots() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t19-ov-empty-src", "t19-ov-empty-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.24", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.25", 52.0, 13.0).await;
    // Intentionally insert no snapshots.

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.173").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview"))
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
    let body = parse_body(&bytes);
    assert!(
        body.get("primary_protocol").is_some(),
        "primary_protocol must be present (as null), body = {body}"
    );
    assert!(
        body["primary_protocol"].is_null(),
        "primary_protocol must be JSON null when no snapshots, body = {body}"
    );

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// T32 — recent_snapshots is always scoped to the resolved primary protocol
// ---------------------------------------------------------------------------

/// With no `?protocol=` override, the server-picked `primary_protocol`
/// (ICMP in this fixture) must filter `recent_snapshots`. Rows of other
/// protocols inside the same window must not leak into the list or the
/// truncation count.
#[tokio::test]
async fn overview_recent_filtered_by_auto_primary() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t32-ov-auto-src", "t32-ov-auto-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.26", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.27", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    // 2 ICMP + 1 TCP within the default 24h window. ICMP wins the
    // priority pick, so recent should contain exactly the 2 ICMP rows.
    insert_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(2)).await;
    insert_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(3)).await;
    insert_snapshot(&pool, src, tgt, "tcp", now - chrono::Duration::minutes(1)).await;

    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.174").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview"))
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
    let body = parse_body(&bytes);

    assert_eq!(body["primary_protocol"], "icmp", "body = {body}");
    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 2, "body = {body}");
    for row in recent {
        assert_eq!(row["protocol"], "icmp", "row = {row}");
    }
    assert_eq!(body["recent_snapshots_truncated"], false, "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

/// When the primary-protocol pool is below the cap but other protocols
/// push the cross-protocol total past it, the truncation flag must stay
/// false (reflects the filtered pool, not the cross-protocol total).
#[tokio::test]
async fn overview_truncated_false_when_filtered_pool_below_limit() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t32-ov-subcap-src", "t32-ov-subcap-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.28", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.29", 52.0, 13.0).await;

    // 101 TCP rows + 5 ICMP rows. `?protocol=icmp` forces the TCP pool
    // out — the filtered pool (5) is well below RECENT_LIMIT.
    let now = chrono::Utc::now();
    for i in 0..101 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "tcp",
            now - chrono::Duration::seconds(i * 5),
        )
        .await;
    }
    for i in 0..5 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "icmp",
            now - chrono::Duration::seconds(i * 5 + 3),
        )
        .await;
    }

    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.175").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview?protocol=icmp"))
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
    let body = parse_body(&bytes);

    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 5, "body = {body}");
    for row in recent {
        assert_eq!(row["protocol"], "icmp", "row = {row}");
    }
    assert_eq!(body["recent_snapshots_truncated"], false, "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

/// When the filtered protocol pool alone exceeds RECENT_LIMIT, the
/// response must report `recent_snapshots_truncated: true` and return
/// exactly 100 rows — all of the filtered protocol — in newest-first
/// order, with no leakage from the noise protocol.
#[tokio::test]
async fn overview_truncated_true_when_filtered_pool_exceeds_limit() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t32-ov-overcap-src", "t32-ov-overcap-tgt");

    insert_agent_detailed(&pool, src, "Source", "US", "10.0.0.30", 37.0, -122.0).await;
    insert_agent_detailed(&pool, tgt, "Target", "DE", "10.0.0.31", 52.0, 13.0).await;

    // 101 ICMP + 50 TCP noise. With `?protocol=icmp` the TCP rows must
    // not shift which rows land in the 100-row window.
    let now = chrono::Utc::now();
    for i in 0..101 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "icmp",
            now - chrono::Duration::seconds(i * 5),
        )
        .await;
    }
    for i in 0..50 {
        insert_snapshot(
            &pool,
            src,
            tgt,
            "tcp",
            now - chrono::Duration::seconds(i * 5 + 2),
        )
        .await;
    }

    let state = common::state_with_admin_and_vm(pool.clone(), "http://127.0.0.1:1").await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);

    let cookie = common::login_as_admin(&app, "203.0.113.176").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/paths/{src}/{tgt}/overview?protocol=icmp"))
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
    let body = parse_body(&bytes);

    let recent = body["recent_snapshots"].as_array().expect("recent array");
    assert_eq!(recent.len(), 100, "body = {body}");
    for row in recent {
        assert_eq!(row["protocol"], "icmp", "row = {row}");
    }
    assert_eq!(body["recent_snapshots_truncated"], true, "body = {body}");

    cleanup_pair(&pool, src, tgt).await;
}

// ---------------------------------------------------------------------------
// T53c I1: hop-hostname stamping on path_overview latest_by_protocol hops
// ---------------------------------------------------------------------------

/// Three IPs used across the hop-hostname overview tests. Same semantics as
/// the user_api hop-hostname tests: positive / negative / cold-miss.
const OV_HOP_IP_POS: &str = "10.53.2.1";
const OV_HOP_IP_NEG: &str = "10.53.2.2";
const OV_HOP_IP_COLD: &str = "10.53.2.3";
const OV_HOP_HOSTNAME_POS: &str = "ov-hop-pos.example.com";

/// Seed a single route-snapshot with three hops using the three test IPs.
async fn insert_three_hop_snapshot(
    pool: &PgPool,
    src: &str,
    tgt: &str,
    protocol: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) {
    let hops = serde_json::json!([
        {
            "position": 1,
            "observed_ips": [{"ip": OV_HOP_IP_POS, "freq": 1.0}],
            "avg_rtt_micros": 1000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 2,
            "observed_ips": [{"ip": OV_HOP_IP_NEG, "freq": 1.0}],
            "avg_rtt_micros": 2000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 3,
            "observed_ips": [{"ip": OV_HOP_IP_COLD, "freq": 1.0}],
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
         VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb)",
    )
    .bind(src)
    .bind(tgt)
    .bind(protocol)
    .bind(observed_at)
    .bind(&hops)
    .bind(&summary)
    .execute(pool)
    .await
    .unwrap();
}

/// Seed the cache for overview hop tests: positive for `OV_HOP_IP_POS`,
/// negative for `OV_HOP_IP_NEG`, nothing for `OV_HOP_IP_COLD`.
async fn seed_ov_hop_hostname_cache(pool: &PgPool) {
    let pos_ip: std::net::IpAddr = OV_HOP_IP_POS.parse().unwrap();
    let neg_ip: std::net::IpAddr = OV_HOP_IP_NEG.parse().unwrap();
    common::seed_hostname_positive(pool, pos_ip, OV_HOP_HOSTNAME_POS).await;
    common::seed_hostname_negative(pool, neg_ip).await;
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(OV_HOP_IP_COLD)
        .execute(pool)
        .await
        .expect("clear cold-miss cache");
}

#[tokio::test]
async fn overview_stamps_positive_hop_hostname() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-ov-hop-pos-src", "t53c-ov-hop-pos-tgt");

    insert_agent_detailed(
        &pool,
        src,
        "OV Hop Pos Src",
        "US",
        "10.20.0.1",
        37.0,
        -122.0,
    )
    .await;
    insert_agent_detailed(&pool, tgt, "OV Hop Pos Tgt", "DE", "10.20.0.2", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    insert_three_hop_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(1)).await;
    seed_ov_hop_hostname_cache(&pool).await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.177").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);

    // The ICMP variant should be present with hop hostnames stamped.
    let hop0_ip0 = &body["latest_by_protocol"]["icmp"]["hops"][0]["observed_ips"][0];
    assert_eq!(
        hop0_ip0["ip"].as_str(),
        Some(OV_HOP_IP_POS),
        "hop 0 ip mismatch: {body}"
    );
    assert_eq!(
        hop0_ip0["hostname"].as_str(),
        Some(OV_HOP_HOSTNAME_POS),
        "hop 0 must have positive hostname: {body}"
    );

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_omits_hop_hostname_on_negative_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-ov-hop-neg-src", "t53c-ov-hop-neg-tgt");

    insert_agent_detailed(
        &pool,
        src,
        "OV Hop Neg Src",
        "US",
        "10.20.1.1",
        37.0,
        -122.0,
    )
    .await;
    insert_agent_detailed(&pool, tgt, "OV Hop Neg Tgt", "DE", "10.20.1.2", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    insert_three_hop_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(1)).await;
    seed_ov_hop_hostname_cache(&pool).await;

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.178").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);

    // Hop 1 (position 2) must NOT have hostname (negative cache hit).
    let hop1_ip0 = &body["latest_by_protocol"]["icmp"]["hops"][1]["observed_ips"][0];
    assert_eq!(
        hop1_ip0["ip"].as_str(),
        Some(OV_HOP_IP_NEG),
        "hop 1 ip mismatch: {body}"
    );
    assert!(
        hop1_ip0.get("hostname").is_none(),
        "negative cache hit must omit hostname: {body}"
    );

    cleanup_pair(&pool, src, tgt).await;
}

#[tokio::test]
async fn overview_cold_hop_miss_enqueues_resolver() {
    let pool = common::shared_migrated_pool().await.clone();
    let (src, tgt) = ("t53c-ov-hop-cold-src", "t53c-ov-hop-cold-tgt");

    insert_agent_detailed(
        &pool,
        src,
        "OV Hop Cold Src",
        "US",
        "10.20.2.1",
        37.0,
        -122.0,
    )
    .await;
    insert_agent_detailed(&pool, tgt, "OV Hop Cold Tgt", "DE", "10.20.2.2", 52.0, 13.0).await;

    let now = chrono::Utc::now();
    insert_three_hop_snapshot(&pool, src, tgt, "icmp", now - chrono::Duration::minutes(1)).await;
    seed_ov_hop_hostname_cache(&pool).await;

    let cold_ip: std::net::IpAddr = OV_HOP_IP_COLD.parse().unwrap();

    let state = common::state_with_admin(pool.clone()).await;
    state.registry.force_refresh().await.expect("refresh");
    let app = meshmon_service::http::router(state);
    let cookie = common::login_as_admin(&app, "203.0.113.179").await;

    let uri = format!("/api/paths/{src}/{tgt}/overview");
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
    let body = parse_body(&bytes);

    // Hop 2 (position 3) must NOT have hostname (cold miss).
    let hop2_ip0 = &body["latest_by_protocol"]["icmp"]["hops"][2]["observed_ips"][0];
    assert_eq!(
        hop2_ip0["ip"].as_str(),
        Some(OV_HOP_IP_COLD),
        "hop 2 ip mismatch: {body}"
    );
    assert!(
        hop2_ip0.get("hostname").is_none(),
        "cold miss must omit hostname: {body}"
    );

    // Enqueue fires: the stub backend writes a cache row for cold-miss IPs.
    assert!(
        common::wait_for_cache_row(&pool, cold_ip).await,
        "resolver never wrote a cache row for {cold_ip} — enqueue was skipped",
    );

    cleanup_pair(&pool, src, tgt).await;
}
