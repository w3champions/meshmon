# One-shot prober

Trippy-backed campaign prober. Per-pair blocking tracer tasks run under
an independent semaphore so campaign traffic cannot starve continuous
MTR and vice versa. This module is the only `CampaignProber` wired into
`tunnel.rs::run_one_session`; `command::StubProber` remains in the crate
but is exercised only by transport-level integration tests.

## Builder matrix

`build_oneshot_config` returns a fresh `trippy_core::Builder` from the
request knobs. `T = req.timeout_ms`, `S = req.probe_stagger_ms`,
`N = req.probe_count.max(1)`.

| Kind       | Protocol | `max_rounds` | `min/max_round_duration` | `read_timeout` | `grace`   | TTL      | `port_direction`  | `trace_identifier`     |
|------------|----------|-------------:|--------------------------|----------------|-----------|----------|-------------------|------------------------|
| `LATENCY`  | `ICMP`   | `Some(N)`    | `S ms`                   | `T ms`         | `500 ms`  | `1..=32` | default           | `Some(next_trace_id())`|
| `LATENCY`  | `TCP`    | `Some(N)`    | `S ms`                   | `T ms`         | `500 ms`  | `1..=32` | `FixedDest(port)` | unset                  |
| `LATENCY`  | `UDP`    | `Some(N)`    | `S ms`                   | `T ms`         | `500 ms`  | `1..=32` | `FixedDest(port)` | unset                  |
| `MTR` any  | —        | `Some(1)`    | `0 ms` / `30 s`          | `30 s`         | `500 ms`  | `1..=32` | per-protocol      | ICMP only              |

MTR always pins `max_rounds(1)` — the request's `probe_count` is ignored
— and uses a hard-coded 30-second round timeout. For LATENCY,
`min_round_duration == max_round_duration` pins the probe cadence so
`trippy-core` does not add internal jitter. `grace_duration` covers
late destination replies (>200 ms RTT paths) and matches the continuous
prober.

## Loss predicates

Every protocol reads `state.target_hop(State::default_flow_id())` after
the blocking `Tracer::run()` returns and interprets the resulting counts
per the table below.

| Protocol | Success predicate                                                              | Silent-batch failure |
|----------|--------------------------------------------------------------------------------|----------------------|
| ICMP     | `total_recv > 0` within `T`                                                    | `TIMEOUT`            |
| TCP      | Any destination reply within `T` (SYN/ACK and RST both count as `total_recv`)  | `TIMEOUT`            |
| UDP      | Destination reply or ICMP Port-Unreachable from the destination IP within `T`  | `TIMEOUT`            |

### TCP REFUSED — coverage gap

`trippy-core 0.13` collapses TCP SYN/ACK and TCP RST into `total_recv`
at the `Hop` level. The public `Hop` surface exposes no per-probe
response type, and `ProbeComplete.icmp_packet_type` is
`IcmpPacketType::NotApplicable` for every TCP probe regardless of
outcome. The oneshot prober therefore cannot emit the per-pair
`MeasurementFailureCode::REFUSED` that the campaign design calls for;
any TCP batch with at least one reply surfaces as a
`MeasurementSummary`. Operators who need the RST-vs-SYN/ACK distinction
rely on the continuous TCP prober's per-probe telemetry. A future
change can layer an explicit refused predicate on top of a secondary
`TcpStream::connect` channel or a trippy-core extension without
restructuring this module.

## MTR aggregation

Single-round MTR. `aggregate_mtr` walks `[1..=target_reached_ttl]`:

- `target_reached_ttl = target_hop.ttl()` when the destination
  responded, otherwise the highest responsive TTL in the snapshot; a
  completely silent trace surfaces as `MeasurementFailureCode::TIMEOUT`
  rather than an empty MTR result.
- Each responsive TTL becomes one `HopSummary`: `position = ttl`,
  `observed_ips` carries one `HopIp` per unique `Hop::addrs()` entry at
  `frequency = 1.0`, `avg_rtt_micros` comes from `Hop::best_ms()`,
  `stddev_rtt_micros = 0` (single probe), `loss_pct = 0`.
- Each silent TTL pads with `observed_ips: []`, zero RTT, and
  `loss_pct = 1.0`.

## Cancellation

`run_one_pair` selects on three branches, biased toward cancel:

1. `cancel.cancelled()` — drops the `Arc<Tracer>` (closing the raw
   socket) and waits up to 1 s for the blocking thread to unwind, then
   emits `MeasurementFailureCode::CANCELLED` regardless of whether the
   thread returned in time.
2. `&mut blocking` — the `spawn_blocking` join handle resolves when
   `Tracer::run()` finishes all rounds.
3. `tokio::time::sleep(max_wall_clock)` — a safety net sized from
   `probe_count * (stagger + timeout) + 5 s` for LATENCY or `35 s` for
   MTR; tripping the net surfaces as `MeasurementFailureCode::TIMEOUT`.

The 1-second drain budget is tighter than the continuous prober's 15 s
because one-shot tracers run at most `probe_count` rounds (≤ 20 in
typical campaigns) rather than the continuous prober's bounded-3600
round loop.

## Shared-resource audit

| Resource                    | Owner (continuous side)                                        | Coexistence strategy for oneshot                                                                                                                                                                     |
|-----------------------------|-----------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ICMP echo identifier        | `IcmpClientPool::allocate_id()` — `AtomicU16`, skip 0           | Oneshot never allocates an ICMP identifier. Raw-socket ICMP campaigns use trippy's `trace_identifier` drawn from `probing::next_trace_id()`. `surge-ping` and `trippy-core` keep independent reply dispatchers.  |
| UDP nonce                   | per-target `nonce_counter: u32` in the continuous dispatcher    | Distinct wire protocol. Continuous UDP speaks the agent secret-echo handshake; campaign UDP uses trippy traceroute probes which the meshmon UDP listener rejects at the secret gate.                   |
| Trippy trace id             | `probing::next_trace_id()` — process-wide monotonic non-zero `AtomicU16`, randomly seeded | **Shared by design.** One allocator, one sequence; uniqueness is guaranteed by construction. No partitioning, no per-class counter.                                               |
| TCP/UDP source port         | OS ephemeral allocator                                          | Kernel-owned; no application-level collision possible.                                                                                                                                                 |
| Tokio blocking thread pool  | default 64 threads (set in `agent::main`)                       | `continuous_cap + campaign_cap` budgets the pool. Operators raising either cap should leave headroom for both classes.                                                                               |

`ONESHOT_PROBE_COLLISIONS_TOTAL` (`AtomicU64` at module scope) mirrors
the continuous `CROSS_CONTAMINATION_TOTAL`. A coexistence integration
test runs two concurrent oneshot batches against the same destination
and asserts both counters stay at 0; a future aggregator that uses
`Tracer::run_with` can bump `ONESHOT_PROBE_COLLISIONS_TOTAL` directly if
it observes a reply that does not belong to any tracer this module
spawned.

## Code anchors

- `OneshotProber::new` — owns the per-prober semaphore
  (`Arc<Semaphore>`), independent of `MESHMON_ICMP_TARGET_CONCURRENCY`.
- `OneshotProber::run_batch` — acquires permit, spawns per-pair task,
  drains the results channel once per target.
- `build_oneshot_config` — the builder matrix above.
- `run_one_pair` — per-pair `spawn_blocking` plus `tokio::select!`
  supervisor.
- `aggregate_latency` / `aggregate_mtr` — protocol-specific
  aggregators.
- `build_summary` / `percentile` / `ms_to_micros` —
  `MeasurementSummary` stats helpers.
- `failure_result` / `success_result` — `MeasurementResult` wire-shape
  helpers.
