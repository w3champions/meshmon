//! Integration tests for `PushRouteSnapshot`.

mod common;

use meshmon_protocol::{
    HopIp, HopSummary, PathSummary, Protocol, RegisterRequest, RouteSnapshotRequest,
};
use sqlx::Row;
use std::net::IpAddr;
use tonic::Code;

fn good_snapshot(source: &str, target: &str) -> RouteSnapshotRequest {
    RouteSnapshotRequest {
        source_id: source.into(),
        target_id: target.into(),
        protocol: Protocol::Icmp as i32,
        observed_at_micros: 1_700_000_000_000_000,
        hops: vec![HopSummary {
            position: 1,
            observed_ips: vec![HopIp {
                ip: vec![10, 0, 0, 2].into(),
                frequency: 1.0,
            }],
            avg_rtt_micros: 1_000,
            stddev_rtt_micros: 100,
            loss_pct: 0.0,
        }],
        path_summary: Some(PathSummary {
            avg_rtt_micros: 1_000,
            loss_pct: 0.0,
            hop_count: 1,
        }),
    }
}

async fn register_agent(state: meshmon_service::state::AppState, id: &str, ip4: [u8; 4]) {
    let mut c = common::grpc_harness::in_process_agent_client(state, IpAddr::from(ip4)).await;
    c.register(RegisterRequest {
        id: id.into(),
        display_name: id.into(),
        location: "".into(),
        ip: ip4.to_vec().into(),
        lat: 0.0,
        lon: 0.0,
        agent_version: "0.1.0".into(),
        tcp_probe_port: 3555,
        udp_probe_port: 3552,
        campaign_max_concurrency: None,
    })
    .await
    .expect("register");
}

#[tokio::test]
async fn snapshot_happy_path() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    register_agent(state.clone(), "s-src", [10, 2, 0, 1]).await;
    register_agent(state.clone(), "s-tgt", [10, 2, 0, 2]).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 1])).await;
    let resp = client
        .push_route_snapshot(good_snapshot("s-src", "s-tgt"))
        .await
        .expect("push");
    let _ = resp.into_inner();

    for _ in 0..50 {
        let row = sqlx::query("SELECT COUNT(*) FROM route_snapshots WHERE source_id = $1")
            .bind("s-src")
            .fetch_one(&pool)
            .await
            .unwrap();
        let count: i64 = row.get(0);
        if count == 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("route_snapshots row did not appear within 5s");
}

#[tokio::test]
async fn snapshot_unknown_source_returns_permission_denied() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 3])).await;

    let err = client
        .push_route_snapshot(good_snapshot("nobody", "s-t"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn snapshot_bad_hop_returns_invalid_argument() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    register_agent(state.clone(), "s-bad", [10, 2, 0, 4]).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 4])).await;
    let mut bad = good_snapshot("s-bad", "s-t2");
    bad.hops[0].position = 0;
    let err = client.push_route_snapshot(bad).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn snapshot_empty_source_id_returns_invalid_argument() {
    // Validator runs before the registry check; empty source_id is a
    // client-side data bug and must surface as InvalidArgument.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 5])).await;

    let mut req = good_snapshot("ignored-because-empty", "s-t3");
    req.source_id = String::new();

    let err = client.push_route_snapshot(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}
