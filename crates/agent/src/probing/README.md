# `probing/`

Per-target measurement primitives for the agent. The supervisor owns
one task per target; each prober here consumes a rate-watch channel
and publishes observations.

## Modules

| File             | Role                                                                       |
|------------------|----------------------------------------------------------------------------|
| `mod.rs`         | Shared types (`ProbeObservation`, `ProbeOutcome`, `HopObservation`, …)     |
| `icmp.rs`        | Continuous ICMP reachability prober (surge-ping)                           |
| `icmp_pool.rs`   | Process-wide `IcmpClientPool`, raw-socket preflight, echo identifier space |
| `tcp.rs`         | Continuous TCP connect-probe                                               |
| `udp.rs`         | Continuous UDP probe + shared sender socket pool                           |
| `echo_tcp.rs`    | Dual-stack TCP echo listener                                               |
| `echo_udp.rs`    | Dual-stack UDP echo listener (secret + allowlist gated)                    |
| `wire.rs`        | UDP wire-protocol helpers                                                  |
| `trippy.rs`      | Persistent-tracer MTR prober (topology signal only)                        |
| `oneshot.rs`     | Trippy-backed campaign prober (dispatch-driven, one batch at a time)       |
| `oneshot.md`     | Engineer-facing deep-dive on `oneshot.rs` — builder matrix, loss rules, audit |

## Signal separation

Reachability (ICMP / TCP / UDP probers) and topology (MTR) flow on
distinct channels into the supervisor: reachability emits
`ProbeObservation` onto `obs_tx`, topology emits `RouteTraceMsg` onto
`route_trace_tx`. Trippy never feeds `RollingStats`; per-hop silences
do not inflate the path's end-to-end loss.

## Shared trace-id allocator

`probing::next_trace_id()` is the process-wide monotonic non-zero
`AtomicU16` used by both the continuous MTR prober and the one-shot
campaign prober. See `oneshot.md § Shared-resource audit` for the
full ownership map and coexistence rules.

## Concurrency pools

- Continuous trippy tracers share a semaphore sized by
  `MESHMON_ICMP_TARGET_CONCURRENCY` (default 32).
- One-shot campaign tracers own their own semaphore sized from the
  cluster-wide `campaign_max_concurrency` cap; the two pools do not
  share permits.

## Testing

Tests that require raw-socket privileges gate on
`icmp_pool::skip_unless_raw_ip_socket!` and self-skip on unprivileged
hosts so the default `cargo xtask test` stays green without
`CAP_NET_RAW`.
