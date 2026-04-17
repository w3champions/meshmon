//! Integration tests for the `OpenTunnel` RPC's pre-accept validation.
//!
//! The shared-bearer-token model gates *which agents* can talk to the
//! service, but the `x-meshmon-source-id` metadata header is
//! identification, not authorization — any authenticated agent could
//! otherwise pass another agent's id and hijack its tunnel slot
//! (`TunnelManager::accept` replaces the existing entry and cancels its
//! driver, severing the legitimate connection). These tests exercise the
//! per-call IP binding that closes that hole: the caller's peer IP must
//! match the IP registered for `source_id` (loopback exempt).

mod common;

use meshmon_protocol::TunnelFrame;
use std::net::IpAddr;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Code, Request};

/// Helper: build a streaming request whose body is an empty
/// `ReceiverStream`. The server validates metadata *before* calling
/// `into_inner()`, so tests that only care about the permission check
/// never need to send any frames.
fn open_tunnel_request(
    source_id: &str,
) -> (
    Request<ReceiverStream<TunnelFrame>>,
    tokio::sync::mpsc::Sender<TunnelFrame>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<TunnelFrame>(1);
    let mut req = Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert("x-meshmon-source-id", source_id.parse().unwrap());
    (req, tx)
}

#[tokio::test]
async fn open_tunnel_rejects_when_peer_ip_does_not_match_registered_ip() {
    // Insert an agent registered at 10.1.0.2, then connect from 10.1.0.1
    // claiming source_id = B. This simulates one authenticated agent
    // trying to hijack another agent's tunnel slot.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    common::insert_agent_with_ip(
        &pool,
        "agent-open-tunnel-victim",
        IpAddr::from([10, 1, 0, 2]),
    )
    .await;
    state
        .registry
        .force_refresh()
        .await
        .expect("registry force_refresh");

    // Caller connects from a non-loopback IP that does NOT match the
    // registered IP for `agent-open-tunnel-victim`.
    let mut client =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([10, 1, 0, 1]))
            .await;

    let (req, _tx) = open_tunnel_request("agent-open-tunnel-victim");
    let err = client.open_tunnel(req).await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert!(
        err.message().contains("source agent IP"),
        "unexpected message: {:?}",
        err.message()
    );

    // The tunnel slot for the victim must remain unchanged — no replacement
    // tunnel was accepted for `agent-open-tunnel-victim`.
    assert_eq!(
        state.tunnel_manager.len(),
        0,
        "no tunnel should have been registered"
    );
}

#[tokio::test]
async fn open_tunnel_allows_loopback_even_when_registered_ip_differs() {
    // The registered IP is 203.0.113.7 (TEST-NET-3) but the caller
    // connects from 127.0.0.1 — loopback is exempt, matching register's
    // developer-loop behaviour.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool.clone()).await;
    common::insert_agent_with_ip(
        &pool,
        "agent-open-tunnel-loopback",
        IpAddr::from([203, 0, 113, 7]),
    )
    .await;
    state
        .registry
        .force_refresh()
        .await
        .expect("registry force_refresh");

    let mut client =
        common::grpc_harness::in_process_agent_client(state.clone(), IpAddr::from([127, 0, 0, 1]))
            .await;

    let (req, tx) = open_tunnel_request("agent-open-tunnel-loopback");
    // The RPC returns once `accept` has wired up the tunnel; the response
    // stream will stay open until we drop `tx` / the tunnel manager is
    // closed. We only care that the call succeeds past the IP check.
    let response = client.open_tunnel(req).await.expect("loopback exempt");

    // Drop the outbound sender first — lets the driver's outer stream EOF
    // so the response body completes cleanly.
    drop(tx);

    // Drain the response so the server-side driver observes the client's
    // stream EOF and the session tears down. The stream may return items
    // or just end; both are acceptable — we just want clean shutdown.
    let mut resp_stream = response.into_inner();
    while let Some(_frame) = resp_stream.next().await {
        // No-op — we only care that the call passed pre-accept validation.
    }

    // Ensure the tunnel manager unregisters the entry after teardown so
    // the shared state is clean for the next test. Poll briefly — the
    // driver task runs asynchronously.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && !state.tunnel_manager.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn open_tunnel_rejects_unknown_source_id() {
    // No agent row for this id → registry snapshot misses → rejected
    // with the existing "unknown source agent" message. This guards
    // the ordering: source-id existence check still runs before the IP
    // match (so the error surface stays stable).
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 1])).await;

    let (req, _tx) = open_tunnel_request("agent-open-tunnel-unknown");
    let err = client.open_tunnel(req).await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied, "{err:?}");
    assert!(
        err.message().contains("unknown source agent"),
        "unexpected message: {:?}",
        err.message()
    );
}

#[tokio::test]
async fn open_tunnel_rejects_empty_source_id() {
    // Pre-existing contract: empty source_id is INVALID_ARGUMENT (not
    // PERMISSION_DENIED) because it's a malformed request, not an
    // unauthorized one.
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_agent_token(pool).await;

    let mut client =
        common::grpc_harness::in_process_agent_client(state, IpAddr::from([10, 2, 0, 2])).await;

    // Can't go through `open_tunnel_request` here — MetadataValue::parse
    // of "" succeeds but the handler's trim-is-empty branch rejects. We
    // build the request manually with the empty value.
    let (_tx, rx) = tokio::sync::mpsc::channel::<TunnelFrame>(1);
    let mut req = Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert("x-meshmon-source-id", "".parse().unwrap());

    let err = client.open_tunnel(req).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");
}
