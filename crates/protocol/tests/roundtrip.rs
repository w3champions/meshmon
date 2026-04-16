//! Round-trip encoding tests for every top-level message in the schema.
//!
//! For each message, construct a fully-populated instance, encode it, decode
//! it back, and assert equality. This is the "Definition of done" check from
//! T02's scope: any wire-format regression fails here before it escapes the
//! crate.

use meshmon_protocol::{
    ip, AgentMetadata, ConfigResponse, DiffDetection, HopIp, HopSummary, MetricsBatch, PathHealth,
    PathHealthThresholds, PathMetrics, PathSummary, Protocol, ProtocolHealth, ProtocolThresholds,
    RateEntry, RegisterRequest, RegisterResponse, RouteSnapshotRequest, Target, TargetsResponse,
    Windows,
};
use prost::Message;

fn assert_roundtrip<M>(msg: &M)
where
    M: Message + Default + PartialEq + std::fmt::Debug,
{
    let mut buf = Vec::with_capacity(msg.encoded_len());
    msg.encode(&mut buf).expect("encode");
    let decoded = M::decode(&buf[..]).expect("decode");
    assert_eq!(msg, &decoded, "roundtrip mismatch");
}

fn sample_ipv4() -> prost::bytes::Bytes {
    ip::from_ipaddr("170.80.110.90".parse().unwrap())
}

fn sample_ipv6() -> prost::bytes::Bytes {
    ip::from_ipaddr("2001:db8::1".parse().unwrap())
}

#[test]
fn register_request_roundtrip() {
    let msg = RegisterRequest {
        id: "brazil-north".into(),
        display_name: "Brazil North".into(),
        location: "Fortaleza, Brazil".into(),
        ip: sample_ipv4(),
        lat: -3.7,
        lon: -38.5,
        agent_version: "0.1.0".into(),
        tcp_probe_port: 3555,
        udp_probe_port: 3552,
    };
    assert_roundtrip(&msg);
}

#[test]
fn register_response_roundtrip() {
    // Empty message — still worth roundtripping so we notice if someone adds a
    // non-default field without thinking.
    let msg = RegisterResponse::default();
    assert_roundtrip(&msg);
}

#[test]
fn metrics_batch_roundtrip() {
    let path = PathMetrics {
        target_id: "eu-west".into(),
        protocol: Protocol::Icmp as i32,
        window_start_micros: 1_712_000_000_000_000,
        window_end_micros: 1_712_000_060_000_000,
        probes_sent: 12,
        probes_successful: 11,
        failure_rate: 1.0 / 12.0,
        rtt_avg_micros: 42_000,
        rtt_min_micros: 30_000,
        rtt_max_micros: 78_000,
        rtt_stddev_micros: 4_500,
        rtt_p50_micros: 40_000,
        rtt_p95_micros: 60_000,
        rtt_p99_micros: 75_000,
        health: ProtocolHealth::Healthy as i32,
    };
    let msg = MetricsBatch {
        source_id: "brazil-north".into(),
        batch_timestamp_micros: 1_712_000_060_500_000,
        agent_metadata: Some(AgentMetadata {
            version: "0.1.0".into(),
            uptime_secs: 3_600,
            local_error_count: 2,
            dropped_count: 0,
        }),
        paths: vec![
            path.clone(),
            PathMetrics {
                protocol: Protocol::Tcp as i32,
                health: ProtocolHealth::Unhealthy as i32,
                ..path
            },
        ],
    };
    assert_roundtrip(&msg);
}

#[test]
fn route_snapshot_request_roundtrip() {
    let hop = HopSummary {
        position: 1,
        observed_ips: vec![
            HopIp {
                ip: sample_ipv4(),
                frequency: 0.78,
            },
            HopIp {
                ip: sample_ipv6(),
                frequency: 0.22,
            },
        ],
        avg_rtt_micros: 2_100,
        stddev_rtt_micros: 180,
        loss_pct: 0.0,
    };
    let msg = RouteSnapshotRequest {
        source_id: "brazil-north".into(),
        target_id: "eu-west".into(),
        protocol: Protocol::Icmp as i32,
        observed_at_micros: 1_712_000_120_000_000,
        hops: vec![hop],
        path_summary: Some(PathSummary {
            avg_rtt_micros: 45_000,
            loss_pct: 0.01,
            hop_count: 12,
        }),
    };
    assert_roundtrip(&msg);
}

#[test]
fn config_response_roundtrip() {
    let thresholds = ProtocolThresholds {
        unhealthy_trigger_pct: 0.9,
        healthy_recovery_pct: 0.1,
        unhealthy_hysteresis_sec: 60,
        healthy_hysteresis_sec: 120,
    };
    let msg = ConfigResponse {
        enabled_protocols: vec![
            Protocol::Icmp as i32,
            Protocol::Tcp as i32,
            Protocol::Udp as i32,
        ],
        priority: vec![
            Protocol::Icmp as i32,
            Protocol::Tcp as i32,
            Protocol::Udp as i32,
        ],
        rates: vec![
            RateEntry {
                primary: Protocol::Icmp as i32,
                health: PathHealth::Normal as i32,
                icmp_pps: 0.2,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            RateEntry {
                primary: Protocol::Icmp as i32,
                health: PathHealth::Degraded as i32,
                icmp_pps: 1.0,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
        ],
        icmp_thresholds: Some(thresholds),
        tcp_thresholds: Some(ProtocolThresholds {
            unhealthy_trigger_pct: 0.5,
            healthy_recovery_pct: 0.05,
            ..thresholds
        }),
        udp_thresholds: Some(thresholds),
        windows: Some(Windows {
            primary_sec: 300,
            diversity_sec: 900,
        }),
        diff_detection: Some(DiffDetection {
            new_ip_min_freq: 0.2,
            missing_ip_max_freq: 0.05,
            hop_count_change: 1,
            rtt_shift_frac: 0.5,
        }),
        path_health_thresholds: Some(PathHealthThresholds {
            degraded_trigger_pct: 0.02,
            degraded_trigger_sec: 120,
            degraded_min_samples: 30,
            normal_recovery_pct: 0.01,
            normal_recovery_sec: 300,
        }),
        udp_probe_secret: vec![0u8; 8].into(),
        udp_probe_previous_secret: Vec::<u8>::new().into(),
    };
    assert_roundtrip(&msg);
}

#[test]
fn targets_response_roundtrip() {
    let msg = TargetsResponse {
        targets: vec![
            Target {
                id: "brazil-north".into(),
                ip: sample_ipv4(),
                display_name: "Brazil North".into(),
                location: "Fortaleza, Brazil".into(),
                lat: -3.7,
                lon: -38.5,
                tcp_probe_port: 3555,
                udp_probe_port: 3552,
            },
            Target {
                id: "eu-west".into(),
                ip: sample_ipv6(),
                display_name: "EU West".into(),
                location: "Frankfurt".into(),
                lat: 50.1,
                lon: 8.7,
                tcp_probe_port: 3555,
                udp_probe_port: 3552,
            },
        ],
    };
    assert_roundtrip(&msg);
}
