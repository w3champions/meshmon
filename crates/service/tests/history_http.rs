//! Integration tests for `/api/history/*` and `/api/campaigns/:id/measurements`.
//!
//! This binary shares the process-wide migrated Postgres pool via
//! [`common::HttpHarness::start`]. Tests pick disjoint agent ids and
//! destination-IP ranges so parallel runs never collide on the
//! `measurements_reuse_idx` or on `campaign_pairs`'s
//! `(campaign_id, source_agent_id, destination_ip, kind)` uniqueness.

mod common;

use serde_json::Value;
use sqlx::query;

#[tokio::test]
async fn history_sources_returns_distinct_agents_with_measurements() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // Seed: two agents with measurements; a third agent with none must
    // not appear in the list.
    query!(
        r#"INSERT INTO agents
             (id, display_name, ip, tcp_probe_port, udp_probe_port)
           VALUES
             ('hist-a', 'Agent A', '10.0.0.1'::inet, 3555, 3552),
             ('hist-b', 'Agent B', '10.0.0.2'::inet, 3555, 3552),
             ('hist-c', 'Agent C', '10.0.0.3'::inet, 3555, 3552)
           ON CONFLICT (id) DO NOTHING"#,
    )
    .execute(pool)
    .await
    .unwrap();

    query!(
        r#"INSERT INTO measurements
             (source_agent_id, destination_ip, protocol, probe_count,
              measured_at, loss_pct, kind)
           VALUES
             ('hist-a', '203.0.113.1'::inet, 'icmp', 10, now(), 0.0, 'campaign'),
             ('hist-b', '203.0.113.2'::inet, 'tcp',  10, now(), 0.0, 'campaign')"#,
    )
    .execute(pool)
    .await
    .unwrap();

    let body: Value = h.get_json("/api/history/sources").await;
    let rows = body.as_array().expect("sources response is an array");

    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|v| v["source_agent_id"].as_str())
        .collect();

    assert!(ids.contains(&"hist-a"), "hist-a must appear; ids = {ids:?}");
    assert!(ids.contains(&"hist-b"), "hist-b must appear; ids = {ids:?}");
    assert!(
        !ids.contains(&"hist-c"),
        "agents without measurements must be excluded; ids = {ids:?}"
    );
}
