//! Integration tests for the `Register` RPC.

mod common;

use meshmon_protocol::{RegisterRequest, RegisterResponse};
use sqlx::Row;
use std::net::IpAddr;
use tonic::Code;

fn sample(id: &str, ip4: [u8; 4]) -> RegisterRequest {
    RegisterRequest {
        id: id.into(),
        display_name: format!("Agent {id}"),
        location: "Berlin, DE".into(),
        ip: ip4.to_vec().into(),
        lat: 52.52,
        lon: 13.405,
        agent_version: "0.1.0".into(),
        tcp_probe_port: 8002,
        udp_probe_port: 8005,
        campaign_max_concurrency: None,
    }
}

#[tokio::test]
async fn register_happy_path_inserts_row() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 0, 1])).await;

    let resp: tonic::Response<RegisterResponse> = client
        .register(sample("agent-reg-happy", [10, 0, 0, 1]))
        .await
        .expect("register");
    let _ = resp.into_inner();

    let row = sqlx::query("SELECT display_name, agent_version FROM agents WHERE id = $1")
        .bind("agent-reg-happy")
        .fetch_one(&pool)
        .await
        .unwrap();
    let name: String = row.get(0);
    let version: Option<String> = row.get(1);
    assert_eq!(name, "Agent agent-reg-happy");
    assert_eq!(version.as_deref(), Some("0.1.0"));
}

#[tokio::test]
async fn register_updates_existing_row_when_ip_matches() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 0, 2])).await;

    let _ = client
        .register(sample("agent-reg-upsert", [10, 0, 0, 2]))
        .await
        .expect("first");

    let mut second = sample("agent-reg-upsert", [10, 0, 0, 2]);
    second.agent_version = "0.2.0".into();
    second.display_name = "Updated".into();
    let _ = client.register(second).await.expect("second");

    let row = sqlx::query("SELECT display_name, agent_version FROM agents WHERE id = $1")
        .bind("agent-reg-upsert")
        .fetch_one(&pool)
        .await
        .unwrap();
    let name: String = row.get(0);
    let version: Option<String> = row.get(1);
    assert_eq!(name, "Updated");
    assert_eq!(version.as_deref(), Some("0.2.0"));
}

#[tokio::test]
async fn register_same_id_different_ip_returns_already_exists() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;

    // First client registers from 10.0.0.3.
    let mut client_a =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 0, 0, 3]))
            .await;
    let _ = client_a
        .register(sample("agent-reg-conflict", [10, 0, 0, 3]))
        .await
        .expect("first");

    // Second client registers from 10.0.0.4 claiming 10.0.0.4 — same id.
    let mut client_b =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 0, 4])).await;
    let err = client_b
        .register(sample("agent-reg-conflict", [10, 0, 0, 4]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::AlreadyExists);

    // DB row still points at the original IP.
    let row = sqlx::query("SELECT host(ip) FROM agents WHERE id = $1")
        .bind("agent-reg-conflict")
        .fetch_one(&pool)
        .await
        .unwrap();
    let ip: String = row.get(0);
    assert_eq!(ip, "10.0.0.3");
}

#[tokio::test]
async fn register_claimed_ip_mismatches_connection_returns_permission_denied() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    // Claimed IP 10.0.0.5 but connection says 10.0.0.99.
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 0, 99])).await;

    let err = client
        .register(sample("agent-reg-spoof", [10, 0, 0, 5]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn register_honors_x_real_ip_when_trust_forwarded_enabled() {
    // Proxy deployments: the TCP peer is the proxy's own IP, but the
    // real client IP arrives via `X-Real-IP` (the header nginx-style
    // proxies emit by default for gRPC locations). `state_with_agent_token`
    // builds a config with `trust_forwarded_headers = true`, so the
    // register handler must honor the metadata over the transport.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    // Connection IP is the "proxy" 172.18.0.10; claimed IP is the real
    // client 10.0.0.7; X-Real-IP advertises the same real client.
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([172, 18, 0, 10])).await;

    let mut req = tonic::Request::new(sample("agent-reg-xrealip", [10, 0, 0, 7]));
    req.metadata_mut()
        .insert("x-real-ip", "10.0.0.7".parse().unwrap());

    let _ = client
        .register(req)
        .await
        .expect("x-real-ip metadata should override transport peer");
}

#[tokio::test]
async fn register_allows_loopback_connection_with_any_claimed_ip() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([127, 0, 0, 1])).await;

    let _ = client
        .register(sample("agent-reg-loopback", [203, 0, 113, 5]))
        .await
        .expect("loopback exempt");
}

#[tokio::test]
async fn register_force_refreshes_registry() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let state_for_check = state.clone();
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 0, 6])).await;

    assert!(state_for_check
        .registry
        .snapshot()
        .get("agent-reg-refresh")
        .is_none());

    let _ = client
        .register(sample("agent-reg-refresh", [10, 0, 0, 6]))
        .await
        .expect("register");

    let snap = state_for_check.registry.snapshot();
    let info = snap
        .get("agent-reg-refresh")
        .expect("registry sees the agent");
    assert_eq!(info.display_name, "Agent agent-reg-refresh");
}

#[tokio::test]
async fn register_rejects_invalid_coordinates() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 0, 1])).await;

    // NaN lat.
    let mut req = sample("agent-reg-coord-nan", [10, 9, 0, 1]);
    req.lat = f64::NAN;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains("lat"), "{:?}", err.message());

    // Infinite lon.
    let mut req = sample("agent-reg-coord-inf", [10, 9, 0, 1]);
    req.lon = f64::INFINITY;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains("lon"), "{:?}", err.message());

    // lat > 90.
    let mut req = sample("agent-reg-coord-bigtlat", [10, 9, 0, 1]);
    req.lat = 91.0;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    // lon < -180.
    let mut req = sample("agent-reg-coord-smalllon", [10, 9, 0, 1]);
    req.lon = -180.1;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn register_rejects_bad_ip_length() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([1, 2, 3, 4])).await;

    let mut bad = sample("agent-reg-bad-ip", [1, 2, 3, 4]);
    bad.ip = vec![1, 2, 3].into(); // not 4 or 16
    let err = client.register(bad).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

/// Regression guard for T42/T14: `Register` must populate the catalogue
/// row, publish an `Updated` event so UI subscribers see the new row in
/// real time, and do so in a way that preserves prior operator edits on
/// subsequent re-registers. The handler previously called
/// `ensure_from_agent` and ignored the returned entry, which silently
/// regressed both halves of the contract.
#[tokio::test]
async fn register_creates_catalogue_row_publishes_updated_and_preserves_operator_edits() {
    use meshmon_service::catalogue::events::CatalogueEvent;

    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    let state_for_check = state.clone();
    // Subscribe *before* the RPC so we don't miss the fan-out.
    let mut rx = state.catalogue_broker.subscribe();

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 42, 0, 1])).await;

    let mut req = sample("agent-reg-catalogue-hook", [10, 42, 0, 1]);
    req.lat = 37.7749;
    req.lon = -122.4194;
    let _ = client.register(req).await.expect("register");

    // Event fan-out — must arrive promptly.
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("broker emitted event within 2s")
        .expect("broker did not close");
    match evt {
        CatalogueEvent::Updated { id: _ } => {}
        other => panic!("expected Updated, got {other:?}"),
    }

    // DB state — agent-derived row with Latitude/Longitude locked.
    let row = sqlx::query(
        "SELECT source::text, operator_edited_fields, latitude, longitude \
         FROM ip_catalogue WHERE ip = '10.42.0.1'::inet",
    )
    .fetch_one(&pool)
    .await
    .expect("catalogue row exists");
    let source: String = row.get(0);
    let fields: Vec<String> = row.get(1);
    let lat: f64 = row.get(2);
    let lon: f64 = row.get(3);
    assert_eq!(source, "agent_registration");
    assert!(fields.iter().any(|f| f == "Latitude"));
    assert!(fields.iter().any(|f| f == "Longitude"));
    assert!((lat - 37.7749).abs() < 1e-9);
    assert!((lon - (-122.4194)).abs() < 1e-9);

    // Simulate an operator edit adding `City` to the lock set, then
    // re-register — ensure_from_agent's array dedup must preserve the
    // operator's `City` alongside the agent's lat/lon locks.
    sqlx::query(
        "UPDATE ip_catalogue
         SET city = 'San Francisco',
             operator_edited_fields = ARRAY['Latitude', 'Longitude', 'City']::text[]
         WHERE ip = '10.42.0.1'::inet",
    )
    .execute(&pool)
    .await
    .expect("operator edit");

    let mut client2 = common::grpc_harness::in_process_agent_client(
        state_for_check,
        IpAddr::from([10, 42, 0, 1]),
    )
    .await;
    let mut req2 = sample("agent-reg-catalogue-hook", [10, 42, 0, 1]);
    req2.lat = 40.0;
    req2.lon = -75.0;
    let _ = client2.register(req2).await.expect("re-register");

    let row = sqlx::query(
        "SELECT operator_edited_fields, city FROM ip_catalogue WHERE ip = '10.42.0.1'::inet",
    )
    .fetch_one(&pool)
    .await
    .expect("row");
    let fields: Vec<String> = row.get(0);
    let city: Option<String> = row.get(1);
    assert!(fields.iter().any(|f| f == "Latitude"));
    assert!(fields.iter().any(|f| f == "Longitude"));
    assert!(
        fields.iter().any(|f| f == "City"),
        "operator City lock must survive re-register, got {fields:?}"
    );
    assert_eq!(city.as_deref(), Some("San Francisco"));
}

#[tokio::test]
async fn concurrent_register_same_id_different_ip_yields_single_winner() {
    // Two callers race to claim the same fresh `id` from different peer IPs.
    // Depending on thread scheduling one is caught by the preflight SELECT
    // (if the first upsert has already committed by the time the second
    // preflights) and one is caught by the atomic `ON CONFLICT ... WHERE
    // agents.ip = EXCLUDED.ip` guard (if both preflights see no row and
    // race to the upsert). Either way exactly one caller must succeed and
    // the other must receive ALREADY_EXISTS; without the guard, both
    // could silently succeed with only the non-ip fields overwritten.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;

    let mut client_a =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 7, 0, 20]))
            .await;
    let mut client_b =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 7, 0, 21]))
            .await;

    let req_a = sample("agent-reg-race", [10, 7, 0, 20]);
    let req_b = sample("agent-reg-race", [10, 7, 0, 21]);

    let (res_a, res_b) = tokio::join!(client_a.register(req_a), client_b.register(req_b));

    let a_ok = res_a.is_ok();
    let b_ok = res_b.is_ok();
    assert_eq!(
        usize::from(a_ok) + usize::from(b_ok),
        1,
        "exactly one caller must win the race; a_ok={a_ok} b_ok={b_ok}",
    );

    let loser_err = if a_ok {
        res_b.unwrap_err()
    } else {
        res_a.unwrap_err()
    };
    assert_eq!(
        loser_err.code(),
        Code::AlreadyExists,
        "losing caller must see ALREADY_EXISTS, got {loser_err:?}",
    );

    // The stored row must reflect the winning IP, not be silently overwritten.
    let stored_ip: sqlx::types::ipnetwork::IpNetwork =
        sqlx::query_scalar("SELECT ip FROM agents WHERE id = $1")
            .bind("agent-reg-race")
            .fetch_one(&pool)
            .await
            .expect("row exists");
    let winner_ip = if a_ok {
        IpAddr::from([10, 7, 0, 20])
    } else {
        IpAddr::from([10, 7, 0, 21])
    };
    assert_eq!(stored_ip.ip(), winner_ip, "DB must reflect the winner's IP");
}

#[tokio::test]
async fn register_without_auth_returns_unauthenticated() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    // Use the override constructor with an empty bearer so the interceptor
    // receives "Bearer " and rejects with UNAUTHENTICATED.
    let mut client = common::grpc_harness::in_process_agent_client_with_token(
        state,
        IpAddr::from([1, 2, 3, 4]),
        "",
    )
    .await;

    let err = client
        .register(sample("x", [1, 2, 3, 4]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn register_rejects_zero_tcp_port() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 5, 1])).await;

    let mut req = sample("agent-reg-tcp-zero", [10, 9, 5, 1]);
    req.tcp_probe_port = 0;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(
        err.message().contains("tcp_probe_port"),
        "message was {:?}",
        err.message()
    );
}

#[tokio::test]
async fn register_rejects_zero_udp_port() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 5, 2])).await;

    let mut req = sample("agent-reg-udp-zero", [10, 9, 5, 2]);
    req.udp_probe_port = 0;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(
        err.message().contains("udp_probe_port"),
        "message was {:?}",
        err.message()
    );
}

#[tokio::test]
async fn register_rejects_out_of_range_tcp_port() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 5, 3])).await;

    let mut req = sample("agent-reg-tcp-too-big", [10, 9, 5, 3]);
    req.tcp_probe_port = 70_000;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(
        err.message().contains("tcp_probe_port"),
        "message was {:?}",
        err.message()
    );
}

#[tokio::test]
async fn register_rejects_out_of_range_udp_port() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 5, 4])).await;

    let mut req = sample("agent-reg-udp-too-big", [10, 9, 5, 4]);
    req.udp_probe_port = 70_000;
    let err = client.register(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(
        err.message().contains("udp_probe_port"),
        "message was {:?}",
        err.message()
    );
}

#[tokio::test]
async fn register_persists_probe_ports() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 6, 1])).await;

    let mut req = sample("agent-reg-ports", [10, 9, 6, 1]);
    req.tcp_probe_port = 4001;
    req.udp_probe_port = 4002;
    let _ = client.register(req).await.expect("register");

    let row = sqlx::query("SELECT tcp_probe_port, udp_probe_port FROM agents WHERE id = $1")
        .bind("agent-reg-ports")
        .fetch_one(&pool)
        .await
        .unwrap();
    let tcp: i32 = row.get(0);
    let udp: i32 = row.get(1);
    assert_eq!(tcp, 4001);
    assert_eq!(udp, 4002);
}

#[tokio::test]
async fn register_upsert_updates_probe_ports() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 6, 2])).await;

    let mut first = sample("agent-reg-ports-upsert", [10, 9, 6, 2]);
    first.tcp_probe_port = 5001;
    first.udp_probe_port = 5002;
    let _ = client.register(first).await.expect("first");

    let mut second = sample("agent-reg-ports-upsert", [10, 9, 6, 2]);
    second.tcp_probe_port = 6001;
    second.udp_probe_port = 6002;
    let _ = client.register(second).await.expect("second");

    let row = sqlx::query("SELECT tcp_probe_port, udp_probe_port FROM agents WHERE id = $1")
        .bind("agent-reg-ports-upsert")
        .fetch_one(&pool)
        .await
        .unwrap();
    let tcp: i32 = row.get(0);
    let udp: i32 = row.get(1);
    assert_eq!(tcp, 6001);
    assert_eq!(udp, 6002);
}
