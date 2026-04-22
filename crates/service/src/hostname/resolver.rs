//! Long-running resolver task: owns the backend, pending-lookup map
//! (single-flight), and semaphore-bounded concurrency.
//!
//! `enqueue` records the caller's session on the pending record (new
//! or joined), spawns at most one backend call per IP, and fans out
//! the outcome only to the sessions that enqueued the lookup.

use crate::hostname::{
    canonicalize, record_negative, record_positive, HostnameBroadcaster, HostnameEvent,
    LookupOutcome, ResolverBackend, SessionId,
};
use futures::FutureExt;
use sqlx::PgPool;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tracing::warn;

/// Resolver handle. Cheap to clone — all state is behind `Arc<Inner>`.
#[derive(Clone)]
pub struct Resolver {
    inner: Arc<Inner>,
}

struct Inner {
    backend: Arc<dyn ResolverBackend>,
    broadcaster: HostnameBroadcaster,
    pool: PgPool,
    semaphore: Arc<Semaphore>,
    pending: Mutex<HashMap<IpAddr, PendingLookup>>,
}

struct PendingLookup {
    sessions: HashSet<SessionId>,
}

impl Resolver {
    /// Construct a resolver bound to `backend`, `broadcaster`, `pool`,
    /// and a `Semaphore` capped at `max_in_flight`.
    pub fn new(
        backend: Arc<dyn ResolverBackend>,
        broadcaster: HostnameBroadcaster,
        pool: PgPool,
        max_in_flight: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                broadcaster,
                pool,
                semaphore: Arc::new(Semaphore::new(max_in_flight)),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Broadcaster the resolver emits events on. Callers use this to
    /// register sessions and subscribe to events.
    pub fn broadcaster(&self) -> &HostnameBroadcaster {
        &self.inner.broadcaster
    }

    /// Record `session` as interested in the resolution of `ip` and
    /// ensure a background lookup is in flight. If a lookup is already
    /// in flight for `ip` the session joins the pending record; only
    /// one backend call is made per IP at a time (single-flight).
    pub async fn enqueue(&self, ip: IpAddr, session: SessionId) {
        let canonical = canonicalize(ip);

        // Synchronously record the session on a pending record (new or
        // joined). The guard is dropped before any await so the lock is
        // strictly bounded.
        let should_spawn = {
            let mut pending = self.inner.pending.lock().expect("pending mutex poisoned");
            if let Some(rec) = pending.get_mut(&canonical) {
                rec.sessions.insert(session);
                false
            } else {
                let mut sessions = HashSet::new();
                sessions.insert(session);
                pending.insert(canonical, PendingLookup { sessions });
                true
            }
        };

        if should_spawn {
            let inner = self.inner.clone();
            tokio::spawn(async move {
                run_lookup(inner, canonical).await;
            });
        }
    }

    /// Force-refresh an IP. Identical flow to `enqueue`; freshness is
    /// enforced by the reader's 90-day filter. If a lookup is already
    /// in flight for `ip` the refresh joins that future (the new row
    /// supersedes any stale one via `DISTINCT ON` at read time).
    pub async fn force_refresh(&self, ip: IpAddr, session: SessionId) {
        self.enqueue(ip, session).await
    }
}

async fn run_lookup(inner: Arc<Inner>, ip: IpAddr) {
    let permit = inner.semaphore.clone().acquire_owned().await;
    let outcome = match permit {
        Ok(_permit) => {
            let result = AssertUnwindSafe(inner.backend.reverse_lookup(ip))
                .catch_unwind()
                .await;
            match result {
                Ok(o) => o,
                Err(_) => {
                    warn!(%ip, "hostname: resolver backend panicked");
                    LookupOutcome::Transient("backend panic".into())
                }
            }
        }
        Err(_) => LookupOutcome::Transient("semaphore closed".into()),
    };

    // Drop the pending record (capturing its sessions) before any DB or
    // SSE work so new `enqueue` calls for the same IP can start a fresh
    // single-flight on retry paths.
    let sessions = {
        let mut pending = inner.pending.lock().expect("pending mutex poisoned");
        pending.remove(&ip).map(|r| r.sessions).unwrap_or_default()
    };

    let event_hostname = match outcome {
        LookupOutcome::Positive(hostname) => {
            if let Err(e) = record_positive(&inner.pool, ip, &hostname).await {
                warn!(%ip, error = %e, "hostname: failed to record positive");
            }
            Some(hostname)
        }
        LookupOutcome::NegativeNxDomain => {
            if let Err(e) = record_negative(&inner.pool, ip).await {
                warn!(%ip, error = %e, "hostname: failed to record negative");
            }
            None
        }
        LookupOutcome::Transient(reason) => {
            warn!(%ip, reason, "hostname: transient resolver failure");
            return; // No cache write, no event emission.
        }
    };

    let session_list: Vec<SessionId> = sessions.into_iter().collect();
    inner.broadcaster.fanout(
        &session_list,
        HostnameEvent {
            ip,
            hostname: event_hostname,
        },
    );
}
