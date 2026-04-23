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

// ---------------------------------------------------------------------------
// T53c: hostname stamping on history endpoints (three-state)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn history_destinations_stamps_hostname_from_positive_cache() {
    use meshmon_service::hostname::record_positive;
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // Unique agent / destination IPs for this test to avoid collisions.
    let src = "hist-hn-dest-pos-src";
    let dest_ip: std::net::IpAddr = "203.0.113.80".parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.1'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind("203.0.113.80")
    .execute(pool)
    .await
    .unwrap();

    // Seed positive cache.
    record_positive(pool, dest_ip, "dest-pos.example.com")
        .await
        .expect("seed dest hostname");

    let body: serde_json::Value = h
        .get_json(&format!("/api/history/destinations?source={src}"))
        .await;

    let rows = body.as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some("203.0.113.80"))
        .expect("destination 203.0.113.80 not found in response");
    assert_eq!(
        row["hostname"], "dest-pos.example.com",
        "positive-cached hostname missing in destinations: {row}"
    );
}

#[tokio::test]
async fn history_destinations_omits_hostname_on_negative_cache() {
    use meshmon_service::hostname::record_negative;
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-hn-dest-neg-src";
    let dest_ip: std::net::IpAddr = "203.0.113.81".parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.2'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind("203.0.113.81")
    .execute(pool)
    .await
    .unwrap();

    // Seed negative cache.
    record_negative(pool, dest_ip)
        .await
        .expect("seed dest negative hostname");

    let body: serde_json::Value = h
        .get_json(&format!("/api/history/destinations?source={src}"))
        .await;

    let rows = body.as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some("203.0.113.81"))
        .expect("destination 203.0.113.81 not found in response");
    assert!(
        row.get("hostname").is_none(),
        "negative-cached destination must omit hostname: {row}"
    );
}

#[tokio::test]
async fn history_measurements_stamps_destination_hostname_from_positive_cache() {
    use meshmon_service::hostname::record_positive;
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-hn-meas-pos-src";
    let dest_str = "203.0.113.82";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.3'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind(dest_str)
    .execute(pool)
    .await
    .unwrap();

    // Seed positive cache.
    record_positive(pool, dest_ip, "meas-dest.example.com")
        .await
        .expect("seed measurement dest hostname");

    let body: serde_json::Value = h
        .get_json(&format!(
            "/api/history/measurements?source={src}&destination={dest_str}"
        ))
        .await;

    let rows = body.as_array().unwrap();
    assert!(!rows.is_empty(), "expected at least one measurement row");
    for row in rows {
        assert_eq!(
            row["destination_hostname"], "meas-dest.example.com",
            "positive-cached destination_hostname missing in measurements: {row}"
        );
    }
}

#[tokio::test]
async fn history_measurements_omits_destination_hostname_on_negative_cache() {
    use meshmon_service::hostname::record_negative;
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-hn-meas-neg-src";
    let dest_str = "203.0.113.83";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.4'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind(dest_str)
    .execute(pool)
    .await
    .unwrap();

    record_negative(pool, dest_ip)
        .await
        .expect("seed measurement dest negative hostname");

    let body: serde_json::Value = h
        .get_json(&format!(
            "/api/history/measurements?source={src}&destination={dest_str}"
        ))
        .await;

    let rows = body.as_array().unwrap();
    assert!(!rows.is_empty(), "expected at least one measurement row");
    for row in rows {
        assert!(
            row.get("destination_hostname").is_none(),
            "negative-cached destination must omit destination_hostname: {row}"
        );
    }
}

#[tokio::test]
async fn history_destinations_cold_miss_omits_hostname_and_enqueues_resolver() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-hn-dest-cold-src";
    let dest_str = "203.0.113.84";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.5'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind(dest_str)
    .execute(pool)
    .await
    .unwrap();

    // Defensive: make sure no stale cache row exists for this IP before
    // the handler runs, so the test observes a genuine cold miss.
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(dest_str)
        .execute(pool)
        .await
        .unwrap();

    let body: serde_json::Value = h
        .get_json(&format!("/api/history/destinations?source={src}"))
        .await;
    let rows = body.as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some(dest_str))
        .expect("destination 203.0.113.84 not found in response");
    assert!(
        row.get("hostname").is_none(),
        "cold-miss destination must omit hostname: {row}"
    );

    // Resolver should have been enqueued. StubHostnameBackend answers
    // unseeded IPs with NegativeNxDomain, so a processed enqueue writes
    // a negative cache row we can observe.
    assert!(
        common::wait_for_cache_row(pool, dest_ip).await,
        "resolver never wrote a cache row for {dest_ip} — enqueue was skipped"
    );
}

#[tokio::test]
async fn history_measurements_cold_miss_omits_destination_hostname_and_enqueues_resolver() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-hn-meas-cold-src";
    let dest_str = "203.0.113.85";
    let dest_ip: std::net::IpAddr = dest_str.parse().unwrap();

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.6'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, loss_pct, kind) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'campaign')",
    )
    .bind(src)
    .bind(dest_str)
    .execute(pool)
    .await
    .unwrap();

    // Defensive: clear stale cache rows before the handler runs.
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(dest_str)
        .execute(pool)
        .await
        .unwrap();

    let body: serde_json::Value = h
        .get_json(&format!(
            "/api/history/measurements?source={src}&destination={dest_str}"
        ))
        .await;
    let rows = body.as_array().unwrap();
    assert!(!rows.is_empty(), "expected at least one measurement row");
    for row in rows {
        assert!(
            row.get("destination_hostname").is_none(),
            "cold-miss destination must omit destination_hostname: {row}"
        );
    }

    assert!(
        common::wait_for_cache_row(pool, dest_ip).await,
        "resolver never wrote a cache row for {dest_ip} — enqueue was skipped"
    );
}

// IP block for the MTR hop hostname test — disjoint from the destination
// IPs above so it cannot collide on either the `measurements.destination_ip`
// index or the `ip_hostname_cache` primary key.
const MTR_HOP_IP_POS: &str = "10.54.1.1";
const MTR_HOP_IP_NEG: &str = "10.54.1.2";
const MTR_HOP_IP_COLD: &str = "10.54.1.3";
const MTR_HOP_HOSTNAME_POS: &str = "mtr-hop-pos.example.com";

#[tokio::test]
async fn history_measurements_stamps_mtr_hop_hostnames_three_state() {
    // Verifies the inner stamp loop that walks
    // `mtr_hops[*].observed_ips[*].hostname`. Three hops, each carrying
    // one IP in a different cache state, in a single response.
    use meshmon_service::hostname::{record_negative, record_positive};

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let src = "hist-mtr-hop-src";
    // Destination IP is a non-mesh address with no cache row so the
    // destination_hostname assertion can't accidentally mask a hop bug.
    let dest_str = "203.0.113.86";

    sqlx::query(
        "INSERT INTO agents (id, display_name, ip, tcp_probe_port, udp_probe_port) \
         VALUES ($1, $1, '10.0.5.7'::inet, 8002, 8005) ON CONFLICT (id) DO NOTHING",
    )
    .bind(src)
    .execute(pool)
    .await
    .unwrap();

    // Seed one `mtr_traces` row with three hops (matches the HopJson /
    // HopIpJson wire shape — same JSONB column type and serde contract
    // as `route_snapshots.hops`).
    let hops = serde_json::json!([
        {
            "position": 1,
            "observed_ips": [{"ip": MTR_HOP_IP_POS, "freq": 1.0}],
            "avg_rtt_micros": 1000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 2,
            "observed_ips": [{"ip": MTR_HOP_IP_NEG, "freq": 1.0}],
            "avg_rtt_micros": 2000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 3,
            "observed_ips": [{"ip": MTR_HOP_IP_COLD, "freq": 1.0}],
            "avg_rtt_micros": 3000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        }
    ]);
    let mtr_id: i64 =
        sqlx::query_scalar("INSERT INTO mtr_traces (hops) VALUES ($1::jsonb) RETURNING id")
            .bind(&hops)
            .fetch_one(pool)
            .await
            .unwrap();

    // One measurement row pointing at the MTR trace. Kind = `detail_mtr`
    // to mirror the production writer's shape.
    sqlx::query(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, \
              loss_pct, kind, mtr_id) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'detail_mtr', $3)",
    )
    .bind(src)
    .bind(dest_str)
    .bind(mtr_id)
    .execute(pool)
    .await
    .unwrap();

    // Seed hostname cache in the three-state configuration. Use the
    // common helpers so any cold-miss writes from earlier test iterations
    // are drained before the authoritative rows are inserted.
    let pos_ip: std::net::IpAddr = MTR_HOP_IP_POS.parse().unwrap();
    let neg_ip: std::net::IpAddr = MTR_HOP_IP_NEG.parse().unwrap();
    record_positive(pool, pos_ip, MTR_HOP_HOSTNAME_POS)
        .await
        .expect("seed mtr hop positive hostname");
    record_negative(pool, neg_ip)
        .await
        .expect("seed mtr hop negative hostname");
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(MTR_HOP_IP_COLD)
        .execute(pool)
        .await
        .unwrap();

    let body: serde_json::Value = h
        .get_json(&format!(
            "/api/history/measurements?source={src}&destination={dest_str}"
        ))
        .await;

    let rows = body.as_array().unwrap();
    // Find the detail_mtr row — earlier tests in this binary may leave
    // other rows sharing a nearby source, but the destination IP is
    // disjoint so the list should be tight.
    let row = rows
        .iter()
        .find(|r| r["mtr_hops"].is_array())
        .expect("expected at least one row with MTR hops present in response");
    let hops_json = row["mtr_hops"].as_array().expect("mtr_hops is array");
    assert_eq!(hops_json.len(), 3, "expected three hops: {row}");

    // Hop 0 — positive cache hit → hostname populated.
    let hop0_ip0 = &hops_json[0]["observed_ips"][0];
    assert_eq!(
        hop0_ip0["ip"].as_str(),
        Some(MTR_HOP_IP_POS),
        "hop 0 ip mismatch: {row}"
    );
    assert_eq!(
        hop0_ip0["hostname"], MTR_HOP_HOSTNAME_POS,
        "positive-cached MTR hop hostname missing: {row}"
    );

    // Hop 1 — negative cache hit → hostname field absent (skip-none).
    let hop1_ip0 = &hops_json[1]["observed_ips"][0];
    assert_eq!(
        hop1_ip0["ip"].as_str(),
        Some(MTR_HOP_IP_NEG),
        "hop 1 ip mismatch: {row}"
    );
    assert!(
        hop1_ip0.get("hostname").is_none(),
        "negative-cached MTR hop must omit hostname: {row}"
    );

    // Hop 2 — cold miss → hostname field absent.
    let hop2_ip0 = &hops_json[2]["observed_ips"][0];
    assert_eq!(
        hop2_ip0["ip"].as_str(),
        Some(MTR_HOP_IP_COLD),
        "hop 2 ip mismatch: {row}"
    );
    assert!(
        hop2_ip0.get("hostname").is_none(),
        "cold-miss MTR hop must omit hostname: {row}"
    );

    // Cleanup: remove the seeded MTR trace + measurement so re-runs in
    // the same shared pool get a clean slate.
    sqlx::query("DELETE FROM measurements WHERE source_agent_id = $1")
        .bind(src)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM mtr_traces WHERE id = $1")
        .bind(mtr_id)
        .execute(pool)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// T53c: hostname stamping on campaign_measurements (destination + MTR hops)
// ---------------------------------------------------------------------------

/// IPs used across campaign_measurements hostname tests. Disjoint from
/// the `history_measurements` IP block so the shared pool doesn't collide.
const CM_DEST_IP_POS: &str = "10.55.1.1";
const CM_DEST_IP_NEG: &str = "10.55.1.2";
const CM_DEST_IP_COLD: &str = "10.55.1.3";
const CM_HOP_IP_POS: &str = "10.55.2.1";
const CM_HOP_IP_NEG: &str = "10.55.2.2";
const CM_HOP_IP_COLD: &str = "10.55.2.3";
const CM_DEST_HOSTNAME_POS: &str = "cm-dest-pos.example.com";
const CM_HOP_HOSTNAME_POS: &str = "cm-hop-pos.example.com";

/// Three-state coverage for `destination_hostname` on `campaign_measurements`:
/// positive cache hit stamped, negative cache hit omitted, cold miss omitted +
/// enqueued, with a joined MTR trace so hop hostnames are also exercised.
#[tokio::test]
async fn campaign_measurements_stamps_destination_and_hop_hostnames_three_state() {
    use meshmon_service::hostname::{record_negative, record_positive};

    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // Three campaigns, each with one destination in a different cache state,
    // and a joined MTR trace on the third (cold-miss) to verify hop stamping.
    let campaign_id_pos =
        common::seed_minimal_campaign_for_measurements(pool, "cm-hn-agent-pos").await;
    let campaign_id_neg =
        common::seed_minimal_campaign_for_measurements(pool, "cm-hn-agent-neg").await;
    let campaign_id_mtr =
        common::seed_minimal_campaign_for_measurements(pool, "cm-hn-agent-mtr").await;

    // Positive-cache destination.
    common::seed_settled_pair(
        pool,
        campaign_id_pos,
        "cm-hn-agent-pos",
        CM_DEST_IP_POS,
        "campaign",
    )
    .await;
    let pos_dest_ip: std::net::IpAddr = CM_DEST_IP_POS.parse().unwrap();
    record_positive(pool, pos_dest_ip, CM_DEST_HOSTNAME_POS)
        .await
        .expect("seed positive dest hostname");

    // Negative-cache destination.
    common::seed_settled_pair(
        pool,
        campaign_id_neg,
        "cm-hn-agent-neg",
        CM_DEST_IP_NEG,
        "campaign",
    )
    .await;
    let neg_dest_ip: std::net::IpAddr = CM_DEST_IP_NEG.parse().unwrap();
    record_negative(pool, neg_dest_ip)
        .await
        .expect("seed negative dest hostname");

    // Cold-miss destination with a joined MTR trace carrying three hop IPs.
    let hops = serde_json::json!([
        {
            "position": 1,
            "observed_ips": [{"ip": CM_HOP_IP_POS, "freq": 1.0}],
            "avg_rtt_micros": 1000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 2,
            "observed_ips": [{"ip": CM_HOP_IP_NEG, "freq": 1.0}],
            "avg_rtt_micros": 2000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        },
        {
            "position": 3,
            "observed_ips": [{"ip": CM_HOP_IP_COLD, "freq": 1.0}],
            "avg_rtt_micros": 3000,
            "stddev_rtt_micros": 100,
            "loss_pct": 0.0
        }
    ]);
    let mtr_id: i64 =
        sqlx::query_scalar("INSERT INTO mtr_traces (hops) VALUES ($1::jsonb) RETURNING id")
            .bind(&hops)
            .fetch_one(pool)
            .await
            .unwrap();
    // Seed a measurement with the MTR trace and link it to a campaign pair.
    let measurement_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
             (source_agent_id, destination_ip, protocol, probe_count, measured_at, \
              loss_pct, kind, mtr_id) \
         VALUES ($1, $2::inet, 'icmp', 10, now(), 0.0, 'detail_mtr', $3) \
         RETURNING id",
    )
    .bind("cm-hn-agent-mtr")
    .bind(CM_DEST_IP_COLD)
    .bind(mtr_id)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, resolution_state, \
              kind, measurement_id, settled_at) \
         VALUES ($1, $2, $3::inet, 'succeeded', 'detail_mtr'::measurement_kind, $4, now())",
    )
    .bind(campaign_id_mtr)
    .bind("cm-hn-agent-mtr")
    .bind(CM_DEST_IP_COLD)
    .bind(measurement_id)
    .execute(pool)
    .await
    .unwrap();

    // Seed hop hostnames: positive for hop 0, negative for hop 1, nothing for hop 2.
    let hop_pos_ip: std::net::IpAddr = CM_HOP_IP_POS.parse().unwrap();
    let hop_neg_ip: std::net::IpAddr = CM_HOP_IP_NEG.parse().unwrap();
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(CM_HOP_IP_COLD)
        .execute(pool)
        .await
        .unwrap();
    record_positive(pool, hop_pos_ip, CM_HOP_HOSTNAME_POS)
        .await
        .expect("seed positive hop hostname");
    record_negative(pool, hop_neg_ip)
        .await
        .expect("seed negative hop hostname");
    // Defensive: clear cold-miss dest so the handler sees a genuine cold miss.
    sqlx::query("DELETE FROM ip_hostname_cache WHERE ip = $1::inet")
        .bind(CM_DEST_IP_COLD)
        .execute(pool)
        .await
        .unwrap();

    // ---- positive destination ----
    let body: serde_json::Value = h
        .get_json(&format!("/api/campaigns/{campaign_id_pos}/measurements"))
        .await;
    let entries = body["entries"].as_array().expect("entries array");
    let row = entries
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some(CM_DEST_IP_POS))
        .expect("pos destination row not found");
    assert_eq!(
        row["destination_hostname"], CM_DEST_HOSTNAME_POS,
        "positive-cached destination_hostname missing: {row}"
    );

    // ---- negative destination ----
    let body: serde_json::Value = h
        .get_json(&format!("/api/campaigns/{campaign_id_neg}/measurements"))
        .await;
    let entries = body["entries"].as_array().expect("entries array");
    let row = entries
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some(CM_DEST_IP_NEG))
        .expect("neg destination row not found");
    assert!(
        row.get("destination_hostname").is_none(),
        "negative-cached destination_hostname must be absent: {row}"
    );

    // ---- cold-miss destination + mtr hop hostnames ----
    let body: serde_json::Value = h
        .get_json(&format!("/api/campaigns/{campaign_id_mtr}/measurements"))
        .await;
    let entries = body["entries"].as_array().expect("entries array");
    let row = entries
        .iter()
        .find(|r| r["destination_ip"].as_str() == Some(CM_DEST_IP_COLD))
        .expect("cold destination row not found");
    assert!(
        row.get("destination_hostname").is_none(),
        "cold-miss destination_hostname must be absent: {row}"
    );

    // Hop hostname assertions on the MTR row.
    let hops_json = row["mtr_hops"].as_array().expect("mtr_hops array");
    assert_eq!(hops_json.len(), 3, "expected three hops: {row}");

    let hop0_ip0 = &hops_json[0]["observed_ips"][0];
    assert_eq!(
        hop0_ip0["ip"].as_str(),
        Some(CM_HOP_IP_POS),
        "hop 0 ip mismatch: {row}"
    );
    assert_eq!(
        hop0_ip0["hostname"], CM_HOP_HOSTNAME_POS,
        "positive-cached MTR hop hostname missing: {row}"
    );

    let hop1_ip0 = &hops_json[1]["observed_ips"][0];
    assert_eq!(
        hop1_ip0["ip"].as_str(),
        Some(CM_HOP_IP_NEG),
        "hop 1 ip mismatch: {row}"
    );
    assert!(
        hop1_ip0.get("hostname").is_none(),
        "negative-cached MTR hop must omit hostname: {row}"
    );

    let hop2_ip0 = &hops_json[2]["observed_ips"][0];
    assert_eq!(
        hop2_ip0["ip"].as_str(),
        Some(CM_HOP_IP_COLD),
        "hop 2 ip mismatch: {row}"
    );
    assert!(
        hop2_ip0.get("hostname").is_none(),
        "cold-miss MTR hop must omit hostname: {row}"
    );

    // Cold-miss destination should have been enqueued. The stub resolver
    // writes a negative row on processing, which is observable here.
    let cold_dest_ip: std::net::IpAddr = CM_DEST_IP_COLD.parse().unwrap();
    assert!(
        common::wait_for_cache_row(pool, cold_dest_ip).await,
        "resolver never wrote a cache row for {cold_dest_ip} — enqueue was skipped"
    );

    // Cleanup: FK ordering — campaign_pairs → measurements → mtr_traces.
    sqlx::query("DELETE FROM campaign_pairs WHERE measurement_id = $1")
        .bind(measurement_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM measurements WHERE id = $1")
        .bind(measurement_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM mtr_traces WHERE id = $1")
        .bind(mtr_id)
        .execute(pool)
        .await
        .unwrap();
}
