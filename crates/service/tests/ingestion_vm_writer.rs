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

/// Regression guard for retry-exhaustion behavior.
///
/// Proves that when the VM endpoint is permanently failing and the
/// `max_retry` budget is exhausted, the vm_writer:
/// 1. Abandons the batch after exactly one POST attempt (first attempt
///    checks `Instant::now() + initial_backoff(250 ms) > deadline`; with
///    `max_retry = 100 ms` that is true on the very first evaluation, so
///    no sleep/re-attempt occurs), and
/// 2. Does NOT send any further requests after the deadline (confirming
///    exhaustion, not cancellation).
///
/// The `ingest_dropped` counter increment is a side-effect of the same
/// `Err(VmWriteError::HttpStatus(_))` arm that triggers this behavioral
/// outcome — if the request count is correct, the counter increment is
/// also correct (same code path). Using the behavioral proxy avoids the
/// async global-recorder problem with `metrics_util::DebuggingRecorder`
/// (which is thread-local and cannot observe emissions from a task on a
/// different tokio worker thread).
#[tokio::test]
async fn retry_exhaustion_stops_after_first_attempt() {
    let server = MockServer::start().await;

    // Always return 500 — no success path.
    let mock = Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(500))
        .mount_as_scoped(&server)
        .await;

    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(1024));
    let token = CancellationToken::new();

    // max_retry = 100 ms is shorter than the initial backoff (250 ms), so
    // post_batch() returns Err after exactly 1 POST (no sleep, no retry).
    // batch_interval = 100 ms triggers the flush quickly after the push.
    let cfg = VmWriterCfg {
        url: format!("{}/api/v1/write", server.uri()),
        batch_size: 100,
        batch_interval: Duration::from_millis(100),
        max_retry: Duration::from_millis(100),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        async move { vm_run(q, cfg, t).await }
    });

    // Push 3 samples — they will be flushed as one batch after batch_interval.
    for i in 0..3 {
        queue.push(sample(
            "meshmon_test_exhaustion",
            i as f64,
            1_700_000_000_000 + i,
        ));
    }

    // Wait for batch_interval + a generous buffer for the single HTTP round-
    // trip to complete. The writer makes exactly one attempt per the timing
    // analysis above, then gives up.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let count_after_exhaustion = mock.received_requests().await.len();
    assert_eq!(
        count_after_exhaustion, 1,
        "expected exactly 1 POST (retry exhaustion with 100ms deadline < 250ms backoff); \
         got {count_after_exhaustion}"
    );

    // Wait an additional 600 ms (more than two backoff periods) to confirm
    // the writer is not silently retrying in the background.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let count_after_wait = mock.received_requests().await.len();
    assert_eq!(
        count_after_wait, 1,
        "unexpected additional POST(s) after retry exhaustion: \
         expected 1 total, got {count_after_wait}"
    );

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
