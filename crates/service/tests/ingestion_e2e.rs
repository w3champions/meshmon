//! End-to-end ingestion: validated batch → wiremock VM + Postgres assertions.

mod common;

use meshmon_protocol::Protocol;
use meshmon_service::ingestion::validator::{
    ValidHop, ValidObservedIp, ValidPath, ValidSummary, ValidatedMetrics, ValidatedSnapshot,
};
use meshmon_service::ingestion::{IngestionConfig, IngestionPipeline};
use prometheus_reqwest_remote_write::WriteRequest;
use prost::Message;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn validated_metrics(source: &str, target: &str) -> ValidatedMetrics {
    ValidatedMetrics {
        source_id: source.to_string(),
        batch_timestamp_micros: 1_700_000_000_500_000,
        agent_version: Some("0.1.0".into()),
        paths: vec![ValidPath {
            target_id: target.to_string(),
            protocol: Protocol::Icmp,
            window_start_micros: 1_700_000_000_000_000,
            window_end_micros: 1_700_000_060_000_000,
            probes_sent: 60,
            probes_successful: 59,
            failure_rate: 0.0166,
            rtt_avg_micros: 1_000,
            rtt_min_micros: 800,
            rtt_max_micros: 1_500,
            rtt_stddev_micros: 100,
            rtt_p50_micros: 1_000,
            rtt_p95_micros: 1_400,
            rtt_p99_micros: 1_500,
            health: meshmon_protocol::ProtocolHealth::Healthy,
        }],
    }
}

fn validated_snapshot(source: &str, target: &str) -> ValidatedSnapshot {
    ValidatedSnapshot {
        source_id: source.to_string(),
        target_id: target.to_string(),
        protocol: Protocol::Icmp,
        observed_at_micros: 1_700_000_120_000_000,
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

fn metric_names(req: &WriteRequest) -> Vec<String> {
    req.timeseries
        .iter()
        .filter_map(|ts| {
            ts.labels
                .iter()
                .find(|l| l.name == "__name__")
                .map(|l| l.value.clone())
        })
        .collect()
}

#[tokio::test]
async fn full_pipeline_metrics_and_snapshot() {
    let pool = common::shared_migrated_pool().await.clone();
    let server = MockServer::start().await;
    let mock = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(204))
        .mount_as_scoped(&server)
        .await;

    let src = format!("a-{}", uuid::Uuid::new_v4().simple());
    let tgt = format!("a-{}", uuid::Uuid::new_v4().simple());
    for id in [&src, &tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, last_seen_at) \
                     VALUES ($1, 'X', '10.0.0.1', NOW() - INTERVAL '1 hour')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let cfg = IngestionConfig {
        vm_url: format!("{}/api/v1/write", server.uri()),
        vm_batch_size: 32,
        vm_batch_interval: Duration::from_millis(100),
        vm_buffer_capacity: 8_192,
        snapshot_buffer_capacity: 256,
        last_seen_debounce: Duration::from_secs(30),
        vm_max_retry: Duration::from_secs(5),
    };
    let token = CancellationToken::new();
    let pipeline = IngestionPipeline::spawn(cfg, pool.clone(), token.clone());

    pipeline.push_metrics(validated_metrics(&src, &tgt));
    pipeline.push_snapshot(validated_snapshot(&src, &tgt));

    // Poll for the expected metric names to appear in the VM-bound stream.
    // `batch_interval` is 100ms, and the snapshot-driven
    // `meshmon_route_changes_total` sample is only emitted *after* the PG
    // insert lands, so waiting a single interval is not enough.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let all_names: std::collections::HashSet<String> = loop {
        let reqs = mock.received_requests().await;
        let names: std::collections::HashSet<String> = reqs
            .iter()
            .flat_map(|r| {
                let raw = snap::raw::Decoder::new().decompress_vec(&r.body).unwrap();
                metric_names(&WriteRequest::decode(raw.as_slice()).unwrap())
            })
            .collect();
        if names.contains("meshmon_path_rtt_avg_micros")
            && names.contains("meshmon_route_changes_total")
        {
            break names;
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for VM samples; got {names:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        all_names.contains("meshmon_path_rtt_avg_micros"),
        "missing rtt_avg in {all_names:?}"
    );
    assert!(
        all_names.contains("meshmon_route_changes_total"),
        "missing route_changes_total in {all_names:?}"
    );

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM route_snapshots WHERE source_id = $1 AND target_id = $2",
    )
    .bind(&src)
    .bind(&tgt)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);

    // last_seen_at is touched fire-and-forget via the debounced updater;
    // poll until the row is freshly touched (or time out).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let touched: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT last_seen_at FROM agents WHERE id = $1")
                .bind(&src)
                .fetch_one(&pool)
                .await
                .unwrap();
        let now = chrono::Utc::now();
        if (now - touched).num_seconds() < 30 {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("last_seen_at never updated within 5s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    token.cancel();
    pipeline.join().await;
}

/// Regression guard for the pg_writer shutdown-drain path.
///
/// Proves that when the cancellation token fires while a snapshot is still
/// queued, the pg_writer drain loop:
/// 1. Completes the `route_snapshots` INSERT (row appears in the DB), and
/// 2. Pushes the resulting `meshmon_route_changes_total` sample to the vm
///    queue before returning, so the vm_writer's grace-period drain can
///    forward it to the wiremock endpoint.
///
/// Without the vm_writer grace-period loop (added in round-2), vm_writer
/// could exit before pg_writer's shutdown drain finishes pushing, silently
/// dropping the final counter sample. This test would then fail on the
/// `route_changes_total` assertion.
#[tokio::test]
async fn shutdown_drain_inserts_snapshot_and_emits_route_changes_counter() {
    let pool = common::shared_migrated_pool().await;
    let server = MockServer::start().await;

    // Accept all remote-write POSTs with 204; we need to capture both the
    // normal-path and drain-path pushes.
    let mock = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(204))
        .mount_as_scoped(&server)
        .await;

    let src = format!("a-{}", uuid::Uuid::new_v4().simple());
    let tgt = format!("a-{}", uuid::Uuid::new_v4().simple());
    for id in [&src, &tgt] {
        sqlx::query(
            "INSERT INTO agents (id, display_name, ip, last_seen_at) \
                     VALUES ($1, 'X', '10.0.0.1', NOW() - INTERVAL '1 hour')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Tight batch_interval so the vm_writer doesn't sit in the empty-queue
    // wait during normal operation and can quickly enter its drain on cancel.
    let cfg = IngestionConfig {
        vm_url: format!("{}/api/v1/write", server.uri()),
        vm_batch_size: 32,
        vm_batch_interval: Duration::from_millis(50),
        vm_buffer_capacity: 8_192,
        snapshot_buffer_capacity: 256,
        last_seen_debounce: Duration::from_secs(30),
        vm_max_retry: Duration::from_secs(5),
    };
    let token = CancellationToken::new();
    let pipeline = IngestionPipeline::spawn(cfg, pool.clone(), token.clone());

    // Push a snapshot, then immediately trigger shutdown before pg_writer
    // has had a chance to process it. The drain path must handle it.
    pipeline.push_snapshot(validated_snapshot(&src, &tgt));
    token.cancel();

    // join() awaits pg_handle first (producer), then vm_handle (consumer).
    // After join both are fully done — no races.
    pipeline.join().await;

    // Assert 1: drain path completed the INSERT.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM route_snapshots WHERE source_id = $1 AND target_id = $2",
    )
    .bind(&src)
    .bind(&tgt)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        count, 1,
        "route_snapshots row missing — pg_writer drain did not insert"
    );

    // Assert 2: the drain path pushed meshmon_route_changes_total and the
    // vm_writer grace-period drain forwarded it to the mock endpoint.
    let reqs = mock.received_requests().await;
    let names: std::collections::HashSet<String> = reqs
        .iter()
        .flat_map(|r| {
            let raw = snap::raw::Decoder::new().decompress_vec(&r.body).unwrap();
            metric_names(&WriteRequest::decode(raw.as_slice()).unwrap())
        })
        .collect();
    assert!(
        names.contains("meshmon_route_changes_total"),
        "meshmon_route_changes_total not received by wiremock — \
         vm_writer exited before pg_writer drain pushed the sample; \
         got metric names: {names:?}"
    );
}
