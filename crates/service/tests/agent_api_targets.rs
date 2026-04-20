//! Integration tests for `GetTargets`.

mod common;

use meshmon_protocol::{GetTargetsRequest, RegisterRequest};
use std::net::IpAddr;
use tonic::Code;

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
async fn excludes_requesting_source() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    register_agent(state.clone(), "tgt-a", [10, 9, 1, 1]).await;
    register_agent(state.clone(), "tgt-b", [10, 9, 1, 2]).await;
    register_agent(state.clone(), "tgt-c", [10, 9, 1, 3]).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 1, 1])).await;
    let resp = client
        .get_targets(GetTargetsRequest {
            source_id: "tgt-a".into(),
        })
        .await
        .expect("get_targets");
    let body = resp.into_inner();
    let ids: Vec<&str> = body.targets.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"tgt-b"));
    assert!(ids.contains(&"tgt-c"));
    assert!(!ids.contains(&"tgt-a"));
}

#[tokio::test]
async fn excludes_stale_agents() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    register_agent(state.clone(), "tgt-fresh", [10, 9, 3, 1]).await;
    register_agent(state.clone(), "tgt-stale", [10, 9, 3, 2]).await;

    sqlx::query("UPDATE agents SET last_seen_at = NOW() - INTERVAL '1 hour' WHERE id = $1")
        .bind("tgt-stale")
        .execute(&pool)
        .await
        .unwrap();
    state.registry.force_refresh().await.unwrap();

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 3, 99])).await;
    let resp = client
        .get_targets(GetTargetsRequest {
            source_id: "ignored".into(),
        })
        .await
        .expect("get_targets");
    let body = resp.into_inner();
    let ids: Vec<&str> = body.targets.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"tgt-fresh"));
    assert!(!ids.contains(&"tgt-stale"));
}

#[tokio::test]
async fn empty_source_id_returns_invalid_argument() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 9, 4, 1])).await;

    let err = client
        .get_targets(GetTargetsRequest {
            source_id: "".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn targets_requires_auth() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client = common::grpc_harness::in_process_agent_client_with_token(
        state,
        IpAddr::from([10, 9, 4, 2]),
        "",
    )
    .await;

    let err = client
        .get_targets(GetTargetsRequest {
            source_id: "whatever".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}
