//! Two agents pushing metrics simultaneously do not corrupt state.
//!
//! Each agent gets its own gRPC client (its own HTTP/2 channel). Both push
//! 20 batches as fast as they can; all RPCs must succeed.

mod common;

use meshmon_protocol::{
    AgentMetadata, MetricsBatch, PathMetrics, Protocol, ProtocolHealth, RegisterRequest,
};
use std::net::IpAddr;

fn batch(source: &str, target: &str, ts: i64) -> MetricsBatch {
    MetricsBatch {
        source_id: source.into(),
        batch_timestamp_micros: ts,
        agent_metadata: Some(AgentMetadata {
            version: "0.1.0".into(),
            uptime_secs: 60,
            local_error_count: 0,
            dropped_count: 0,
        }),
        paths: vec![PathMetrics {
            target_id: target.into(),
            protocol: Protocol::Icmp as i32,
            window_start_micros: ts - 60_000_000,
            window_end_micros: ts,
            probes_sent: 10,
            probes_successful: 10,
            failure_rate: 0.0,
            rtt_avg_micros: 1_000,
            rtt_min_micros: 800,
            rtt_max_micros: 1_200,
            rtt_stddev_micros: 100,
            rtt_p50_micros: 1_000,
            rtt_p95_micros: 1_100,
            rtt_p99_micros: 1_200,
            health: ProtocolHealth::Healthy as i32,
        }],
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
    })
    .await
    .expect("register");
}

#[tokio::test]
async fn two_agents_push_concurrently() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;
    register_agent(state.clone(), "cc-a", [10, 5, 0, 1]).await;
    register_agent(state.clone(), "cc-b", [10, 5, 0, 2]).await;
    register_agent(state.clone(), "cc-c", [10, 5, 0, 3]).await;

    // Two independent client channels against the one in-process server.
    let mut client_a =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 5, 0, 1]))
            .await;
    let mut client_b =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 5, 0, 2])).await;

    let base = 1_700_000_000_000_000i64;

    let a = tokio::spawn(async move {
        for i in 0..20 {
            client_a
                .push_metrics(batch("cc-a", "cc-c", base + i * 60_000_000))
                .await
                .unwrap_or_else(|e| panic!("a iter {i}: {e}"));
        }
    });
    let b = tokio::spawn(async move {
        for i in 0..20 {
            client_b
                .push_metrics(batch("cc-b", "cc-c", base + i * 60_000_000))
                .await
                .unwrap_or_else(|e| panic!("b iter {i}: {e}"));
        }
    });

    a.await.unwrap();
    b.await.unwrap();
}
