//! TTL-cached facets snapshot.
//!
//! [`FacetsCache`] wraps [`super::repo::facets`] with a coarse time-based
//! cache. Callers receive the cached [`FacetsResponse`] while it is still
//! fresh (age < `ttl`); on expiry, the next caller refreshes from the
//! database. Concurrent callers during a refresh serialise on the inner
//! `Mutex` (single-flight semantics) so a cache miss never fans out into
//! multiple simultaneous aggregation queries.
//!
//! The chosen TTL is intentionally coarse (30 s in production) — facets
//! are a UI filter hint, not a source of truth, and the aggregation
//! query is the most expensive read in the catalogue surface. A single
//! mutex across the refresh is acceptable because:
//!
//! - All readers serialise on the inner `tokio::sync::Mutex`. For
//!   warm-cache hits the critical section is brief (elapsed-time check
//!   + clone of the cached [`FacetsResponse`]).
//! - If a concurrent caller is refreshing, other readers wait for the
//!   DB round-trip to complete — acceptable for a 30 s-TTL UI hint
//!   endpoint, not acceptable for hot paths.
//! - Readers during a refresh queue up on the mutex and all receive the
//!   freshly-fetched value from the single in-flight query.
//! - `tokio::sync::Mutex` holds across `.await`, which is required here
//!   because we call the async [`super::repo::facets`] inside the
//!   critical section.

use super::repo::{self, FacetsResponse};
use sqlx::PgPool;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// TTL-cached wrapper around [`super::repo::facets`].
///
/// Construct once per process and share via `Arc` — the `AppState`
/// wiring stores `Arc<FacetsCache>`. The first call always hits the
/// database; subsequent calls served from the in-memory cache until
/// the snapshot ages past `ttl`.
pub struct FacetsCache {
    /// `Some((fetched_at, value))` once populated; `None` until the
    /// first successful refresh. The mutex provides the single-flight
    /// guarantee across concurrent refreshes.
    inner: Mutex<Option<(Instant, FacetsResponse)>>,
    /// Maximum age a cached snapshot is served before the next caller
    /// triggers a refresh.
    ttl: Duration,
}

impl FacetsCache {
    /// Default TTL used by the production `AppState` wiring. Facets are
    /// a UI filter hint, not real-time data — 30 s keeps the expensive
    /// aggregation query off the hot path without making the filter
    /// feel stale.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

    /// Construct a cache with the given freshness window. The cache is
    /// empty until the first [`Self::get`] call refreshes it — no
    /// preloading at startup.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(None),
            ttl,
        }
    }

    /// Return the cached facets snapshot, refreshing from `pool` if the
    /// cache is empty or older than the configured TTL.
    ///
    /// On refresh failure the error is propagated and the cache is left
    /// untouched — a subsequent caller will retry against the pool. No
    /// negative caching.
    pub async fn get(&self, pool: &PgPool) -> Result<FacetsResponse, sqlx::Error> {
        let mut guard = self.inner.lock().await;
        if let Some((fetched_at, value)) = guard.as_ref() {
            if fetched_at.elapsed() < self.ttl {
                return Ok(value.clone());
            }
        }
        let fresh = repo::facets(pool).await?;
        *guard = Some((Instant::now(), fresh.clone()));
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_leaves_cache_empty() {
        let cache = FacetsCache::new(Duration::from_secs(30));
        // Poll-lock is synchronous in tests; the mutex is freshly-created
        // so the try_lock cannot contend with anything else.
        let guard = cache.inner.try_lock().expect("uncontended");
        assert!(guard.is_none(), "cache must start empty");
    }
}
