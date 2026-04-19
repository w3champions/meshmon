//! Integration tests for the operator catalogue HTTP surface
//! (`POST /api/catalogue`, `GET /api/catalogue`, `GET /api/catalogue/{id}`).
//!
//! The catalogue table is globally unique on `ip`, and this binary
//! shares a Postgres database with every other test in the suite via
//! `common::shared_migrated_pool`. Each test therefore picks IPs from a
//! per-test subrange of `198.51.100.0/24` (RFC 5737 TEST-NET-2) so
//! parallel runs never collide on `ON CONFLICT` bookkeeping.
//!
//! | Test                                  | IP range                 |
//! |---------------------------------------|--------------------------|
//! | `paste_inserts_rows_and_reports_…`    | `198.51.100.11–15`       |
//! | `get_one_returns_row_by_id`           | `198.51.100.21`          |
//! | `list_supports_country_filter`        | `198.51.100.41–42`       |

mod common;

use axum::http::StatusCode;
use sqlx::types::ipnetwork::IpNetwork;
use std::str::FromStr;

#[tokio::test]
async fn paste_inserts_rows_and_reports_invalid() {
    let h = common::HttpHarness::start().await;

    // Mix accepted IPs, a wider CIDR (rejected), and garbage (rejected)
    // so every paste-response bucket is exercised.
    let body = serde_json::json!({
        "ips": [
            "198.51.100.11",
            "198.51.100.12",
            "198.51.100.13/32",
            "10.0.0.0/24",   // wider-than-host → invalid
            "not-an-ip",     // garbage → invalid
        ]
    });

    let resp: serde_json::Value = h.post_json("/api/catalogue", &body).await;

    let created = resp["created"].as_array().expect("created is array");
    let invalid = resp["invalid"].as_array().expect("invalid is array");

    // 3 accepted (11, 12, 13), regardless of whether they already exist
    // in the shared DB from a prior run (created + existing together).
    let existing = resp["existing"].as_array().expect("existing is array");
    assert_eq!(created.len() + existing.len(), 3, "body = {resp}");
    assert_eq!(invalid.len(), 2, "body = {resp}");

    // Invalid reasons must be non-empty strings — the exact labels are
    // stable (see `handlers::reason_label`) but we keep the assertion
    // tolerant so cosmetic relabelling doesn't break the test.
    for entry in invalid {
        assert!(entry["token"].is_string(), "entry = {entry}");
        assert!(
            entry["reason"].as_str().map(str::is_empty) == Some(false),
            "entry = {entry}"
        );
    }
}

#[tokio::test]
async fn get_one_returns_row_by_id() {
    let h = common::HttpHarness::start().await;

    let paste: serde_json::Value = h
        .post_json(
            "/api/catalogue",
            &serde_json::json!({ "ips": ["198.51.100.21"] }),
        )
        .await;

    // The row may have been inserted in a previous run; the created or
    // existing bucket has it regardless.
    let row = paste["created"]
        .as_array()
        .and_then(|a| a.first())
        .or_else(|| paste["existing"].as_array().and_then(|a| a.first()))
        .expect("paste surfaced the row");
    let id = row["id"].as_str().expect("id is string");

    let (status, body) = h.get(&format!("/api/catalogue/{id}")).await;
    assert_eq!(status, StatusCode::OK, "body = {body}");

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
    assert_eq!(parsed["id"], *id, "body = {parsed}");
    assert_eq!(parsed["ip"], "198.51.100.21", "body = {parsed}");

    // Unknown id → 404.
    let unknown = uuid::Uuid::new_v4();
    let (status, _body) = h.get(&format!("/api/catalogue/{unknown}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_supports_country_filter() {
    let h = common::HttpHarness::start().await;

    // Seed two rows so the filter assertion has something to distinguish.
    // Shared-pool note: other tests' rows may coexist, so we can't assert
    // the unfiltered count directly — instead we stamp a per-test country
    // code and assert that the filter surfaces exactly our seeded rows.
    let _: serde_json::Value = h
        .post_json(
            "/api/catalogue",
            &serde_json::json!({ "ips": ["198.51.100.41", "198.51.100.42"] }),
        )
        .await;

    // Seed a unique country code on the first row only via direct SQL.
    // Uses `sqlx::query(...)` (dynamic, not the macro) so the test does
    // not write to the committed `.sqlx/` offline cache.
    //
    // The column is `CHAR(2)` (see migrations/20260419120000_ip_catalogue.up.sql).
    // `ZZ` is an ISO 3166 user-assigned code reserved for private use —
    // real enrichment providers never emit it, so the shared pool can't
    // already contain a row with `country_code = 'ZZ'` from another test.
    const TEST_COUNTRY: &str = "ZZ";
    sqlx::query("UPDATE ip_catalogue SET country_code = $2 WHERE ip = $1")
        .bind(IpNetwork::from_str("198.51.100.41/32").unwrap())
        .bind(TEST_COUNTRY)
        .execute(&h.state.pool)
        .await
        .expect("seed country_code on .41");
    // Belt-and-braces: ensure .42 is NOT stamped with TEST_COUNTRY.
    sqlx::query("UPDATE ip_catalogue SET country_code = NULL WHERE ip = $1")
        .bind(IpNetwork::from_str("198.51.100.42/32").unwrap())
        .execute(&h.state.pool)
        .await
        .expect("clear country_code on .42");

    // Plain GET — no filters — must succeed and include at least our two rows.
    let (status, body) = h.get("/api/catalogue").await;
    assert_eq!(status, StatusCode::OK, "body = {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
    let entries = parsed["entries"].as_array().expect("entries is array");
    assert!(parsed["total"].is_number(), "body = {parsed}");
    let all_ips: Vec<&str> = entries.iter().filter_map(|e| e["ip"].as_str()).collect();
    assert!(
        all_ips.contains(&"198.51.100.41"),
        "unfiltered list should contain .41; got = {all_ips:?}"
    );
    assert!(
        all_ips.contains(&"198.51.100.42"),
        "unfiltered list should contain .42; got = {all_ips:?}"
    );

    // Country filter on the sentinel must surface exactly the stamped row.
    let (status, body) = h
        .get(&format!("/api/catalogue?country_code={TEST_COUNTRY}"))
        .await;
    assert_eq!(status, StatusCode::OK, "body = {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
    let entries = parsed["entries"].as_array().expect("entries is array");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one entry for country_code={TEST_COUNTRY}; body = {parsed}"
    );
    assert_eq!(
        entries[0]["ip"].as_str(),
        Some("198.51.100.41"),
        "filtered entry must be .41; body = {parsed}"
    );
    assert_eq!(
        parsed["total"].as_i64(),
        Some(1),
        "total must be 1 for country_code={TEST_COUNTRY}; body = {parsed}"
    );

    // A country code that matches no rows must return an empty page.
    // `YY` is another ISO 3166 user-assigned code — guaranteed-empty.
    let (status, body) = h.get("/api/catalogue?country_code=YY").await;
    assert_eq!(status, StatusCode::OK, "body = {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
    let entries = parsed["entries"].as_array().expect("entries is array");
    assert_eq!(entries.len(), 0, "expected zero entries; body = {parsed}");
    assert_eq!(
        parsed["total"].as_i64(),
        Some(0),
        "total must be 0 for unknown country; body = {parsed}"
    );
}
