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
             ('hist-a', 'Agent A', '10.0.0.1'::inet, 8002, 8005),
             ('hist-b', 'Agent B', '10.0.0.2'::inet, 8002, 8005),
             ('hist-c', 'Agent C', '10.0.0.3'::inet, 8002, 8005)
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
             ('hist-d', 'Agent D', '10.0.1.1'::inet, 8002, 8005)
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
             ('hist-e', 'Agent E', '10.0.2.1'::inet, 8002, 8005)
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
        .get_json("/api/history/measurements?source=hist-e&destination=203.0.113.21&protocols=icmp")
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

#[tokio::test]
async fn campaign_measurements_shows_pending_and_settled_rows() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // One campaign + three pairs sharing the same (source, dest):
    //   campaign kind: settled (measurement row + latency)
    //   detail_ping : settled (second measurement row)
    //   detail_mtr  : dispatched, no measurement yet
    let campaign_id = common::seed_minimal_campaign_for_measurements(pool, "camp-agent-1").await;
    common::seed_settled_pair(
        pool,
        campaign_id,
        "camp-agent-1",
        "203.0.113.50",
        "campaign",
    )
    .await;
    common::seed_settled_pair(
        pool,
        campaign_id,
        "camp-agent-1",
        "203.0.113.50",
        "detail_ping",
    )
    .await;
    common::seed_pending_pair(
        pool,
        campaign_id,
        "camp-agent-1",
        "203.0.113.50",
        "detail_mtr",
    )
    .await;

    // No filter — all three rows (including the in-flight detail_mtr).
    let body: Value = h
        .get_json(&format!("/api/campaigns/{campaign_id}/measurements"))
        .await;
    assert_eq!(
        body["entries"].as_array().unwrap().len(),
        3,
        "no-filter page: body = {body}"
    );

    // kind=campaign filters to one row.
    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/measurements?kind=campaign"
        ))
        .await;
    assert_eq!(
        body["entries"].as_array().unwrap().len(),
        1,
        "kind=campaign page: body = {body}"
    );

    // kind=detail_mtr returns the dispatched row with measured_at=null
    // and resolution_state='dispatched'.
    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/measurements?kind=detail_mtr"
        ))
        .await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "detail_mtr page: body = {body}");
    assert!(
        entries[0]["measured_at"].is_null(),
        "dispatched detail_mtr must have null measured_at: {:?}",
        entries[0],
    );
    assert_eq!(
        entries[0]["resolution_state"], "dispatched",
        "detail_mtr row: {:?}",
        entries[0],
    );
    assert_eq!(
        entries[0]["pair_kind"], "detail_mtr",
        "detail_mtr row: {:?}",
        entries[0],
    );
}

#[tokio::test]
async fn history_measurements_rejects_invalid_protocol() {
    let h = common::HttpHarness::start().await;

    // No seeding required — the 400 fires on the parsed query params
    // before the DB is touched.
    let body = h
        .get_expect_status(
            "/api/history/measurements?source=hist-inv&destination=203.0.113.99&protocols=icmp,banana",
            400,
        )
        .await;
    assert_eq!(
        body["error"], "invalid_protocols",
        "expected invalid_protocols envelope; got {body}"
    );
}

#[tokio::test]
async fn history_measurements_rejects_malformed_destination() {
    let h = common::HttpHarness::start().await;

    let body = h
        .get_expect_status(
            "/api/history/measurements?source=hist-malformed&destination=not-an-ip",
            400,
        )
        .await;
    assert_eq!(
        body["error"], "invalid_destination_ip",
        "expected invalid_destination_ip envelope; got {body}"
    );
}

#[tokio::test]
async fn campaign_measurements_cursor_walks_pages() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // Six settled pairs on a shared (source, dest); distinct
    // `measured_at` values one hour apart so keyset ordering is
    // deterministic. The seed helper writes `now()`; we UPDATE after
    // insert to backdate each measurement.
    let campaign_id =
        common::seed_minimal_campaign_for_measurements(pool, "camp-cursor-agent").await;
    let base = Utc::now();
    let mut measurement_ids = Vec::new();
    // Use six distinct pair kinds — the `(campaign_id, source,
    // destination_ip, kind)` unique index forbids duplicates, but by
    // rotating through campaign / detail_ping / detail_mtr across two
    // destinations we get six pairs on the same campaign.
    let rows = [
        ("203.0.113.160", "campaign"),
        ("203.0.113.160", "detail_ping"),
        ("203.0.113.160", "detail_mtr"),
        ("203.0.113.161", "campaign"),
        ("203.0.113.161", "detail_ping"),
        ("203.0.113.161", "detail_mtr"),
    ];
    for (i, (dest, kind)) in rows.iter().enumerate() {
        let (_pair_id, measurement_id) =
            common::seed_settled_pair(pool, campaign_id, "camp-cursor-agent", dest, kind).await;
        let measured_at = base - Duration::hours(i as i64);
        query!(
            "UPDATE measurements SET measured_at = $1 WHERE id = $2",
            measured_at,
            measurement_id,
        )
        .execute(pool)
        .await
        .unwrap();
        measurement_ids.push(measurement_id);
    }

    // Page 1: first 4 rows (page full), cursor present.
    let page1: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/measurements?limit=4"
        ))
        .await;
    let entries1 = page1["entries"].as_array().unwrap();
    assert_eq!(entries1.len(), 4, "page1 size: {page1}");
    let cursor = page1["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("page1 missing next_cursor: {page1}"))
        .to_owned();
    assert!(!cursor.is_empty(), "page1 cursor empty: {page1}");

    // Page 2: remaining 2 rows (page under-full), no further cursor —
    // `next_cursor` is only emitted when `entries.len() == limit`, so
    // a partial page terminates the walk.
    let page2: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/measurements?limit=4&cursor={cursor}",
        ))
        .await;
    let entries2 = page2["entries"].as_array().unwrap();
    assert_eq!(entries2.len(), 2, "page2 size: {page2}");
    assert!(
        page2["next_cursor"].is_null(),
        "page2 next_cursor must be null on final (partial) page: {page2}",
    );

    // Ordering continuity: every page1 measured_at >= every page2
    // measured_at (ORDER BY measured_at DESC).
    let page1_last = entries1
        .last()
        .unwrap()
        .get("measured_at")
        .and_then(|v| v.as_str())
        .expect("page1 last measured_at")
        .to_owned();
    let page2_first = entries2
        .first()
        .unwrap()
        .get("measured_at")
        .and_then(|v| v.as_str())
        .expect("page2 first measured_at")
        .to_owned();
    assert!(
        page1_last >= page2_first,
        "keyset ordering broken: page1_last={page1_last}, page2_first={page2_first}",
    );
}

#[tokio::test]
async fn campaign_measurements_rejects_malformed_cursor() {
    // A malformed `?cursor=` must 400 rather than silently restart at
    // page 1 — the client would otherwise render stale page-1 rows as
    // "next page" and duplicate entries.
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;
    let campaign_id =
        common::seed_minimal_campaign_for_measurements(pool, "camp-bad-cursor-agent").await;
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{campaign_id}/measurements?cursor=not-a-valid-base64-json"
        ))
        .await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "malformed cursor must 400; got {status} with body {body}"
    );
    let parsed: Value = serde_json::from_str(&body).expect("error envelope is JSON");
    assert_eq!(
        parsed["error"], "invalid_cursor",
        "expected invalid_cursor envelope; got {parsed}"
    );
}

#[tokio::test]
async fn campaign_measurements_filters_by_measurement_id() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let campaign_id = common::seed_minimal_campaign_for_measurements(pool, "camp-mid-agent").await;
    // Three settled pairs across three distinct kinds on the same
    // destination — pick the middle one and confirm the handler
    // returns exactly that row.
    let (_p1, _m1) = common::seed_settled_pair(
        pool,
        campaign_id,
        "camp-mid-agent",
        "203.0.113.170",
        "campaign",
    )
    .await;
    let (_p2, m2) = common::seed_settled_pair(
        pool,
        campaign_id,
        "camp-mid-agent",
        "203.0.113.170",
        "detail_ping",
    )
    .await;
    let (_p3, _m3) = common::seed_settled_pair(
        pool,
        campaign_id,
        "camp-mid-agent",
        "203.0.113.170",
        "detail_mtr",
    )
    .await;

    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{campaign_id}/measurements?measurement_id={m2}",
        ))
        .await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        1,
        "measurement_id filter must return exactly one row: {body}",
    );
    assert_eq!(
        entries[0]["measurement_id"].as_i64(),
        Some(m2),
        "row must carry the requested measurement_id: {body}",
    );
}
