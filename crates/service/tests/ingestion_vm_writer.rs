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

/// Sum the samples across every POST the mock has seen so far.
fn total_samples(reqs: &[wiremock::Request]) -> usize {
    reqs.iter()
        .map(|r| {
            decode_write_request(&r.body)
                .timeseries
                .iter()
                .map(|ts| ts.samples.len())
                .sum::<usize>()
        })
        .sum()
}

/// Poll `mock.received_requests()` until the sum of samples reaches
/// `expected_min` or `timeout` elapses, then return the last observed
/// total.
///
/// The writer task flushes asynchronously, so a fixed-duration sleep
/// before `assert_eq!` made these tests flake under heavy parallel load
/// (nextest running dozens of tests). Polling is robust to that.
async fn wait_for_samples(
    mock: &wiremock::MockGuard,
    expected_min: usize,
    timeout: Duration,
) -> usize {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let total = total_samples(&mock.received_requests().await);
        if total >= expected_min {
            return total;
        }
        if tokio::time::Instant::now() >= deadline {
            return total;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll `mock.received_requests().len()` until it reaches `expected_min`
/// or `timeout` elapses. Used by tests that count requests rather than
/// samples.
async fn wait_for_request_count(
    mock: &wiremock::MockGuard,
    expected_min: usize,
    timeout: Duration,
) -> usize {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let count = mock.received_requests().await.len();
        if count >= expected_min {
            return count;
        }
        if tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
    });

    for i in 0..3 {
        queue.push(sample(
            "meshmon_path_failure_rate",
            i as f64,
            1_700_000_000_000 + i,
        ));
    }

    // Poll rather than fixed-sleep so the test is robust under load — a
    // fixed 300ms was not enough when nextest ran this in parallel with
    // the rest of the suite.
    let total = wait_for_samples(&mock, 3, Duration::from_secs(5)).await;
    assert_eq!(total, 3, "expected 3 samples flushed, got {total}");

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
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
    });

    for i in 0..15 {
        queue.push(sample("meshmon_test", i as f64, 1_700_000_000_000));
    }

    // Poll until the size-driven flush landed (>=10 samples). A fixed
    // 200ms sleep was not enough under parallel nextest load.
    let total = wait_for_samples(&mock, 10, Duration::from_secs(5)).await;
    assert!(
        total >= 10,
        "expected size-driven flush of >=10 samples, got {total}"
    );

    token.cancel();
    let _ = handle.await;
}

/// Regression guard for the "flush as soon as queue reaches batch_size"
/// trigger (the "size OR interval" contract).
///
/// Earlier revisions of `run()` checked `queue.len() < cfg.batch_size`
/// **once** (before sleeping `batch_interval`), so samples that arrived
/// *during* the sleep would remain queued until the timer fired even if
/// the queue grew past `batch_size`. Under sustained bursts that delay
/// caused avoidable drops.
///
/// Proves that with a 30s interval and a batch_size of 5, pushing 5
/// samples triggers a POST within well under 500ms — i.e. the race arm
/// that watches queue growth is live, not the timer.
#[tokio::test]
async fn writer_flushes_immediately_when_batch_size_reached() {
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
        batch_size: 5,
        batch_interval: Duration::from_secs(30),
        max_retry: Duration::from_secs(1),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
    });

    // Push exactly batch_size samples. If the batch-size race is live,
    // the writer flushes well inside the 500ms window. If the interval
    // timer is still authoritative the POST won't land for ~30s.
    for i in 0..5 {
        queue.push(sample(
            "meshmon_test_burst",
            i as f64,
            1_700_000_000_000 + i,
        ));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

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
    assert_eq!(
        total, 5,
        "expected size-driven flush within 500ms (batch_interval=30s); \
         got {total} samples — batch-size race arm is not firing"
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
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
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
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
    });

    // Push 3 samples — they will be flushed as one batch after batch_interval.
    for i in 0..3 {
        queue.push(sample(
            "meshmon_test_exhaustion",
            i as f64,
            1_700_000_000_000 + i,
        ));
    }

    // Poll until the first POST lands. The writer makes exactly one
    // attempt per the timing analysis above, then gives up. A fixed
    // 400ms sleep here flaked under parallel nextest load because the
    // batch-interval timer did not fire in time.
    let count_after_exhaustion = wait_for_request_count(&mock, 1, Duration::from_secs(5)).await;
    assert_eq!(
        count_after_exhaustion, 1,
        "expected exactly 1 POST (retry exhaustion with 100ms deadline < 250ms backoff); \
         got {count_after_exhaustion}"
    );

    // Wait a further "two backoff periods" to confirm no late retries
    // sneak in. This leg is fundamentally a hold-firm check, so a
    // fixed sleep is correct — a poll would short-circuit on the
    // existing single request and defeat the assertion.
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

/// Regression guard for the unified drain-send timeout.
///
/// Proves that when the VM hangs well beyond reqwest's 15s client timeout
/// and cancellation fires mid-send, the vm_writer exits within the
/// cancel+2s grace budget rather than waiting on the underlying reqwest
/// send. Uses a 15s-delayed `ResponseTemplate` so any regression that
/// reintroduces the pre-cancel `send_fut.await` branch would block here
/// for at least 15s, well past the 5s test budget.
#[tokio::test]
async fn drain_send_timeout_bounds_shutdown_against_hanging_vm() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/write"))
        .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(15)))
        .mount(&server)
        .await;

    let queue: Arc<DropOldest<PromSample>> = Arc::new(DropOldest::new(1024));
    let token = CancellationToken::new();
    let cfg = VmWriterCfg {
        url: format!("{}/api/v1/write", server.uri()),
        batch_size: 100,
        batch_interval: Duration::from_millis(50),
        max_retry: Duration::from_secs(60),
    };
    let handle = tokio::spawn({
        let q = queue.clone();
        let t = token.clone();
        // Signal pg drain done up-front: these unit tests don't run a
        // concurrent pg_writer, so the drain-wait guard would otherwise
        // keep the writer alive indefinitely after cancel.
        let pg_drain_complete = CancellationToken::new();
        pg_drain_complete.cancel();
        async move { vm_run(q, cfg, t, pg_drain_complete).await }
    });

    for i in 0..3 {
        queue.push(sample("meshmon_test_hang", i as f64, 1_700_000_000_000 + i));
    }

    // Let the writer pick up the batch and start the (hanging) POST.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let t0 = std::time::Instant::now();
    token.cancel();

    // Budget: cancel+2s grace for the in-flight send + 500ms drain grace
    // period + some slack for batching/task scheduling. If the TOCTOU
    // window regresses, this will hit the 15s mock delay and blow past 5s.
    let shutdown_result = tokio::time::timeout(Duration::from_secs(5), handle).await;
    let elapsed = t0.elapsed();

    shutdown_result
        .expect("vm_writer did not exit within 5s of cancel (hanging-VM budget breached)")
        .expect("vm_writer task panicked");

    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown took {elapsed:?}; expected < 5s (cancel+2s grace + drain grace + slack)"
    );
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
