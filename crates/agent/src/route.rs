//! Per-target route state tracker.
//!
//! See spec 02 § "Route state tracker". Pure, clock-injected logic: no
//! tokio runtime, no async, no mpsc — just a regular struct the
//! per-target supervisor owns, mutates on every trippy probe via
//! [`RouteTracker::observe`], and polls via [`RouteTracker::build_snapshot`] +
//! [`RouteTracker::diff_against`] on its 60 s snapshot tick.
//!
//! The tracker records the [`Protocol`] it's currently accumulating for
//! and a rolling window sized from `ProbeConfig.primary_window_sec` (via
//! [`RouteTracker::set_window`] on config updates). On a primary swing
//! the supervisor calls [`RouteTracker::reset_for_protocol`], which
//! drops the accumulator and the cached `last_reported` snapshot; the
//! first non-empty snapshot after the reset is emitted as the new
//! baseline (same path as the first-after-startup emission).
//!
//! [`RouteTracker::set_last_reported`] is a deliberate mutation API
//! separate from [`RouteTracker::diff_against`]: the diff is a
//! read-only comparison, and the supervisor only advances `last_reported`
//! after a successful emit on the snapshot channel. A send failure
//! leaves the cached baseline untouched so the next tick retries the
//! diff unchanged.
//!
//! ## Complexity targets
//! - `observe(hops, now)`: O(H) per call where H = hops.len() (typically ≤ 30).
//! - `build_snapshot(now, now_wall)`: O(H · K + K · log K) where K = number of
//!   distinct IPs per position (typically 1–4). The sort is per-position.
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
    /// calls [`RouteTracker::reset_for_protocol`] once a primary is elected.
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

        // `now.checked_sub` returns `None` immediately after runtime start
        // when the window would reach before the tokio runtime epoch. In
        // that case nothing is old enough to purge yet; fall through to
        // the insert step unchanged.
        if let Some(cutoff) = now.checked_sub(self.window) {
            self.purge_stale(cutoff);
        }

        // Star hops (ip = None) are ignored because they can't be
        // attributed to a (position, ip) key. The histogram tracks OBSERVED
        // IPs; star hops are the absence of observation.
        for obs in hops {
            let Some(ip) = obs.ip else { continue };
            let key = (obs.position, ip);
            let acc = self.hops.entry(key).or_default();
            acc.insert(now, obs.rtt_micros);
            *self.position_totals.entry(obs.position).or_insert(0) += 1;
        }
    }

    /// Walk the accumulator and drop samples older than `cutoff`. Keeps
    /// `position_totals` in sync and removes `(position, ip)` entries that
    /// became empty so ghost zero-count rows can't surface in
    /// `build_snapshot`. Boundary semantics match
    /// `stats::RollingStats::purge_old`: a sample with `t == cutoff` is
    /// expired.
    fn purge_stale(&mut self, cutoff: Instant) {
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

        if self.hops.is_empty() {
            return None;
        }

        // Group accumulator entries by position. Iterate position_totals
        // as the canonical set of active positions.
        let mut positions: Vec<u8> = self.position_totals.keys().copied().collect();
        positions.sort();

        let mut hops_out: Vec<HopSummary> = Vec::with_capacity(positions.len());
        for position in positions {
            let total = *self.position_totals.get(&position).unwrap_or(&0);
            if total == 0 {
                // Defensive: should have been pruned. Skip instead of
                // dividing by zero.
                continue;
            }
            let total_f = total as f64;
            let mut ips: Vec<ObservedIp> = Vec::new();
            let mut sum_weighted_rtt: u64 = 0;
            let mut sum_weighted_rtt_sq: u128 = 0;
            let mut sum_seen: u32 = 0;
            let mut sum_successful: u32 = 0;

            for (key, acc) in self.hops.iter() {
                if key.0 != position {
                    continue;
                }
                let frequency = acc.seen as f64 / total_f;
                ips.push(ObservedIp {
                    ip: key.1,
                    frequency,
                });
                sum_weighted_rtt += acc.sum_rtt_micros;
                sum_weighted_rtt_sq += acc.sum_rtt_sq_micros;
                sum_seen += acc.seen;
                sum_successful += acc.successful;
            }

            // Sort IPs by frequency descending; tiebreak on IP for deterministic output.
            ips.sort_by(|a, b| {
                b.frequency
                    .partial_cmp(&a.frequency)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.ip.cmp(&b.ip))
            });

            let avg_rtt_micros = if sum_successful == 0 {
                0
            } else {
                (sum_weighted_rtt / sum_successful as u64).min(u32::MAX as u64) as u32
            };
            let stddev_rtt_micros = if sum_successful < 2 {
                0
            } else {
                let n = sum_successful as f64;
                let mean = sum_weighted_rtt as f64 / n;
                let mean_sq = sum_weighted_rtt_sq as f64 / n;
                let var = (mean_sq - mean * mean).max(0.0);
                let stddev = var.sqrt();
                if !stddev.is_finite() || stddev < 0.0 {
                    0
                } else if stddev >= u32::MAX as f64 {
                    u32::MAX
                } else {
                    stddev as u32
                }
            };
            let loss_pct = if sum_seen == 0 {
                0.0
            } else {
                (sum_seen - sum_successful) as f64 / sum_seen as f64
            };

            hops_out.push(HopSummary {
                position,
                observed_ips: ips,
                avg_rtt_micros,
                stddev_rtt_micros,
                loss_pct,
            });
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
    /// `Some(RouteDiff)` when at least one of the four rules from spec 02
    /// § Diff detection fires. Free function form to keep the tracker's
    /// mutable state untouched by the diff check.
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

        // Rule 3 — hop count change.
        let from = last.hops.len();
        let to = new.hops.len();
        let delta = from.abs_diff(to);
        if delta >= thresholds.hop_count_change as usize {
            reasons.push(DiffReason::HopCountChanged { from, to });
        }

        // Build position → HopSummary lookups for O(H) rules 1/2/4.
        let last_by_pos: HashMap<u8, &HopSummary> =
            last.hops.iter().map(|h| (h.position, h)).collect();
        let new_by_pos: HashMap<u8, &HopSummary> =
            new.hops.iter().map(|h| (h.position, h)).collect();

        // Rule 1 — new IP ≥ threshold at any position.
        for new_hop in &new.hops {
            let last_hop = last_by_pos.get(&new_hop.position);
            for ip_entry in &new_hop.observed_ips {
                if ip_entry.frequency < thresholds.new_ip_min_freq {
                    continue;
                }
                // Was this IP present at this position in `last`?
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

        // Rule 2 — previously-seen (≥ new_ip_min_freq) IP is now below missing_ip_max_freq.
        for last_hop in &last.hops {
            let new_hop = new_by_pos.get(&last_hop.position);
            for ip_entry in &last_hop.observed_ips {
                if ip_entry.frequency < thresholds.new_ip_min_freq {
                    continue;
                }
                // Look up same IP in new snapshot; treat absent as frequency 0.
                let new_freq = new_hop
                    .and_then(|h| {
                        h.observed_ips
                            .iter()
                            .find(|o| o.ip == ip_entry.ip)
                            .map(|o| o.frequency)
                    })
                    .unwrap_or(0.0);
                if new_freq < thresholds.missing_ip_max_freq {
                    reasons.push(DiffReason::MissingIp {
                        position: last_hop.position,
                    });
                    break; // one reason per position is enough
                }
            }
        }

        // Rule 4 — avg RTT shift ≥ threshold at any hop present in both.
        for new_hop in &new.hops {
            let Some(last_hop) = last_by_pos.get(&new_hop.position) else {
                continue;
            };
            let old = last_hop.avg_rtt_micros;
            let new_rtt = new_hop.avg_rtt_micros;
            if old == 0 {
                // Can't compute a relative shift against zero. Skip —
                // rule 1 or 3 will catch truly meaningful changes.
                continue;
            }
            let shift = (new_rtt as f64 - old as f64).abs() / old as f64;
            if shift >= thresholds.rtt_shift_frac {
                reasons.push(DiffReason::RttShift {
                    position: new_hop.position,
                });
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
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSnapshotEnvelope {
    pub target_id: String,
    pub snapshot: RouteSnapshot,
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
    fn observe_star_hop_is_ignored() {
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
        let mut t = RouteTracker::new(five_min());
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
            missing_ip_max_freq: 0.05,
            hop_count_change: 1,
            rtt_shift_frac: 0.50,
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
        let mut t = RouteTracker::new(five_min());
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

    // ---------------- Rule 1: new IP ≥ threshold ----------------

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

    // ---------------- Rule 2: IP dropped below missing threshold ----------------

    #[test]
    fn diff_rule2_fires_when_previously_seen_ip_vanishes() {
        let last = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.78), (ipv4(10, 0, 0, 2), 0.22)],
                5_000,
            )],
        );
        // IP .2 dropped to 2% (below 5% missing threshold).
        let new = snap(
            Protocol::Icmp,
            &[(
                3,
                vec![(ipv4(10, 0, 0, 1), 0.98), (ipv4(10, 0, 0, 2), 0.02)],
                5_000,
            )],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(
            diff.reasons
                .iter()
                .any(|r| matches!(r, DiffReason::MissingIp { position: 3 })),
            "got {:?}",
            diff.reasons,
        );
    }

    #[test]
    fn diff_rule2_fires_when_ip_fully_disappears() {
        // IP previously at frequency ≥ threshold is absent from the new
        // snapshot. Treated as frequency = 0 → below 5% → MissingIp.
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
            &[(3, vec![(ipv4(10, 0, 0, 1), 1.0)], 5_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::MissingIp { position: 3 })),);
    }

    #[test]
    fn diff_rule2_does_not_fire_when_ip_was_minor() {
        // IP was at 10% before (NOT ≥ 20% threshold), still at 1% now.
        // Rule 2 requires the IP to have been "previously at frequency
        // ≥ new_ip_min_freq" — otherwise a noisy low-freq IP flickering
        // out would spam diffs.
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
        // IP .2 was at 10% — below new_ip_min_freq (20%). Disappearing doesn't fire rule 2.
        // Also doesn't fire rule 1 (no new IP above 20%).
        // Also doesn't fire rule 3 (hop count unchanged).
        // Also doesn't fire rule 4 (RTT unchanged).
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    // ---------------- Rule 3: hop count change ----------------

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
        // With hop_count_change = 2, a 1-hop change must not fire Rule 3.
        // To isolate Rule 3, also disable Rule 1 by setting a freq
        // threshold no real frequency can cross — otherwise a newly-added
        // hop's IP would trip Rule 1 at the new position.
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
        thr.new_ip_min_freq = 2.0; // disable Rule 1 so only Rule 3 can fire
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &thr), None);
    }

    // ---------------- Rule 4: RTT shift ----------------

    #[test]
    fn diff_rule4_fires_when_hop_rtt_shifts_above_threshold() {
        let last = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 10_000)],
        );
        // 60% shift (10_000 → 16_000) exceeds default 50% threshold.
        let new = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 16_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        let diff = t.diff_against(&new, &default_diff()).expect("diff");
        assert!(diff
            .reasons
            .iter()
            .any(|r| matches!(r, DiffReason::RttShift { position: 2 })));
    }

    #[test]
    fn diff_rule4_does_not_fire_below_threshold() {
        let last = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 10_000)],
        );
        // 40% shift.
        let new = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 14_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        assert_eq!(t.diff_against(&new, &default_diff()), None);
    }

    #[test]
    fn diff_rule4_handles_zero_previous_rtt_gracefully() {
        // Previous avg_rtt = 0 (no successful samples last window); new
        // snapshot has a real RTT. We can't compute a relative shift
        // against zero; treat as "no rule 4 fire" — rule 1 or 3 will
        // typically catch meaningful changes in that case. This is a
        // correctness corner: division by zero would otherwise explode.
        let last = snap(Protocol::Icmp, &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 0)]);
        let new = snap(
            Protocol::Icmp,
            &[(2, vec![(ipv4(10, 0, 0, 2), 1.0)], 5_000)],
        );
        let t = tracker_with_last(Protocol::Icmp, last);
        // Hop count unchanged, IP membership unchanged → no diff, no panic.
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
        // Both rule 1 (new IP at position 2 at 100%) AND rule 3 (hop count +1).
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
        assert!(t.position_totals.is_empty());
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
}
