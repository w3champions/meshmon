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

| Kind       | Protocol | `max_rounds` | `min_round_duration` | `max_round_duration` | `read_timeout` | `grace`   | TTL      | `port_direction`  | `trace_identifier`     |
|------------|----------|-------------:|----------------------|----------------------|----------------|-----------|----------|-------------------|------------------------|
| `LATENCY`  | `ICMP`   | `Some(N)`    | `S ms`               | `T ms`               | `T ms`         | `500 ms`  | `1..=32` | default           | `Some(next_trace_id())`|
| `LATENCY`  | `TCP`    | `Some(N)`    | `S ms`               | `T ms`               | `T ms`         | `500 ms`  | `1..=32` | `FixedDest(port)` | unset                  |
| `LATENCY`  | `UDP`    | `Some(N)`    | `S ms`               | `T ms`               | `T ms`         | `500 ms`  | `1..=32` | `FixedDest(port)` | unset                  |
| `MTR` any  | —        | `Some(1)`    | `0 ms`               | `30 s`               | `30 s`         | `500 ms`  | `1..=32` | per-protocol      | ICMP only              |

MTR always pins `max_rounds(1)` — the request's `probe_count` is ignored
— and uses a hard-coded 30-second round timeout. For LATENCY,
`min_round_duration = S ms` holds the cadence floor so rounds don't
fire faster than the caller asked, and `max_round_duration = T ms`
gives each round enough headroom to collect destination replies at WAN
RTTs before closing. Tying `max_round_duration` to the stagger (an
earlier wiring) ended rounds before replies landed and made trippy's
`highest_ttl_for_round` point at whichever TTL happened to fire last,
which in turn caused `aggregate_latency` to source counters from an
intermediate hop on unreachable destinations. `grace_duration` covers
late destination replies (>200 ms RTT paths) and matches the continuous
prober.

## Loss predicates

Every protocol reads the **destination-reached hop** after the blocking
`Tracer::run()` returns. `aggregate_latency` and `aggregate_mtr`
identify that hop by walking `state.hops()` and picking the one whose
`Hop::addrs()` contains the destination IP — **not** by reading
`State::target_hop()`, which tracks `highest_ttl_for_round` and drifts
to an intermediate hop on unreachable destinations (turning a router's
Time-Exceeded RTT into a fake destination reply). A reply is guaranteed
to carry the destination IP on the destination hop because:

- ICMP Echo Reply → source = destination.
- TCP SYN/ACK or RST from the destination → source = destination.
- UDP service reply → source = destination.
- ICMP Port-Unreachable from the destination itself → source =
  destination.

Intermediate-hop Time-Exceeded messages carry the router's IP on the
router's lower-TTL hop, so they cannot leak into the destination
match. If no hop ever carries the destination IP, the probe truly did
not reach → emit `MeasurementFailureCode::Timeout`.

| Protocol | Success predicate                                                              | Silent-batch failure |
|----------|--------------------------------------------------------------------------------|----------------------|
| ICMP     | `total_recv > 0` on the destination-matched hop within `T`                     | `TIMEOUT`            |
| TCP      | Any reply on the destination-matched hop within `T` (SYN/ACK and RST both count) | `TIMEOUT`          |
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

- `target_reached_ttl` is the TTL of the hop whose `addrs()` contains
  the destination IP. If no hop saw the destination, it falls back to
  the highest responsive TTL in the snapshot. A completely silent
  trace surfaces as `MeasurementFailureCode::TIMEOUT` rather than an
  empty MTR result. The destination-IP match is the same spec §4.3
  signal used by `aggregate_latency` — avoids `State::target_hop()`
  drift on unreachable targets.
- Each responsive TTL becomes one `HopSummary`: `position = ttl`,
  `observed_ips` carries one `HopIp` per unique `Hop::addrs()` entry at
  `frequency = 1.0`, `avg_rtt_micros` comes from `Hop::best_ms()`,
  `stddev_rtt_micros = 0` (single probe), `loss_ratio = 0`.
- Each silent TTL pads with `observed_ips: []`, zero RTT, and
  `loss_ratio = 1.0`.

## Cancellation

`run_one_pair` selects on three branches, biased toward cancel:

1. `cancel.cancelled()` — drops the outer `Arc<Tracer>` and awaits the
   blocking join handle for up to 1 s, then emits
   `MeasurementFailureCode::CANCELLED`.
2. `&mut blocking` — the `spawn_blocking` join handle resolves when
   `Tracer::run()` finishes all rounds.
3. `tokio::time::sleep(max_wall_clock)` — a safety net sized from
   `probe_count * (stagger + timeout) + 5 s` for LATENCY or `35 s` for
   MTR; tripping the net emits `MeasurementFailureCode::TIMEOUT`.

### Cancellation caveat (trippy-core 0.13)

`Tracer::run()` does not expose a cancellation hook, so dropping the
outer `Arc<Tracer>` does **not** stop the blocking thread — the
`spawn_blocking` task holds its own `Arc` and keeps the raw socket
open until `run()` returns naturally. The 1-second drain budget only
guarantees a fast wire-visible `CANCELLED` emission; the underlying
blocking thread can continue running for up to
`max_rounds * (probe_stagger + read_timeout) + grace` after cancel
(≈ 20 s worst case with a 20-probe campaign at `stagger = 50 ms`,
`timeout = 950 ms`).

This has two operational consequences:

- Operators must size the tokio blocking pool (default 64 threads) to
  absorb `continuous_cap + campaign_cap` simultaneously in the worst
  case, otherwise a burst of cancellations can saturate the pool before
  the previous tracers finish unwinding.
- `meshmon_campaign_probe_collisions_total` on the agent side is the
  canary for trace-id contention while leaked tracers are still
  running; a future trippy-core patch that adds a cancellation hook
  would retire this caveat.

The wall-clock arm (branch 3) shares the caveat — we detach and emit
`TIMEOUT` on the wire, but the blocking thread follows the same
bounded-natural-exit path.

## Shared-resource audit

| Resource                    | Owner (continuous side)                                        | Coexistence strategy for oneshot                                                                                                                                                                     |
|-----------------------------|-----------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| ICMP echo identifier        | `IcmpClientPool::allocate_id()` — `AtomicU16`, skip 0           | Oneshot never allocates an ICMP identifier. Raw-socket ICMP campaigns use trippy's `trace_identifier` drawn from `probing::next_trace_id()`. `surge-ping` and `trippy-core` keep independent reply dispatchers.  |
| UDP nonce                   | per-target `nonce_counter: u32` in the continuous dispatcher    | Distinct wire protocol. Continuous UDP speaks the agent secret-echo handshake; campaign UDP uses trippy traceroute probes which the meshmon UDP listener rejects at the secret gate.                   |
| Trippy trace id             | `probing::next_trace_id()` — process-wide monotonic non-zero `AtomicU16`, randomly seeded | **Shared by design.** One allocator, one sequence; uniqueness is guaranteed by construction. No partitioning, no per-class counter.                                               |
| TCP/UDP source port         | OS ephemeral allocator                                          | Kernel-owned; no application-level collision possible.                                                                                                                                                 |
| Tokio blocking thread pool  | default 64 threads (set in `agent::main`)                       | `continuous_cap + campaign_cap` budgets the pool. Operators raising either cap should leave headroom for both classes.                                                                               |

`ONESHOT_PROBE_COLLISIONS_TOTAL` (`AtomicU64` at module scope) mirrors
the continuous `CROSS_CONTAMINATION_TOTAL` exactly: an in-process
counter with no Prometheus registration — the agent has no `/metrics`
scrape endpoint. The total is logged at shutdown alongside
`contamination_total` via `bootstrap.rs::shutdown` (`tracing::info!`),
and the coexistence integration test asserts both counters stay at 0.
No production path bumps the counter today because trippy-core's reply
dispatcher already filters mismatched replies at the library level; a
future aggregator that uses `Tracer::run_with` could `fetch_add` it
directly when it observes an unowned reply. The service-side
Prometheus counter `meshmon_campaign_probe_collisions_total` stays
seeded at 0 as a placeholder for a future cross-agent aggregation
path — T46 does not plumb per-agent collision counts through the
ingestion pipeline.

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
