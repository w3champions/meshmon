//! Integration tests for the `PushMetrics` RPC.

mod common;

use meshmon_protocol::{
    AgentMetadata, MetricsBatch, PathMetrics, Protocol, ProtocolHealth, RegisterRequest,
};
use std::net::IpAddr;
use tonic::Code;

/// Build a well-formed `PathMetrics` entry with valid in-range values.
/// Uses a 60-second window (60_000_000 µs) and ICMP + Healthy.
fn good_path(target: &str) -> PathMetrics {
    PathMetrics {
        target_id: target.into(),
        protocol: Protocol::Icmp as i32,
        window_start_micros: 0,
        window_end_micros: 60_000_000,
        probes_sent: 60,
        probes_successful: 60,
        failure_rate: 0.0,
        rtt_avg_micros: 1_000,
        rtt_min_micros: 800,
        rtt_max_micros: 1_200,
        rtt_stddev_micros: 100,
        rtt_p50_micros: 1_000,
        rtt_p95_micros: 1_150,
        rtt_p99_micros: 1_190,
        health: ProtocolHealth::Healthy as i32,
    }
}

/// Build a well-formed `MetricsBatch` for `source` reporting measurements
/// toward `target`. Includes a minimal `AgentMetadata`.
fn good_batch(source: &str, target: &str) -> MetricsBatch {
    MetricsBatch {
        source_id: source.into(),
        batch_timestamp_micros: 60_000_000,
        agent_metadata: Some(AgentMetadata {
            version: "0.1.0".into(),
            uptime_secs: 0,
            local_error_count: 0,
            dropped_count: 0,
        }),
        paths: vec![good_path(target)],
    }
}

/// Register an agent via the in-process client so the registry snapshot
/// contains it before `push_metrics` is called.
async fn register_agent(state: meshmon_service::state::AppState, id: &str, ip4: [u8; 4]) {
    let mut client = common::grpc_harness::in_process_agent_client(state, IpAddr::from(ip4)).await;
    client
        .register(RegisterRequest {
            id: id.into(),
            display_name: format!("Agent {id}"),
            location: String::new(),
            ip: ip4.to_vec().into(),
            lat: 0.0,
            lon: 0.0,
            agent_version: "0.1.0".into(),
        })
        .await
        .expect("register");
}

// ---------------------------------------------------------------------------
// Test 1: happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_happy_path() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone());

    // Seed both source and target into the agents table so the registry
    // snapshot accepts the source and the validator is happy with the target.
    register_agent(state.clone(), "metrics-src-happy", [10, 1, 0, 1]).await;
    register_agent(state.clone(), "metrics-tgt-happy", [10, 1, 0, 2]).await;

    // Open a fresh client for the push itself (from the source's IP).
    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 1, 0, 1])).await;

    let resp = client
        .push_metrics(good_batch("metrics-src-happy", "metrics-tgt-happy"))
        .await;
    assert!(resp.is_ok(), "expected Ok, got {resp:?}");
}

// ---------------------------------------------------------------------------
// Test 2: unknown source → PermissionDenied
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_unknown_source_returns_permission_denied() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone());

    // Deliberately do NOT register "metrics-src-unknown" — only the target.
    register_agent(state.clone(), "metrics-tgt-perm", [10, 2, 0, 2]).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 1])).await;

    let err = client
        .push_metrics(good_batch("metrics-src-unknown", "metrics-tgt-perm"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

// ---------------------------------------------------------------------------
// Test 3: failure_rate > 1.0 → InvalidArgument
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_bad_range_returns_invalid_argument() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone());

    // Register source so it passes the registry check.
    register_agent(state.clone(), "metrics-src-badrange", [10, 3, 0, 1]).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 3, 0, 1])).await;

    let mut batch = good_batch("metrics-src-badrange", "metrics-tgt-badrange");
    // Inject an invalid failure_rate (> 1.0).
    batch.paths[0].failure_rate = 1.5;

    let err = client.push_metrics(batch).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

// ---------------------------------------------------------------------------
// Test 4: empty source_id → InvalidArgument (not PermissionDenied).
// The validator runs before the registry check so that malformed payloads
// surface as INVALID_ARGUMENT. A missing source_id is a client-side data
// bug, not an authorization question.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_empty_source_id_returns_invalid_argument() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool);

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 4, 0, 1])).await;

    let mut batch = good_batch("ignored-because-empty", "metrics-tgt-empty");
    batch.source_id = String::new();

    let err = client.push_metrics(batch).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}
