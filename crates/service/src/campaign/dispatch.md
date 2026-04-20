# `campaign::rpc_dispatcher` — dispatch transport

The real `PairDispatcher`. One instance per service process. Cheap to
clone (interior state is `Arc`-owned) and holds references to:

- `TunnelManager` — per-agent `tonic::Channel`s routed through yamux.
- `AgentRegistry` — consulted per-dispatch for the effective per-agent
  concurrency override.
- `SettleWriter` — one owned copy; the writer handles the terminal
  `campaign_pairs` UPDATE and the `campaign_pair_settled` NOTIFY.

## Per-call flow

1. **Resolve the tunnel channel.** `TunnelManager::channel_for(agent_id)`
   returns the tonic channel for the agent's tunnel. Missing tunnel
   means every pair joins `DispatchOutcome::rejected_ids` with
   `skipped_reason = Some("agent_unreachable")`.
2. **Acquire a per-agent semaphore permit.** Effective concurrency is
   `registry.snapshot().get(agent).campaign_max_concurrency
     .unwrap_or(default_agent_concurrency).max(1)`. The semaphore is
   cached in a `DashMap<agent_id, AgentSemaphore>` and rebuilt when
   the effective value changes (operator concurrency tweak takes
   effect on the next dispatch).
3. **Reserve per-destination tokens.** A process-wide
   `moka::future::Cache<IpAddr, Arc<Mutex<Bucket>>>` holds a leaky
   bucket per destination. `reserve_tokens` draws one token per pair;
   pairs that lose the draw are added to `rate_limited` and surface
   as `rejected_ids`. If every pair is rate-limited the batch returns
   early with `skipped_reason = Some("rate_limited")`.
4. **Build `RunMeasurementBatchRequest`.** Every pair in a batch
   shares the same campaign (scheduler invariant:
   `take_pending_batch` is per-`(campaign, agent)`), so per-campaign
   knobs come from the head pair. `MeasurementKind` is `Mtr` when
   `probe_count == 1`, `Latency` otherwise. Targets are truncated at
   `max_batch_size`.
5. **Open the server-streaming RPC.** `AgentCommandClient::new(channel)
   .run_measurement_batch(req)`. An open-phase error returns
   `skipped_reason = Some("rpc_error:<code>")` and every allowed pair
   joins `rejected_ids`.
6. **Drain the stream into `SettleWriter::settle`.** The writer
   returns a `SettleOutcome`:
   - `Settled` → `dispatched_ok += 1`.
   - `RaceLost` → the `resolution_state='dispatched'` gate rejected
     the update (concurrent reset landed); drop silently. The
     scheduler owns the next step for that row.
   - `MalformedNoOutcome` → the agent sent a result with no `outcome`
     field (protocol violation). The pair joins `rejected_ids` so the
     scheduler reverts it; silent dropping would leave the pair stuck
     in `dispatched` forever.
   - `Err(_)` → the pair joins `rejected_ids`.
7. **Sweep up missing pairs.** Any `pair_id` the agent never produced
   a result for joins `rejected_ids` — the scheduler reverts those on
   the next tick.

## `DispatchOutcome` population rules

| Field | Population |
|---|---|
| `dispatched` | Count of pairs whose results streamed back AND whose writer settle returned `Settled`. |
| `rejected_ids` | Pairs blocked by the per-destination bucket, pairs whose response never arrived, pairs whose stream errored mid-flight, pairs whose writer call returned `MalformedNoOutcome`, pairs whose writer call returned `Err`. |
| `skipped_reason` | Set only when the batch failed before any pair streamed: `"agent_unreachable"`, `"rpc_error:<code>"`, `"rate_limited"` (bucket consumed every pair), `"semaphore_closed"`. |

Agent-reported failures (`MeasurementFailure` — `NO_ROUTE`, `TIMEOUT`,
etc.) are **settled** by the writer, not rejected. The writer maps
the code to the right terminal `resolution_state` + `last_error` tag.

## Cancellation

The scheduler owns the cancellation token. If its task drops the
dispatch future, the tonic response stream drops, which closes the
HTTP/2 stream with `CANCEL`. The agent's handler observes the cancel
within ~500 ms and winds down the prober. No extra bookkeeping is
needed on the service side.

## Metrics

Each dispatch records:

- `meshmon_campaign_batches_total{agent_id,kind,outcome}` — outcomes
  are `ok`, `partial`, `rpc_error`.
- `meshmon_campaign_batch_duration_seconds{agent_id,kind}` — wall
  time between RPC open and the final stream event.
- `meshmon_campaign_pairs_inflight{agent_id}` — gauge bumped on each
  allowed batch and decremented via an RAII `InflightGuard` on every
  exit path.
- `meshmon_campaign_dest_bucket_wait_seconds` — histogram of the wall
  time each pair spent acquiring (or failing to acquire) a
  per-destination token.

## Cross-references

- `crates/service/src/campaign/writer.rs` — `SettleWriter`; owns the
  per-result transaction, the `resolution_state='dispatched'` gate,
  and the `campaign_pair_settled` NOTIFY.
- `crates/service/src/campaign/scheduler.rs` — caller; reverts
  `rejected_ids` back to `pending` on the next tick.
- `crates/agent/src/command/measurements.rs` — agent-side receiver
  (`AgentCommandService::run_measurement_batch`).
