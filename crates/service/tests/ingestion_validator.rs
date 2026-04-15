//! Pure-function tests for the ingestion validator. No I/O.

use meshmon_protocol::{AgentMetadata, MetricsBatch, PathMetrics, Protocol, ProtocolHealth};
use meshmon_service::ingestion::validator::{
    validate_metrics, ValidationError, MAX_PATHS_PER_BATCH, MAX_PROBES_PER_WINDOW, MAX_RTT_MICROS,
};

fn good_path(target: &str) -> PathMetrics {
    PathMetrics {
        target_id: target.to_string(),
        protocol: Protocol::Icmp as i32,
        window_start_micros: 1_700_000_000_000_000,
        window_end_micros: 1_700_000_060_000_000,
        probes_sent: 60,
        probes_successful: 60,
        failure_rate: 0.0,
        rtt_avg_micros: 1_000,
        rtt_min_micros: 800,
        rtt_max_micros: 1_500,
        rtt_stddev_micros: 100,
        rtt_p50_micros: 1_000,
        rtt_p95_micros: 1_400,
        rtt_p99_micros: 1_500,
        health: ProtocolHealth::Healthy as i32,
    }
}

fn good_batch() -> MetricsBatch {
    MetricsBatch {
        source_id: "agent-a".into(),
        batch_timestamp_micros: 1_700_000_060_500_000,
        agent_metadata: Some(AgentMetadata {
            version: "0.1.0".into(),
            uptime_secs: 60,
            local_error_count: 0,
            dropped_count: 0,
        }),
        paths: vec![good_path("agent-b")],
    }
}

#[test]
fn happy_path_yields_validated_batch() {
    let v = validate_metrics(good_batch()).expect("validate");
    assert_eq!(v.source_id, "agent-a");
    assert_eq!(v.paths.len(), 1);
    assert_eq!(v.paths[0].target_id, "agent-b");
    assert_eq!(v.paths[0].protocol, Protocol::Icmp);
    assert_eq!(v.agent_version.as_deref(), Some("0.1.0"));
}

#[test]
fn empty_source_id_rejected() {
    let mut b = good_batch();
    b.source_id = String::new();
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::EmptySourceId
    ));
}

#[test]
fn empty_target_id_rejected() {
    let mut b = good_batch();
    b.paths[0].target_id = String::new();
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::EmptyTargetId
    ));
}

#[test]
fn unspecified_protocol_rejected() {
    let mut b = good_batch();
    b.paths[0].protocol = Protocol::Unspecified as i32;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::UnspecifiedProtocol
    ));
}

#[test]
fn failure_rate_above_one_rejected() {
    let mut b = good_batch();
    b.paths[0].failure_rate = 1.5;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::FailureRateOutOfRange { .. }
    ));
}

#[test]
fn negative_failure_rate_rejected() {
    let mut b = good_batch();
    b.paths[0].failure_rate = -0.01;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::FailureRateOutOfRange { .. }
    ));
}

#[test]
fn rtt_above_max_rejected() {
    let mut b = good_batch();
    b.paths[0].rtt_avg_micros = MAX_RTT_MICROS + 1;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::RttOutOfRange { .. }
    ));
}

#[test]
fn probes_sent_above_cap_rejected() {
    let mut b = good_batch();
    b.paths[0].probes_sent = MAX_PROBES_PER_WINDOW + 1;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::ProbeCountOutOfRange { .. }
    ));
}

#[test]
fn probes_successful_exceeds_sent_rejected() {
    let mut b = good_batch();
    b.paths[0].probes_sent = 10;
    b.paths[0].probes_successful = 11;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::ProbesSuccessfulExceedsSent { .. }
    ));
}

#[test]
fn end_before_start_rejected() {
    let mut b = good_batch();
    b.paths[0].window_end_micros = b.paths[0].window_start_micros - 1;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::InvalidWindow { .. }
    ));
}

#[test]
fn too_many_paths_rejected() {
    let mut b = good_batch();
    b.paths = (0..(MAX_PATHS_PER_BATCH + 1))
        .map(|i| good_path(&format!("agent-{i}")))
        .collect();
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::TooManyPaths { .. }
    ));
}

#[test]
fn missing_agent_metadata_rejected() {
    let mut b = good_batch();
    b.agent_metadata = None;
    assert!(matches!(
        validate_metrics(b).unwrap_err(),
        ValidationError::MissingAgentMetadata
    ));
}

use meshmon_protocol::{HopIp, HopSummary, PathSummary, RouteSnapshotRequest};
use meshmon_service::ingestion::validator::{validate_snapshot, MAX_HOPS_PER_SNAPSHOT};

fn good_snapshot() -> RouteSnapshotRequest {
    RouteSnapshotRequest {
        source_id: "agent-a".into(),
        target_id: "agent-b".into(),
        protocol: Protocol::Icmp as i32,
        observed_at_micros: 1_700_000_000_000_000,
        hops: vec![HopSummary {
            position: 1,
            observed_ips: vec![HopIp {
                ip: vec![10, 0, 0, 1].into(),
                frequency: 1.0,
            }],
            avg_rtt_micros: 500,
            stddev_rtt_micros: 50,
            loss_pct: 0.0,
        }],
        path_summary: Some(PathSummary {
            avg_rtt_micros: 500,
            loss_pct: 0.0,
            hop_count: 1,
        }),
    }
}

#[test]
fn snapshot_happy_path() {
    let v = validate_snapshot(good_snapshot()).expect("validate");
    assert_eq!(v.source_id, "agent-a");
    assert_eq!(v.target_id, "agent-b");
    assert_eq!(v.hops.len(), 1);
    assert_eq!(
        v.hops[0].observed_ips[0].ip,
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1))
    );
}

#[test]
fn snapshot_missing_summary_rejected() {
    let mut s = good_snapshot();
    s.path_summary = None;
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::MissingPathSummary
    ));
}

#[test]
fn snapshot_invalid_ip_len_rejected() {
    let mut s = good_snapshot();
    s.hops[0].observed_ips[0].ip = vec![1, 2, 3].into();
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::InvalidHopIp { .. }
    ));
}

#[test]
fn snapshot_hop_frequency_out_of_range_rejected() {
    let mut s = good_snapshot();
    s.hops[0].observed_ips[0].frequency = 1.5;
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::HopFrequencyOutOfRange { .. }
    ));
}

#[test]
fn snapshot_hop_loss_out_of_range_rejected() {
    let mut s = good_snapshot();
    s.hops[0].loss_pct = 1.1;
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::HopLossOutOfRange { .. }
    ));
}

#[test]
fn snapshot_too_many_hops_rejected() {
    let mut s = good_snapshot();
    s.hops = (1..=(MAX_HOPS_PER_SNAPSHOT as u32 + 1))
        .map(|i| HopSummary {
            position: i,
            observed_ips: vec![HopIp {
                ip: vec![10, 0, 0, 1].into(),
                frequency: 1.0,
            }],
            avg_rtt_micros: 100,
            stddev_rtt_micros: 0,
            loss_pct: 0.0,
        })
        .collect();
    s.path_summary.as_mut().unwrap().hop_count = s.hops.len() as u32;
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::TooManyHops { .. }
    ));
}

#[test]
fn snapshot_hop_count_mismatch_rejected() {
    let mut s = good_snapshot();
    s.path_summary.as_mut().unwrap().hop_count = 5;
    assert!(matches!(
        validate_snapshot(s).unwrap_err(),
        ValidationError::HopCountMismatch { .. }
    ));
}
