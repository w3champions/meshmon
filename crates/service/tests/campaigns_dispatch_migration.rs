mod common;

use common::acquire;

#[tokio::test]
async fn migration_creates_mtr_traces_and_extends_measurements_and_agents() {
    let db = acquire(/*with_timescale=*/ false).await;
    meshmon_service::db::run_migrations(&db.pool)
        .await
        .expect("migrations apply");

    // mtr_traces exists.
    let table_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'mtr_traces')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(table_exists, "mtr_traces table missing");

    // measurements.mtr_id column + FK.
    let mtr_id_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_name = 'measurements' AND column_name = 'mtr_id')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(mtr_id_exists, "measurements.mtr_id missing");

    let fk_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.table_constraints \
         WHERE constraint_type = 'FOREIGN KEY' \
           AND table_name = 'measurements' \
           AND constraint_name LIKE '%mtr_id%')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(fk_exists, "measurements.mtr_id FK missing");

    // agents.campaign_max_concurrency column.
    let concurrency_col: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_name = 'agents' AND column_name = 'campaign_max_concurrency')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(concurrency_col, "agents.campaign_max_concurrency missing");

    // Round-trip: insert an mtr_trace row, reference from a measurements row.
    let trace_id: i64 =
        sqlx::query_scalar("INSERT INTO mtr_traces (hops) VALUES ('[]'::jsonb) RETURNING id")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let m_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, mtr_id) \
         VALUES ('agent-a', '203.0.113.5', 'icmp', 1, $1) RETURNING id",
    )
    .bind(trace_id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(m_id > 0);

    db.close().await;
}
