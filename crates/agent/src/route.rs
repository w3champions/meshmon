//! Per-target route state tracker.
//!
//! See spec 02 § "Route state tracker". Pure, clock-injected logic: no
//! tokio, no async, no mpsc. Owned by the per-target supervisor, which
//! feeds per-hop observations via [`RouteTracker::observe`] on every
//! trippy probe and polls [`RouteTracker::build_snapshot`] +
//! [`RouteTracker::diff_against`] on its 60 s snapshot tick.
//!
//! The tracker records the [`Protocol`] it's currently accumulating for.
//! On a primary swing the supervisor calls
//! [`RouteTracker::reset_for_protocol`], which drops the accumulator and
//! the cached `last_reported` snapshot; the first non-empty snapshot
//! after the reset is emitted as the new baseline (same path as the
//! first-after-startup emission).
//!
//! ## Complexity targets
//! - `observe(hops, now)`: O(H) per call where H = hops.len() (typically ≤ 30).
//! - `build_snapshot(now, now_wall)`: O(H · K + K · log K) where K = number of
//!   distinct IPs per position (typically 1–4). The sort is per-position.
//! - `diff_against(last)`: O(H · K) — two hashmap walks over current vs. last.
//!
//! ## What lands here vs. what doesn't
//! - T15 owns all accumulator logic, snapshot construction, diff detection.
//! - The supervisor (see [`supervisor`](crate::supervisor)) owns the mpsc
//!   channel, the 60 s timer, the primary-swing reset call, and the
//!   protocol-filter rule that skips hops from non-tracked protocols.
//! - The emitter (T16) owns the `Receiver` side of the snapshot channel
//!   and the wall-clock → i64 conversion at wire-encoding time.

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
    /// `0` when no successful observation (all timeouts).
    pub avg_rtt_micros: u32,
    /// Population stddev of RTT across successful observations. `0` when
    /// `successful < 2`.
    pub stddev_rtt_micros: u32,
    /// Loss fraction at this hop over the current window (`0.0 .. 1.0`).
    pub loss_pct: f64,
}

/// Canonical per-target route snapshot. Emitted by the tracker when a
/// meaningful diff is detected; consumed by the T16 emitter.
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
    /// Helper for T16: convert the snapshot timestamp to the `i64 micros`
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
    /// Rule 1: a new IP crossed `new_ip_min_freq` at some hop.
    NewIp { position: u8 },
    /// Rule 2: a previously-≥`new_ip_min_freq` IP dropped below `missing_ip_max_freq`.
    MissingIp { position: u8 },
    /// Rule 3: hop count changed by ≥ `hop_count_change`.
    HopCountChanged { from: usize, to: usize },
    /// Rule 4: some hop's avg RTT shifted by ≥ `rtt_shift_frac`.
    RttShift { position: u8 },
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
#[allow(dead_code)]
struct HopObservationsAcc {
    /// `(t, rtt)` samples. `rtt = None` means the hop timed out this round.
    samples: VecDeque<(Instant, Option<u32>)>,
    seen: u32,
    successful: u32,
    sum_rtt_micros: u64,
    sum_rtt_sq_micros: u128,
}

#[allow(dead_code)]
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

    fn avg_rtt_micros(&self) -> u32 {
        if self.successful == 0 {
            return 0;
        }
        (self.sum_rtt_micros / self.successful as u64).min(u32::MAX as u64) as u32
    }

    fn stddev_rtt_micros(&self) -> u32 {
        if self.successful < 2 {
            return 0;
        }
        let n = self.successful as f64;
        let mean = self.sum_rtt_micros as f64 / n;
        let mean_sq = self.sum_rtt_sq_micros as f64 / n;
        let var = (mean_sq - mean * mean).max(0.0);
        let stddev = var.sqrt();
        if !stddev.is_finite() || stddev < 0.0 {
            0
        } else if stddev >= u32::MAX as f64 {
            u32::MAX
        } else {
            stddev as u32
        }
    }

    fn loss_pct(&self) -> f64 {
        if self.seen == 0 {
            return 0.0;
        }
        let lost = (self.seen - self.successful) as f64;
        lost / self.seen as f64
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
    /// Maintained by `observe`; consumed by `build_snapshot` (Task 4).
    hops: HashMap<(u8, IpAddr), HopObservationsAcc>,
    /// Sum of `HopObservationsAcc.seen` across all IPs at each position.
    /// Maintained O(1) on insert + O(1) on purge so hop-frequency
    /// computation in `build_snapshot` is O(1) per `(position, ip)`.
    position_totals: HashMap<u8, u32>,
    /// Most-recently emitted snapshot, for the next `diff_against` call.
    last_reported: Option<RouteSnapshot>,
}

impl RouteTracker {
    /// Build a new tracker. `window` should be the primary-protocol window
    /// (`primary_window_sec`, default 300 s). Starts with `protocol = None`
    /// so the tracker silently drops observations until the supervisor
    /// calls [`reset_for_protocol`] once T14 elects a primary.
    pub fn new(window: Duration) -> Self {
        Self {
            protocol: None,
            window,
            hops: HashMap::new(),
            position_totals: HashMap::new(),
            last_reported: None,
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
        self.position_totals.clear();
        self.last_reported = None;
    }

    /// Ingest the hop observations from one trippy round. Purges samples
    /// older than `now - window` before inserting the new ones.
    pub fn observe(&mut self, hops: &[HopObservation], now: Instant) {
        if self.protocol.is_none() {
            return;
        }

        // Purge samples older than the window. We walk the entire
        // accumulator because hop entries age independently (one IP at
        // position 3 might purge while another IP at position 3 stays
        // fresh). The walk is O(K) in total entries (typically ≤ ~30
        // hops × a few IPs each = < 100), cheap.
        //
        // Boundary semantics match `stats::RollingStats::purge_old`: a
        // sample with `t == now - window` is exactly window-old and is
        // purged.
        let Some(cutoff) = now.checked_sub(self.window) else {
            // Runtime clock is before the window — nothing is old enough yet.
            // Still insert new observations below.
            for obs in hops {
                let Some(ip) = obs.ip else { continue };
                let key = (obs.position, ip);
                let acc = self.hops.entry(key).or_default();
                acc.insert(now, obs.rtt_micros);
                *self.position_totals.entry(obs.position).or_insert(0) += 1;
            }
            return;
        };

        // Purge step — walk all entries, prune old samples, decrement
        // per-position totals, drop entries that became empty.
        let mut empty_keys: Vec<(u8, IpAddr)> = Vec::new();
        for (key, acc) in self.hops.iter_mut() {
            let purged = acc.purge(cutoff);
            if purged > 0 {
                if let Some(total) = self.position_totals.get_mut(&key.0) {
                    *total = total.saturating_sub(purged);
                }
            }
            if acc.is_empty() {
                empty_keys.push(*key);
            }
        }
        for key in &empty_keys {
            self.hops.remove(key);
            // If this was the last IP at that position, drop the total too.
            if let Some(total) = self.position_totals.get(&key.0) {
                if *total == 0 {
                    self.position_totals.remove(&key.0);
                }
            }
        }

        // Insert step — record each hop with an IP. Star hops (ip = None)
        // are ignored because they can't be attributed to a (position, ip)
        // key. This matches the spec: the histogram tracks OBSERVED IPs,
        // and star hops are the absence of observation.
        for obs in hops {
            let Some(ip) = obs.ip else { continue };
            let key = (obs.position, ip);
            let acc = self.hops.entry(key).or_default();
            acc.insert(now, obs.rtt_micros);
            *self.position_totals.entry(obs.position).or_insert(0) += 1;
        }
    }

    /// Build a canonical snapshot from the current window. Returns `None`
    /// when the accumulator is empty (nothing to summarize).
    pub fn build_snapshot(
        &mut self,
        _now: Instant,
        _now_wall: SystemTime,
    ) -> Option<RouteSnapshot> {
        unimplemented!("Task 4")
    }

    /// Compare `new` to the tracker's `last_reported` snapshot. Returns
    /// `Some(RouteDiff)` when at least one of the four rules from spec 02
    /// § Diff detection fires. Free function form to keep the tracker's
    /// mutable state untouched by the diff check.
    ///
    /// Caller (supervisor) is responsible for updating `last_reported`
    /// after a successful emit via [`set_last_reported`].
    pub fn diff_against(
        &self,
        _new: &RouteSnapshot,
        _thresholds: &DiffDetection,
    ) -> Option<RouteDiff> {
        unimplemented!("Task 5")
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

// ---------------------------------------------------------------------------
// Tests — populated incrementally from Task 2 onward.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn five_min() -> Duration {
        Duration::from_secs(300)
    }

    #[allow(dead_code)]
    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn new_tracker_has_no_protocol_no_last_reported() {
        let t = RouteTracker::new(five_min());
        assert_eq!(t.protocol(), None);
        assert_eq!(t.window(), five_min());
        assert!(t.last_reported().is_none());
    }

    #[test]
    fn reset_for_protocol_sets_protocol() {
        let mut t = RouteTracker::new(five_min());
        t.reset_for_protocol(Some(Protocol::Icmp));
        assert_eq!(t.protocol(), Some(Protocol::Icmp));
    }

    #[test]
    fn reset_for_protocol_to_none_clears_protocol() {
        let mut t = RouteTracker::new(five_min());
        t.reset_for_protocol(Some(Protocol::Icmp));
        t.reset_for_protocol(None);
        assert_eq!(t.protocol(), None);
    }

    #[test]
    fn reset_clears_last_reported() {
        let mut t = RouteTracker::new(five_min());
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
        let mut t = RouteTracker::new(window);
        t.reset_for_protocol(Some(protocol));
        t
    }

    #[test]
    fn observe_noop_when_no_protocol() {
        let mut t = RouteTracker::new(five_min());
        // No reset_for_protocol call — protocol is None.
        let now = Instant::now();
        t.observe(&[hop(1, Some(ipv4(10, 0, 0, 1)), Some(5_000))], now);
        // Internal accumulator must remain empty.
        assert!(t.hops.is_empty());
        assert!(t.position_totals.is_empty());
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
        assert_eq!(t.position_totals.get(&1).copied(), Some(1));
    }

    #[test]
    fn observe_hop_timeout_counts_sample_but_not_rtt() {
        // A hop with ip = None AND rtt = None = total timeout ("star hop").
        // Star hops have no IP, so they are NOT recorded in the accumulator
        // at all — there's no (position, ip) key to attribute them to.
        let mut t = tracker_ready(five_min(), Protocol::Icmp);
        t.observe(&[hop(3, None, None)], Instant::now());
        assert!(t.hops.is_empty(), "star hops carry no IP and are ignored");
        assert!(t.position_totals.is_empty());
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
        assert_eq!(t.position_totals.get(&2).copied(), Some(1));
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
        assert_eq!(t.position_totals.get(&1).copied(), Some(3));
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
        assert_eq!(t.position_totals.get(&1).copied(), Some(2));
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
        assert_eq!(t.position_totals.get(&3).copied(), Some(3));
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
        assert_eq!(t.position_totals.get(&1).copied(), None);
    }
}
