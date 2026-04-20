//! End-to-end integration of `AgentCommandService` with `StubProber`.
//!
//! Spins up a real tonic server on a loopback TCP port and drives it
//! with a real `AgentCommandClient`. Proves the transport contract:
//!   * one `MeasurementResult` streams back per target, correlated by
//!     `pair_id`;
//!   * dropping the client-side stream releases the server-side
//!     semaphore permit, so subsequent calls proceed without stalling.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use meshmon_agent::command::{AgentCommandService, CampaignProber, StubProber};
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    AgentCommandClient, AgentCommandServer, MeasurementKind, MeasurementResult, MeasurementTarget,
    Protocol, RunMeasurementBatchRequest,
};
use tokio::sync::{mpsc, Notify};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::Status;

/// Bind an ephemeral loopback TCP port, spawn a tonic server serving an
/// `AgentCommandService` wrapped around `prober`, and return the bound
/// `SocketAddr`.
async fn spawn_service_with<P: CampaignProber>(
    prober: P,
    max_concurrency: usize,
) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(prober), max_concurrency);
    tokio::spawn(async move {
        Server::builder()
            .add_service(AgentCommandServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .ok();
    });
    // Give the server a moment to come up before dialing. A retry loop
    // would be sturdier but this is a loopback socket — 50 ms is plenty
    // and keeps the test fast.
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

/// Default test server backed by the production `StubProber`.
async fn spawn_service(max_concurrency: usize) -> std::net::SocketAddr {
    spawn_service_with(StubProber, max_concurrency).await
}

/// Test-only prober that blocks until cancelled. Used to hold the
/// per-agent permit for the saturation contract test without relying on
/// HTTP/2 flow-control backpressure, which is sensitive to target count
/// and message size in ways that make the assertion flaky.
struct HoldingProber;

#[async_trait]
impl CampaignProber for HoldingProber {
    async fn run_batch(
        &self,
        _req: RunMeasurementBatchRequest,
        cancel: CancellationToken,
        _results: mpsc::Sender<Result<MeasurementResult, Status>>,
    ) {
        cancel.cancelled().await;
    }
}

fn latency_request(batch_id: u64, targets: usize) -> RunMeasurementBatchRequest {
    RunMeasurementBatchRequest {
        batch_id,
        kind: MeasurementKind::Latency as i32,
        protocol: Protocol::Icmp as i32,
        probe_count: 10,
        timeout_ms: 2000,
        probe_stagger_ms: 100,
        targets: (0..targets)
            .map(|i| MeasurementTarget {
                pair_id: i as u64,
                destination_ip: vec![203, 0, 113, (i % 255) as u8].into(),
                destination_port: 0,
            })
            .collect(),
    }
}

#[tokio::test]
async fn run_measurement_batch_streams_one_result_per_target() {
    let addr = spawn_service(16).await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = AgentCommandClient::new(channel);

    let req = latency_request(1, 5);
    let mut stream = client
        .run_measurement_batch(req)
        .await
        .expect("stream opens")
        .into_inner();

    let mut seen: Vec<u64> = Vec::new();
    while let Some(item) = stream.next().await {
        let result = item.expect("stub never emits Err");
        match result.outcome {
            Some(Outcome::Success(_)) => seen.push(result.pair_id),
            other => panic!("expected success outcome, got {other:?}"),
        }
    }
    assert_eq!(seen.len(), 5);
    seen.sort();
    assert_eq!(seen, vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn dropping_stream_cancels_the_batch() {
    // Cap set to 1 so the liveness check below would stall if dropping
    // the first stream leaked its permit.
    let addr = spawn_service(1).await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = AgentCommandClient::new(channel);

    // First batch is oversized so it has plenty of pairs left when we
    // drop it — if cancellation leaks, the subsequent call in this test
    // would block on the permit.
    let mut stream = client
        .run_measurement_batch(latency_request(1, 50))
        .await
        .expect("first stream opens")
        .into_inner();

    // Read one result to confirm the batch is actually running, then
    // drop the stream. The server-side `CancelOnDrop` guard must fire,
    // the prober must observe the cancel, and the permit must release.
    let _first = stream
        .next()
        .await
        .expect("first result arrives")
        .expect("first result is Ok");
    drop(stream);

    // Brief pause so the server observes the HTTP/2 stream going away
    // and releases the permit before we dial again.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Second call must not starve. With max_concurrency=1, a leaked
    // permit would make this call hang forever on the semaphore.
    let mut stream2 = tokio::time::timeout(
        Duration::from_secs(2),
        client.run_measurement_batch(latency_request(2, 1)),
    )
    .await
    .expect("second call did not starve")
    .expect("second stream opens")
    .into_inner();
    let result = stream2
        .next()
        .await
        .expect("second batch emits result")
        .expect("result is Ok");
    assert_eq!(result.pair_id, 0);
}

#[tokio::test]
async fn saturated_agent_returns_resource_exhausted() {
    // Capacity 1 + `HoldingProber`: the first call keeps its permit for
    // the life of its stream (the prober only unblocks on cancel), so
    // the second call collides deterministically.
    let addr = spawn_service_with(HoldingProber, 1).await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = AgentCommandClient::new(channel);

    // Bind the stream to a name WITHOUT a leading underscore: Rust drops
    // `_foo` at end of statement, but `foo` (even with `_` prefix) lives
    // to the end of the enclosing scope. We want the first stream to
    // hold its permit until we have confirmed the second call is
    // refused.
    let held = client
        .clone()
        .run_measurement_batch(latency_request(1, 1))
        .await
        .expect("first stream opens")
        .into_inner();

    // Small pause so the server-side task has taken the permit.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let err = client
        .run_measurement_batch(latency_request(2, 1))
        .await
        .expect_err("saturated agent rejects");
    assert_eq!(err.code(), tonic::Code::ResourceExhausted);

    // Drop the held stream now — the permit release after this point
    // has no effect on the assertion above. Dropping also cancels the
    // `HoldingProber` so the server-side task exits cleanly.
    drop(held);
}
