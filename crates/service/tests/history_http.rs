//! Integration tests for `/api/history/*` and `/api/campaigns/:id/measurements`.
//!
//! This binary shares the process-wide migrated Postgres pool via
//! [`common::HttpHarness::start`]. Tests pick disjoint agent ids and
//! destination-IP ranges so parallel runs never collide on the
//! `measurements_reuse_idx` or on `campaign_pairs`'s
//! `(campaign_id, source_agent_id, destination_ip, kind)` uniqueness.

mod common;

use chrono::{Duration, Utc};
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

#[tokio::test]
async fn history_destinations_filters_by_source_and_partial_match() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    query!(
        r#"INSERT INTO agents
             (id, display_name, ip, tcp_probe_port, udp_probe_port)
           VALUES
             ('hist-d', 'Agent D', '10.0.1.1'::inet, 3555, 3552)
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
             ('hist-d', '203.0.113.11'::inet, 'icmp', 10, now(), 0.0, 'campaign'),
             ('hist-d', '203.0.113.12'::inet, 'icmp', 10, now(), 0.0, 'campaign'),
             ('hist-d', '198.51.100.9'::inet, 'tcp',  10, now(), 0.0, 'campaign')"#,
    )
    .execute(pool)
    .await
    .unwrap();

    // Full list for the source.
    let body: Value = h.get_json("/api/history/destinations?source=hist-d").await;
    let rows = body.as_array().unwrap();
    let ips: Vec<&str> = rows
        .iter()
        .filter_map(|r| r["destination_ip"].as_str())
        .collect();
    assert!(
        ips.contains(&"203.0.113.11"),
        "full list missing 203.0.113.11; ips = {ips:?}"
    );
    assert!(
        ips.contains(&"203.0.113.12"),
        "full list missing 203.0.113.12; ips = {ips:?}"
    );
    assert!(
        ips.contains(&"198.51.100.9"),
        "full list missing 198.51.100.9; ips = {ips:?}"
    );

    // Partial-match narrowing.
    let body: Value = h
        .get_json("/api/history/destinations?source=hist-d&q=198")
        .await;
    let rows = body.as_array().unwrap();
    let ips: Vec<&str> = rows
        .iter()
        .filter_map(|r| r["destination_ip"].as_str())
        .collect();
    assert!(
        ips.contains(&"198.51.100.9"),
        "filtered list missing 198.51.100.9; ips = {ips:?}"
    );
    assert!(
        !ips.contains(&"203.0.113.11"),
        "filtered list unexpectedly contains 203.0.113.11; ips = {ips:?}"
    );
}

#[tokio::test]
async fn history_measurements_returns_joined_rows_in_range() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    query!(
        r#"INSERT INTO agents
             (id, display_name, ip, tcp_probe_port, udp_probe_port)
           VALUES
             ('hist-e', 'Agent E', '10.0.2.1'::inet, 3555, 3552)
           ON CONFLICT (id) DO NOTHING"#,
    )
    .execute(pool)
    .await
    .unwrap();

    let recent = Utc::now();
    let old = recent - Duration::days(60);

    query!(
        r#"INSERT INTO measurements
             (source_agent_id, destination_ip, protocol, probe_count,
              measured_at, latency_avg_ms, loss_pct, kind)
           VALUES
             ('hist-e', '203.0.113.21'::inet, 'icmp', 10, $1, 12.0, 0.0, 'campaign'),
             ('hist-e', '203.0.113.21'::inet, 'tcp',  10, $1, 15.0, 0.0, 'campaign'),
             ('hist-e', '203.0.113.21'::inet, 'icmp', 10, $2, 99.0, 1.0, 'campaign')"#,
        recent,
        old,
    )
    .execute(pool)
    .await
    .unwrap();

    // Default (no from/to) returns all three rows.
    let body: Value = h
        .get_json("/api/history/measurements?source=hist-e&destination=203.0.113.21")
        .await;
    let rows = body.as_array().unwrap();
    assert_eq!(
        rows.len(),
        3,
        "expected three rows when no range filter; body = {body}"
    );

    // Protocol filter narrows to two ICMP rows.
    let body: Value = h
        .get_json(
            "/api/history/measurements?source=hist-e&destination=203.0.113.21&protocols=icmp",
        )
        .await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2, "expected only icmp rows; body = {body}");
    for r in rows {
        assert_eq!(
            r["protocol"], "icmp",
            "unexpected protocol in filtered row: {r}"
        );
    }
}
