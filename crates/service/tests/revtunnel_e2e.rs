//! End-to-end reverse-tunnel integration tests (Tasks 15–17).
//!
//! These tests bind a real `TcpListener` and run a full tonic server so
//! the agent's yamux-over-HTTP/2 session exercises the same code path as
//! production (hyper HTTP/2, tonic streaming, yamux multiplexing).
//!
//! # Test matrix
//!
//! | Test | Scenario |
//! |------|----------|
//! | `service_triggers_agent_refresh_via_tunnel` | Happy path: service opens a Channel to the agent, calls `RefreshConfig`, agent's `Notify` fires. |
//! | `agent_reconnects_after_tunnel_dropped`     | Service drops all tunnels; agent's reconnect loop re-establishes within 10 s. |
//! | `active_tunnel_ends_cleanly_on_graceful_shutdown` | Shutdown token fires; agent task exits within 10 s. |

#[path = "common/mod.rs"]
mod common;

use common::{insert_agent, shared_migrated_pool, state_with_agent_token, TEST_AGENT_TOKEN};
use meshmon_agent::command::{AgentCommandService, StubProber};
use meshmon_protocol::{
    AgentApiServer, AgentCommandClient, AgentCommandServer, RefreshConfigRequest,
};
use meshmon_revtunnel::TunnelClient;
use meshmon_service::grpc::{agent_api::AgentApiImpl, MAX_GRPC_DECODING_BYTES};
use meshmon_service::http::auth::agent_grpc_interceptor;
use meshmon_service::state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Endpoint, Server};

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Bind a real TCP listener and run a tonic server serving `AgentApiServer`
/// with the production bearer interceptor attached.
///
/// Returns the bound `SocketAddr`. The server runs until `shutdown_token` is
/// cancelled. Using `serve_with_incoming_shutdown` so tonic drives its own
/// HTTP/2 stack directly over the accepted connections — no TLS, no
/// `auto::Builder` wrapping needed for the test.
async fn spawn_test_service(state: AppState, shutdown_token: CancellationToken) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test service");
    let addr = listener.local_addr().expect("local_addr");

    let impl_ = AgentApiImpl::new(state.clone());
    let sized_server =
        AgentApiServer::new(impl_).max_decoding_message_size(MAX_GRPC_DECODING_BYTES);
    let svc = tonic::service::interceptor::InterceptedService::new(
        sized_server,
        agent_grpc_interceptor(state.clone()),
    );

    let incoming = TcpListenerStream::new(listener);
    let shutdown = async move { shutdown_token.cancelled().await };

    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, shutdown)
            .await;
    });

    addr
}

/// Poll `state.tunnel_manager.len()` every 50 ms until it equals `expected`
/// or the `timeout` expires. Panics on timeout with a descriptive message.
async fn wait_for_tunnel(state: &AppState, expected: usize, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if state.tunnel_manager.len() == expected {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "tunnel_manager.len() never reached {} (currently {})",
                expected,
                state.tunnel_manager.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Spawn the agent-side tunnel task for `source_id` against the service at
/// `addr`. Returns `(refresh_trigger, agent_task_handle, tunnel_cancel)`.
///
/// The caller is responsible for awaiting / aborting `agent_task_handle` and
/// cancelling `tunnel_cancel` for cleanup.
fn spawn_agent(
    addr: SocketAddr,
    source_id: &'static str,
) -> (
    Arc<Notify>,
    tokio::task::JoinHandle<Result<(), meshmon_revtunnel::TunnelError>>,
    CancellationToken,
) {
    let refresh_trigger = Arc::new(Notify::new());
    let tunnel_cancel = CancellationToken::new();

    let handle = {
        let refresh_trigger = refresh_trigger.clone();
        let tunnel_cancel = tunnel_cancel.clone();
        tokio::spawn(async move {
            let channel = Endpoint::try_from(format!("http://{addr}"))
                .unwrap()
                .connect()
                .await
                .expect("agent dials service");

            let router_factory = move || {
                Server::builder().add_service(AgentCommandServer::new(AgentCommandService::new(
                    refresh_trigger.clone(),
                    Arc::new(StubProber),
                    16,
                )))
            };
            TunnelClient::open_and_run(
                channel,
                source_id,
                TEST_AGENT_TOKEN,
                router_factory,
                tunnel_cancel,
            )
            .await
        })
    };

    (refresh_trigger, handle, tunnel_cancel)
}

// ---------------------------------------------------------------------------
// Task 15: happy path — service triggers agent RefreshConfig via the tunnel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn service_triggers_agent_refresh_via_tunnel() {
    // 1. Set up pool and state; insert the agent row so the registry recognises it.
    let pool = shared_migrated_pool().await;
    let state = state_with_agent_token(pool.clone()).await;
    insert_agent(&pool, "e2e-agent").await;
    state
        .registry
        .force_refresh()
        .await
        .expect("registry force_refresh");

    // 2. Spawn the test service (real TcpListener).
    let shutdown_token = CancellationToken::new();
    let addr = spawn_test_service(state.clone(), shutdown_token.clone()).await;

    // 3. Connect the agent tunnel.
    let (refresh_trigger, agent_task, tunnel_cancel) = spawn_agent(addr, "e2e-agent");

    // 4. Wait until the manager sees exactly one tunnel.
    wait_for_tunnel(&state, 1, Duration::from_secs(5)).await;

    // 5. Retrieve the Channel registered under "e2e-agent".
    let snap = state.tunnel_manager.snapshot();
    let channel = snap
        .iter()
        .find(|(id, _)| id == "e2e-agent")
        .map(|(_, ch)| ch.clone())
        .expect("e2e-agent channel not in snapshot");

    // 6. Call RefreshConfig through the tunnel Channel.
    let mut cmd_client = AgentCommandClient::new(channel);
    cmd_client
        .refresh_config(tonic::Request::new(RefreshConfigRequest {}))
        .await
        .expect("RefreshConfig returned error");

    // 7. Assert the Notify fires within 2 s.
    let notified = refresh_trigger.notified();
    tokio::time::timeout(Duration::from_secs(2), notified)
        .await
        .expect("refresh_trigger was not notified within 2 s");

    // 8. Cleanup.
    tunnel_cancel.cancel();
    shutdown_token.cancel();
    let _ = agent_task.await;
}

// ---------------------------------------------------------------------------
// Task 16: reconnect after tunnel drop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_reconnects_after_tunnel_dropped() {
    let pool = shared_migrated_pool().await;
    let state = state_with_agent_token(pool.clone()).await;
    insert_agent(&pool, "e2e-reconnect").await;
    state
        .registry
        .force_refresh()
        .await
        .expect("registry force_refresh");

    let shutdown_token = CancellationToken::new();
    let addr = spawn_test_service(state.clone(), shutdown_token.clone()).await;

    // The agent tunnel task is long-lived (reconnect loop); we don't own a
    // one-shot JoinHandle here — we keep a separate cancel token for the
    // outer reconnect loop by spawning the agent with its own loop.
    //
    // TunnelClient::open_and_run does NOT have a built-in reconnect loop;
    // that lives in `crates/agent/src/tunnel.rs` (AgentRuntime). For this
    // integration test we wrap `open_and_run` in a simple retry loop directly
    // so we avoid the full AgentRuntime dependency.
    let refresh_trigger = Arc::new(Notify::new());
    let tunnel_cancel = CancellationToken::new();

    let handle = {
        let refresh_trigger = refresh_trigger.clone();
        let tunnel_cancel = tunnel_cancel.clone();
        tokio::spawn(async move {
            // Simple reconnect loop: attempt until cancelled.
            loop {
                if tunnel_cancel.is_cancelled() {
                    break Ok::<(), meshmon_revtunnel::TunnelError>(());
                }
                let channel = match Endpoint::try_from(format!("http://{addr}"))
                    .unwrap()
                    .connect()
                    .await
                {
                    Ok(ch) => ch,
                    Err(_) => {
                        if tunnel_cancel.is_cancelled() {
                            break Ok(());
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };

                let rt = refresh_trigger.clone();
                let router_factory = move || {
                    Server::builder().add_service(AgentCommandServer::new(
                        AgentCommandService::new(rt.clone(), Arc::new(StubProber), 16),
                    ))
                };
                // open_and_run returns when the session ends (either cleanly
                // or with an error). We then loop immediately to reconnect.
                let _ = TunnelClient::open_and_run(
                    channel,
                    "e2e-reconnect",
                    TEST_AGENT_TOKEN,
                    router_factory,
                    tunnel_cancel.clone(),
                )
                .await;

                if tunnel_cancel.is_cancelled() {
                    break Ok(());
                }
                // Small back-off before reconnecting.
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    };

    // Wait for the first tunnel to appear.
    wait_for_tunnel(&state, 1, Duration::from_secs(5)).await;

    // Drop all registered channels. The driver task observes the EOF on the
    // yamux session (because the sender half of `out_rx` is gone) and calls
    // `unregister`.
    state.tunnel_manager.close_all();

    // Wait for the driver task to unregister (async, so poll).
    wait_for_tunnel(&state, 0, Duration::from_secs(5)).await;

    // Agent reconnect loop fires after ~200 ms back-off. Allow up to 10 s for
    // the full reconnect cycle on slow CI.
    wait_for_tunnel(&state, 1, Duration::from_secs(10)).await;

    // Verify the new Channel works end-to-end.
    let snap = state.tunnel_manager.snapshot();
    let channel = snap
        .iter()
        .find(|(id, _)| id == "e2e-reconnect")
        .map(|(_, ch)| ch.clone())
        .expect("e2e-reconnect channel not in snapshot after reconnect");

    let mut cmd_client = AgentCommandClient::new(channel);
    cmd_client
        .refresh_config(tonic::Request::new(RefreshConfigRequest {}))
        .await
        .expect("RefreshConfig on new tunnel returned error");

    let notified = refresh_trigger.notified();
    tokio::time::timeout(Duration::from_secs(2), notified)
        .await
        .expect("refresh_trigger not notified after reconnect");

    // Cleanup.
    tunnel_cancel.cancel();
    shutdown_token.cancel();
    let _ = handle.await;
}

// ---------------------------------------------------------------------------
// Task 17: graceful shutdown with an active tunnel
// ---------------------------------------------------------------------------

/// Proves that server-side shutdown alone tears active tunnels down cleanly,
/// without requiring cooperation from the agent side.
///
/// # What this test proves
///
/// `close_all()` cancels the `TunnelManager`'s master token, which cascades
/// into every driver's effective cancel token. The driver exits its select
/// loop, drops the yamux connection (and thereby the `TunnelIo` which owns
/// the outbound mpsc sender). The sender dropping causes the
/// `ReceiverStream<Result<TunnelFrame, Status>>` returned by `accept` to EOF.
/// Tonic observes the response body ending and completes the `OpenTunnel` RPC.
/// The agent's `open_and_run` future then returns, allowing the reconnect loop
/// to proceed.
///
/// `tunnel_cancel` is NOT fired until after we confirm the driver unregistered.
/// It is only used to stop the agent's reconnect loop so the test can finish —
/// in real production, agents keep reconnecting (expected; they succeed when
/// the new service process comes up).
#[tokio::test]
async fn active_tunnel_ends_cleanly_on_graceful_shutdown() {
    let pool = shared_migrated_pool().await;
    let state = state_with_agent_token(pool.clone()).await;
    insert_agent(&pool, "e2e-shutdown").await;
    state
        .registry
        .force_refresh()
        .await
        .expect("registry force_refresh");

    let shutdown_token = CancellationToken::new();
    let addr = spawn_test_service(state.clone(), shutdown_token.clone()).await;

    let (_refresh_trigger, agent_task, tunnel_cancel) = spawn_agent(addr, "e2e-shutdown");

    // Wait for the tunnel to come up.
    wait_for_tunnel(&state, 1, Duration::from_secs(5)).await;

    // Step 1: Server-side shutdown — mirrors production main.rs order.
    // close_all() cancels the master token → drivers exit → response streams
    // EOF → the agent's open_and_run future returns (RPC completes on both
    // sides). We do NOT cancel tunnel_cancel here; the whole point is to
    // prove server-side shutdown alone tears the tunnel down.
    state.tunnel_manager.close_all();

    // Step 2: Wait for the driver to unregister (async — driver task runs
    // concurrently). This is the key assertion: drivers actually exited.
    wait_for_tunnel(&state, 0, Duration::from_secs(5)).await;

    // Step 3: Stop the agent's reconnect loop so the test can finish.
    // In production agents keep reconnecting; here we need the task to exit.
    tunnel_cancel.cancel();

    // Step 4: Stop the service's accept loop.
    shutdown_token.cancel();

    // Step 5: The agent task should exit within 5 s once tunnel_cancel fires.
    let result = tokio::time::timeout(Duration::from_secs(5), agent_task)
        .await
        .expect("agent task did not exit within 5 s after graceful shutdown");

    // The task itself should not have panicked.
    assert!(
        result.is_ok(),
        "agent task panicked: {:?}",
        result.unwrap_err()
    );
}
