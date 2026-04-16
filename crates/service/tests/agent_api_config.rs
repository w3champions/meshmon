//! Integration tests for `GetConfig`.

mod common;

use arc_swap::ArcSwap;
use meshmon_protocol::{GetConfigRequest, Protocol};
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::watch;
use tonic::Code;

#[tokio::test]
async fn config_returns_defaults() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 99, 1])).await;

    let resp = client
        .get_config(GetConfigRequest::default())
        .await
        .expect("get_config");
    let body = resp.into_inner();
    assert_eq!(
        body.enabled_protocols,
        vec![
            Protocol::Icmp as i32,
            Protocol::Tcp as i32,
            Protocol::Udp as i32
        ]
    );
    assert_eq!(body.windows.unwrap().primary_sec, 300);
    let icmp = body.icmp_thresholds.unwrap();
    assert!((icmp.unhealthy_trigger_pct - 0.90).abs() < 1e-9);
    let tcp = body.tcp_thresholds.unwrap();
    assert!((tcp.unhealthy_trigger_pct - 0.50).abs() < 1e-9);
    assert_eq!(body.rates.len(), 9);
}

#[tokio::test]
async fn config_requires_auth() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    let mut client = common::grpc_harness::in_process_agent_client_with_token(
        state,
        IpAddr::from([10, 0, 99, 2]),
        "",
    )
    .await;

    let err = client
        .get_config(GetConfigRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn config_reflects_operator_override() {
    let pool = common::shared_migrated_pool().await.clone();
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[agent_api]
shared_token = "{token}"

[probing.windows]
primary_sec = 120
"#,
        token = common::TEST_AGENT_TOKEN
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = common::dummy_ingestion(pool.clone());
    let registry = common::dummy_registry(pool.clone());
    let state = AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        common::test_prometheus_handle().await,
    );

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 0, 99, 3])).await;
    let resp = client
        .get_config(GetConfigRequest::default())
        .await
        .expect("get_config");
    let body = resp.into_inner();
    let windows = body.windows.unwrap();
    assert_eq!(windows.primary_sec, 120);
    assert_eq!(windows.diversity_sec, 900, "other fields default");
}
