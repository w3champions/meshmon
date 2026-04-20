//! Skeleton-phase smoke test: OneshotProber exists, accepts a batch, and
//! emits one MeasurementResult per pair. Later tasks replace the stubbed
//! AgentError outcomes with real protocol aggregation.

use meshmon_agent::command::CampaignProber;
use meshmon_agent::probing::oneshot::OneshotProber;
use meshmon_protocol::{MeasurementKind, MeasurementTarget, Protocol, RunMeasurementBatchRequest};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn oneshot_prober_emits_one_result_per_target() {
    let prober = OneshotProber::new(4);
    let req = RunMeasurementBatchRequest {
        batch_id: 1,
        kind: MeasurementKind::Latency as i32,
        protocol: Protocol::Icmp as i32,
        probe_count: 2,
        timeout_ms: 500,
        probe_stagger_ms: 50,
        targets: (0..3)
            .map(|i| MeasurementTarget {
                pair_id: i,
                destination_ip: vec![127, 0, 0, 1].into(),
                destination_port: 0,
            })
            .collect(),
    };
    let (tx, mut rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    prober.run_batch(req, cancel, tx).await;

    let mut pair_ids = Vec::new();
    while let Some(item) = rx.recv().await {
        pair_ids.push(item.expect("no tonic error").pair_id);
    }
    pair_ids.sort();
    assert_eq!(pair_ids, vec![0, 1, 2]);
}
