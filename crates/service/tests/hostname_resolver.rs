mod common;

use common::{PanicHostnameBackend, StubHostnameBackend};
use meshmon_service::hostname::{
    canonicalize, HostnameBroadcaster, HostnameEvent, LookupOutcome, Resolver, SessionId,
};
use std::{net::IpAddr, time::Duration};
use tokio::sync::mpsc;

async fn recv(rx: &mut mpsc::Receiver<HostnameEvent>, timeout: Duration) -> Option<HostnameEvent> {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .ok()
        .flatten()
}

#[tokio::test]
async fn positive_lookup_writes_row_and_emits_event() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "203.0.113.110".parse().unwrap();
    backend.set(ip, LookupOutcome::Positive("mail.example.com".into()));

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-4");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend.clone(), bcast, pool.clone(), 8);
    resolver.enqueue(ip, session).await;

    let ev = recv(&mut rx, Duration::from_secs(1))
        .await
        .expect("event should fire");
    assert_eq!(ev.ip, ip);
    assert_eq!(ev.hostname, Some("mail.example.com".into()));

    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(Some("mail.example.com".into())));
}

#[tokio::test]
async fn nxdomain_writes_negative_row_and_emits_null_event() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "203.0.113.111".parse().unwrap();
    backend.set(ip, LookupOutcome::NegativeNxDomain);

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-6");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 8);
    resolver.enqueue(ip, session).await;

    let ev = recv(&mut rx, Duration::from_secs(1)).await.expect("event");
    assert_eq!(ev.hostname, None);

    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(None));
}

#[tokio::test]
async fn transient_failure_does_not_write_or_emit() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "203.0.113.112".parse().unwrap();
    backend.set(ip, LookupOutcome::Transient("timeout".into()));

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-7");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 8);
    resolver.enqueue(ip, session).await;

    let ev = recv(&mut rx, Duration::from_millis(300)).await;
    assert!(ev.is_none(), "no event should fire for transient failure");
    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert!(!map.contains_key(&ip), "no row should be written");
}

#[tokio::test]
async fn single_flight_dedups_concurrent_enqueues() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "203.0.113.113".parse().unwrap();
    backend.set_with_delay(
        ip,
        LookupOutcome::Positive("dedup.example.com".into()),
        Duration::from_millis(200),
    );

    let bcast = HostnameBroadcaster::new();
    let a = SessionId::new("t10-8-A");
    let b = SessionId::new("t10-8-B");
    let (_ha, mut rx_a) = bcast.register(a.clone(), 4);
    let (_hb, mut rx_b) = bcast.register(b.clone(), 4);

    let resolver = Resolver::new(backend.clone(), bcast, pool.clone(), 8);
    resolver.enqueue(ip, a).await;
    resolver.enqueue(ip, b).await;

    let ev_a = recv(&mut rx_a, Duration::from_secs(1)).await.unwrap();
    let ev_b = recv(&mut rx_b, Duration::from_secs(1)).await.unwrap();
    assert_eq!(ev_a.hostname, Some("dedup.example.com".into()));
    assert_eq!(ev_b.hostname, Some("dedup.example.com".into()));
    assert_eq!(backend.call_count(ip), 1, "single-flight dedup failed");
}

#[tokio::test]
async fn canonicalizes_v4_mapped_v6_before_enqueue() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let plain: IpAddr = "203.0.113.114".parse().unwrap();
    let mapped: IpAddr = "::ffff:203.0.113.114".parse().unwrap();
    backend.set(plain, LookupOutcome::Positive("canon.example.com".into()));

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-9");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend.clone(), bcast, pool.clone(), 8);
    resolver.enqueue(mapped, session).await;

    let ev = recv(&mut rx, Duration::from_secs(1)).await.expect("event");
    assert_eq!(ev.ip, canonicalize(mapped));
    assert_eq!(backend.call_count(plain), 1);
}

#[tokio::test]
async fn ipv6_roundtrip() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "2001:db8::dead".parse().unwrap();
    backend.set(ip, LookupOutcome::Positive("v6.example.com".into()));

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-10");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 8);
    resolver.enqueue(ip, session).await;

    let ev = recv(&mut rx, Duration::from_secs(1)).await.expect("event");
    assert_eq!(ev.ip, ip);
    assert_eq!(ev.hostname, Some("v6.example.com".into()));
}

#[tokio::test]
async fn private_range_negative_cache() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    backend.set(ip, LookupOutcome::NegativeNxDomain);

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-11");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 8);
    resolver.enqueue(ip, session).await;

    let ev = recv(&mut rx, Duration::from_secs(1)).await.expect("event");
    assert_eq!(ev.hostname, None);
}

#[tokio::test]
async fn semaphore_bounds_concurrency() {
    use std::time::Instant;

    let pool = common::shared_migrated_pool().await.clone();
    let backend = StubHostnameBackend::new();

    // Four distinct IPs, each sleeping 100 ms. With max_in_flight=1,
    // wall clock should be ~400 ms (serialised), not ~100 ms (parallel).
    let ips: Vec<IpAddr> = (0..4)
        .map(|i| format!("203.0.113.{}", 150 + i).parse().unwrap())
        .collect();
    for ip in &ips {
        backend.set_with_delay(
            *ip,
            LookupOutcome::Positive("x".into()),
            Duration::from_millis(100),
        );
    }

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-12");
    let (_handle, mut rx) = bcast.register(session.clone(), 16);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 1);
    let start = Instant::now();
    for ip in &ips {
        resolver.enqueue(*ip, session.clone()).await;
    }

    let mut received = 0usize;
    while received < 4 {
        if recv(&mut rx, Duration::from_secs(5)).await.is_some() {
            received += 1;
        } else {
            panic!("timed out waiting for events (received {received})");
        }
    }

    let elapsed = start.elapsed();
    // Four 100ms backends under max_in_flight=1 must serialise, so the
    // wall-clock floor is ~300ms (75% of ideal — leaves headroom for
    // noisy CI schedulers while still detecting parallel execution).
    assert!(
        elapsed >= Duration::from_millis(300),
        "expected serialised ~400ms; got {elapsed:?}"
    );
}

#[tokio::test]
async fn panic_in_backend_is_contained() {
    let pool = common::shared_migrated_pool().await.clone();
    let backend = PanicHostnameBackend::new();
    let ip: IpAddr = "203.0.113.160".parse().unwrap();

    let bcast = HostnameBroadcaster::new();
    let session = SessionId::new("t10-13");
    let (_handle, mut rx) = bcast.register(session.clone(), 4);

    let resolver = Resolver::new(backend, bcast, pool.clone(), 8);

    // First enqueue: backend panics, pending record drops, no event.
    resolver.enqueue(ip, session.clone()).await;
    let first = recv(&mut rx, Duration::from_millis(200)).await;
    assert!(first.is_none(), "first attempt should emit no event");

    // Second enqueue: backend succeeds, event flows.
    tokio::time::sleep(Duration::from_millis(50)).await; // let first spawn complete
    resolver.enqueue(ip, session).await;
    let second = recv(&mut rx, Duration::from_secs(1)).await;
    assert!(
        second.is_some(),
        "second attempt should succeed after panic"
    );
    assert_eq!(
        second.unwrap().hostname,
        Some("recovered.example.com".into()),
        "pending record must have been cleared"
    );
}
