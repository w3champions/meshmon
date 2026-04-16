//! Rolling-window stats per `(target, protocol)`.
//!
//! See spec 02 § "Aggregation: 5-minute sliding window with running
//! counters". Owned by the per-target supervisor: one `RollingStats` per
//! enabled protocol. The supervisor routes incoming
//! [`ProbeObservation`](crate::probing::ProbeObservation)s into the matching
//! stats via [`insert`](RollingStats::insert), runs
//! [`purge_old`](RollingStats::purge_old) on every 10s eval tick, and reads
//! [`summary_fast`](RollingStats::summary_fast) on the same tick to feed
//! the T14 state machine. T16 calls
//! [`summary_with_percentiles`](RollingStats::summary_with_percentiles)
//! once per 60s emit tick.
//!
//! ## Complexity targets (spec 02)
//! - `insert`: O(1)
//! - `purge_old`: amortized O(1) per purged entry
//! - `summary_fast`: O(1) (returns `None` for min/max while their dirty
//!   bit is set so callers tolerate the lazy recompute)
//! - `summary_with_percentiles`: O(N log N) (clone live RTT values, sort,
//!   index p50/p95/p99; also clears any pending min/max dirty bits in the
//!   single scan)
//!
//! ## What lands here vs. what doesn't
//! Protocol semantics (e.g. UDP `Refused` is not a sample) are the
//! supervisor's responsibility, not this module's — see
//! [`supervisor`](crate::supervisor) for the routing rules.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use crate::probing::ProbeObservation;

/// One observation as stored internally — projected from
/// [`ProbeObservation`] to drop fields `RollingStats` does not need
/// (`protocol`, `target_id`, `hops`). Storing only what we use keeps the
/// `VecDeque` cache-friendly and makes the per-stats memory cost
/// independent of protocol-specific payload size.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Monotonic instant the probe was observed (== `ProbeObservation.observed_at`).
    // Written by `insert`; read by `purge_old` in Task 5. Suppress
    // until then so `cargo clippy -D warnings` stays green.
    #[allow(dead_code)]
    t: Instant,
    /// `Some(rtt)` on success, `None` on failure (`Timeout` / `Error`).
    /// `Refused` is filtered upstream and never reaches `insert`.
    rtt_micros: Option<u32>,
}

/// Fast O(1) summary for the supervisor's 10s state-eval tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FastSummary {
    /// Total observations in the current window (after the last purge).
    pub sample_count: u64,
    /// Successful observations in the window.
    pub successful: u64,
    /// `1.0 - successful/sample_count`. Returns `0.0` when `sample_count == 0`
    /// (treated as "no data, neutral state" by the T14 state machine, per
    /// spec 02 § Aggregation: zero-handling).
    pub failure_rate: f64,
    /// Mean RTT across successful samples in microseconds. `None` when
    /// `successful == 0`.
    pub mean_rtt_micros: Option<f64>,
    /// Population standard deviation of RTT across successful samples.
    /// `None` when `successful < 2` — a single-sample population stddev
    /// is always 0.0 and carries no signal, so we omit it here.
    pub stddev_rtt_micros: Option<f64>,
    /// Smallest RTT in the window. `None` when `successful == 0` OR when
    /// the lazy recompute is pending — callers needing a guaranteed
    /// non-`None` value should call [`RollingStats::summary_with_percentiles`]
    /// instead, which clears the dirty bit and recomputes.
    pub min_rtt_micros: Option<u32>,
    /// Largest RTT in the window. Same semantics as `min_rtt_micros`.
    pub max_rtt_micros: Option<u32>,
}

/// Full summary including percentiles. Once Task 6 fills in the percentile
/// scan, min/max will always be exact (resolved via that scan and the
/// dirty-bit clear). Until then, min/max inherit `summary_fast`'s
/// `None`-when-dirty semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub sample_count: u64,
    pub successful: u64,
    pub failure_rate: f64,
    pub mean_rtt_micros: Option<f64>,
    pub stddev_rtt_micros: Option<f64>,
    pub min_rtt_micros: Option<u32>,
    pub max_rtt_micros: Option<u32>,
    pub p50_rtt_micros: Option<u32>,
    pub p95_rtt_micros: Option<u32>,
    pub p99_rtt_micros: Option<u32>,
}

/// Sliding-window stats, one instance per `(target, protocol)`.
#[derive(Debug)]
pub struct RollingStats {
    window: Duration,
    samples: VecDeque<Sample>,

    // Running counters (updated O(1) on insert/purge).
    sent: u64,
    successful: u64,
    sum_rtt_micros: u64,
    /// `u128` chosen so the worst-case sum-of-squares (900 samples × max
    /// `u32` RTT² ≈ 1.66e22) cannot overflow. See plan version-check gate.
    sum_rtt_sq_micros: u128,

    // Lazy min/max with dirty bits.
    min_rtt_micros: Option<u32>,
    max_rtt_micros: Option<u32>,
    min_rtt_dirty: bool,
    max_rtt_dirty: bool,
}

impl RollingStats {
    /// Create a new empty stats instance with the given window size.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            samples: VecDeque::new(),
            sent: 0,
            successful: 0,
            sum_rtt_micros: 0,
            sum_rtt_sq_micros: 0,
            min_rtt_micros: None,
            max_rtt_micros: None,
            min_rtt_dirty: false,
            max_rtt_dirty: false,
        }
    }

    /// Replace the active window without discarding samples or counters.
    /// The next `purge_old` call applies the new threshold.
    ///
    /// Used by the supervisor when the T14 state machine swaps which
    /// protocol is primary — the previously-primary stats becomes a
    /// diversity stats, and vice versa, without losing history (which
    /// would create a "no data" gap right after a transition and risk
    /// flapping).
    pub fn set_window(&mut self, window: Duration) {
        self.window = window;
    }

    /// Currently configured window size.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Number of buffered samples (post last `purge_old`). Public for tests
    /// and operator metrics; the canonical sample count for callers is
    /// `summary_fast().sample_count`.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// `true` when no samples are buffered.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Insert a single observation. O(1).
    ///
    /// The caller (supervisor) is responsible for filtering protocol
    /// outcomes that should not contribute to rolling stats — currently
    /// only UDP `Refused` (spec 02 § Probe outcomes). Other failures
    /// (`Timeout`, TCP `Refused`, `Error`) are valid samples and arrive
    /// here with `outcome.is_success() == false`.
    pub fn insert(&mut self, obs: &ProbeObservation) {
        let sample = Sample {
            t: obs.observed_at,
            rtt_micros: obs.outcome.rtt_micros(),
        };
        self.sent += 1;
        if let Some(rtt) = sample.rtt_micros {
            self.successful += 1;
            // Both sums are bounded by the worst-case window analysis in
            // the plan's version-check gate: 900 × u32::MAX ≈ 3.87e12 for
            // the linear sum (u64 has ~1.84e19 head-room) and 900 ×
            // u32::MAX² ≈ 1.66e22 for the squared sum (u128 has ~3.4e38).
            // Neither can overflow within any plausible window.
            self.sum_rtt_micros += rtt as u64;
            self.sum_rtt_sq_micros += (rtt as u128) * (rtt as u128);
            // Optimistic min/max: safe even when the dirty bit is set
            // because summary_fast returns None for min/max while dirty,
            // and the next percentile scan (Task 6) recomputes from the
            // deque regardless.
            self.min_rtt_micros = Some(match self.min_rtt_micros {
                Some(cur) => cur.min(rtt),
                None => rtt,
            });
            self.max_rtt_micros = Some(match self.max_rtt_micros {
                Some(cur) => cur.max(rtt),
                None => rtt,
            });
        }
        self.samples.push_back(sample);
    }

    /// Drop samples older than `now - self.window`. Amortized O(1) per
    /// purged entry. Marks min/max dirty if the purged sample held the
    /// current extremum.
    pub fn purge_old(&mut self, _now: Instant) {
        unimplemented!("Task 5")
    }

    /// O(1) summary for state-eval. Min/max are returned as `None` while
    /// dirty — call `summary_with_percentiles` for the resolved values.
    pub fn summary_fast(&self) -> FastSummary {
        let sample_count = self.sent;
        let successful = self.successful;
        let failure_rate = if sample_count == 0 {
            0.0
        } else {
            1.0 - (successful as f64 / sample_count as f64)
        };
        let mean_rtt_micros = if successful == 0 {
            None
        } else {
            Some(self.sum_rtt_micros as f64 / successful as f64)
        };
        let stddev_rtt_micros = if successful < 2 {
            None
        } else {
            // Population variance: E[X²] - E[X]². Both terms in f64 to
            // keep the subtraction stable; clamp negative results to 0
            // (can occur from rounding when all samples are equal).
            let n = successful as f64;
            let mean = mean_rtt_micros.expect("checked successful >= 2");
            let mean_sq = self.sum_rtt_sq_micros as f64 / n;
            let var = (mean_sq - mean * mean).max(0.0);
            Some(var.sqrt())
        };
        let min_rtt_micros = if self.min_rtt_dirty {
            None
        } else {
            self.min_rtt_micros
        };
        let max_rtt_micros = if self.max_rtt_dirty {
            None
        } else {
            self.max_rtt_micros
        };
        FastSummary {
            sample_count,
            successful,
            failure_rate,
            mean_rtt_micros,
            stddev_rtt_micros,
            min_rtt_micros,
            max_rtt_micros,
        }
    }

    /// O(N log N) summary including percentiles. Once Task 6 lands the
    /// percentile scan, this also resolves any pending min/max dirty
    /// bits in the same pass.
    pub fn summary_with_percentiles(&mut self) -> Summary {
        // `&mut self` is intentional even before Task 6: that task will
        // clear `min_rtt_dirty` / `max_rtt_dirty` here during the deque
        // scan, so the receiver type cannot change later without breaking
        // call sites.
        let f = self.summary_fast();
        Summary {
            sample_count: f.sample_count,
            successful: f.successful,
            failure_rate: f.failure_rate,
            mean_rtt_micros: f.mean_rtt_micros,
            stddev_rtt_micros: f.stddev_rtt_micros,
            min_rtt_micros: f.min_rtt_micros,
            max_rtt_micros: f.max_rtt_micros,
            p50_rtt_micros: None,
            p95_rtt_micros: None,
            p99_rtt_micros: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn five_min() -> Duration {
        Duration::from_secs(300)
    }

    #[test]
    fn empty_window_returns_neutral_summary() {
        let stats = RollingStats::new(five_min());
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.successful, 0);
        assert_eq!(s.failure_rate, 0.0);
        assert_eq!(s.mean_rtt_micros, None);
        assert_eq!(s.stddev_rtt_micros, None);
        assert_eq!(s.min_rtt_micros, None);
        assert_eq!(s.max_rtt_micros, None);
    }

    #[test]
    fn empty_window_percentiles_are_none() {
        let mut stats = RollingStats::new(five_min());
        let s = stats.summary_with_percentiles();
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.p50_rtt_micros, None);
        assert_eq!(s.p95_rtt_micros, None);
        assert_eq!(s.p99_rtt_micros, None);
    }

    #[test]
    fn new_stats_reports_window_and_emptiness() {
        let stats = RollingStats::new(five_min());
        assert_eq!(stats.window(), five_min());
        assert!(stats.is_empty());
        assert_eq!(stats.len(), 0);
    }

    use crate::probing::{ProbeObservation, ProbeOutcome};
    use meshmon_protocol::Protocol;

    fn obs_at(t: Instant, outcome: ProbeOutcome) -> ProbeObservation {
        ProbeObservation {
            protocol: Protocol::Icmp,
            target_id: "peer".to_string(),
            outcome,
            hops: None,
            observed_at: t,
        }
    }

    fn ok(t: Instant, rtt_micros: u32) -> ProbeObservation {
        obs_at(t, ProbeOutcome::Success { rtt_micros })
    }

    fn timeout(t: Instant) -> ProbeObservation {
        obs_at(t, ProbeOutcome::Timeout)
    }

    #[test]
    fn single_success_yields_mean_equal_to_rtt() {
        let mut stats = RollingStats::new(five_min());
        stats.insert(&ok(Instant::now(), 1_500));
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 1);
        assert_eq!(s.successful, 1);
        assert_eq!(s.failure_rate, 0.0);
        assert_eq!(s.mean_rtt_micros, Some(1_500.0));
        // stddev requires >= 2 samples (single-sample population stddev
        // is always 0 and carries no signal — see field doc).
        assert_eq!(s.stddev_rtt_micros, None);
        assert_eq!(s.min_rtt_micros, Some(1_500));
        assert_eq!(s.max_rtt_micros, Some(1_500));
    }

    #[test]
    fn single_failure_yields_failure_rate_one() {
        let mut stats = RollingStats::new(five_min());
        stats.insert(&timeout(Instant::now()));
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 1);
        assert_eq!(s.successful, 0);
        assert_eq!(s.failure_rate, 1.0);
        assert_eq!(s.mean_rtt_micros, None);
        assert_eq!(s.min_rtt_micros, None);
        assert_eq!(s.max_rtt_micros, None);
    }

    #[test]
    fn mixed_observations_compute_failure_rate() {
        let mut stats = RollingStats::new(five_min());
        let now = Instant::now();
        for rtt in [1_000_u32, 2_000, 3_000, 4_000] {
            stats.insert(&ok(now, rtt));
        }
        stats.insert(&timeout(now));
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 5);
        assert_eq!(s.successful, 4);
        assert!(
            (s.failure_rate - 0.2).abs() < 1e-9,
            "got {}",
            s.failure_rate
        );
        // mean of {1000,2000,3000,4000} = 2500.
        assert_eq!(s.mean_rtt_micros, Some(2_500.0));
        // Population stddev of {1000,2000,3000,4000} ≈ 1118.034 µs.
        let stddev = s.stddev_rtt_micros.unwrap();
        assert!(
            (stddev - 1_118.033_988_749_895).abs() < 1e-3,
            "got {stddev}"
        );
        assert_eq!(s.min_rtt_micros, Some(1_000));
        assert_eq!(s.max_rtt_micros, Some(4_000));
    }

    #[test]
    fn equal_rtts_have_zero_stddev() {
        let mut stats = RollingStats::new(five_min());
        let now = Instant::now();
        for _ in 0..10 {
            stats.insert(&ok(now, 5_000));
        }
        let s = stats.summary_fast();
        assert_eq!(s.successful, 10);
        // Floating-point subtraction of equal terms can produce tiny
        // negatives; we clamped to 0 in summary_fast.
        assert_eq!(s.stddev_rtt_micros, Some(0.0));
    }

    #[test]
    fn error_outcome_counts_as_failure() {
        let mut stats = RollingStats::new(five_min());
        stats.insert(&obs_at(Instant::now(), ProbeOutcome::Error("bind".into())));
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 1);
        assert_eq!(s.successful, 0);
        assert_eq!(s.failure_rate, 1.0);
    }

    #[test]
    fn tcp_refused_counts_as_failure_when_supervisor_routes_it() {
        // RollingStats itself does NOT inspect protocol; if `Refused`
        // reaches `insert` it is a failure (per spec 02 — TCP Refused
        // means RST, which is genuine connect failure). The supervisor
        // is responsible for dropping UDP Refused upstream.
        let mut stats = RollingStats::new(five_min());
        stats.insert(&obs_at(Instant::now(), ProbeOutcome::Refused));
        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 1);
        assert_eq!(s.successful, 0);
        assert_eq!(s.failure_rate, 1.0);
    }

    // `Duration` is already imported at the top of `mod tests` for the
    // `five_min()` helper — the plan-recommended `use std::time::Duration;`
    // here would shadow-collide (E0252).

    /// Helper: build a `RollingStats` and inject samples at controlled
    /// monotonic offsets relative to a base instant. We sidestep
    /// `tokio::time::pause()` here because `RollingStats` itself doesn't
    /// touch the clock — tests pass the `now` argument directly.
    fn rolling_with_samples(
        window: Duration,
        base: Instant,
        rtts_with_offsets: &[(u64, u32)],
    ) -> RollingStats {
        let mut stats = RollingStats::new(window);
        for (offset_secs, rtt) in rtts_with_offsets {
            stats.insert(&ok(base + Duration::from_secs(*offset_secs), *rtt));
        }
        stats
    }

    #[ignore = "purge_old in Task 5; summary_with_percentiles dirty-resolve in Task 6"]
    #[test]
    fn purge_of_extremum_marks_dirty_then_lazy_recomputes() {
        let base = Instant::now();
        let win = Duration::from_secs(60);
        let mut stats = rolling_with_samples(
            win,
            base,
            // (offset_secs, rtt_micros)
            // Sample at t+0 holds the min (1000) AND will be purged by t+90.
            // Sample at t+30 holds the max (5000) — still in the window at t+90.
            &[
                (0, 1_000),
                (10, 2_000),
                (20, 3_000),
                (30, 5_000),
                (40, 4_000),
            ],
        );

        // Pre-purge sanity.
        let s = stats.summary_fast();
        assert_eq!(s.min_rtt_micros, Some(1_000));
        assert_eq!(s.max_rtt_micros, Some(5_000));

        // Advance 90s — only the t+0 sample falls out of the 60s window.
        // (t+90 - 60 = t+30, so anything with `t < t+30` is purged → only
        // t+0 is dropped.)
        stats.purge_old(base + Duration::from_secs(90));

        let s = stats.summary_fast();
        assert_eq!(s.sample_count, 4, "one sample purged");
        // min was held by the purged sample — dirty bit set → None.
        assert_eq!(s.min_rtt_micros, None, "min should be dirty");
        // max held by t+30 (still in window) — clean.
        assert_eq!(s.max_rtt_micros, Some(5_000));

        // summary_with_percentiles must clear the dirty bit and resolve.
        let resolved = stats.summary_with_percentiles();
        // After purge the surviving samples have RTTs {2000,3000,5000,4000}.
        assert_eq!(resolved.min_rtt_micros, Some(2_000));
        assert_eq!(resolved.max_rtt_micros, Some(5_000));

        // Subsequent summary_fast should now return the resolved values.
        let s = stats.summary_fast();
        assert_eq!(s.min_rtt_micros, Some(2_000));
        assert_eq!(s.max_rtt_micros, Some(5_000));
    }

    #[ignore = "purge_old in Task 5"]
    #[test]
    fn purge_of_non_extremum_leaves_min_max_clean() {
        let base = Instant::now();
        let win = Duration::from_secs(60);
        let mut stats = rolling_with_samples(
            win,
            base,
            // t+0 sample is the median, NOT the extremum.
            &[(0, 3_000), (10, 1_000), (20, 5_000), (30, 4_000)],
        );

        stats.purge_old(base + Duration::from_secs(90));
        let s = stats.summary_fast();
        // Min (1000 at t+10) and max (5000 at t+20) both still in window.
        assert_eq!(s.min_rtt_micros, Some(1_000));
        assert_eq!(s.max_rtt_micros, Some(5_000));
    }

    #[ignore = "purge_old in Task 5"]
    #[test]
    fn purging_all_samples_clears_min_max() {
        let base = Instant::now();
        let win = Duration::from_secs(60);
        let mut stats = rolling_with_samples(win, base, &[(0, 1_000), (10, 2_000)]);
        // Force every sample out.
        stats.purge_old(base + Duration::from_secs(120));
        let resolved = stats.summary_with_percentiles();
        assert_eq!(resolved.sample_count, 0);
        assert_eq!(resolved.min_rtt_micros, None);
        assert_eq!(resolved.max_rtt_micros, None);
    }
}
