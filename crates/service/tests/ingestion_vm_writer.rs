//! VM writer batching, snappy/protobuf encoding, and retry behavior.

mod common;

use meshmon_service::ingestion::queue::DropOldest;
use meshmon_service::ingestion::vm_writer::{run as vm_run, PromSample, VmWriterCfg};
use prometheus_reqwest_remote_write::WriteRequest;
use prost::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample(name: &str, value: f64, ts_ms: i64) -> PromSample {
    PromSample {
        metric: name.to_string(),
        labels: vec![],
        value,
        timestamp_ms: ts_ms,
    }
}

fn decode_write_request(body: &[u8]) -> WriteRequest {
    let raw = snap::raw::Decoder::new()
        .decompress_vec(body)
        .expect("snappy decode");
    WriteRequest::decode(raw.as_slice()).expect("protobuf decode")
}

#[tokio::test]
async fn samples_reach_fake_vm_in_a_batch() {
    let server = MockServer::start().await;
    let mock = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .and(header("Content-Encoding", "snappy"))
        .and(header("Content-Type", "application/x-protobuf"))
        .respond_with(ResponseTemplate::new(204))
        .mount_as_scoped(&server)
        .await;

    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(1024));
    let token = CancellationToken::new();
    let cfg = VmWriterCfg {
        url: format!("{}/api/v1/write", server.uri()),
        batch_size: 100,
        batch_interval: Duration::from_millis(100),
        max_retry: Duration::from_secs(1),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        async move { vm_run(q, cfg, t).await }
    });

    for i in 0..3 {
        queue.push(sample(
            "meshmon_path_failure_rate",
            i as f64,
            1_700_000_000_000 + i,
        ));
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let reqs = mock.received_requests().await;
    assert!(!reqs.is_empty(), "no POSTs landed");
    let total_samples: usize = reqs
        .iter()
        .map(|r| {
            decode_write_request(&r.body)
                .timeseries
                .iter()
                .map(|ts| ts.samples.len())
                .sum::<usize>()
        })
        .sum();
    assert_eq!(total_samples, 3);

    token.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn flushes_when_batch_size_reached() {
    let server = MockServer::start().await;
    let mock = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(204))
        .mount_as_scoped(&server)
        .await;

    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(1024));
    let token = CancellationToken::new();
    let cfg = VmWriterCfg {
        url: format!("{}/api/v1/write", server.uri()),
        batch_size: 10,
        batch_interval: Duration::from_secs(60),
        max_retry: Duration::from_secs(1),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        async move { vm_run(q, cfg, t).await }
    });

    for i in 0..15 {
        queue.push(sample("meshmon_test", i as f64, 1_700_000_000_000));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let total: usize = mock
        .received_requests()
        .await
        .iter()
        .map(|r| {
            decode_write_request(&r.body)
                .timeseries
                .iter()
                .map(|ts| ts.samples.len())
                .sum::<usize>()
        })
        .sum();
    assert!(
        total >= 10,
        "expected size-driven flush of >=10 samples, got {total}"
    );

    token.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn retries_after_failure() {
    let server = MockServer::start().await;
    // First two attempts fail, then success.
    Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    let success = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(204))
        .mount_as_scoped(&server)
        .await;

    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(1024));
    let token = CancellationToken::new();
    let cfg = VmWriterCfg {
        url: format!("{}/api/v1/write", server.uri()),
        batch_size: 100,
        batch_interval: Duration::from_millis(100),
        max_retry: Duration::from_secs(5),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        async move { vm_run(q, cfg, t).await }
    });

    for i in 0..5 {
        queue.push(sample("meshmon_test", i as f64, 1_700_000_000_000));
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let total: usize = success
        .received_requests()
        .await
        .iter()
        .map(|r| {
            decode_write_request(&r.body)
                .timeseries
                .iter()
                .map(|ts| ts.samples.len())
                .sum::<usize>()
        })
        .sum();
    assert_eq!(total, 5);

    token.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn buffer_overflow_drops_oldest() {
    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(3));
    queue.push(sample("m", 1.0, 1));
    queue.push(sample("m", 2.0, 2));
    queue.push(sample("m", 3.0, 3));
    let dropped = queue.push(sample("m", 4.0, 4));
    assert!(dropped, "fourth push should drop the oldest");
    let mut out = Vec::new();
    queue.drain_into(&mut out, 10);
    let values: Vec<f64> = out.iter().map(|s| s.value).collect();
    assert_eq!(values, vec![2.0, 3.0, 4.0]);
}
