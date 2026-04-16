//! Integration test for the agent-API per-IP rate limit. Uses
//! `trust_forwarded_headers = true` so the smart-IP extractor honors
//! metadata-level forwarding headers set on the request.

mod common;

use arc_swap::ArcSwap;
use meshmon_protocol::GetConfigRequest;
use meshmon_service::config::Config;
use meshmon_service::state::AppState;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::watch;
use tonic::Code;

fn state_with_tight_limit(pool: sqlx::PgPool) -> AppState {
    let toml = format!(
        r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[agent_api]
shared_token = "{token}"
rate_limit_per_minute = 60
rate_limit_burst = 3
"#,
        token = common::TEST_AGENT_TOKEN
    );
    let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
    let swap = Arc::new(ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);
    let ingestion = common::dummy_ingestion(pool.clone());
    let registry = common::dummy_registry(pool.clone());
    AppState::new(swap, rx, pool, ingestion, registry)
}

#[tokio::test]
async fn exceeding_burst_returns_resource_exhausted() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = state_with_tight_limit(pool);
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([203, 0, 113, 90])).await;

    for i in 0..3 {
        let result = client.get_config(GetConfigRequest::default()).await;
        if let Err(err) = result {
            assert_ne!(err.code(), Code::ResourceExhausted, "pre-limit call {i}");
        }
    }
    let err = client
        .get_config(GetConfigRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ResourceExhausted);
}
