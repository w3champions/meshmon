mod common;

use std::{collections::HashMap, net::IpAddr};

#[tokio::test]
async fn hostnames_for_returns_positive_negative_and_cold_cache_states() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();

    let a: IpAddr = "203.0.113.10".parse().unwrap();
    let b: IpAddr = "203.0.113.11".parse().unwrap();
    let c: IpAddr = "203.0.113.12".parse().unwrap();

    meshmon_service::hostname::record_positive(&mut *tx, a, "mail.example.com")
        .await
        .unwrap();
    meshmon_service::hostname::record_negative(&mut *tx, b)
        .await
        .unwrap();
    // c is not seeded → cold cache.

    let map: HashMap<IpAddr, Option<String>> =
        meshmon_service::hostname::hostnames_for(&mut *tx, &[a, b, c])
            .await
            .unwrap();

    assert_eq!(map.get(&a).cloned(), Some(Some("mail.example.com".into())));
    assert_eq!(map.get(&b).cloned(), Some(None));
    assert!(!map.contains_key(&c));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn hostnames_for_most_recent_row_wins() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();
    let ip: IpAddr = "203.0.113.20".parse().unwrap();

    // Explicit backdated timestamp on the "old" row avoids a same-tick
    // ordering flake: if both inserts used DEFAULT NOW() they could land
    // at an identical timestamp and DISTINCT ON would pick
    // non-deterministically under nextest's parallel execution.
    sqlx::query(
        "INSERT INTO ip_hostname_cache (ip, hostname, resolved_at) \
         VALUES ($1, 'old.example.com', NOW() - INTERVAL '1 second')",
    )
    .bind(ip)
    .execute(&mut *tx)
    .await
    .unwrap();

    meshmon_service::hostname::record_positive(&mut *tx, ip, "new.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&mut *tx, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(Some("new.example.com".into())));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn hostnames_for_ignores_expired_rows() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();
    let ip: IpAddr = "203.0.113.30".parse().unwrap();

    sqlx::query(
        "INSERT INTO ip_hostname_cache (ip, hostname, resolved_at) \
         VALUES ($1, 'stale.example.com', NOW() - INTERVAL '91 days')",
    )
    .bind(ip)
    .execute(&mut *tx)
    .await
    .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&mut *tx, &[ip])
        .await
        .unwrap();
    assert!(!map.contains_key(&ip));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn hostnames_for_canonicalizes_ipv4_mapped_v6() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();
    let plain: IpAddr = "203.0.113.40".parse().unwrap();
    let mapped: IpAddr = "::ffff:203.0.113.40".parse().unwrap();

    meshmon_service::hostname::record_positive(&mut *tx, mapped, "dual.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&mut *tx, &[plain])
        .await
        .unwrap();
    assert_eq!(
        map.get(&plain).cloned(),
        Some(Some("dual.example.com".into()))
    );

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn hostnames_for_supports_ipv6() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();
    let ip: IpAddr = "2001:db8::1".parse().unwrap();

    meshmon_service::hostname::record_positive(&mut *tx, ip, "v6.example.com")
        .await
        .unwrap();

    let map = meshmon_service::hostname::hostnames_for(&mut *tx, &[ip])
        .await
        .unwrap();
    assert_eq!(map.get(&ip).cloned(), Some(Some("v6.example.com".into())));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn hostnames_for_empty_input_returns_empty_map() {
    let pool = common::shared_migrated_pool().await.clone();
    let mut tx = pool.begin().await.unwrap();
    let map = meshmon_service::hostname::hostnames_for(&mut *tx, &[])
        .await
        .unwrap();
    assert!(map.is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn retention_policy_drops_expired_rows() {
    // Retention drops whole chunks. Seed one row at -91 days; give
    // it its own chunk by keeping all other rows in this test out
    // of the same 7-day window.
    //
    // `shared_migrated_pool` creates its database from `template0`, which
    // means the `timescaledb` extension is not installed there. This test
    // needs real TimescaleDB background-job machinery
    // (`timescaledb_information.jobs`, `run_job()`), so it uses
    // `common::acquire(true)` to get a fresh database with the extension
    // explicitly installed, then runs the full migration stack.
    let db = common::acquire(true).await;
    let pool = &db.pool;
    meshmon_service::db::run_migrations(pool)
        .await
        .expect("run_migrations on TimescaleDB");

    let ip: IpAddr = "203.0.113.50".parse().unwrap();

    sqlx::query(
        "INSERT INTO ip_hostname_cache (ip, hostname, resolved_at) \
         VALUES ($1, 'to-be-dropped.example.com', NOW() - INTERVAL '91 days')",
    )
    .bind(ip)
    .execute(pool)
    .await
    .unwrap();

    let pre: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ip_hostname_cache WHERE ip = $1")
        .bind(ip)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(pre, 1);

    // Run the retention job synchronously. Use LIKE to stay robust
    // across TimescaleDB minor-version renames of the proc name.
    let job_id: i32 = sqlx::query_scalar(
        "SELECT job_id::int4 FROM timescaledb_information.jobs \
         WHERE proc_name LIKE 'policy_retention%' \
         AND hypertable_name = 'ip_hostname_cache'",
    )
    .fetch_one(pool)
    .await
    .unwrap();

    sqlx::query("CALL run_job($1)")
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();

    let post: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ip_hostname_cache WHERE ip = $1")
        .bind(ip)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(post, 0, "retention policy should drop expired chunk");

    db.close().await;
}
