//! DB-backed tests asserting that `agents.campaign_max_concurrency`
//! is plumbed through the registry snapshot. The Register-handler side
//! of the override lives in `agent_api_register.rs`; this binary only
//! exercises the registry load path so the
//! `AgentInfo.campaign_max_concurrency` contract stays visible.

#[path = "common/mod.rs"]
mod common;

use meshmon_service::registry::AgentRegistry;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn register_persists_campaign_max_concurrency_override() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    // Insert an agent row with the override set directly in SQL
    // (handler-level tests live in agent_api_register.rs; here we
    // exercise the registry snapshot contract).
    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, campaign_max_concurrency) \
         VALUES ('agent-override', 'Override', '203.0.113.50'::inet, 7000, 7001, 32) \
         ON CONFLICT (id) DO UPDATE SET campaign_max_concurrency = EXCLUDED.campaign_max_concurrency, \
                                         last_seen_at = now()",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let snap = registry.snapshot();
    let agent = snap.get("agent-override").expect("agent present");
    assert_eq!(agent.campaign_max_concurrency, Some(32));

    db.close().await;
}

#[tokio::test]
async fn register_snapshot_defaults_to_none_when_unset() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ('agent-default', 'Default', '203.0.113.51'::inet, 7000, 7001) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let registry = Arc::new(AgentRegistry::new(
        db.pool.clone(),
        Duration::from_secs(60),
        Duration::from_secs(300),
    ));
    registry.initial_load().await.unwrap();

    let snap = registry.snapshot();
    let agent = snap.get("agent-default").expect("agent present");
    assert_eq!(agent.campaign_max_concurrency, None);

    db.close().await;
}

#[tokio::test]
async fn register_snapshot_rejects_negative_override_at_db_check() {
    // The migration attaches
    //   CHECK (campaign_max_concurrency IS NULL OR campaign_max_concurrency >= 1)
    // so a handler-bypassing INSERT of 0 must be rejected at the DB
    // boundary. This test pins the invariant so a future schema edit
    // that drops the CHECK does not silently allow a permanently
    // starved agent.
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let insert_err = sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port, campaign_max_concurrency) \
         VALUES ('agent-zero', 'Zero', '203.0.113.52'::inet, 7000, 7001, 0)",
    )
    .execute(&db.pool)
    .await;
    assert!(
        insert_err.is_err(),
        "DB must reject campaign_max_concurrency = 0",
    );

    db.close().await;
}
