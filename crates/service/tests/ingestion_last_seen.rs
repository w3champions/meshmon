//! Tests for the debounced `agents.last_seen_at` updater.
//!
//! Both tests use `common::acquire(false)` (a private per-test database)
//! rather than `common::shared_migrated_pool()`. `LastSeenUpdater::spawn`
//! takes an owned `PgPool` and commits writes directly, so the transaction-
//! rollback isolation contract for the shared pool cannot be met. A private
//! database provides equivalent isolation at ~100 ms overhead per test.

mod common;

use meshmon_service::ingestion::last_seen::LastSeenUpdater;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn touch_writes_last_seen() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrate private DB");
    let agent_id = format!("a-{}", uuid::Uuid::new_v4().simple());

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, 'A', '10.0.0.1', 3555, 3552, NOW() - INTERVAL '1 hour')",
    )
    .bind(&agent_id)
    .execute(&db.pool)
    .await
    .unwrap();

    let token = CancellationToken::new();
    let pg_drain_complete = CancellationToken::new();
    let updater = LastSeenUpdater::spawn(
        db.pool.clone(),
        Duration::from_secs(30),
        token.clone(),
        pg_drain_complete.clone(),
    );

    updater.touch(&agent_id, Some("0.2.0".into()));
    token.cancel();
    // No concurrent pg_writer in this unit test — signal drain-done so the
    // updater's idle-exit path can collapse to the grace window.
    pg_drain_complete.cancel();
    updater.join().await;

    let row = sqlx::query!(
        r#"SELECT agent_version, EXTRACT(EPOCH FROM (NOW() - last_seen_at))::DOUBLE PRECISION AS "lag!"
           FROM agents WHERE id = $1"#,
        agent_id,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(row.agent_version.as_deref(), Some("0.2.0"));
    assert!(row.lag < 5.0, "lag {} > 5s", row.lag);

    db.close().await;
}

#[tokio::test]
async fn second_touch_within_debounce_skips_db_write() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrate private DB");
    let agent_id = format!("a-{}", uuid::Uuid::new_v4().simple());

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, last_seen_at) \
         VALUES ($1, 'A', '10.0.0.1', 3555, 3552, NOW() - INTERVAL '1 hour')",
    )
    .bind(&agent_id)
    .execute(&db.pool)
    .await
    .unwrap();

    let token = CancellationToken::new();
    let pg_drain_complete = CancellationToken::new();
    let updater = LastSeenUpdater::spawn(
        db.pool.clone(),
        Duration::from_secs(30),
        token.clone(),
        pg_drain_complete.clone(),
    );

    updater.touch(&agent_id, None);
    // Give the updater task time to process the first touch and write to DB.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_first: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM agents WHERE id = $1")
            .bind(&agent_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();

    updater.touch(&agent_id, None);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after_second: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM agents WHERE id = $1")
            .bind(&agent_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();

    assert_eq!(after_first, after_second);

    token.cancel();
    pg_drain_complete.cancel();
    updater.join().await;

    db.close().await;
}
