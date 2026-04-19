//! Integration test for the T42 ip_catalogue migration.
//!
//! Verifies that `20260419120000_ip_catalogue.up.sql` correctly:
//!   - Drops `lat` and `lon` from `agents`
//!   - Creates `ip_catalogue` with the full expected column set
//!   - Creates the `agents_with_catalogue` view
//!   - Enforces the `ip_catalogue_ip_key` UNIQUE constraint

mod common;

use meshmon_service::db::run_migrations;

/// Lock shared with `migrations.rs` to serialize `run_migrations` calls
/// that touch the cluster-level `meshmon_grafana` role. Each integration-test
/// binary compiles `common/mod.rs` and this file independently, so this
/// lock is local to the `catalogue_migration` binary — no cross-binary
/// coordination is needed.
static GRAFANA_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const GRAFANA_PASSWORD_ENV: &str = "MESHMON_PG_GRAFANA_PASSWORD";

#[tokio::test]
async fn migration_applies_cleanly_and_drops_agent_geo_columns() {
    let db = common::acquire(false).await;

    // Run all migrations (including the new ip_catalogue migration) under the
    // grafana lock to avoid cluster-level role races with sibling tests.
    {
        let _guard = GRAFANA_TEST_LOCK.lock().await;
        std::env::remove_var(GRAFANA_PASSWORD_ENV);
        run_migrations(&db.pool)
            .await
            .expect("run_migrations must succeed");
    }

    // --- Assertion 1: `agents` no longer has `lat` or `lon` columns --------
    for dropped_col in ["lat", "lon"] {
        let exists: (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name   = 'agents'
                  AND column_name  = $1
            )",
        )
        .bind(dropped_col)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !exists.0,
            "agents.{dropped_col} must have been dropped by the migration"
        );
    }

    // --- Assertion 2: `ip_catalogue` table exists with all required columns -
    let expected_columns = [
        "asn",
        "city",
        "country_code",
        "country_name",
        "created_at",
        "created_by",
        "display_name",
        "enriched_at",
        "enrichment_status",
        "id",
        "ip",
        "latitude",
        "longitude",
        "network_operator",
        "notes",
        "operator_edited_fields",
        "source",
        "website",
    ];

    for col in expected_columns {
        let exists: (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_schema = 'public'
                  AND table_name   = 'ip_catalogue'
                  AND column_name  = $1
            )",
        )
        .bind(col)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            exists.0,
            "ip_catalogue.{col} must exist after the migration"
        );
    }

    // --- Assertion 3: `agents_with_catalogue` view is queryable -------------
    sqlx::query("SELECT * FROM agents_with_catalogue LIMIT 1")
        .execute(&db.pool)
        .await
        .expect("agents_with_catalogue view must be queryable");

    // --- Assertion 4: UNIQUE constraint on `ip` is enforced -----------------
    sqlx::query(
        "INSERT INTO ip_catalogue (id, ip, source, enrichment_status)
         VALUES (gen_random_uuid(), '203.0.113.5', 'operator', 'pending')",
    )
    .execute(&db.pool)
    .await
    .expect("first insert into ip_catalogue must succeed");

    let err = sqlx::query(
        "INSERT INTO ip_catalogue (id, ip, source, enrichment_status)
         VALUES (gen_random_uuid(), '203.0.113.5', 'operator', 'pending')",
    )
    .execute(&db.pool)
    .await
    .expect_err("duplicate ip must violate the UNIQUE constraint");

    let msg = err.to_string();
    assert!(
        msg.contains("ip_catalogue_ip_key"),
        "expected unique constraint violation naming 'ip_catalogue_ip_key', got: {msg}"
    );

    db.close().await;
}
