//! Wire-compatibility smoke tests for the T45 campaign messages.

use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    MeasurementFailure, MeasurementFailureCode, MeasurementKind, MeasurementResult,
    MeasurementSummary, MeasurementTarget, Protocol, RegisterRequest, RunMeasurementBatchRequest,
};
use prost::Message;

#[test]
fn run_measurement_batch_request_roundtrips() {
    let msg = RunMeasurementBatchRequest {
        batch_id: 42,
        kind: MeasurementKind::Latency as i32,
        protocol: Protocol::Icmp as i32,
        probe_count: 10,
        timeout_ms: 2000,
        probe_stagger_ms: 100,
        targets: vec![MeasurementTarget {
            pair_id: 7,
            destination_ip: vec![203, 0, 113, 9].into(),
            destination_port: 0,
        }],
    };
    let bytes = msg.encode_to_vec();
    let decoded = RunMeasurementBatchRequest::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.batch_id, 42);
    assert_eq!(decoded.targets.len(), 1);
    assert_eq!(decoded.targets[0].pair_id, 7);
}

#[test]
fn measurement_result_success_roundtrips() {
    let msg = MeasurementResult {
        pair_id: 7,
        outcome: Some(Outcome::Success(MeasurementSummary {
            attempted: 10,
            succeeded: 9,
            latency_min_ms: 1.0,
            latency_avg_ms: 2.5,
            latency_median_ms: 2.0,
            latency_p95_ms: 4.0,
            latency_max_ms: 5.0,
            latency_stddev_ms: 1.2,
            loss_ratio: 10.0,
        })),
    };
    let bytes = msg.encode_to_vec();
    let decoded = MeasurementResult::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.pair_id, 7);
    match decoded.outcome {
        Some(Outcome::Success(s)) => assert_eq!(s.succeeded, 9),
        other => panic!("expected success, got {other:?}"),
    }
}

#[test]
fn measurement_result_failure_roundtrips() {
    let msg = MeasurementResult {
        pair_id: 7,
        outcome: Some(Outcome::Failure(MeasurementFailure {
            code: MeasurementFailureCode::Timeout as i32,
            detail: "10/10 probes timed out".into(),
        })),
    };
    let bytes = msg.encode_to_vec();
    let decoded = MeasurementResult::decode(bytes.as_slice()).expect("decode");
    match decoded.outcome {
        Some(Outcome::Failure(f)) => assert_eq!(f.code, MeasurementFailureCode::Timeout as i32),
        other => panic!("expected failure, got {other:?}"),
    }
}

#[test]
fn register_request_campaign_max_concurrency_is_optional() {
    let none = RegisterRequest {
        id: "a".into(),
        display_name: "A".into(),
        location: String::new(),
        ip: vec![127, 0, 0, 1].into(),
        lat: 0.0,
        lon: 0.0,
        agent_version: "0.1.0".into(),
        tcp_probe_port: 7000,
        udp_probe_port: 7001,
        campaign_max_concurrency: None,
    };
    let bytes = none.encode_to_vec();
    let decoded = RegisterRequest::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.campaign_max_concurrency, None);

    let some = RegisterRequest {
        campaign_max_concurrency: Some(32),
        ..none
    };
    let bytes = some.encode_to_vec();
    let decoded = RegisterRequest::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.campaign_max_concurrency, Some(32));
}
