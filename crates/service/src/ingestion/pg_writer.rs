//! Postgres writer for route snapshots.
//!
//! Owns the `INSERT INTO route_snapshots` statement. JSONB shapes live in
//! [`crate::ingestion::json_shapes`] — they are wire-surface for the
//! frontend.
//!
//! Snapshots are inserted as they arrive (no batching). Volume estimate
//! (spec 04): peak ~3700 rows/day. One insert per snapshot is cheap and
//! keeps the JSONB write atomic.

use crate::ingestion::json_shapes::{HopJson, PathSummaryJson};
use crate::ingestion::metrics::pg_snapshot_duration;
use crate::ingestion::protocol_label;
use crate::ingestion::validator::ValidatedSnapshot;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::types::Json;
use sqlx::PgPool;
use std::time::Instant;

/// Insert a validated snapshot. Returns the new row's `id`.
pub async fn insert_snapshot(pool: &PgPool, snap: &ValidatedSnapshot) -> Result<i64, sqlx::Error> {
    let started = Instant::now();
    let observed_at = micros_to_datetime(snap.observed_at_micros);
    let proto_str = protocol_label(snap.protocol);
    let hops: Vec<HopJson> = snap.hops.iter().map(HopJson::from).collect();
    let summary = PathSummaryJson::from(&snap.path_summary);

    let row = sqlx::query!(
        r#"INSERT INTO route_snapshots
            (source_id, target_id, protocol, observed_at, hops, path_summary)
         VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb)
         RETURNING id AS "id!""#,
        snap.source_id,
        snap.target_id,
        proto_str,
        observed_at,
        Json(hops) as Json<Vec<HopJson>>,
        Json(summary) as Json<PathSummaryJson>,
    )
    .fetch_one(pool)
    .await?;

    pg_snapshot_duration().record(started.elapsed().as_secs_f64());
    Ok(row.id)
}

fn micros_to_datetime(micros: i64) -> DateTime<Utc> {
    let secs = micros.div_euclid(1_000_000);
    let nanos = (micros.rem_euclid(1_000_000) * 1_000) as u32;
    Utc.timestamp_opt(secs, nanos)
        .single()
        .unwrap_or_else(Utc::now)
}
