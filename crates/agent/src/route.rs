//! Per-target route state tracker.
//!
//! Pure, clock-injected logic: no tokio runtime, no async, no mpsc — just a
//! regular struct the per-target supervisor owns, mutates on every trippy probe
//! via [`RouteTracker::observe`], and polls via [`RouteTracker::build_snapshot`]
//! + [`RouteTracker::diff_against`] on its 60 s snapshot tick.
//!
//! ## Dual accumulator model
//!
//! Two rolling `HashMap`s drive the tracker:
//!
//! - `hops: HashMap<(u8, IpAddr), HopObservationsAcc>` — one entry per
//!   observed (position, IP) pair over the current window.
//! - `position_probes: HashMap<u8, HopObservationsAcc>` — one entry per
//!   TTL position, counting every probe regardless of whether an IP responded.
//!
//! Every probe to TTL `p` lands in `position_probes[p]`; IP-bearing probes
//! additionally populate `hops[(p, ip)]` at the same timestamp.
//! Silent hops (no ICMP response) are retained in `position_probes` so
//! hop-level loss can be computed accurately.
//!
//! ## Snapshot construction
//!
//! `build_snapshot` sorts positions, computes per-hop summaries, then truncates
//! the list at the first position where `target_ip` appears among the observed
//! IPs. This matches mtr's "stop at destination" semantics and eliminates
//! trippy's over-probing artefacts where the target responds to TTLs beyond
//! the real path length.
//!
//! - Hop-level loss: `loss_pct(p) = 1 - successful/seen` from `position_probes[p]`.
//!   No per-IP loss attribution; silent probes are accounted at the position level.
//! - IP frequency: `frequency(ip, p) = hops[(p, ip)].seen / position_probes[p].seen`.
//!   Frequencies at a position sum to `1 - loss_pct(p)`.
//!
//! ## Diff detection
//!
//! `diff_against` compares a fresh snapshot against `last_reported` and
//! fires only on *structural* route changes:
//!
//! - `NewIp` — a previously-unseen IP crosses `new_ip_min_freq` at some
//!   position (a different router is on the path).
//! - `HopCountChanged` — path length changed by at least `hop_count_change`.
//!
//! Per-hop packet loss and per-hop RTT are measurement signals, not route
//! signals: they live in rolling stats + alerts, not in route snapshots.
//! Emitting on those would mean every near-silent first-hop router
//! (sub-percent ICMP reply rate) produces a "route changed" event every
//! 60 s tick.
//!
//! ## Complexity
//! - `observe(hops, now)`: O(H) per call where H = hops.len() (typically ≤ 30).
//! - `build_snapshot(now, now_wall)`: O(P · H_total + P · log P) per call
//!   (P = positions in window, H_total = total (pos,ip) entries).
//! - `diff_against(last)`: O(H · K) — two hashmap walks over current vs. last.
//!
//! ## Ownership boundary
//! - This module owns all accumulator logic, snapshot construction, and
//!   diff detection.
//! - The supervisor (see [`supervisor`](crate::supervisor)) owns the mpsc
//!   channel, the 60 s timer, the primary-swing reset call, and the
//!   protocol-filter rule that skips hops from non-tracked protocols.
//! - The emitter owns the `Receiver` side of the snapshot channel and
//!   the wall-clock → `i64` conversion at wire-encoding time; see
//!   [`RouteSnapshot::observed_at_micros_i64`].
//!
//! The tracker is per-target and clock-injected: `RouteTracker::new` takes a
//! window `Duration` and the target `IpAddr`; no tokio primitives are owned
//! at construction time. The window size is updated live via
//! [`RouteTracker::set_window`] when `ProbeConfig.primary_window_sec` changes.
//! On a primary-protocol swing the supervisor calls
//! [`RouteTracker::reset_for_protocol`], which drops both accumulators and
//! `last_reported`; the first non-empty snapshot after the reset becomes the
//! new baseline. [`RouteTracker::set_last_reported`] is a deliberate separate
//! API so the supervisor only advances the baseline after a successful emit.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use crate::probing::HopObservation;
use meshmon_protocol::{DiffDetection, Protocol};

/// One hop's (IP, frequency) pair in a canonical snapshot. Mirrors the
/// `HopIp` protobuf, but we keep an internal type so the tracker doesn't
/// depend on proto serialization concerns.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedIp {
    pub ip: IpAddr,
    /// Frequency in `[0.0, 1.0]` — fraction of observations at this hop
    /// position that saw this IP.
    pub frequency: f64,
}

/// One hop in a canonical snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct HopSummary {
    /// 1-indexed TTL / hop position.
    pub position: u8,
    /// Observed IPs at this position, sorted by frequency descending.
    pub observed_ips: Vec<ObservedIp>,
    /// Mean RTT across successful observations at this hop, in microseconds.
    /// `0` when no successful observation (all timeouts). Treated as
    /// "no RTT data" by downstream aggregators — a measured RTT of 0 µs
    /// is not physically possible on a routed network.
    pub avg_rtt_micros: u32,
    /// Population stddev of RTT across successful observations. `0` when
    /// `successful < 2`.
    pub stddev_rtt_micros: u32,
    /// Loss fraction at this hop over the current window (`0.0 .. 1.0`).
    pub loss_pct: f64,
}

/// Canonical per-target route snapshot. Emitted by the tracker when a
/// meaningful diff is detected; consumed by the emitter.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSnapshot {
    /// Protocol the snapshot was accumulated under (matches `RouteSnapshotRequest.protocol`).
    pub protocol: Protocol,
    /// Wall-clock time the snapshot was built. Converted to `i64 micros`
    /// at wire-encoding time by the emitter.
    pub observed_at: SystemTime,
    /// Hops in ascending `position` order.
    pub hops: Vec<HopSummary>,
}

impl RouteSnapshot {
    /// Convert the snapshot timestamp to the `i64 micros`
    /// form required by `RouteSnapshotRequest.observed_at_micros`.
    /// Clamps to `i64::MAX` if the duration somehow overflows (it cannot
    /// in practice — Unix epoch → now is ~1.8e15 micros in 2026).
    pub fn observed_at_micros_i64(&self) -> i64 {
        let d = self
            .observed_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
    }
}

/// Why a diff fired. Carried on `RouteDiff` for logging / observability;
/// never surfaced on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffReason {
    /// A new IP crossed `new_ip_min_freq` at some hop.
    NewIp { position: u8 },
    /// Hop count changed by ≥ `hop_count_change`.
    HopCountChanged { from: usize, to: usize },
}

/// Result of a `diff_against` call. The `reasons` field contains every
/// rule that fired on this snapshot pair; at least one entry is
/// guaranteed when the outer `Option<RouteDiff>` is `Some`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDiff {
    pub reasons: Vec<DiffReason>,
}

/// Per-`(position, ip)` rolling observation accumulator. Rolling window
/// semantics mirror `RollingStats`: samples older than `now - window`
/// are purged in O(K) amortized. RTT running sums are maintained O(1)
/// on insert/purge; timeouts contribute to `seen` but not to
/// `successful` / `sum_rtt_*`.
#[derive(Debug, Clone, Default)]
struct HopObservationsAcc {
    /// `(t, rtt)` samples. `rtt = None` means the hop timed out this round.
    samples: VecDeque<(Instant, Option<u32>)>,
    seen: u32,
    successful: u32,
    sum_rtt_micros: u64,
    sum_rtt_sq_micros: u128,
}

impl HopObservationsAcc {
    fn insert(&mut self, t: Instant, rtt: Option<u32>) {
        self.samples.push_back((t, rtt));
        self.seen += 1;
        if let Some(r) = rtt {
            self.successful += 1;
            self.sum_rtt_micros += r as u64;
            self.sum_rtt_sq_micros += (r as u128) * (r as u128);
        }
    }

    fn purge(&mut self, cutoff: Instant) -> u32 {
        let mut purged = 0u32;
        while let Some(&(t, _)) = self.samples.front() {
            if t > cutoff {
                break;
            }
            let (_, rtt) = self.samples.pop_front().expect("checked front");
            self.seen -= 1;
            if let Some(r) = rtt {
                self.successful -= 1;
                self.sum_rtt_micros -= r as u64;
                self.sum_rtt_sq_micros -= (r as u128) * (r as u128);
            }
            purged += 1;
        }
        purged
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Per-target route-state tracker. One instance per supervisor.
#[derive(Debug)]
pub struct RouteTracker {
    /// Protocol currently being accumulated (== the primary). `None` means
    /// "no primary elected yet" or "path is Unreachable"; the tracker
    /// drops all incoming hops while `None`.
    protocol: Option<Protocol>,
    /// Rolling window; primary_window_sec from `ProbeConfig` (default 300 s).
    window: Duration,
    /// `(position, ip)` → per-hop accumulator. Flat for cache locality.
    /// Maintained by `observe`; consumed by `build_snapshot`.
    hops: HashMap<(u8, IpAddr), HopObservationsAcc>,
    /// Per-position probe accumulator. Every probe to TTL `p` (silent or
    /// IP-bearing) lands here; IP-bearing probes additionally land in
    /// `hops[(p, ip)]` at the same timestamp. Replaces the former
    /// `position_totals: HashMap<u8, u32>` scalar.
    position_probes: HashMap<u8, HopObservationsAcc>,
    /// Most-recently emitted snapshot, for the next `diff_against` call.
    last_reported: Option<RouteSnapshot>,
    /// IP address of the probe target. Used by `build_snapshot` to truncate
    /// the hop list at the first position where the destination responded.
    target_ip: IpAddr,
}

impl RouteTracker {
    /// Build a new tracker. `window` should be the primary-protocol window
    /// (`primary_window_sec`, default 300 s). Starts with `protocol = None`
    /// so the tracker silently drops observations until the supervisor
    /// calls [`RouteTracker::reset_for_protocol`] once a primary is elected.
    pub fn new(window: Duration, target_ip: IpAddr) -> Self {
        Self {
            protocol: None,
            window,
            hops: HashMap::new(),
            position_probes: HashMap::new(),
            last_reported: None,
            // Canonicalize at construction so the truncation predicate in
            // build_snapshot always compares canonical-form IPs on both sides.
            target_ip: target_ip.to_canonical(),
        }
    }

    /// Protocol the tracker is currently accumulating under, or `None`
    /// when no primary is elected.
    pub fn protocol(&self) -> Option<Protocol> {
        self.protocol
    }

    /// Update the window size (called by the supervisor when
    /// `ProbeConfig.primary_window_sec` changes).
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// Currently configured window size — exposed for tests + diagnostics.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Reset state for a new protocol (primary swing). Drops the
    /// accumulator and `last_reported` so the next non-empty snapshot
    /// is a fresh baseline. Passing `None` puts the tracker in the
    /// "no primary" state (no observations recorded until the next
    /// non-`None` reset).
    pub fn reset_for_protocol(&mut self, protocol: Option<Protocol>) {
        self.protocol = protocol;
        self.hops.clear();
        self.position_probes.clear();
        self.last_reported = None;
        #[cfg(debug_assertions)]
        self.assert_consistency();
    }

    /// Ingest the hop observations from one trippy round. Purges samples
    /// older than `now - window` before inserting the new ones.
    pub fn observe(&mut self, hops: &[HopObservation], now: Instant) {
        if self.protocol.is_none() {
            return;
        }

        // `now.checked_sub` returns `None` immediately after runtime start
        // when the window would reach before the tokio runtime epoch. In
        // that case nothing is old enough to purge yet; fall through to
        // the insert step unchanged.
        if let Some(cutoff) = now.checked_sub(self.window) {
            self.purge_stale(cutoff);
        }

        for obs in hops {
            // Record the probe at this position, silent or not.
            let pos_acc = self.position_probes.entry(obs.position).or_default();
            pos_acc.insert(now, obs.rtt_micros);
            // If an IP responded, also record at (position, ip).
            // Canonicalize the IP so the truncation predicate in build_snapshot
            // matches target_ip (also canonical) regardless of whether trippy
            // returns IPv4-mapped-IPv6 or plain IPv4 from the kernel.
            if let Some(ip) = obs.ip {
                let ip = ip.to_canonical();
                let acc = self.hops.entry((obs.position, ip)).or_default();
                acc.insert(now, obs.rtt_micros);
            }
        }
        #[cfg(debug_assertions)]
        self.assert_consistency();
    }

    /// Walk the accumulator and drop samples older than `cutoff`. Keeps
    /// `position_probes` in sync and removes `(position, ip)` entries that
    /// became empty so ghost zero-count rows can't surface in
    /// `build_snapshot`. Boundary semantics match
    /// `stats::RollingStats::purge_old`: a sample with `t == cutoff` is
    /// expired.
    fn purge_stale(&mut self, cutoff: Instant) {
        let mut empty_ip_keys: Vec<(u8, IpAddr)> = Vec::new();
        for (key, acc) in self.hops.iter_mut() {
            acc.purge(cutoff);
            if acc.is_empty() {
                empty_ip_keys.push(*key);
            }
        }
        for key in &empty_ip_keys {
            self.hops.remove(key);
        }

        let mut empty_positions: Vec<u8> = Vec::new();
        for (pos, acc) in self.position_probes.iter_mut() {
            acc.purge(cutoff);
            if acc.is_empty() {
                empty_positions.push(*pos);
            }
        }
        for pos in &empty_positions {
            self.position_probes.remove(pos);
        }

        #[cfg(debug_assertions)]
        self.assert_consistency();
    }

    #[cfg(debug_assertions)]
    fn assert_consistency(&self) {
        for (pos, _) in self.hops.keys() {
            debug_assert!(
                self.position_probes.contains_key(pos),
                "hops has entry for pos {pos} but position_probes does not",
            );
        }
        let mut ip_sums: HashMap<u8, u32> = HashMap::new();
        for ((pos, _), acc) in &self.hops {
            *ip_sums.entry(*pos).or_default() += acc.seen;
        }
        for (pos, acc) in &self.position_probes {
            let ip_sum = ip_sums.get(pos).copied().unwrap_or(0);
            debug_assert!(
                acc.seen >= ip_sum,
                "position_probes[{pos}].seen={} < Σ hops={ip_sum}",
                acc.seen,
            );
        }
    }

    /// Build a canonical snapshot from the current window. Returns `None`
    /// when the accumulator is empty (nothing to summarize).
    pub fn build_snapshot(&mut self, now: Instant, now_wall: SystemTime) -> Option<RouteSnapshot> {
        let protocol = self.protocol?;
        // Refresh the window so a quiet interval (no observe() calls) doesn't
        // expose stale samples through the snapshot.
        if let Some(cutoff) = now.checked_sub(self.window) {
            self.purge_stale(cutoff);
        }
        if self.position_probes.is_empty() {
            return None;
        }

        let mut positions: Vec<u8> = self.position_probes.keys().copied().collect();
        positions.sort();

        let mut hops_out: Vec<HopSummary> = Vec::with_capacity(positions.len());
        // NOTE: scan is O(P · H_total). Fine at the hop counts meshmon sees
        // (≤ 30 positions, typically < 5 IPs per position). If future paths
        // need higher-ECMP support, pre-group `self.hops` by position before
        // the outer loop to reduce to O(H_total + P·K).
        for position in positions {
            let Some(pos_acc) = self.position_probes.get(&position) else {
                continue;
            };
            if pos_acc.seen == 0 {
                continue;
            }
            let total_f = pos_acc.seen as f64;

            // Collect IPs observed at this position.
            let mut observed_ips: Vec<ObservedIp> = Vec::new();
            for ((p, ip), ip_acc) in self.hops.iter() {
                if *p != position {
                    continue;
                }
                let frequency = ip_acc.seen as f64 / total_f;
                observed_ips.push(ObservedIp { ip: *ip, frequency });
            }
            observed_ips.sort_by(|a, b| {
                b.frequency
                    .partial_cmp(&a.frequency)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.ip.cmp(&b.ip))
            });

            // RTT mean + stddev from the position accumulator: covers every
            // responding probe at this TTL regardless of which IP answered.
            let avg_rtt_micros = if pos_acc.successful == 0 {
                0
            } else {
                (pos_acc.sum_rtt_micros / pos_acc.successful as u64).min(u32::MAX as u64) as u32
            };
            let stddev_rtt_micros = if pos_acc.successful < 2 {
                0
            } else {
                let n = pos_acc.successful as f64;
                let mean = pos_acc.sum_rtt_micros as f64 / n;
                let mean_sq = pos_acc.sum_rtt_sq_micros as f64 / n;
                let var = (mean_sq - mean * mean).max(0.0);
                let stddev = var.sqrt();
                if !stddev.is_finite() {
                    0
                } else if stddev >= u32::MAX as f64 {
                    u32::MAX
                } else {
                    stddev as u32
                }
            };

            // Hop-level loss: silent probes / all probes at this TTL.
            let loss_pct = (pos_acc.seen - pos_acc.successful) as f64 / pos_acc.seen as f64;

            hops_out.push(HopSummary {
                position,
                observed_ips,
                avg_rtt_micros,
                stddev_rtt_micros,
                loss_pct,
            });
        }

        // Truncate at the first position whose observed_ips contains the
        // target IP at any frequency > 0. Matches mtr's "stop at destination"
        // semantics and drops trippy's over-probe replies where the
        // destination responds to TTLs > the real hop count.
        if let Some(idx) = hops_out.iter().position(|h| {
            h.observed_ips
                .iter()
                .any(|o| o.ip == self.target_ip && o.frequency > 0.0)
        }) {
            hops_out.truncate(idx + 1);
        }

        if hops_out.is_empty() {
            return None;
        }

        Some(RouteSnapshot {
            protocol,
            observed_at: now_wall,
            hops: hops_out,
        })
    }

    /// Compare `new` to the tracker's `last_reported` snapshot. Returns
    /// `Some(RouteDiff)` when the path's structure has changed — either
    /// a new IP crosses `new_ip_min_freq` at some position, or the hop
    /// count moves by at least `hop_count_change`. Free function form to
    /// keep the tracker's mutable state untouched by the diff check.
    ///
    /// Caller (supervisor) is responsible for updating `last_reported`
    /// after a successful emit via [`RouteTracker::set_last_reported`].
    pub fn diff_against(
        &self,
        new: &RouteSnapshot,
        thresholds: &DiffDetection,
    ) -> Option<RouteDiff> {
        let last = self.last_reported.as_ref()?;

        // The supervisor calls `reset_for_protocol` on every primary swing,
        // which clears `last_reported`. The early `let last = ...?;` above
        // makes this function a no-op when that happens, so `last` can only
        // carry the tracker's current protocol.
        debug_assert_eq!(
            new.protocol, last.protocol,
            "protocol mismatch should be prevented by supervisor reset"
        );

        let mut reasons: Vec<DiffReason> = Vec::new();

        // Hop count change — path got longer or shorter.
        let from = last.hops.len();
        let to = new.hops.len();
        let delta = from.abs_diff(to);
        if delta >= thresholds.hop_count_change as usize {
            reasons.push(DiffReason::HopCountChanged { from, to });
        }

        // New-IP rule — a previously-unseen IP crosses the minimum
        // frequency at any position.
        let last_by_pos: HashMap<u8, &HopSummary> =
            last.hops.iter().map(|h| (h.position, h)).collect();
        for new_hop in &new.hops {
            let last_hop = last_by_pos.get(&new_hop.position);
            for ip_entry in &new_hop.observed_ips {
                if ip_entry.frequency < thresholds.new_ip_min_freq {
                    continue;
                }
                let previously_present = last_hop
                    .map(|h| h.observed_ips.iter().any(|o| o.ip == ip_entry.ip))
                    .unwrap_or(false);
                if !previously_present {
                    reasons.push(DiffReason::NewIp {
                        position: new_hop.position,
                    });
                    break; // one reason per position is enough
                }
            }
        }

        if reasons.is_empty() {
            None
        } else {
            Some(RouteDiff { reasons })
        }
    }

    /// Update `last_reported` after a successful emit. Exposed as a
    /// discrete call so the supervisor can conditionally update it
    /// (e.g. only after a non-blocking `try_send` succeeds).
    pub fn set_last_reported(&mut self, snap: RouteSnapshot) {
        self.last_reported = Some(snap);
    }

    /// Current `last_reported` snapshot — `None` until the first emit
    /// or immediately after a `reset_for_protocol` call.
    pub fn last_reported(&self) -> Option<&RouteSnapshot> {
        self.last_reported.as_ref()
    }
}

/// Envelope pushed onto the supervisor → emitter channel. Stamps
/// `target_id` so the emitter can construct a [`RouteSnapshotRequest`]
/// without having to look up supervisors by target.
///
/// `path_summary` carries the **end-to-end** loss / RTT for the
/// `RouteSnapshotRequest.path_summary` wire field. It is sourced from the
/// elected primary protocol's `RollingStats`, not from the trippy
/// destination hop, so the matrix view agrees with the alerts pipeline
/// (`meshmon_path_failure_rate`). The supervisor populates this at
/// snapshot-tick time from `last_state.primary` + the matching
/// per-protocol `RollingStats`. When the primary is `None` (path is
/// unhealthy / not yet elected), the emitter encodes loss as `1.0` to
/// surface the unreachable state in the matrix.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSnapshotEnvelope {
    pub target_id: String,
    pub snapshot: RouteSnapshot,
    pub path_summary: PathSummarySource,
}

/// End-to-end path summary inputs sourced from the elected primary
/// protocol's [`crate::stats::RollingStats`]. Carried on the envelope so
/// the emitter encodes the wire `PathSummary` without having to reach
/// back into supervisor state.
///
/// Trippy destination-hop accounting (`hops.last()`) is intentionally
/// **not** used here — its 60 s window with 500 ms grace inflates phantom
/// loss on healthy LANs whenever the destination's reply lands past the
/// grace deadline. The dedicated per-protocol pinger's rolling stats are
/// the authoritative end-to-end signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathSummarySource {
    /// The protocol the supervisor's `TargetStateMachine` elected as
    /// primary on the most recent eval tick, or `None` when no protocol
    /// has met the sample-floor yet (cold start) or all protocols are
    /// unhealthy. `None` makes the emitter encode `loss_pct = 1.0`.
    pub primary_protocol: Option<Protocol>,
    /// Loss fraction from the elected protocol's rolling stats, in
    /// `[0.0, 1.0]`. Ignored when `primary_protocol` is `None`.
    pub loss_pct: f64,
    /// Mean RTT from the elected protocol's rolling stats, in
    /// microseconds. `0` when no successful samples in the window.
    pub avg_rtt_micros: u32,
}

impl PathSummarySource {
    /// Builder for the unhealthy / unelected case — encoded as 100% loss
    /// on the wire so the matrix renders red.
    pub fn unhealthy() -> Self {
        Self {
            primary_protocol: None,
            loss_pct: 1.0,
            avg_rtt_micros: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn five_min() -> Duration {
        Duration::from_secs(300)
    }

    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn new_tracker_has_no_protocol_no_last_reported() {
        let t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        assert_eq!(t.protocol(), None);
        assert_eq!(t.window(), five_min());
        assert!(t.last_reported().is_none());
    }

    #[test]
    fn reset_for_protocol_sets_protocol() {
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        t.reset_for_protocol(Some(Protocol::Icmp));
        assert_eq!(t.protocol(), Some(Protocol::Icmp));
    }

    #[test]
    fn reset_for_protocol_to_none_clears_protocol() {
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        t.reset_for_protocol(Some(Protocol::Icmp));
        t.reset_for_protocol(None);
        assert_eq!(t.protocol(), None);
    }

    #[test]
    fn reset_clears_last_reported() {
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        t.reset_for_protocol(Some(Protocol::Icmp));
        t.set_last_reported(RouteSnapshot {
            protocol: Protocol::Icmp,
            observed_at: SystemTime::UNIX_EPOCH,
            hops: vec![],
        });
        assert!(t.last_reported().is_some());
        t.reset_for_protocol(Some(Protocol::Tcp));
        assert!(
            t.last_reported().is_none(),
            "primary swing must clear last_reported so the first post-swing snapshot emits unconditionally",
        );
    }

    fn hop(position: u8, ip: Option<IpAddr>, rtt_micros: Option<u32>) -> HopObservation {
        HopObservation {
            position,
            ip,
            rtt_micros,
        }
    }

    fn tracker_ready(window: Duration, protocol: Protocol) -> RouteTracker {
        let mut t = RouteTracker::new(window, ipv4(127, 0, 0, 1));
        t.reset_for_protocol(Some(protocol));
        t
    }

    #[test]
    fn observe_noop_when_no_protocol() {
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        // No reset_for_protocol call — protocol is None.
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        // Internal accumulator must remain empty.
        assert!(t.hops.is_empty());
        assert!(t.position_probes.is_empty());
    }

    #[test]
    fn observe_single_success_populates_accumulator() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        let key = (1u8, ipv4(10, 0, 0, 1));
        let acc = t.hops.get(&key).expect("inserted");
        assert_eq!(acc.seen, 1);
        assert_eq!(acc.successful, 1);
        assert_eq!(acc.sum_rtt_micros, 5_000);
        assert_eq!(acc.sum_rtt_sq_micros, 25_000_000);
        assert_eq!(t.position_probes.get(&1).map(|a| a.seen), Some(1));
    }

    #[test]
    fn observe_star_hop_is_ignored() {
        // A hop with ip = None AND rtt = None = total timeout ("star hop").
        // Star hops have no IP, so they are NOT recorded in the per-(pos,ip)
        // hops map — but they DO land in position_probes (probe was sent).
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        t.observe(&[hop(3, None, None)], Instant::now());
        assert!(
            t.hops.is_empty(),
            "star hops carry no IP and are not in hops map"
        );
        assert!(
            !t.position_probes.is_empty(),
            "silent hops land in position_probes"
        );
    }

    #[test]
    fn observe_partial_timeout_with_ip_counts_as_loss() {
        // A hop with ip = Some (router responded once) but rtt = None on
        // this particular round — count as a loss sample at that IP.
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), None)], Instant::now());
        let acc = t.hops.get(&(2u8, ipv4(10, 0, 0, 2))).expect("inserted");
        assert_eq!(acc.seen, 1);
        assert_eq!(acc.successful, 0);
        assert_eq!(acc.sum_rtt_micros, 0);
        assert_eq!(t.position_probes.get(&2).map(|a| a.seen), Some(1));
    }

    #[test]
    fn observe_repeated_same_ip_increments_counters() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        for rtt in [1_000_u32, 2_000, 3_000] {
            t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(rtt))], now);
        }
        let acc = t.hops.get(&(1u8, ipv4(10, 0, 0, 1))).expect("inserted");
        assert_eq!(acc.seen, 3);
        assert_eq!(acc.successful, 3);
        assert_eq!(acc.sum_rtt_micros, 6_000);
        assert_eq!(
            acc.sum_rtt_sq_micros,
            1_000_u128.pow(2) + 2_000_u128.pow(2) + 3_000_u128.pow(2)
        );
        assert_eq!(t.position_probes.get(&1).map(|a| a.seen), Some(3));
    }

    #[test]
    fn observe_purges_old_samples() {
        let base = Instant::now();
        let mut t = tracker_ready(Duration::from_secs(60), Protocol::Icmp);
        // First round at t+0.
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], base);
        // Second round at t+35s — strictly inside the window relative to t+90
        // (cutoff = t+30; t+35 > t+30 so it survives).
        t.observe(
            &[hop(1, Some(ipv4(10, 0, 0, 1)), Some(2_000))],
            base + Duration::from_secs(35),
        );
        // Fast-forward to t+90s. The t+0 sample falls out.
        t.observe(
            &[hop(1, Some(ipv4(10, 0, 0, 1)), Some(3_000))],
            base + Duration::from_secs(90),
        );
        let acc = t
            .hops
            .get(&(1u8, ipv4(10, 0, 0, 1)))
            .expect("still present");
        assert_eq!(acc.seen, 2, "t+0 sample was purged");
        assert_eq!(acc.sum_rtt_micros, 2_000 + 3_000);
        assert_eq!(t.position_probes.get(&1).map(|a| a.seen), Some(2));
    }

    #[test]
    fn observe_multiple_ips_at_same_position() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        // ECMP-ish: position 3 sees two different IPs.
        t.observe(&[hop(3, Some(ipv4(10, 1, 0, 1)), Some(4_000))], now);
        t.observe(&[hop(3, Some(ipv4(10, 1, 0, 2)), Some(4_500))], now);
        t.observe(&[hop(3, Some(ipv4(10, 1, 0, 1)), Some(4_100))], now);
        let a = t.hops.get(&(3u8, ipv4(10, 1, 0, 1))).unwrap();
        let b = t.hops.get(&(3u8, ipv4(10, 1, 0, 2))).unwrap();
        assert_eq!(a.seen, 2);
        assert_eq!(b.seen, 1);
        assert_eq!(t.position_probes.get(&3).map(|a| a.seen), Some(3));
    }

    #[test]
    fn observe_purge_of_all_samples_cleans_up_entry() {
        let base = Instant::now();
        let mut t = tracker_ready(Duration::from_secs(60), Protocol::Icmp);
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], base);
        // All samples at position 1, IP .1 fall out at t+120.
        t.observe(
            &[hop(2, Some(ipv4(10, 0, 0, 2)), Some(2_000))],
            base + Duration::from_secs(120),
        );
        // The (1, .1) accumulator is now empty — the implementation MUST
        // remove it from the HashMap so it doesn't pollute build_snapshot's
        // iteration with ghost zero-count entries.
        assert!(
            !t.hops.contains_key(&(1u8, ipv4(10, 0, 0, 1))),
            "empty accumulator entries must be pruned, hops={:?}",
            t.hops.keys().collect::<Vec<_>>(),
        );
        assert_eq!(t.position_probes.get(&1).map(|a| a.seen), None);
    }

    fn systemtime_epoch_plus(micros: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_micros(micros)
    }

    #[test]
    fn build_snapshot_returns_none_for_empty_accumulator() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        let wall = systemtime_epoch_plus(1_000);
        assert!(t.build_snapshot(now, wall).is_none());
    }

    #[test]
    fn build_snapshot_returns_none_when_no_protocol() {
        // Even if hops somehow landed in the accumulator (they can't
        // via observe(), but defensively), a no-protocol tracker has
        // nothing meaningful to emit.
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        let now = Instant::now();
        let wall = systemtime_epoch_plus(1_000);
        assert!(t.build_snapshot(now, wall).is_none());
    }

    #[test]
    fn build_snapshot_single_hop_single_ip() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        let wall = systemtime_epoch_plus(2_000_000);
        let snap = t.build_snapshot(now, wall).expect("non-empty");
        assert_eq!(snap.protocol, Protocol::Icmp);
        assert_eq!(snap.observed_at, wall);
        assert_eq!(snap.hops.len(), 1);
        let h = &snap.hops[0];
        assert_eq!(h.position, 1);
        assert_eq!(h.observed_ips.len(), 1);
        assert_eq!(h.observed_ips[0].ip, ipv4(10, 0, 0, 1));
        assert!((h.observed_ips[0].frequency - 1.0).abs() < 1e-9);
        assert_eq!(h.avg_rtt_micros, 5_000);
        assert_eq!(h.stddev_rtt_micros, 0, "single sample → stddev = 0");
        assert!((h.loss_pct - 0.0).abs() < 1e-9);
    }

    #[test]
    fn build_snapshot_multi_ip_per_hop_is_sorted_by_frequency_desc() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        // Hop 3 ECMP: IP A seen 7 times, IP B seen 3 times → 70/30 split.
        for _ in 0..7 {
            t.observe(&[hop(3, Some(ipv4(10, 1, 0, 1)), Some(4_000))], now);
        }
        for _ in 0..3 {
            t.observe(&[hop(3, Some(ipv4(10, 1, 0, 2)), Some(4_500))], now);
        }
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(snap.hops.len(), 1);
        let h = &snap.hops[0];
        assert_eq!(h.observed_ips.len(), 2);
        // Sorted by frequency descending.
        assert_eq!(h.observed_ips[0].ip, ipv4(10, 1, 0, 1));
        assert!((h.observed_ips[0].frequency - 0.7).abs() < 1e-9);
        assert_eq!(h.observed_ips[1].ip, ipv4(10, 1, 0, 2));
        assert!((h.observed_ips[1].frequency - 0.3).abs() < 1e-9);
    }

    #[test]
    fn build_snapshot_hops_are_sorted_by_position_asc() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        // Insert hops out of order.
        t.observe(&[hop(5, Some(ipv4(10, 0, 0, 5)), Some(5_000))], now);
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
        t.observe(&[hop(3, Some(ipv4(10, 0, 0, 3)), Some(3_000))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(positions, vec![1, 3, 5]);
    }

    #[test]
    fn build_snapshot_loss_pct_reflects_timeouts() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        // 3 successful, 1 timeout at the same IP → 25% loss.
        for rtt in [1_000_u32, 2_000, 3_000] {
            t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), Some(rtt))], now);
        }
        t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), None)], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let h = &snap.hops[0];
        assert_eq!(h.position, 2);
        assert!((h.loss_pct - 0.25).abs() < 1e-9, "got {}", h.loss_pct);
        // avg = (1000+2000+3000)/3 = 2000 (timeouts don't count toward mean).
        assert_eq!(h.avg_rtt_micros, 2_000);
    }

    #[test]
    fn build_snapshot_stddev_matches_population_formula() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        // RTTs {1000, 2000, 3000, 4000} → mean 2500, population stddev = ~1118.0.
        for rtt in [1_000_u32, 2_000, 3_000, 4_000] {
            t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(rtt))], now);
        }
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let h = &snap.hops[0];
        assert_eq!(h.avg_rtt_micros, 2_500);
        // Allow ±2 µs rounding on the u32 cast.
        let diff = (h.stddev_rtt_micros as i64 - 1_118).abs();
        assert!(
            diff <= 2,
            "stddev = {}, diff = {}",
            h.stddev_rtt_micros,
            diff
        );
    }

    #[test]
    fn build_snapshot_observed_at_is_wall_parameter_not_recomputed() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        let wall = systemtime_epoch_plus(1_712_000_000_000_000); // arbitrary
        let snap = t.build_snapshot(now, wall).unwrap();
        assert_eq!(snap.observed_at, wall);
        assert_eq!(snap.observed_at_micros_i64(), 1_712_000_000_000_000);
    }

    fn default_diff() -> DiffDetection {
        DiffDetection {
            new_ip_min_freq: 0.20,
            hop_count_change: 1,
        }
    }

    /// `(position, [(ip, frequency)…], avg_rtt_micros)` — one hop row for the
    /// `snap` test helper. Named to sidestep clippy::type_complexity.
    type HopSpec = (u8, Vec<(IpAddr, f64)>, u32);

    /// Helper: build a snapshot from a list of `(position, Vec<(ip, freq)>, avg_rtt)`.
    fn snap(protocol: Protocol, hops: &[HopSpec]) -> RouteSnapshot {
        RouteSnapshot {
            protocol,
            observed_at: SystemTime::UNIX_EPOCH,
            hops: hops
                .iter()
                .map(|(pos, ips, rtt)| HopSummary {
                    position: *pos,
                    observed_ips: ips
                        .iter()
                        .map(|(ip, f)| ObservedIp {
                            ip: *ip,
                            frequency: *f,
                        })
                        .collect(),
                    avg_rtt_micros: *rtt,
                    stddev_rtt_micros: 0,
                    loss_pct: 0.0,
                })
                .collect(),
        }
    }

    fn tracker_with_last(protocol: Protocol, last: RouteSnapshot) -> RouteTracker {
        let mut t = RouteTracker::new(five_min(), ipv4(127, 0, 0, 1));
        t.reset_for_protocol(Some(protocol));
        t.set_last_reported(last);
        t
    }

    #[test]
    fn diff_returns_none_when_snapshots_identical() {
        let a = snap(
            Protocol::Icmp,
            &[(1, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, a.clone());
        assert_eq!(t.diff_against(&a, &default_diff()), None);
    }

    // ---------------- NewIp: new IP ≥ threshold ----------------

    #[test]
    fn diff_rule1_fires_on_new_ip_above_threshold() {
        let last = snap(
            Protocol::Icmp,
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        // Position 3 now shows a second IP at 30% (above 20% threshold).
        let new = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.70), (ipv4(10, 0, 0, 2), 0.30)],
                5_000,
            )],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(
            diff.reasons
                .iter()
                .any(|r| matches!(r, DiffReason::NewIp { position: 3 })),
            "expected NewIp at position 3, got {:?}",
            diff.reasons,
        );
    }

    #[test]
    fn diff_rule1_does_not_fire_below_threshold() {
        let last = snap(
            Protocol::Icmp,
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        // Second IP at 5% — below the 20% threshold.
        let new = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.95), (ipv4(10, 0, 0, 2), 0.05)],
                5_000,
            )],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    #[test]
    fn diff_rule1_at_exact_threshold_fires() {
        let last = snap(
            Protocol::Icmp,
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        // Exactly at 20%: the comparison is `>=`, so this fires.
        let new = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.80), (ipv4(10, 0, 0, 2), 0.20)],
                5_000,
            )],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t
            .diff_against(&new, &default_diff())
            .expect("diff at boundary");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::NewIp { position: 3 })),);
    }

    // ---------------- Per-hop IP frequency changes must not diff ----------------

    #[test]
    fn diff_does_not_fire_when_previously_seen_ip_loses_frequency() {
        // A previously-seen IP dropping off is not by itself a route change:
        // it's either been replaced (in which case the NewIp rule fires on
        // the replacement) or it's just gone silent (a measurement signal,
        // not a topology signal). Pair with
        // `diff_ignores_pure_loss_and_rtt_noise_at_near_silent_hop`.
        let last = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.78), (ipv4(10, 0, 0, 2), 0.22)],
                5_000,
            )],
        );
        let new = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.98), (ipv4(10, 0, 0, 2), 0.02)],
                5_000,
            )],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    #[test]
    fn diff_does_not_fire_when_minor_ip_disappears() {
        let last = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.90), (ipv4(10, 0, 0, 2), 0.10)],
                5_000,
            )],
        );
        let new = snap(
            Protocol::Icmp,
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    // ---------------- HopCountChanged: hop count change ----------------

    #[test]
    fn diff_rule3_fires_when_route_lengthens() {
        let last = snap(
            Protocol::Icmp,
            &[(1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000)],
        );
        let new = snap(
            Protocol::Icmp,
            &[
                (1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000),
                (2, vec![(ipv4(10, 0, 0, 2), 1.0)], 2_000),
            ],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::HopCountChanged { from: 1, to: 2 })));
    }

    #[test]
    fn diff_rule3_fires_when_route_shortens() {
        let last = snap(
            Protocol::Icmp,
            &[
                (1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000),
                (2, vec![(ipv4(10, 0, 0, 2), 1.0)], 2_000),
            ],
        );
        let new = snap(
            Protocol::Icmp,
            &[(1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::HopCountChanged { from: 2, to: 1 })));
    }

    #[test]
    fn diff_rule3_respects_threshold_of_2() {
        // With hop_count_change = 2, a 1-hop change must not fire
        // HopCountChanged. To isolate it, also disable NewIp by setting a
        // freq threshold no real frequency can cross — otherwise a
        // newly-added hop's IP would trip NewIp at the new position.
        let last = snap(
            Protocol::Icmp,
            &[(1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000)],
        );
        let new = snap(
            Protocol::Icmp,
            &[
                (1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000),
                (2, vec![(ipv4(10, 0, 0, 2), 1.0)], 2_000),
            ],
        );
        let mut thr = default_diff();
        thr.hop_count_change = 2;
        thr.new_ip_min_freq = 2.0; // disable NewIp so only HopCountChanged can fire
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &thr), None);
    }

    // ---------------- Per-hop RTT shifts must not diff ----------------

    #[test]
    fn diff_returns_none_on_large_rtt_shift_same_topology() {
        // 60 % avg-RTT shift at an otherwise-stable hop. Per-hop RTT is a
        // measurement signal (jitter / latency), not a route signal.
        let last = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 10_000)],
        );
        let new = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 16_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    // ---------------- Multi-rule ----------------
    //
    // Protocol mismatch is not tested here: the supervisor calls
    // `reset_for_protocol` on every primary swing, which clears
    // `last_reported`, and the early `last_reported.as_ref()?` in
    // `diff_against` makes protocol mismatch unreachable. The
    // `debug_assert_eq!` in `diff_against` pins this invariant under
    // debug builds.

    #[test]
    fn diff_collects_multiple_reasons() {
        let last = snap(
            Protocol::Icmp,
            &[(1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000)],
        );
        // Both NewIp (new IP at position 2 at 100%) AND HopCountChanged (+1).
        let new = snap(
            Protocol::Icmp,
            &[
                (1, vec![(ipv4(10, 0, 0, 1), 1.0)], 1_000),
                (2, vec![(ipv4(10, 0, 0, 2), 1.0)], 2_000),
            ],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(
            diff.reasons.len() >= 2,
            "expected at least two reasons, got {:?}",
            diff.reasons,
        );
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::NewIp { .. })));
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::HopCountChanged { .. })));
    }

    #[test]
    fn dod1_constant_observations_produce_stable_snapshots_no_diffs() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let mut now = Instant::now();
        let wall_base = SystemTime::UNIX_EPOCH;

        // Establish baseline: 30 seconds of identical observations.
        for _ in 0..30 {
            t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
            now += Duration::from_secs(1);
        }
        let first = t.build_snapshot(now, wall_base).expect("first snap");
        t.set_last_reported(first.clone());

        // Another 60 s of identical observations → next snapshot identical → no diff.
        for _ in 0..60 {
            t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
            now += Duration::from_secs(1);
        }
        let second = t
            .build_snapshot(now, wall_base + Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            t.diff_against(&second, &default_diff()),
            None,
            "stable stream must not produce diffs",
        );
    }

    #[test]
    fn dod2_new_ip_at_30pct_fires_once_then_stable() {
        // Use a 20-second window so the baseline rolls out of the rolling
        // accumulator by the time we evaluate the post-change snapshot —
        // otherwise the 100% baseline dilutes the 30% into 15% overall and
        // the DoD-intent "30% new IP" wouldn't cross the threshold.
        let mut t = tracker_ready(Duration::from_secs(20), Protocol::Icmp);
        let mut now = Instant::now();

        // Baseline: 20 rounds, position 3 only ever shows .1.
        for _ in 0..20 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
            now += Duration::from_secs(1);
        }
        let baseline = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        t.set_last_reported(baseline);

        // Now inject 30% of the next 20 rounds showing a NEW IP.
        for i in 0..20 {
            let obs_ip = if i % 10 < 3 {
                ipv4(10, 0, 0, 2)
            } else {
                ipv4(10, 0, 0, 1)
            };
            t.observe(&[hop(3, Some(obs_ip), Some(5_000))], now);
            now += Duration::from_secs(1);
        }
        let after = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        let diff = t
            .diff_against(&after, &default_diff())
            .expect("diff expected");
        assert!(
            diff.reasons
                .iter()
                .any(|r| matches!(r, DiffReason::NewIp { position: 3 })),
            "expected NewIp @ position 3, got {:?}",
            diff.reasons,
        );

        // After promoting `after` to last_reported, a second snapshot with
        // the same 70/30 distribution must NOT re-diff.
        t.set_last_reported(after);
        for i in 0..20 {
            let obs_ip = if i % 10 < 3 {
                ipv4(10, 0, 0, 2)
            } else {
                ipv4(10, 0, 0, 1)
            };
            t.observe(&[hop(3, Some(obs_ip), Some(5_000))], now);
            now += Duration::from_secs(1);
        }
        let third = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(
            t.diff_against(&third, &default_diff()),
            None,
            "second snapshot with same distribution must not diff",
        );
    }

    #[test]
    fn dod3_new_ip_at_5pct_does_not_diff() {
        // 20-second window — mirrors dod2 so that by the time we evaluate
        // the post-change snapshot, only the 5% post-change window is
        // represented in the rolling accumulator.
        let mut t = tracker_ready(Duration::from_secs(20), Protocol::Icmp);
        let mut now = Instant::now();

        // Baseline: 20 rounds at .1.
        for _ in 0..20 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
            now += Duration::from_secs(1);
        }
        let baseline = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        t.set_last_reported(baseline);

        // Next 20 rounds: 5% .2, 95% .1 — i.e. 1 sample of .2 and 19 of .1,
        // so freq(.2) ≈ 5% — below 20% threshold.
        for i in 0..20 {
            let obs_ip = if i == 0 {
                ipv4(10, 0, 0, 2)
            } else {
                ipv4(10, 0, 0, 1)
            };
            t.observe(&[hop(3, Some(obs_ip), Some(5_000))], now);
            now += Duration::from_secs(1);
        }
        let after = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(
            t.diff_against(&after, &default_diff()),
            None,
            "5% new IP must not trigger diff",
        );
    }

    #[test]
    fn dod4_hop_count_increase_by_one_fires() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let mut now = Instant::now();

        // Baseline: single-hop route.
        for _ in 0..10 {
            t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
            now += Duration::from_secs(1);
        }
        let baseline = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        t.set_last_reported(baseline);

        // New window: two-hop route.
        for _ in 0..10 {
            t.observe(
                &[
                    hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000)),
                    hop(2, Some(ipv4(10, 0, 0, 2)), Some(2_000)),
                ],
                now,
            );
            now += Duration::from_secs(1);
        }
        let after = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        let diff = t.diff_against(&after, &default_diff()).expect("diff");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::HopCountChanged { from: 1, to: 2 })));
    }

    #[test]
    fn dod5_ecmp_flicker_steady_absorbs_across_multiple_snapshots() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let mut now = Instant::now();

        // Helper: produce a steady 80/20 distribution at position 3 over
        // 20 observations (16 of .1, 4 of .2).
        let inject_80_20 = |t: &mut RouteTracker, now: &mut Instant| {
            for i in 0..20 {
                let ip = if i < 16 {
                    ipv4(10, 0, 0, 1)
                } else {
                    ipv4(10, 0, 0, 2)
                };
                t.observe(&[hop(3, Some(ip), Some(5_000))], *now);
                *now += Duration::from_secs(1);
            }
        };

        // First window: establish baseline.
        inject_80_20(&mut t, &mut now);
        let snap1 = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(snap1.hops.len(), 1);
        let h = &snap1.hops[0];
        assert_eq!(h.observed_ips.len(), 2);
        assert!(h.observed_ips[0].frequency > 0.7);
        assert!(h.observed_ips[1].frequency > 0.19);
        t.set_last_reported(snap1);

        // Subsequent THREE snapshots: same 80/20 distribution → all must
        // produce None.
        for n in 0..3 {
            inject_80_20(&mut t, &mut now);
            let snap = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
            let diff = t.diff_against(&snap, &default_diff());
            assert_eq!(
                diff,
                None,
                "ECMP 80/20 must not diff across tick #{}: got {:?}",
                n + 2,
                diff,
            );
        }
    }

    #[test]
    fn primary_swing_resets_accumulator_and_last_reported() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
        let snap = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        t.set_last_reported(snap);
        assert!(t.last_reported().is_some());
        assert!(!t.hops.is_empty());

        // Swing to TCP.
        t.reset_for_protocol(Some(Protocol::Tcp));
        assert!(t.last_reported().is_none());
        assert!(t.hops.is_empty());
        assert!(t.position_probes.is_empty());
        assert_eq!(t.protocol(), Some(Protocol::Tcp));

        // Next observation under TCP, snapshot emits as new baseline.
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 99)), Some(1_000))], now);
        let first_after_swing = t.build_snapshot(now, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(first_after_swing.protocol, Protocol::Tcp);
        assert_eq!(first_after_swing.hops.len(), 1);
        assert_eq!(
            first_after_swing.hops[0].observed_ips[0].ip,
            ipv4(10, 0, 0, 99)
        );

        // diff_against without a last_reported is always None — supervisor
        // branch takes the "is_none()" path and emits unconditionally.
        assert_eq!(t.diff_against(&first_after_swing, &default_diff()), None);
    }

    #[test]
    fn observe_silent_hop_populates_position_probes_only() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(5, None, None)], now);
        assert!(
            t.hops.is_empty(),
            "silent hop has no IP, must not land in per-(pos,ip) map",
        );
        let acc = t
            .position_probes
            .get(&5)
            .expect("silent hop in position_probes");
        assert_eq!(acc.seen, 1);
        assert_eq!(acc.successful, 0);
    }

    #[test]
    fn observe_ip_bearing_hop_populates_both_maps() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        let pacc = t.position_probes.get(&1).expect("in position_probes");
        let iacc = t.hops.get(&(1, ipv4(10, 0, 0, 1))).expect("in hops");
        assert_eq!(pacc.seen, 1);
        assert_eq!(pacc.successful, 1);
        assert_eq!(iacc.seen, 1);
        assert_eq!(iacc.successful, 1);
    }

    #[test]
    fn purge_keeps_maps_in_sync() {
        let base = Instant::now();
        let mut t = tracker_ready(Duration::from_secs(60), Protocol::Icmp);
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], base);
        t.observe(&[hop(2, None, None)], base);
        t.observe(
            &[hop(3, Some(ipv4(10, 0, 0, 3)), Some(3_000))],
            base + Duration::from_secs(120),
        );
        assert!(
            !t.hops.contains_key(&(1, ipv4(10, 0, 0, 1))),
            "pos 1 IP expired"
        );
        assert!(!t.position_probes.contains_key(&1), "pos 1 probes expired");
        assert!(
            !t.position_probes.contains_key(&2),
            "pos 2 silent probe expired"
        );
        assert_eq!(t.position_probes.get(&3).unwrap().seen, 1);
    }

    #[test]
    fn reset_for_protocol_clears_position_probes() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(
            &[
                hop(1, None, None),
                hop(2, Some(ipv4(10, 0, 0, 2)), Some(1_000)),
            ],
            now,
        );
        assert!(!t.position_probes.is_empty());
        t.reset_for_protocol(Some(Protocol::Tcp));
        assert!(
            t.position_probes.is_empty(),
            "swing must clear position_probes"
        );
        assert!(t.hops.is_empty());
    }

    // ---------------------------------------------------------------------------
    // silent-hop emission and hop-level loss from position_probes
    // ---------------------------------------------------------------------------

    #[test]
    fn build_snapshot_emits_silent_hop_with_empty_ips_and_loss_one() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(5, None, None), hop(5, None, None)], now);
        let snap = t
            .build_snapshot(now, systemtime_epoch_plus(0))
            .expect("snap");
        assert_eq!(snap.hops.len(), 1);
        let h = &snap.hops[0];
        assert_eq!(h.position, 5);
        assert!(h.observed_ips.is_empty(), "silent hop has no IPs");
        assert!((h.loss_pct - 1.0).abs() < 1e-9, "silent hop is 100% loss");
        assert_eq!(h.avg_rtt_micros, 0);
    }

    #[test]
    fn build_snapshot_partial_silence_loss_matches_silent_fraction() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        for _ in 0..7 {
            t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), Some(1_000))], now);
        }
        for _ in 0..3 {
            t.observe(&[hop(2, None, None)], now);
        }
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let h = &snap.hops[0];
        assert!((h.loss_pct - 0.30).abs() < 1e-9, "loss={}", h.loss_pct);
        assert_eq!(h.observed_ips.len(), 1);
        let f = h.observed_ips[0].frequency;
        assert!((f - 0.70).abs() < 1e-9, "freq={f}");
    }

    #[test]
    fn build_snapshot_ecmp_with_silence_frequencies() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        for _ in 0..4 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
        }
        for _ in 0..4 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 2)), Some(1_500))], now);
        }
        for _ in 0..2 {
            t.observe(&[hop(3, None, None)], now);
        }
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let h = &snap.hops[0];
        assert!((h.loss_pct - 0.20).abs() < 1e-9);
        assert_eq!(h.observed_ips.len(), 2);
        let total: f64 = h.observed_ips.iter().map(|o| o.frequency).sum();
        assert!(
            (total - 0.80).abs() < 1e-9,
            "freqs sum to {total}, want 0.80 (= 1 - loss)"
        );
    }

    #[test]
    fn build_snapshot_avg_rtt_ignores_silent_samples() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(2_000))], now);
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(4_000))], now);
        t.observe(&[hop(1, None, None)], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(snap.hops[0].avg_rtt_micros, 3_000);
    }

    #[test]
    fn build_snapshot_sort_order_with_silent_middle_hop() {
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000))], now);
        t.observe(&[hop(5, None, None)], now);
        t.observe(&[hop(3, Some(ipv4(10, 0, 0, 3)), Some(3_000))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(positions, vec![1, 3, 5]);
        assert!(snap.hops[2].observed_ips.is_empty(), "position 5 silent");
    }

    #[test]
    fn purge_removes_empty_ip_entry_but_keeps_position_probes_with_silent_samples() {
        let base = Instant::now();
        let mut t = tracker_ready(Duration::from_secs(60), Protocol::Icmp);
        // IP-bearing at t=0, silent at t=30s (both at position 4).
        t.observe(&[hop(4, Some(ipv4(10, 0, 0, 4)), Some(2_000))], base);
        t.observe(&[hop(4, None, None)], base + Duration::from_secs(30));
        // Advance past 60s window — only the IP-bearing sample expires.
        t.observe(&[hop(4, None, None)], base + Duration::from_secs(70));
        assert!(
            !t.hops.contains_key(&(4, ipv4(10, 0, 0, 4))),
            "IP-bearing sample at pos 4 should have purged",
        );
        // position_probes still holds the two silent samples.
        let pacc = t
            .position_probes
            .get(&4)
            .expect("silent samples keep pos 4 alive");
        assert_eq!(pacc.seen, 2, "two silent samples within window");
        assert_eq!(pacc.successful, 0, "neither sample produced an RTT");
    }

    #[test]
    fn consistency_debug_assert_passes_after_mixed_observe_sequence() {
        use rand::rngs::SmallRng;
        use rand::{Rng, SeedableRng};
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        let mut rng = SmallRng::seed_from_u64(0xD00D_BEEF);
        let base = Instant::now();
        for i in 0..100 {
            let pos: u8 = rng.random_range(1..=30);
            let is_silent = rng.random_bool(0.3);
            let hop_obs = if is_silent {
                hop(pos, None, None)
            } else {
                hop(
                    pos,
                    Some(ipv4(10, 0, 0, rng.random_range(1..=5))),
                    Some(rng.random_range(500..=5_000)),
                )
            };
            t.observe(&[hop_obs], base + Duration::from_millis(i * 10));
        }
        // If assert_consistency had fired, we'd have panicked inside observe.
        // Final direct call for belt-and-suspenders.
        #[cfg(debug_assertions)]
        t.assert_consistency();
    }

    #[test]
    fn build_snapshot_truncates_after_first_target_ip_hit() {
        let target: IpAddr = ipv4(208, 83, 237, 164);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(100))], now);
        t.observe(&[hop(13, Some(target), Some(260_000))], now);
        t.observe(&[hop(14, Some(target), Some(260_000))], now);
        t.observe(&[hop(15, Some(target), Some(260_000))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(positions, vec![1, 13], "positions past destination dropped");
    }

    #[test]
    fn build_snapshot_keeps_silent_hops_before_destination() {
        let target: IpAddr = ipv4(45, 248, 78, 119);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        t.observe(
            &[
                hop(1, Some(ipv4(10, 0, 0, 1)), Some(100)),
                hop(5, None, None),
                hop(13, Some(target), Some(260_000)),
            ],
            now,
        );
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(positions, vec![1, 5, 13]);
        assert!(
            snap.hops[1].observed_ips.is_empty(),
            "pos 5 silent retained"
        );
        assert_eq!(snap.hops[2].observed_ips[0].ip, target);
    }

    #[test]
    fn build_snapshot_no_truncation_when_target_ip_absent() {
        let target: IpAddr = ipv4(1, 1, 1, 1);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(100))], now);
        t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), Some(200))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(snap.hops.len(), 2, "no target IP → no truncation");
    }

    #[test]
    fn build_snapshot_truncates_at_first_target_ip_even_at_very_low_freq() {
        let target: IpAddr = ipv4(45, 248, 78, 119);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        for _ in 0..99 {
            t.observe(&[hop(11, Some(ipv4(10, 0, 0, 99)), Some(1_000))], now);
        }
        t.observe(&[hop(11, Some(target), Some(1_000))], now);
        t.observe(&[hop(12, Some(ipv4(10, 0, 0, 12)), Some(2_000))], now);
        t.observe(&[hop(13, Some(ipv4(10, 0, 0, 13)), Some(3_000))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(
            positions,
            vec![11],
            "truncate at first dest hit, any freq > 0"
        );
    }

    // ---------------------------------------------------------------------------
    // diff rules under per-probe denominator with silent hops
    // ---------------------------------------------------------------------------

    #[test]
    fn diff_rule1_fires_when_new_ip_crosses_20pct_of_all_probes() {
        let last = snap(
            Protocol::Icmp,
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        let mut t = tracker_with_last(Protocol::Icmp, last);
        // Assign a target IP that is not present in any observed hop to prevent
        // truncation from interfering with the assertion.
        t.target_ip = ipv4(45, 248, 78, 119);

        let now = Instant::now();
        for _ in 0..8 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        }
        for _ in 0..2 {
            t.observe(&[hop(3, Some(ipv4(10, 0, 0, 2)), Some(5_000))], now);
        }
        let new = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(
            diff.reasons
                .iter()
                .any(|r| matches!(r, DiffReason::NewIp { position: 3, .. })),
            "expected DiffReason::NewIp at pos 3: {:?}",
            diff.reasons,
        );
    }

    #[test]
    fn diff_rule3_stable_across_silent_padded_snapshots() {
        let target: IpAddr = ipv4(45, 248, 78, 119);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let mut now = Instant::now();

        for _ in 0..30 {
            t.observe(
                &[
                    hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000)),
                    hop(2, None, None),
                    hop(3, Some(target), Some(3_000)),
                ],
                now,
            );
            now += Duration::from_secs(1);
        }
        let first = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(first.hops.len(), 3, "silent pad keeps pos 2");
        t.set_last_reported(first);

        for _ in 0..60 {
            t.observe(
                &[
                    hop(1, Some(ipv4(10, 0, 0, 1)), Some(1_000)),
                    hop(2, None, None),
                    hop(3, Some(target), Some(3_000)),
                ],
                now,
            );
            now += Duration::from_secs(1);
        }
        let second = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(t.diff_against(&second, &default_diff()), None);
    }

    // ---- Definition-of-Done scenario tests ----

    #[test]
    fn dod_over_probing_past_destination_no_longer_diffs() {
        let target: IpAddr = ipv4(208, 83, 237, 164);
        let mut t = RouteTracker::new(Duration::from_secs(300), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let mut now = Instant::now();

        // Baseline: 5 rounds, each with dest at pos 13 and over-probes at 14, 15.
        for _ in 0..5 {
            t.observe(
                &[
                    hop(1, Some(ipv4(10, 0, 0, 1)), Some(100)),
                    hop(13, Some(target), Some(260_000)),
                    hop(14, Some(target), Some(260_000)),
                    hop(15, Some(target), Some(260_000)),
                ],
                now,
            );
            now += Duration::from_secs(60);
        }
        let first = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(
            first.hops.len(),
            2,
            "truncated to pos 13; pos 14/15 dropped"
        );
        t.set_last_reported(first);

        // Next window: same pattern, but over-probe reaches pos 16 only
        // (destination rate-limited at 14/15 this round).
        for _ in 0..5 {
            t.observe(
                &[
                    hop(1, Some(ipv4(10, 0, 0, 1)), Some(100)),
                    hop(13, Some(target), Some(260_000)),
                    hop(16, Some(target), Some(260_000)),
                ],
                now,
            );
            now += Duration::from_secs(60);
        }
        let second = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(
            second.hops.len(),
            2,
            "over-probe variance must not change reported hop count",
        );
        assert_eq!(
            t.diff_against(&second, &default_diff()),
            None,
            "truncated snapshots must be identical → no diff",
        );
    }

    #[test]
    fn dod_silent_middle_hop_stable_across_snapshots() {
        let target: IpAddr = ipv4(45, 248, 78, 119);
        let mut t = RouteTracker::new(Duration::from_secs(300), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let mut now = Instant::now();

        fn emit(t: &mut RouteTracker, now: &mut Instant, target: IpAddr) {
            for _ in 0..20 {
                t.observe(
                    &[
                        hop(1, Some(ipv4(10, 0, 0, 1)), Some(100)),
                        hop(2, Some(ipv4(10, 0, 0, 2)), Some(200)),
                        hop(3, Some(ipv4(10, 0, 0, 3)), Some(300)),
                        hop(4, Some(ipv4(10, 0, 0, 4)), Some(400)),
                        hop(5, None, None),
                        hop(6, Some(ipv4(10, 0, 0, 6)), Some(600)),
                        hop(13, Some(target), Some(260_000)),
                    ],
                    *now,
                );
                *now += Duration::from_secs(1);
            }
        }

        emit(&mut t, &mut now, target);
        let first = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(
            first.hops.iter().map(|h| h.position).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 13],
            "silent pos 5 retained, no over-probe positions present",
        );
        t.set_last_reported(first);

        for n in 0..3 {
            emit(&mut t, &mut now, target);
            let next = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
            assert_eq!(
                t.diff_against(&next, &default_diff()),
                None,
                "stable silent-middle path must not diff on tick {}",
                n + 2,
            );
        }
    }

    #[test]
    fn build_snapshot_truncates_when_target_is_ipv4_mapped_ipv6() {
        // Constructor canonicalizes, so passing ::ffff:a.b.c.d is equivalent
        // to passing a.b.c.d. observe() also canonicalizes, so hops carrying
        // either form converge to the same stored key.
        let target: IpAddr = "::ffff:10.0.0.99".parse().unwrap();
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        // Hop reports the destination as IPv4-mapped-IPv6 too; must still truncate.
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(100))], now);
        t.observe(
            &[hop(3, Some("::ffff:10.0.0.99".parse().unwrap()), Some(250))],
            now,
        );
        t.observe(&[hop(4, Some(ipv4(10, 0, 0, 4)), Some(350))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        let positions: Vec<u8> = snap.hops.iter().map(|h| h.position).collect();
        assert_eq!(
            positions,
            vec![1, 3],
            "truncation finds target despite mapped-v6 form"
        );
    }

    #[test]
    fn build_snapshot_target_ip_at_pos_1_truncates_immediately() {
        let target: IpAddr = ipv4(10, 0, 0, 1);
        let mut t = RouteTracker::new(five_min(), target);
        t.reset_for_protocol(Some(Protocol::Icmp));
        let now = Instant::now();
        // Target responds at pos 1; positions 2 and 3 also see responses
        // but those are dropped because the destination was already reached.
        t.observe(&[hop(1, Some(target), Some(50))], now);
        t.observe(&[hop(2, Some(ipv4(10, 0, 0, 2)), Some(150))], now);
        t.observe(&[hop(3, Some(ipv4(10, 0, 0, 3)), Some(250))], now);
        let snap = t.build_snapshot(now, systemtime_epoch_plus(0)).unwrap();
        assert_eq!(
            snap.hops.len(),
            1,
            "target at pos 1 truncates to single hop"
        );
        assert_eq!(snap.hops[0].position, 1);
        assert_eq!(snap.hops[0].observed_ips[0].ip, target);
    }

    // ---------------------------------------------------------------------------
    // Structural-only diff contract
    //
    // The diff engine only fires on topology changes (new IP at a position, or
    // hop count change). Per-hop loss and per-hop RTT are measurement signals,
    // not route signals — they belong in rolling stats and alerts, not in the
    // route-snapshot stream.
    // ---------------------------------------------------------------------------

    #[test]
    fn diff_ignores_pure_loss_and_rtt_noise_at_near_silent_hop() {
        // Reproduces the production pattern: the same 15-IP path produces
        // back-to-back snapshots where every position keeps the same
        // dominant IP, but a first-hop router rate-limits ICMP replies to
        // <1 % of probes. With so few successful samples its avg_rtt swings
        // by 100s of % between snapshots. No IP changed, no hop count
        // changed — must not diff.
        let last = RouteSnapshot {
            protocol: Protocol::Icmp,
            observed_at: SystemTime::UNIX_EPOCH,
            hops: vec![
                HopSummary {
                    position: 1,
                    observed_ips: vec![ObservedIp {
                        ip: ipv4(188, 208, 143, 254),
                        frequency: 0.0034,
                    }],
                    avg_rtt_micros: 331,
                    stddev_rtt_micros: 0,
                    loss_pct: 0.9966,
                },
                HopSummary {
                    position: 2,
                    observed_ips: vec![ObservedIp {
                        ip: ipv4(193, 109, 190, 156),
                        frequency: 1.0,
                    }],
                    avg_rtt_micros: 1664,
                    stddev_rtt_micros: 2094,
                    loss_pct: 0.0,
                },
            ],
        };
        let new = RouteSnapshot {
            protocol: Protocol::Icmp,
            observed_at: SystemTime::UNIX_EPOCH,
            hops: vec![
                HopSummary {
                    position: 1,
                    // Same IP, same (near-zero) frequency, but a 256 % RTT
                    // jump driven by a single extra sample landing in the
                    // rolling window.
                    observed_ips: vec![ObservedIp {
                        ip: ipv4(188, 208, 143, 254),
                        frequency: 0.0068,
                    }],
                    avg_rtt_micros: 1181,
                    stddev_rtt_micros: 850,
                    loss_pct: 0.9932,
                },
                HopSummary {
                    position: 2,
                    // Slight IP-level packet loss increase at an otherwise
                    // stable hop — must also not fire.
                    observed_ips: vec![ObservedIp {
                        ip: ipv4(193, 109, 190, 156),
                        frequency: 0.85,
                    }],
                    avg_rtt_micros: 1664,
                    stddev_rtt_micros: 2094,
                    loss_pct: 0.15,
                },
            ],
        };
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }
}
