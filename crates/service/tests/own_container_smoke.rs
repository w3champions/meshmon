mod common;

#[tokio::test]
async fn own_container_returns_isolated_timescale_instance() {
    let db = common::own_container().await;

    // Two parallel containers must not collide on cluster-wide state
    // (roles). This creates a role in the dedicated container only.
    sqlx::query("CREATE ROLE t35_smoke_role NOLOGIN")
        .execute(&db.pool)
        .await
        .expect("CREATE ROLE in dedicated cluster must succeed");

    db.close().await;
}
