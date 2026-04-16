//! `route_snapshots` insert path.

mod common;

use meshmon_protocol::Protocol;
use meshmon_service::ingestion::pg_writer::insert_snapshot;
use meshmon_service::ingestion::validator::{
    ValidHop, ValidObservedIp, ValidSummary, ValidatedSnapshot,
};
use serde_json::Value;
use sqlx::Row;

fn validated() -> ValidatedSnapshot {
    ValidatedSnapshot {
        source_id: "agent-a".to_string(),
        target_id: "agent-b".to_string(),
        protocol: Protocol::Icmp,
        observed_at_micros: 1_700_000_000_000_000,
        hops: vec![ValidHop {
            position: 1,
            observed_ips: vec![ValidObservedIp {
                ip: "10.0.0.1".parse().unwrap(),
                frequency: 1.0,
            }],
            avg_rtt_micros: 500,
            stddev_rtt_micros: 50,
            loss_pct: 0.0,
        }],
        path_summary: ValidSummary {
            avg_rtt_micros: 500,
            loss_pct: 0.0,
            hop_count: 1,
        },
    }
}

#[tokio::test]
async fn insert_writes_row_with_jsonb_hops() {
    let pool = common::shared_migrated_pool().await.clone();

    let src = format!("a-{}", uuid::Uuid::new_v4().simple());
    let tgt = format!("a-{}", uuid::Uuid::new_v4().simple());
    for id in [&src, &tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ($1, 'X', '10.0.0.1', 3555, 3552)",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut s = validated();
    s.source_id = src.clone();
    s.target_id = tgt.clone();
    let id = insert_snapshot(&pool, &s).await.expect("insert");
    assert!(id > 0);

    let row = sqlx::query("SELECT protocol, hops, path_summary FROM route_snapshots WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let proto: String = row.try_get("protocol").unwrap();
    assert_eq!(proto, "icmp");
    let hops: Value = row.try_get("hops").unwrap();
    assert_eq!(hops[0]["position"], 1);
    assert_eq!(hops[0]["observed_ips"][0]["ip"], "10.0.0.1");
    let summary: Value = row.try_get("path_summary").unwrap();
    assert_eq!(summary["hop_count"], 1);
}

#[tokio::test]
async fn insert_rejects_out_of_range_observed_at() {
    let pool = common::shared_migrated_pool().await;

    let src = format!("a-{}", uuid::Uuid::new_v4().simple());
    let tgt = format!("a-{}", uuid::Uuid::new_v4().simple());
    for id in [&src, &tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
             VALUES ($1, 'X', '10.0.0.1', 3555, 3552)",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut s = validated(); // reuse existing helper
    s.source_id = src;
    s.target_id = tgt;
    s.observed_at_micros = i64::MAX; // overflows chrono's DateTime<Utc> range

    let err = insert_snapshot(&pool, &s)
        .await
        .expect_err("out-of-range observed_at should error");
    // The error surface is whatever insert_snapshot returns.
    // Assert the error stringifies with the expected marker.
    let msg = format!("{err:?}");
    assert!(
        msg.contains("outside chrono's representable range"),
        "unexpected error: {msg}"
    );
}
