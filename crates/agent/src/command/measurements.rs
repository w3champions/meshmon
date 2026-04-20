//! `AgentCommand` service implementation with a swappable prober.
//!
//! The `RefreshConfig` handler wakes the agent's refresh loop via a shared
//! `Notify`. The `RunMeasurementBatch` handler delegates measurement
//! execution to a [`CampaignProber`] implementation — [`StubProber`] for
//! transport-level tests, a real trippy-backed prober in production.
//!
//! Concurrency is capped per-agent by a `Semaphore` sized from the
//! cluster-configured `campaign_max_concurrency`. Saturated agents return
//! `Status::resource_exhausted` so the dispatcher can revert pairs to the
//! scheduler instead of silently stalling.
//!
//! Cancellation: the response stream owns a `CancellationToken` whose
//! `Drop` fires cancellation, so dropping the client-side stream tears
//! down the in-flight batch within the prober's observation window
//! (contracted at ~500 ms).

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use meshmon_protocol::pb::measurement_result::Outcome;
use meshmon_protocol::{
    AgentCommand, MeasurementKind, MeasurementResult, MeasurementSummary, Protocol,
    RefreshConfigRequest, RefreshConfigResponse, RunMeasurementBatchRequest,
};
use tokio::sync::{mpsc, Notify, Semaphore};
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, info};

/// Producer contract implemented by the stub and by production probers.
///
/// Implementations MUST:
///   * Emit one `MeasurementResult` per `MeasurementTarget` in
///     `req.targets`, correlated by `pair_id`.
///   * Observe `cancel` and stop producing within ~500 ms once cancelled.
///   * Never send more than one result per pair.
///
/// Senders that return `Err(SendError)` indicate the service-side stream
/// was dropped; probers should bail out immediately rather than keep
/// probing for a consumer that no longer exists.
#[async_trait]
pub trait CampaignProber: Send + Sync + 'static {
    /// Run the batch, pushing one `MeasurementResult` per target onto
    /// `results` in order. See trait docs for the cancellation and
    /// correlation contract.
    async fn run_batch(
        &self,
        req: RunMeasurementBatchRequest,
        cancel: CancellationToken,
        results: mpsc::Sender<Result<MeasurementResult, Status>>,
    );
}

/// Deterministic transport-level prober used for integration tests.
///
/// Emits a success `MeasurementSummary` for every target with latency
/// fields pinned to 1.0 ms and loss 0.0. Real probers replace this at
/// `AgentCommandService` construction time.
pub struct StubProber;

#[async_trait]
impl CampaignProber for StubProber {
    async fn run_batch(
        &self,
        req: RunMeasurementBatchRequest,
        cancel: CancellationToken,
        results: mpsc::Sender<Result<MeasurementResult, Status>>,
    ) {
        for target in req.targets {
            if cancel.is_cancelled() {
                debug!(pair_id = target.pair_id, "stub prober cancelled");
                return;
            }
            let result = MeasurementResult {
                pair_id: target.pair_id,
                outcome: Some(Outcome::Success(MeasurementSummary {
                    attempted: req.probe_count,
                    succeeded: req.probe_count,
                    latency_min_ms: 1.0,
                    latency_avg_ms: 1.0,
                    latency_median_ms: 1.0,
                    latency_p95_ms: 1.0,
                    latency_max_ms: 1.0,
                    latency_stddev_ms: 0.0,
                    loss_pct: 0.0,
                })),
            };
            if results.send(Ok(result)).await.is_err() {
                // Receiver dropped — service cancelled the stream.
                return;
            }
        }
    }
}

/// Agent-side `AgentCommand` service. Composes the `RefreshConfig`
/// handler with the `RunMeasurementBatch` handler + a [`CampaignProber`].
pub struct AgentCommandService<P: CampaignProber> {
    refresh_trigger: Arc<Notify>,
    prober: Arc<P>,
    /// Caps concurrent in-flight batches on this agent. Sized from
    /// `AgentEnv::campaign_max_concurrency` or the cluster default.
    semaphore: Arc<Semaphore>,
}

impl<P: CampaignProber> AgentCommandService<P> {
    /// Build a service bound to `refresh_trigger` (shared with the
    /// refresh loop), `prober`, and `max_concurrency` concurrent batches.
    /// A value of 0 is treated as 1 — the semaphore is never zero-sized
    /// because that would permanently block every batch.
    pub fn new(refresh_trigger: Arc<Notify>, prober: Arc<P>, max_concurrency: usize) -> Self {
        Self {
            refresh_trigger,
            prober,
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
        }
    }
}

/// Cancels its inner token on `Drop`. Stored inside the response stream
/// so the `CancellationToken` fires the moment tonic drops the stream,
/// which is what happens when the client disconnects or drops its
/// handle to the server-streaming call.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[tonic::async_trait]
impl<P: CampaignProber> AgentCommand for AgentCommandService<P> {
    async fn refresh_config(
        &self,
        _request: Request<RefreshConfigRequest>,
    ) -> Result<Response<RefreshConfigResponse>, Status> {
        info!("received RefreshConfig; waking refresh loop");
        self.refresh_trigger.notify_one();
        Ok(Response::new(RefreshConfigResponse {}))
    }

    type RunMeasurementBatchStream =
        Pin<Box<dyn Stream<Item = Result<MeasurementResult, Status>> + Send + 'static>>;

    async fn run_measurement_batch(
        &self,
        request: Request<RunMeasurementBatchRequest>,
    ) -> Result<Response<Self::RunMeasurementBatchStream>, Status> {
        let req = request.into_inner();
        if req.targets.is_empty() {
            // No work: surface an immediately-closed stream rather than
            // consuming a semaphore permit for a no-op.
            let empty = tokio_stream::empty();
            return Ok(Response::new(Box::pin(empty)));
        }

        // Reject `UNSPECIFIED` enum values up front. Protobuf decodes
        // unknown variants to the zero-value (`UNSPECIFIED`) when the
        // sender omits the field — treating that as a valid probe kind
        // would make a version-skewed service silently run the wrong
        // probe. Every real prober can safely assume validated enums.
        if req.kind == MeasurementKind::Unspecified as i32 {
            return Err(Status::invalid_argument("kind must not be UNSPECIFIED"));
        }
        if req.protocol == Protocol::Unspecified as i32 {
            return Err(Status::invalid_argument("protocol must not be UNSPECIFIED"));
        }

        // Per-agent cap: reject when the agent is already saturated.
        // Acquiring the permit inside the spawned task would let the
        // service open the stream even when we're over capacity and
        // then silently stall — refusing here surfaces backpressure to
        // the dispatcher immediately.
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return Err(Status::resource_exhausted("agent at capacity")),
        };

        // Channel capacity: clamp to [1, 64] targets worth of buffering
        // so we don't allocate a pathologically large channel for huge
        // batches while still preserving enough slack to absorb short
        // bursts without blocking the prober on every send.
        let capacity = req.targets.len().clamp(1, 64);
        let (tx, rx) = mpsc::channel::<Result<MeasurementResult, Status>>(capacity);
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let prober = self.prober.clone();
        let batch_id = req.batch_id;

        // Spawn the prober so we can return the stream immediately. The
        // permit moves into the task so it's released when the batch
        // terminates (normal completion, cancellation, or panic).
        tokio::spawn(async move {
            let _permit_guard = permit;
            debug!(batch_id, "stub prober batch started");
            prober.run_batch(req, cancel_clone, tx).await;
            debug!(batch_id, "stub prober batch finished");
        });

        // Wrap the receiver in a stream whose `Drop` cancels the batch:
        // when tonic drops the response stream (client disconnect, client
        // drops its handle, HTTP/2 RST_STREAM), `CancelOnDrop::drop`
        // fires, the prober observes `cancel` and exits, and the spawned
        // task drops the sender — unblocking any in-flight forward.
        let guard = CancelOnDrop(cancel);
        let stream = async_stream::stream! {
            let _guard = guard;
            let mut rx = rx;
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshmon_protocol::{MeasurementKind, MeasurementTarget, Protocol};
    use std::time::Duration;

    fn latency_request(targets: usize) -> RunMeasurementBatchRequest {
        RunMeasurementBatchRequest {
            batch_id: 1,
            kind: MeasurementKind::Latency as i32,
            protocol: Protocol::Icmp as i32,
            probe_count: 8,
            timeout_ms: 2000,
            probe_stagger_ms: 100,
            targets: (0..targets)
                .map(|i| MeasurementTarget {
                    pair_id: i as u64,
                    destination_ip: vec![203, 0, 113, (i % 255) as u8].into(),
                    destination_port: 0,
                })
                .collect(),
        }
    }

    // -- RefreshConfig --------------------------------------------------------

    #[tokio::test]
    async fn refresh_config_wakes_the_trigger() {
        let trigger = Arc::new(Notify::new());
        let svc = AgentCommandService::new(trigger.clone(), Arc::new(StubProber), 4);

        // Arm the notified future before firing so we observe the wake
        // (Notify is level-triggered on pending waiters).
        let notified = trigger.notified();
        tokio::pin!(notified);

        svc.refresh_config(Request::new(RefreshConfigRequest {}))
            .await
            .expect("handler returned Ok");

        tokio::time::timeout(Duration::from_millis(100), notified.as_mut())
            .await
            .expect("notified future resolved within 100ms");
    }

    #[tokio::test]
    async fn refresh_config_returns_empty_response() {
        let trigger = Arc::new(Notify::new());
        let svc = AgentCommandService::new(trigger, Arc::new(StubProber), 4);
        let response = svc
            .refresh_config(Request::new(RefreshConfigRequest {}))
            .await
            .expect("handler ok");
        let _empty = response.into_inner();
    }

    // -- StubProber -----------------------------------------------------------

    #[tokio::test]
    async fn stub_prober_emits_one_result_per_target_with_matching_pair_id() {
        let prober = StubProber;
        let req = latency_request(5);
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        prober.run_batch(req, cancel, tx).await;

        let mut ids = Vec::new();
        while let Some(item) = rx.recv().await {
            let result = item.expect("stub never emits Err");
            match result.outcome {
                Some(Outcome::Success(ref s)) => {
                    assert_eq!(s.attempted, 8);
                    assert_eq!(s.succeeded, 8);
                    assert!((s.latency_avg_ms - 1.0).abs() < f32::EPSILON);
                    assert!((s.loss_pct).abs() < f32::EPSILON);
                }
                other => panic!("expected success outcome, got {other:?}"),
            }
            ids.push(result.pair_id);
        }
        ids.sort();
        assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn stub_prober_stops_when_cancelled() {
        let prober = StubProber;
        let req = latency_request(100);
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        cancel.cancel();

        prober.run_batch(req, cancel, tx).await;

        // A pre-cancelled batch may still emit the first result because
        // the `is_cancelled` check runs at the top of each iteration and
        // the producer races the consumer for ordering. The hard contract
        // is that the producer terminates — not that zero results ship.
        let mut count = 0;
        while rx.recv().await.is_some() {
            count += 1;
        }
        assert!(
            count < 100,
            "stub should stop early on cancel; got {count} results",
        );
    }

    #[tokio::test]
    async fn stub_prober_bails_when_receiver_is_dropped() {
        let prober = StubProber;
        let req = latency_request(50);
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let cancel = CancellationToken::new();

        // Should return promptly rather than hanging on a closed channel.
        tokio::time::timeout(
            Duration::from_millis(200),
            prober.run_batch(req, cancel, tx),
        )
        .await
        .expect("stub returned after receiver dropped");
    }

    // -- AgentCommandService --------------------------------------------------

    #[tokio::test]
    async fn run_measurement_batch_rejects_unspecified_kind() {
        let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(StubProber), 4);
        let mut req = latency_request(1);
        req.kind = MeasurementKind::Unspecified as i32;

        // `Response<Self::RunMeasurementBatchStream>` doesn't derive
        // Debug (trait-object stream inside), so `.expect_err` is not
        // available — match instead.
        match svc.run_measurement_batch(Request::new(req)).await {
            Err(status) => assert_eq!(status.code(), tonic::Code::InvalidArgument),
            Ok(_) => panic!("unspecified kind must be rejected"),
        }
    }

    #[tokio::test]
    async fn run_measurement_batch_rejects_unspecified_protocol() {
        let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(StubProber), 4);
        let mut req = latency_request(1);
        req.protocol = Protocol::Unspecified as i32;

        match svc.run_measurement_batch(Request::new(req)).await {
            Err(status) => assert_eq!(status.code(), tonic::Code::InvalidArgument),
            Ok(_) => panic!("unspecified protocol must be rejected"),
        }
    }

    #[tokio::test]
    async fn run_measurement_batch_empty_targets_returns_empty_stream() {
        let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(StubProber), 4);
        let mut req = latency_request(0);
        req.targets.clear();

        let response = svc
            .run_measurement_batch(Request::new(req))
            .await
            .expect("empty stream is a success");
        let mut stream = response.into_inner();

        use futures_util::StreamExt;
        assert!(stream.next().await.is_none(), "empty stream yields None");
    }

    #[tokio::test]
    async fn run_measurement_batch_rejects_when_saturated() {
        // Capacity 1 means a second concurrent call cannot acquire.
        let svc = AgentCommandService::new(Arc::new(Notify::new()), Arc::new(SlowProber), 1);

        let response1 = svc
            .run_measurement_batch(Request::new(latency_request(1)))
            .await
            .expect("first call succeeds");

        // Second call while the first holds the permit.
        let result = svc
            .run_measurement_batch(Request::new(latency_request(1)))
            .await;

        match result {
            Err(status) if status.code() == tonic::Code::ResourceExhausted => {}
            Err(status) => panic!("expected ResourceExhausted, got {status}"),
            Ok(_) => panic!("expected ResourceExhausted, got Ok"),
        }

        // Drop the first stream so the permit is released.
        drop(response1);
    }

    /// Prober that blocks forever (until cancelled) to hold the permit open
    /// for saturation tests.
    struct SlowProber;

    #[async_trait]
    impl CampaignProber for SlowProber {
        async fn run_batch(
            &self,
            _req: RunMeasurementBatchRequest,
            cancel: CancellationToken,
            _results: mpsc::Sender<Result<MeasurementResult, Status>>,
        ) {
            cancel.cancelled().await;
        }
    }
}
