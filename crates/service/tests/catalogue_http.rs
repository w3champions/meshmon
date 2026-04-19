//! Integration tests for the operator catalogue HTTP surface
//! (`POST /api/catalogue`, `GET /api/catalogue`, `GET /api/catalogue/{id}`,
//! `PATCH /api/catalogue/{id}`, `DELETE /api/catalogue/{id}`).
//!
//! The catalogue table is globally unique on `ip`, and this binary
//! shares a Postgres database with every other test in the suite via
//! `common::shared_migrated_pool`. Each test therefore picks IPs from a
//! per-test subrange of `198.51.100.0/24` (RFC 5737 TEST-NET-2) so
//! parallel runs never collide on `ON CONFLICT` bookkeeping.
//!
//! | Test                                            | IP range             |
//! |-------------------------------------------------|----------------------|
//! | `paste_inserts_rows_and_reports_…`              | `198.51.100.11–15`   |
//! | `get_one_returns_row_by_id`                     | `198.51.100.21`      |
//! | `list_supports_country_filter`                  | `198.51.100.41–42`   |
//! | `patch_sets_fields_and_marks_edited`            | `198.51.100.61`      |
//! | `revert_to_auto_removes_mark_and_clears_value`  | `198.51.100.71`      |
//! | `delete_removes_entry`                          | `198.51.100.81`      |
//! | `delete_missing_row_is_idempotent_no_event`     | (random UUID only)   |
//! | `patch_revert_wins_over_concurrent_set`         | `198.51.100.91`      |
//! | `reenrich_sets_pending_and_returns_202`         | `198.51.100.101`     |
//! | `reenrich_many_marks_all_known_ids_pending`     | `198.51.100.103–104` |
//! | `ip_prefix_filter_matches_exact_host_and_cidr`  | `198.51.100.111–113` |
//! | `facets_response_has_expected_array_shape`      | (no seeded IPs)      |

mod common;

use axum::http::StatusCode;
use meshmon_service::catalogue::events::CatalogueEvent;
use sqlx::types::ipnetwork::IpNetwork;
use std::str::FromStr;
use std::time::Duration;

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

/// Helper: resolve a catalogue row id for the given IP via the paste
/// endpoint, whether the row was newly created or already existed.
async fn ensure_row_id(h: &common::HttpHarness, ip: &str) -> String {
    let paste: serde_json::Value = h
        .post_json("/api/catalogue", &serde_json::json!({ "ips": [ip] }))
        .await;
    let row = paste["created"]
        .as_array()
        .and_then(|a| a.first())
        .or_else(|| paste["existing"].as_array().and_then(|a| a.first()))
        .unwrap_or_else(|| panic!("paste surfaced no row for {ip}: {paste}"));
    row["id"].as_str().expect("id is string").to_string()
}

#[tokio::test]
async fn patch_sets_fields_and_marks_edited() {
    let h = common::HttpHarness::start().await;
    let id = ensure_row_id(&h, "198.51.100.61").await;

    // Subscribe to the catalogue broker BEFORE the PATCH so we can assert
    // the handler fans out a `CatalogueEvent::Updated` for the row id
    // without depending on the enrichment runner or other side effects.
    let mut rx = h.state.catalogue_broker.subscribe();

    // Patch display_name + city; the handler must write both columns and
    // append the two PascalCase names to `operator_edited_fields`.
    let body = serde_json::json!({
        "display_name": "Operator-Labelled Host",
        "city": "Berlin",
    });
    let resp: serde_json::Value = h.patch_json(&format!("/api/catalogue/{id}"), &body).await;

    assert_eq!(resp["display_name"], "Operator-Labelled Host");
    assert_eq!(resp["city"], "Berlin");

    let edited: Vec<&str> = resp["operator_edited_fields"]
        .as_array()
        .expect("operator_edited_fields is array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        edited.contains(&"DisplayName"),
        "expected DisplayName in lock set; got {edited:?}"
    );
    assert!(
        edited.contains(&"City"),
        "expected City in lock set; got {edited:?}"
    );

    // Drain the broker until we see `Updated { id }` or time out. Other
    // tests sharing the same process may have published unrelated events
    // onto the broker before we subscribed (the subscription is per-
    // state, so only events newer than `rx.subscribe()` reach us, but we
    // still want to tolerate spurious Created/etc. from unrelated rows
    // should the fan-out wiring change later).
    let expected_id = uuid::Uuid::parse_str(resp["id"].as_str().expect("id is string"))
        .expect("resp id is a valid uuid");
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timed out waiting for Updated event")
            .expect("broker recv failed");
        if let CatalogueEvent::Updated { id: got } = ev {
            assert_eq!(got, expected_id, "Updated event id must match row id");
            break;
        }
    }
}

#[tokio::test]
async fn revert_to_auto_removes_mark_and_clears_value() {
    let h = common::HttpHarness::start().await;
    let id = ensure_row_id(&h, "198.51.100.71").await;

    // Step 1: stamp a city so the lock and the value both exist.
    let after_set: serde_json::Value = h
        .patch_json(
            &format!("/api/catalogue/{id}"),
            &serde_json::json!({ "city": "Munich" }),
        )
        .await;
    assert_eq!(after_set["city"], "Munich");
    let edited: Vec<&str> = after_set["operator_edited_fields"]
        .as_array()
        .expect("operator_edited_fields is array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        edited.contains(&"City"),
        "expected City in lock set after initial patch; got {edited:?}"
    );

    // Step 2: revert City — the value must drop to NULL (absent from
    // the response body since the DTO skips `None`) and the lock must
    // disappear from `operator_edited_fields`.
    let after_revert: serde_json::Value = h
        .patch_json(
            &format!("/api/catalogue/{id}"),
            &serde_json::json!({ "revert_to_auto": ["City"] }),
        )
        .await;
    assert!(
        after_revert.get("city").is_none() || after_revert["city"].is_null(),
        "city must be cleared; body = {after_revert}"
    );
    let edited: Vec<&str> = after_revert["operator_edited_fields"]
        .as_array()
        .expect("operator_edited_fields is array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        !edited.contains(&"City"),
        "City must no longer be in lock set after revert; got {edited:?}"
    );
}

#[tokio::test]
async fn delete_removes_entry() {
    let h = common::HttpHarness::start().await;
    let id = ensure_row_id(&h, "198.51.100.81").await;

    // Subscribe to the broker BEFORE the DELETE so we can assert the
    // handler publishes a `CatalogueEvent::Deleted` carrying the row id.
    let mut rx = h.state.catalogue_broker.subscribe();

    // DELETE must return 204 No Content with an empty body.
    let (status, body) = h.delete(&format!("/api/catalogue/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body = {body}");
    assert!(body.is_empty(), "204 body must be empty, got {body:?}");

    // Subsequent GET of the same id must 404.
    let (status, _body) = h.get(&format!("/api/catalogue/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The delete must have broadcast `Deleted { id }`. Drain until we
    // see the matching event or time out.
    let expected_id = uuid::Uuid::parse_str(&id).expect("id is a valid uuid");
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timed out waiting for Deleted event")
            .expect("broker recv failed");
        if let CatalogueEvent::Deleted { id: got } = ev {
            assert_eq!(got, expected_id, "Deleted event id must match row id");
            break;
        }
    }
}

#[tokio::test]
async fn delete_missing_row_is_idempotent_no_event() {
    let h = common::HttpHarness::start().await;

    // Subscribe first so any stray event would be observable.
    let mut rx = h.state.catalogue_broker.subscribe();

    // Pick a random UUID that was never inserted by any test.
    let missing = uuid::Uuid::new_v4();

    let (status, body) = h.delete(&format!("/api/catalogue/{missing}")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "idempotent delete must 204; body = {body}"
    );
    assert!(body.is_empty(), "204 body must be empty, got {body:?}");

    // No event must fire when the row was already absent. The broker is
    // shared with handlers on this `AppState`, so a narrow 200 ms window
    // is enough to catch an accidental publish.
    let res = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
        res.is_err(),
        "delete of a missing row must not publish any event; observed {:?}",
        res
    );
}

#[tokio::test]
async fn reenrich_sets_pending_and_returns_202() {
    let h = common::HttpHarness::start().await;
    let id = ensure_row_id(&h, "198.51.100.101").await;

    // Pre-set the row to `enriched` so the assertion below tests the
    // *flip* back to `pending`, not an accident of insertion state.
    // Uses `sqlx::query(...)` (dynamic) to stay out of the offline cache.
    sqlx::query(
        "UPDATE ip_catalogue SET enrichment_status = 'enriched', enriched_at = NOW() \
         WHERE id = $1::uuid",
    )
    .bind(&id)
    .execute(&h.state.pool)
    .await
    .expect("seed enriched state");

    // `POST /api/catalogue/{id}/reenrich` has no body — 202 Accepted on
    // success. The endpoint does a synchronous existence check,
    // flips the row back to `pending` via `mark_enrichment_start`, and
    // enqueues on the bounded channel (this test harness drops the
    // receiver via `common::test_enrichment_queue`, so the enqueue
    // silently no-ops — which is exactly why the DB-side flip matters).
    let (status, body) = h.post_empty(&format!("/api/catalogue/{id}/reenrich")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body = {body}");
    assert!(body.is_empty(), "202 body must be empty, got {body:?}");

    // Regression guard for the C-P3 fix: the row must be `pending` now,
    // regardless of whether the enqueue landed on the closed receiver.
    // Without the `mark_enrichment_start` hop the sweep (which only scans
    // `pending`) would never recover a queue-full drop.
    let status_row: String =
        sqlx::query_scalar("SELECT enrichment_status::text FROM ip_catalogue WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&h.state.pool)
            .await
            .expect("status lookup");
    assert_eq!(status_row, "pending", "row must be flipped to pending");

    // Unknown id → 404 with `{"error": "not_found"}`.
    let unknown = uuid::Uuid::new_v4();
    let (status, body) = h
        .post_empty(&format!("/api/catalogue/{unknown}/reenrich"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body = {body}");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("404 body must be JSON envelope");
    assert_eq!(parsed["error"], "not_found", "body = {parsed}");
}

#[tokio::test]
async fn reenrich_many_marks_all_known_ids_pending() {
    let h = common::HttpHarness::start().await;
    let id_a = ensure_row_id(&h, "198.51.100.103").await;
    let id_b = ensure_row_id(&h, "198.51.100.104").await;

    // Both rows start `enriched` so the assertion tests the flip path.
    sqlx::query(
        "UPDATE ip_catalogue SET enrichment_status = 'enriched', enriched_at = NOW() \
         WHERE id = ANY($1::uuid[])",
    )
    .bind([
        uuid::Uuid::parse_str(&id_a).unwrap(),
        uuid::Uuid::parse_str(&id_b).unwrap(),
    ])
    .execute(&h.state.pool)
    .await
    .expect("seed enriched state");

    // Dispatch POST /api/catalogue/reenrich directly on the axum `Service`
    // — the shared `post_json` helper asserts 200, but the bulk endpoint
    // returns 202 with an empty body, so it would panic there.
    use axum::http::{header, Request};
    use tower::util::ServiceExt;
    let unknown = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({ "ids": [&id_a, &unknown, &id_b] });
    let req = Request::builder()
        .method("POST")
        .uri("/api/catalogue/reenrich")
        .header(header::COOKIE, &h.cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .expect("build POST request");
    let resp = h.app.clone().oneshot(req).await.expect("oneshot dispatch");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    // Both known rows must be `pending`; the unknown id is silently no-op'd.
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, enrichment_status::text FROM ip_catalogue WHERE id = ANY($1::uuid[])",
    )
    .bind([
        uuid::Uuid::parse_str(&id_a).unwrap(),
        uuid::Uuid::parse_str(&id_b).unwrap(),
    ])
    .fetch_all(&h.state.pool)
    .await
    .expect("status lookup");
    assert_eq!(rows.len(), 2, "both known rows present");
    for (row_id, status) in rows {
        assert_eq!(status, "pending", "row {row_id} must be flipped to pending");
    }
}

#[tokio::test]
async fn ip_prefix_filter_matches_exact_host_and_cidr() {
    let h = common::HttpHarness::start().await;

    // Seed three distinct rows in TEST-NET-2 so we can assert:
    //   - bare-IP query matches its own /32 (regression guard for the
    //     `<<=` fix; strict `<<` containment would miss this case).
    //   - `/24` prefix matches every row in that /24.
    //   - rows outside the filter are excluded.
    let _: serde_json::Value = h
        .post_json(
            "/api/catalogue",
            &serde_json::json!({
                "ips": ["198.51.100.111", "198.51.100.112", "198.51.100.113"]
            }),
        )
        .await;

    // Bare-host filter — must return exactly the `.111` row.
    let body: serde_json::Value = h.get_json("/api/catalogue?ip_prefix=198.51.100.111").await;
    let ips: Vec<&str> = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|e| e["ip"].as_str())
        .collect();
    assert!(
        ips.contains(&"198.51.100.111"),
        "bare-host ip_prefix must match the /32 row; got {ips:?}"
    );
    assert!(
        !ips.contains(&"198.51.100.112") && !ips.contains(&"198.51.100.113"),
        "bare-host ip_prefix must not match sibling /32 rows; got {ips:?}"
    );

    // `/32` explicit form — same behaviour as the bare host.
    let body: serde_json::Value = h
        .get_json("/api/catalogue?ip_prefix=198.51.100.112/32")
        .await;
    let ips: Vec<&str> = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|e| e["ip"].as_str())
        .collect();
    assert!(ips.contains(&"198.51.100.112"), "got {ips:?}");
    assert!(
        !ips.contains(&"198.51.100.111") && !ips.contains(&"198.51.100.113"),
        "got {ips:?}"
    );

    // `/24` CIDR prefix — all three seeded IPs must be present (and
    // possibly other rows from parallel tests in the same /24).
    let body: serde_json::Value = h.get_json("/api/catalogue?ip_prefix=198.51.100.0/24").await;
    let ips: Vec<&str> = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|e| e["ip"].as_str())
        .collect();
    for expected in ["198.51.100.111", "198.51.100.112", "198.51.100.113"] {
        assert!(
            ips.contains(&expected),
            "/24 CIDR filter must include {expected}; got {ips:?}"
        );
    }
}

#[tokio::test]
async fn facets_response_has_expected_array_shape() {
    let h = common::HttpHarness::start().await;

    // Shape-only assertion: the catalogue table is shared across tests in
    // this binary, so the DB may contain rows seeded by earlier tests. We
    // assert on the SHAPE of the response (every bucket is a JSON array)
    // rather than on emptiness; the four keys must always be present and
    // array-typed regardless of the concrete row count. This matches the
    // facets-as-UI-hint contract — clients render whatever comes back.
    let body: serde_json::Value = h.get_json("/api/catalogue/facets").await;
    for key in ["countries", "asns", "networks", "cities"] {
        assert!(
            body[key].is_array(),
            "facets response must expose {key} as array; body = {body}"
        );
    }
}

#[tokio::test]
async fn patch_revert_wins_over_concurrent_set() {
    let h = common::HttpHarness::start().await;
    let id = ensure_row_id(&h, "198.51.100.91").await;

    // Send a PATCH that simultaneously writes a city value AND reverts
    // the same field. `repo::patch` documents that revert wins: the SQL
    // CASE evaluates the clear branch first, and `operator_edited_fields`
    // subtracts the removed name from the lock set before the union with
    // added names adds it back. The handler mirrors this behavior
    // transparently — so city must end up NULL and `City` must NOT be in
    // `operator_edited_fields`.
    let body = serde_json::json!({
        "city": "Berlin",
        "revert_to_auto": ["City"],
    });
    let resp: serde_json::Value = h.patch_json(&format!("/api/catalogue/{id}"), &body).await;

    assert!(
        resp.get("city").is_none() || resp["city"].is_null(),
        "revert must win over concurrent set — city must be null; body = {resp}"
    );
    let edited: Vec<&str> = resp["operator_edited_fields"]
        .as_array()
        .expect("operator_edited_fields is array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        !edited.contains(&"City"),
        "City must not be locked after revert-wins; got {edited:?}"
    );
}
