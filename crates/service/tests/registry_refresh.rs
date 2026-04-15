//! DB-backed tests for the registry. Uses `common::acquire` for DDL
//! isolation (each test owns a throwaway database).

#[path = "common/mod.rs"]
mod common;

use chrono::Duration as ChronoDuration;
use meshmon_service::registry::{refresh_once_for_test, RegistrySnapshot};
use sqlx::PgPool;

async fn seed(pool: &PgPool, id: &str, offset: ChronoDuration) {
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, last_seen_at) \
         VALUES ($1, $2, '10.0.0.1'::inet, NOW() + $3)",
    )
    .bind(id)
    .bind(format!("Agent {id}"))
    .bind(offset)
    .execute(pool)
    .await
    .expect("insert");
}

#[tokio::test]
async fn refresh_once_loads_every_agent() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    seed(&db.pool, "fresh", ChronoDuration::zero()).await;
    seed(&db.pool, "stale", ChronoDuration::minutes(-60)).await;

    let snap: RegistrySnapshot = refresh_once_for_test(&db.pool).await.expect("refresh");
    assert_eq!(snap.len(), 2);
    assert!(snap.get("fresh").is_some());
    assert!(snap.get("stale").is_some());

    db.close().await;
}

#[tokio::test]
async fn refresh_once_returns_empty_when_no_agents() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let snap = refresh_once_for_test(&db.pool).await.expect("refresh");
    assert_eq!(snap.len(), 0);
    assert!(snap.is_empty());

    db.close().await;
}

use meshmon_service::registry::AgentRegistry;
use std::time::Duration as StdDuration;

#[tokio::test]
async fn initial_load_populates_snapshot() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed(&db.pool, "a", ChronoDuration::zero()).await;

    let reg = AgentRegistry::new(db.pool.clone(), StdDuration::from_secs(60));
    assert!(reg.snapshot().is_empty(), "empty before initial_load");
    reg.initial_load().await.expect("initial_load");
    assert_eq!(reg.snapshot().len(), 1);
    assert!(reg.snapshot().get("a").is_some());

    db.close().await;
}

#[tokio::test]
async fn force_refresh_picks_up_new_agents_immediately() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let reg = AgentRegistry::new(db.pool.clone(), StdDuration::from_secs(3600));
    reg.initial_load().await.expect("initial_load");
    assert!(reg.snapshot().is_empty());

    seed(&db.pool, "new", ChronoDuration::zero()).await;
    reg.force_refresh().await.expect("force_refresh");
    assert_eq!(reg.snapshot().len(), 1);

    db.close().await;
}

#[tokio::test]
async fn snapshot_is_cheap_arc_clone() {
    // Sanity: the `Arc<RegistrySnapshot>` returned by `snapshot()` points
    // at the same allocation when called twice without a refresh.
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let reg = AgentRegistry::new(db.pool.clone(), StdDuration::from_secs(60));
    reg.initial_load().await.unwrap();
    let a = reg.snapshot();
    let b = reg.snapshot();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    db.close().await;
}
