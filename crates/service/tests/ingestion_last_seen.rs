//! Tests for the debounced `agents.last_seen_at` updater.

mod common;

use meshmon_service::ingestion::last_seen::LastSeenUpdater;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn touch_writes_last_seen() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = format!("a-{}", uuid::Uuid::new_v4().simple());

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, last_seen_at) \
         VALUES ($1, 'A', '10.0.0.1', NOW() - INTERVAL '1 hour')",
    )
    .bind(&agent_id)
    .execute(&pool)
    .await
    .unwrap();

    let token = CancellationToken::new();
    let updater = LastSeenUpdater::spawn(pool.clone(), Duration::from_secs(30), token.clone());

    updater.touch(&agent_id, Some("0.2.0".into()));
    token.cancel();
    updater.join().await;

    let row = sqlx::query!(
        r#"SELECT agent_version, EXTRACT(EPOCH FROM (NOW() - last_seen_at))::DOUBLE PRECISION AS "lag!"
           FROM agents WHERE id = $1"#,
        agent_id,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.agent_version.as_deref(), Some("0.2.0"));
    assert!(row.lag < 5.0, "lag {} > 5s", row.lag);
}

#[tokio::test]
async fn second_touch_within_debounce_skips_db_write() {
    let pool = common::shared_migrated_pool().await.clone();
    let agent_id = format!("a-{}", uuid::Uuid::new_v4().simple());

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, last_seen_at) \
         VALUES ($1, 'A', '10.0.0.1', NOW() - INTERVAL '1 hour')",
    )
    .bind(&agent_id)
    .execute(&pool)
    .await
    .unwrap();

    let token = CancellationToken::new();
    let updater = LastSeenUpdater::spawn(pool.clone(), Duration::from_secs(30), token.clone());

    updater.touch(&agent_id, None);
    // Give the updater task time to process the first touch and write to DB.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_first: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM agents WHERE id = $1")
            .bind(&agent_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    updater.touch(&agent_id, None);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after_second: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM agents WHERE id = $1")
            .bind(&agent_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(after_first, after_second);

    token.cancel();
    updater.join().await;
}
