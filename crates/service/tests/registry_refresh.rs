//! DB-backed tests for the registry. Uses `common::acquire` for DDL
//! isolation (each test owns a throwaway database).

#[path = "common/mod.rs"]
mod common;

use chrono::Duration as ChronoDuration;
use meshmon_service::registry::{refresh_once_for_test, AgentRegistry, RegistrySnapshot};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::time::{timeout, Duration as TokioDuration};
use tokio_util::sync::CancellationToken;

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

#[tokio::test]
async fn initial_load_populates_snapshot() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed(&db.pool, "a", ChronoDuration::zero()).await;

    let reg = AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_secs(60),
        StdDuration::from_secs(5 * 60),
    );
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

    let reg = AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_secs(3600),
        StdDuration::from_secs(5 * 60),
    );
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
    let reg = AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_secs(60),
        StdDuration::from_secs(5 * 60),
    );
    reg.initial_load().await.unwrap();
    let a = reg.snapshot();
    let b = reg.snapshot();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    db.close().await;
}

#[tokio::test]
async fn refresh_loop_picks_up_new_agents_within_one_interval() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let reg = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_millis(100),
        StdDuration::from_secs(5 * 60),
    ));
    reg.initial_load().await.unwrap();

    let token = CancellationToken::new();
    let handle = reg.clone().spawn_refresh(token.clone());

    seed(&db.pool, "bob", ChronoDuration::zero()).await;

    // Poll for up to ~900ms (9 intervals at 100ms cadence). The refresh
    // loop must observe "bob" within this window.
    let mut seen = false;
    for _ in 0..30 {
        tokio::time::sleep(TokioDuration::from_millis(30)).await;
        if reg.snapshot().get("bob").is_some() {
            seen = true;
            break;
        }
    }
    assert!(seen, "refresh loop did not observe new agent");

    token.cancel();
    timeout(TokioDuration::from_secs(2), handle)
        .await
        .expect("refresh task did not exit within 2s")
        .expect("task panicked");

    db.close().await;
}

#[tokio::test]
async fn refresh_loop_exits_promptly_on_cancellation() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    let reg = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_secs(60), // long sleep; cancellation must interrupt
        StdDuration::from_secs(5 * 60),
    ));
    reg.initial_load().await.unwrap();

    let token = CancellationToken::new();
    let handle = reg.clone().spawn_refresh(token.clone());

    token.cancel();
    timeout(TokioDuration::from_millis(500), handle)
        .await
        .expect("refresh loop did not exit within 500ms after cancel")
        .expect("task panicked");

    db.close().await;
}

#[tokio::test]
async fn snapshot_preserved_during_db_outage() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed(&db.pool, "a", ChronoDuration::zero()).await;

    let reg = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_millis(50),
        StdDuration::from_secs(5 * 60),
    ));
    reg.initial_load().await.unwrap();
    assert_eq!(reg.snapshot().len(), 1);

    let token = CancellationToken::new();
    let handle = reg.clone().spawn_refresh(token.clone());

    // Simulate outage: close the pool. `refresh_once` will return
    // `sqlx::Error::PoolClosed`. The loop must keep running and NOT
    // overwrite the snapshot.
    db.pool.close().await;

    // Wait longer than several intervals — snapshot should stay at 1.
    tokio::time::sleep(TokioDuration::from_millis(500)).await;
    assert_eq!(
        reg.snapshot().len(),
        1,
        "snapshot was wiped during DB outage",
    );

    token.cancel();
    timeout(TokioDuration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    db.close().await;
}

use metrics_util::debugging::{DebuggingRecorder, Snapshotter};

fn install_recorder() -> Snapshotter {
    let recorder = DebuggingRecorder::new();
    let snap = recorder.snapshotter();
    // install_recorder errors if one is already installed; fine for tests
    // that run in isolated test binaries.
    let _ = recorder.install();
    snap
}

#[tokio::test]
async fn refresh_emits_state_split_metric() {
    let snapshotter = install_recorder();

    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();
    seed(&db.pool, "fresh", ChronoDuration::zero()).await;
    seed(&db.pool, "stale", ChronoDuration::minutes(-60)).await;

    let reg = AgentRegistry::new(
        db.pool.clone(),
        StdDuration::from_secs(60),
        StdDuration::from_secs(5 * 60),
    );
    reg.initial_load().await.unwrap();

    let snap = snapshotter.snapshot().into_vec();
    let mut active = None;
    let mut stale = None;
    for (key, _unit, _desc, value) in snap {
        let name = key.key().name();
        if name != "meshmon_service_registry_agents_total" {
            continue;
        }
        let labels: Vec<_> = key.key().labels().collect();
        let state_label = labels
            .iter()
            .find(|l| l.key() == "state")
            .map(|l| l.value().to_string());
        if let metrics_util::debugging::DebugValue::Gauge(g) = value {
            match state_label.as_deref() {
                Some("active") => active = Some(g.into_inner()),
                Some("stale") => stale = Some(g.into_inner()),
                _ => {}
            }
        }
    }
    assert_eq!(active, Some(1.0), "active count");
    assert_eq!(stale, Some(1.0), "stale count");

    db.close().await;
}
