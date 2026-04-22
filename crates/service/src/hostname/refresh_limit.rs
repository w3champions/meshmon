//! Per-session refresh-rate bucket for the hostname resolver pipeline.
//!
//! Each session is capped at `max_per_window` refresh calls per `window`
//! (production: 60 calls / 60 seconds). Buckets are lazily created on
//! first access and periodically swept by [`HostnameRefreshLimiter::sweep`]
//! to avoid unbounded memory growth for sessions that never come back.
use crate::hostname::SessionId;
use dashmap::DashMap;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Per-session refresh-rate bucket. Caps at 60 calls / 60 seconds.
pub struct HostnameRefreshLimiter {
    buckets: DashMap<SessionId, RefreshBucket>,
    max_per_window: u32,
    window: Duration,
}

struct RefreshBucket {
    window_started: Instant,
    count: u32,
}

impl HostnameRefreshLimiter {
    /// Construct a limiter with the given cap and sliding window. Wraps
    /// the new limiter in an `Arc` so it can be shared between handlers
    /// and the sweeper task without additional wrapping at call sites.
    pub fn new(max_per_window: u32, window: Duration) -> Arc<Self> {
        Arc::new(Self {
            buckets: DashMap::new(),
            max_per_window,
            window,
        })
    }

    /// Default construction for production: 60 calls / minute.
    pub fn default_production() -> Arc<Self> {
        Self::new(60, Duration::from_secs(60))
    }

    /// Attempt to increment the session's bucket. Returns `true` if
    /// the call is allowed, `false` if it exceeds the cap.
    pub fn check_and_increment(&self, session: &SessionId) -> bool {
        let now = Instant::now();
        let mut entry = self
            .buckets
            .entry(session.clone())
            .or_insert(RefreshBucket {
                window_started: now,
                count: 0,
            });
        if now.duration_since(entry.window_started) >= self.window {
            entry.window_started = now;
            entry.count = 0;
        }
        if entry.count >= self.max_per_window {
            false
        } else {
            entry.count += 1;
            true
        }
    }

    /// Evict buckets whose window ended more than `window`
    /// seconds ago. Called periodically by the sweeper task in
    /// `AppState::new`.
    pub fn sweep(&self) {
        let cutoff = Instant::now() - self.window * 2;
        self.buckets.retain(|_, b| b.window_started > cutoff);
    }

    /// Number of sessions currently tracked. Useful for asserting that
    /// [`Self::sweep`] evicts idle sessions.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cap_allows() {
        let limiter = HostnameRefreshLimiter::new(60, Duration::from_secs(60));
        let session = SessionId::new("a");
        for _ in 0..60 {
            assert!(limiter.check_and_increment(&session));
        }
    }

    #[test]
    fn at_cap_rejects() {
        let limiter = HostnameRefreshLimiter::new(60, Duration::from_secs(60));
        let session = SessionId::new("a");
        for _ in 0..60 {
            assert!(limiter.check_and_increment(&session));
        }
        assert!(!limiter.check_and_increment(&session));
    }

    #[test]
    fn window_resets_cap() {
        let limiter = HostnameRefreshLimiter::new(3, Duration::from_millis(20));
        let session = SessionId::new("a");
        for _ in 0..3 {
            assert!(limiter.check_and_increment(&session));
        }
        assert!(!limiter.check_and_increment(&session));
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check_and_increment(&session));
    }

    #[test]
    fn sessions_are_isolated() {
        let limiter = HostnameRefreshLimiter::new(3, Duration::from_secs(60));
        let a = SessionId::new("a");
        let b = SessionId::new("b");
        for _ in 0..3 {
            assert!(limiter.check_and_increment(&a));
        }
        assert!(!limiter.check_and_increment(&a));
        assert!(limiter.check_and_increment(&b));
    }

    #[test]
    fn sweep_drops_idle_buckets() {
        let limiter = HostnameRefreshLimiter::new(3, Duration::from_millis(10));
        let session = SessionId::new("a");
        assert!(limiter.check_and_increment(&session));
        assert_eq!(limiter.bucket_count(), 1);
        std::thread::sleep(Duration::from_millis(30));
        limiter.sweep();
        assert_eq!(limiter.bucket_count(), 0);
    }
}
