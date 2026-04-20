# Agent command surface

The agent hosts a tonic service (`AgentCommand`) reachable via the
reverse tunnel the service opens at startup. Methods:

- `RefreshConfig` — wakes the refresh loop so the agent re-fetches
  config + targets immediately.
- `RunMeasurementBatch` — the service streams one-off campaign
  measurement results. The prober implementation is injected at
  `AgentCommandService::new` time via the `CampaignProber` trait; the
  default `StubProber` returns deterministic dummy summaries for
  transport-level tests. Production deployments plug in a real
  trippy-backed prober at the same construction seam.

## Cancellation

Dropping the client stream cancels the batch via a `CancellationToken`
owned by the forwarding stream's `Drop`. Probers must observe the
token within ~500 ms and wind down their in-flight probes; the
service side maps the cancellation to `rejected_ids` so the scheduler
reverts the pair for a later tick.

## Concurrency

`AgentCommandService::new(trigger, prober, max_concurrency)` sizes a
`Semaphore`. `max_concurrency` is sourced from
`MESHMON_CAMPAIGN_MAX_CONCURRENCY` in bootstrap and advertised to the
service via `RegisterRequest.campaign_max_concurrency` so both sides
enforce the same cap. A concurrent batch above the cap returns
`Status::resource_exhausted`, which the service-side dispatcher maps
to `DispatchOutcome::rejected_ids` for every pair in the refused
batch.

## `CampaignProber` — the prober seam

`CampaignProber` is the trait production swaps against
`StubProber`. Implementers emit one `MeasurementResult` per
`MeasurementTarget` (correlated by `pair_id`) on the outbound sender
and honour the cancellation token. The trait stays intentionally
narrow so transport-level tests and protocol-accurate test fakes can
coexist with the real trippy-backed prober without touching
`AgentCommandService` itself.
