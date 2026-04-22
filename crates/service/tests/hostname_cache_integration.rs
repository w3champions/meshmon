mod common;

use std::{collections::HashMap, net::IpAddr};

#[tokio::test]
async fn hostnames_for_returns_positive_negative_and_cold_cache_states() {
    let pool = common::shared_migrated_pool().await.clone();

    let a: IpAddr = "203.0.113.10".parse().unwrap();
    let b: IpAddr = "203.0.113.11".parse().unwrap();
    let c: IpAddr = "203.0.113.12".parse().unwrap();

    meshmon_service::hostname::record_positive(&pool, a, "mail.example.com")
        .await
        .unwrap();
    meshmon_service::hostname::record_negative(&pool, b)
        .await
        .unwrap();
    // c is not seeded → cold cache.

    let map: HashMap<IpAddr, Option<String>> =
        meshmon_service::hostname::hostnames_for(&pool, &[a, b, c])
            .await
            .unwrap();

    assert_eq!(map.get(&a).cloned(), Some(Some("mail.example.com".into())));
    assert_eq!(map.get(&b).cloned(), Some(None));
    assert!(!map.contains_key(&c));
}

#[tokio::test]
async fn hostnames_for_most_recent_row_wins() {
    let pool = common::shared_migrated_pool().await.clone();
    let ip: IpAddr = "203.0.113.20".parse().unwrap();

    meshmon_service::hostname::record_positive(&pool, ip, "old.example.com")
        .await
        .unwrap();
    meshmon_service::hostname::record_positive(&pool, ip, "new.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(Some("new.example.com".into())));
}

#[tokio::test]
async fn hostnames_for_ignores_expired_rows() {
    let pool = common::shared_migrated_pool().await.clone();
    let ip: IpAddr = "203.0.113.30".parse().unwrap();

    sqlx::query(
        "INSERT INTO ip_hostname_cache (ip, hostname, resolved_at) \
         VALUES ($1, 'stale.example.com', NOW() - INTERVAL '91 days')",
    )
    .bind(ip)
    .execute(&pool)
    .await
    .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert!(!map.contains_key(&ip));
}

#[tokio::test]
async fn hostnames_for_canonicalizes_ipv4_mapped_v6() {
    let pool = common::shared_migrated_pool().await.clone();
    let plain: IpAddr = "203.0.113.40".parse().unwrap();
    let mapped: IpAddr = "::ffff:203.0.113.40".parse().unwrap();

    meshmon_service::hostname::record_positive(&pool, mapped, "dual.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&pool, &[plain])
        .await
        .unwrap();
    assert_eq!(
        map.get(&plain).cloned(),
        Some(Some("dual.example.com".into()))
    );
}

#[tokio::test]
async fn hostnames_for_supports_ipv6() {
    let pool = common::shared_migrated_pool().await.clone();
    let ip: IpAddr = "2001:db8::1".parse().unwrap();

    meshmon_service::hostname::record_positive(&pool, ip, "v6.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&pool, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(Some("v6.example.com".into())));
}

#[tokio::test]
async fn hostnames_for_empty_input_returns_empty_map() {
    let pool = common::shared_migrated_pool().await.clone();
    let map = meshmon_service::hostname::hostnames_for(&pool, &[])
        .await
        .unwrap();
    assert!(map.is_empty());
}
