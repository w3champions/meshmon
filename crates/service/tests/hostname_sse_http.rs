//! HTTP-layer integration tests for `/api/hostnames/stream` +
//! `/api/hostnames/{ip}/refresh`.
//!
//! These tests drive the real axum router through a live TCP listener
//! (SSE cannot run under `oneshot`) and verify:
//!
//! 1. An event enqueued for this session arrives on the SSE stream.
//! 2. Events do not leak across sessions.
//! 3. Dropping the stream client-side removes the session from the
//!    broadcaster registry (tracked via `session_count()`).
//! 4. The `/refresh` endpoint returns 202 and the event lands on the
//!    caller's SSE stream.
//!
//! To avoid test-to-server session-id mapping brittleness, every
//! "enqueue a lookup" step goes through `POST /api/hostnames/{ip}/refresh`
//! with the same cookie used to subscribe. The refresh handler reads
//! `AuthSession::session.id()` via the real Axum extractor, so the
//! session id it reports to the resolver always matches whatever the
//! SSE subscription saw.

mod common;

use common::{HttpHarness, StubHostnameBackend};
use futures::StreamExt;
use meshmon_service::hostname::{LookupOutcome, ResolverBackend};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

/// Perform a `POST /api/hostnames/{ip}/refresh` with the given cookie and
/// assert the response is `202 ACCEPTED`. Used as the "enqueue a lookup"
/// trigger in these tests so the session id on the server side is
/// guaranteed to match the cookie attached to the SSE subscription.
async fn refresh_ok(client: &reqwest::Client, base_url: &str, cookie: &str, ip: &str) {
    let url = format!("{base_url}/api/hostnames/{ip}/refresh");
    let resp = client
        .post(&url)
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {url}: {e}"));
    assert_eq!(
        resp.status().as_u16(),
        202,
        "expected 202 from refresh, got {} (url = {url})",
        resp.status(),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_stream_delivers_event_for_self_enqueued_lookup() {
    let ip: std::net::IpAddr = Ipv4Addr::new(203, 0, 113, 10).into();
    let backend = StubHostnameBackend::new();
    backend.set(ip, LookupOutcome::Positive("host-a.example.com".into()));
    let harness =
        HttpHarness::start_with_hostname_resolver(backend as Arc<dyn ResolverBackend>).await;

    // Subscribe first so no event can slip past the subscription.
    let mut sse = harness.sse("/api/hostnames/stream").await;

    refresh_ok(
        harness.client(),
        &harness.base_url(),
        &harness.cookie,
        &ip.to_string(),
    )
    .await;

    let event = timeout(Duration::from_secs(5), sse.next())
        .await
        .expect("SSE event arrives within 5s")
        .expect("stream still open")
        .expect("payload is valid JSON");
    assert_eq!(event["ip"], ip.to_string());
    assert_eq!(event["hostname"], "host-a.example.com");
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_events_do_not_leak_between_sessions() {
    let ip: std::net::IpAddr = Ipv4Addr::new(203, 0, 113, 11).into();
    let backend = StubHostnameBackend::new();
    backend.set(ip, LookupOutcome::Positive("host-b.example.com".into()));
    let harness =
        HttpHarness::start_with_hostname_resolver(backend as Arc<dyn ResolverBackend>).await;

    // Session A: the default admin cookie.
    let cookie_a = harness.cookie.clone();
    // Session B: a second login under a different client IP so
    // tower_sessions issues a distinct session id.
    let cookie_b = harness
        .login_additional_session("203.0.113.201")
        .await;

    let base_url = harness.base_url();

    // Both sessions subscribe before any enqueue happens.
    let mut sse_a = common::subscribe_sse(
        harness.client(),
        &base_url,
        "/api/hostnames/stream",
        &cookie_a,
    )
    .await;
    let mut sse_b = common::subscribe_sse(
        harness.client(),
        &base_url,
        "/api/hostnames/stream",
        &cookie_b,
    )
    .await;

    // Session A enqueues a lookup. Session B must not see the event.
    refresh_ok(harness.client(), &base_url, &cookie_a, &ip.to_string()).await;

    let event = timeout(Duration::from_secs(5), sse_a.next())
        .await
        .expect("session A receives event within 5s")
        .expect("stream still open")
        .expect("payload is valid JSON");
    assert_eq!(event["ip"], ip.to_string());

    // Session B sees nothing for a window generous enough to catch a
    // spurious fanout. The lookup has already completed for A, so any
    // leak would land immediately.
    assert!(
        timeout(Duration::from_millis(500), sse_b.next()).await.is_err(),
        "session B must not receive events scoped to session A",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_disconnect_removes_session_from_broadcaster() {
    let backend = StubHostnameBackend::new();
    let harness =
        HttpHarness::start_with_hostname_resolver(backend as Arc<dyn ResolverBackend>).await;

    let baseline = harness.state.hostname_broadcaster.session_count();
    let sse = harness.sse("/api/hostnames/stream").await;

    // Wait for the server side to process the GET and register the
    // session. A short bounded loop avoids a raw sleep while keeping
    // the test deterministic.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if harness.state.hostname_broadcaster.session_count() == baseline + 1 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "session_count never bumped above {baseline} after subscribe (observed = {})",
                harness.state.hostname_broadcaster.session_count(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Dropping the stream closes the underlying HTTP connection;
    // axum drops the SSE response future; `ReceiverStream` drops the
    // receiver; the `move` closure carrying `SessionHandle` drops;
    // `SessionHandle::drop` removes the entry from the DashMap.
    drop(sse);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if harness.state.hostname_broadcaster.session_count() == baseline {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "session_count never returned to {baseline} after drop (observed = {})",
                harness.state.hostname_broadcaster.session_count(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_endpoint_delivers_event_on_same_session_stream() {
    let ip: std::net::IpAddr = Ipv4Addr::new(203, 0, 113, 12).into();
    let backend = StubHostnameBackend::new();
    backend.set(ip, LookupOutcome::Positive("host-c.example.com".into()));
    let harness =
        HttpHarness::start_with_hostname_resolver(backend as Arc<dyn ResolverBackend>).await;

    let mut sse = harness.sse("/api/hostnames/stream").await;

    // The refresh endpoint extracts AuthSession via the real axum
    // extractor, so the SessionId it reports matches the cookie used
    // for the SSE subscription by construction.
    refresh_ok(
        harness.client(),
        &harness.base_url(),
        &harness.cookie,
        &ip.to_string(),
    )
    .await;

    let event = timeout(Duration::from_secs(5), sse.next())
        .await
        .expect("SSE event arrives within 5s")
        .expect("stream still open")
        .expect("payload is valid JSON");
    assert_eq!(event["ip"], ip.to_string());
    assert_eq!(event["hostname"], "host-c.example.com");
}
