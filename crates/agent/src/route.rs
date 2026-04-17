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
    /// Consumed by `observe` / `build_snapshot` in Tasks 3–4.
    #[allow(dead_code)]
    hops: HashMap<(u8, IpAddr), HopObservationsAcc>,
    /// Sum of `HopObservationsAcc.seen` across all IPs at each position.
    /// Maintained O(1) on insert + O(1) on purge so hop-frequency
    /// computation in `build_snapshot` is O(1) per `(position, ip)`.
    /// Consumed by `observe` / `build_snapshot` in Tasks 3–4.
    #[allow(dead_code)]
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
    pub fn observe(&mut self, _hops: &[HopObservation], _now: Instant) {
        unimplemented!("Task 3")
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
}
