# Agent command surface

The agent hosts a tonic service (`AgentCommand`) reachable via the reverse
tunnel the service opens at startup. Methods:

- `RefreshConfig` — wakes the refresh loop so the agent re-fetches
  config + targets immediately.
- `RunMeasurementBatch` — service streams one-off campaign measurement
  results. The prober is pluggable via `CampaignProber`; the default
  `StubProber` returns deterministic dummy summaries for transport-level
  tests. Production deployments swap in a real trippy-backed prober at
  `AgentCommandService` construction time.

## Cancellation

Dropping the client stream cancels the batch via a `CancellationToken`
that the forwarding stream ties to its `Drop`. Probers must observe the
token within ~500 ms.

## Concurrency

`AgentCommandService::new(trigger, prober, max_concurrency)` sizes a
`Semaphore`; a concurrent batch above the cap returns
`Status::resource_exhausted`. The service-side dispatcher sees the
overflow as a `DispatchOutcome::rejected_ids` entry for every pair in
the refused batch.

## Swapping the prober

`CampaignProber` is the seam — implementers just need to emit one
`MeasurementResult` per `MeasurementTarget` (correlated by `pair_id`)
and honour the cancel token. Tests swap in fakes; production swaps in
the real prober.
