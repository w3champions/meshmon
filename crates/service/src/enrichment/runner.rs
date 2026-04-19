//! Background task that drains the enrichment queue, walks the provider
//! chain, and persists the merged result.
//!
//! The runner is the single enforcement point for the first-writer-wins +
//! operator-lock merge contract. Providers are pure — they only compute
//! fields; the runner is where per-row persistence and SSE broadcast
//! happen. Each processed row emits exactly one
//! [`CatalogueEvent::EnrichmentProgress`] on the broker so SSE clients see
//! the terminal status transition (`pending → enriched` or
//! `pending → failed`).
//!
//! # Scheduling
//!
//! Two inputs drive work:
//!
//! - The MPSC queue fed by write-path handlers (paste, agent register, …)
//!   via [`EnrichmentQueue::enqueue`]. `biased` `tokio::select!` gives
//!   this channel priority so fresh work overtakes the sweep.
//! - A periodic sweep that scoops up stale `pending` rows older than
//!   30 seconds, so a brief process restart or a queue-full drop doesn't
//!   leave rows stuck.
//!
//! The runner terminates when the queue sender is dropped (channel
//! closed). Wiring into the service lifecycle is handled by a later task.

use super::{EnrichmentProvider, MergedFields};
use crate::catalogue::{
    events::{CatalogueBroker, CatalogueEvent},
    repo,
};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

/// Producer handle for the runner's enrichment work queue.
///
/// Cloning the producer is intentionally not supported here — call-sites
/// keep a single handle and pass it through state. Capacity is bounded
/// so a storm of paste inserts can't exhaust memory; when the queue is
/// full, [`Self::enqueue`] returns `false` and the periodic sweep picks
/// the row up on the next cycle.
pub struct EnrichmentQueue {
    tx: mpsc::Sender<Uuid>,
}

impl EnrichmentQueue {
    /// Construct a queue with the given capacity and return the paired
    /// receiver. The receiver is consumed by [`Runner::new`] — typical
    /// wiring keeps the producer on `AppState` and moves the receiver
    /// into the spawned runner task.
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<Uuid>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    /// Enqueue a row id for enrichment without blocking the caller.
    ///
    /// Returns `true` when the id was accepted and `false` on back-pressure
    /// (queue full) or when the runner has shut down. A `false` on Full is
    /// logged at `warn` — the sweep will pick the row up once its
    /// `created_at` crosses the 30-second staleness threshold.
    pub fn enqueue(&self, id: Uuid) -> bool {
        match self.tx.try_send(id) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(%id, "enrichment queue full — deferring to sweep");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Background enrichment worker.
///
/// Owns the provider chain, the DB pool, the broker handle, and the
/// receiver half of [`EnrichmentQueue`]. Constructed via [`Runner::new`]
/// and driven by [`Runner::run`] on a spawned task. The chain order is
/// fixed at construction time and determines first-writer-wins priority
/// (earlier providers win on conflicts).
pub struct Runner {
    pool: PgPool,
    chain: Vec<Arc<dyn EnrichmentProvider>>,
    broker: CatalogueBroker,
    rx: mpsc::Receiver<Uuid>,
    sweep_interval: Duration,
}

impl Runner {
    /// Assemble a runner from its collaborators.
    ///
    /// - `pool` — shared Postgres pool; the runner opens its own queries.
    /// - `chain` — ordered provider chain. Earlier entries win conflicts.
    /// - `broker` — in-process fan-out for SSE clients.
    /// - `rx` — paired receiver from [`EnrichmentQueue::new`].
    /// - `sweep_interval` — how often to scan for stale `pending` rows.
    ///   Production uses ~30 s; tests configure short intervals to keep
    ///   suite runtime low.
    pub fn new(
        pool: PgPool,
        chain: Vec<Arc<dyn EnrichmentProvider>>,
        broker: CatalogueBroker,
        rx: mpsc::Receiver<Uuid>,
        sweep_interval: Duration,
    ) -> Self {
        Self {
            pool,
            chain,
            broker,
            rx,
            sweep_interval,
        }
    }

    /// Drive the runner until the queue sender is dropped.
    ///
    /// Queue deliveries take priority over the sweep via `biased`
    /// `tokio::select!`; the sweep is a safety net for rows that missed
    /// the queue (restart-after-enqueue, queue-full drop).
    pub async fn run(mut self) {
        let mut ticker = tokio::time::interval(self.sweep_interval);
        // `Delay` spaces ticks evenly after the select! arm unblocks; the
        // default `Burst` would fire every missed tick back-to-back,
        // producing spurious sweep cycles after a long `process_one`.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        info!(
            "enrichment runner: started ({} providers)",
            self.chain.len()
        );
        loop {
            tokio::select! {
                biased;
                maybe_id = self.rx.recv() => match maybe_id {
                    Some(id) => self.process_one(id).await,
                    None => break,
                },
                _ = ticker.tick() => self.sweep().await,
            }
        }
        info!("enrichment runner: stopped");
    }

    /// Process a single row id end-to-end: load the row, flip the
    /// enrichment timestamp, walk the provider chain, persist merged
    /// output, and broadcast the terminal status.
    async fn process_one(&self, id: Uuid) {
        let entry = match repo::find_by_id(&self.pool, id).await {
            Ok(Some(e)) => e,
            Ok(None) => return,
            Err(e) => {
                warn!(%id, error = %e, "enrichment: find_by_id failed");
                return;
            }
        };
        if let Err(e) = repo::mark_enrichment_start(&self.pool, id).await {
            warn!(%id, error = %e, "enrichment: mark_start failed");
            return;
        }
        let locked = entry.operator_edited_fields.clone();
        let mut merged = MergedFields::default();
        for provider in &self.chain {
            match provider.lookup(entry.ip).await {
                Ok(res) => merged.apply(provider.id(), res, &locked),
                Err(e) => {
                    warn!(%id, provider = provider.id(), error = %e, "enrichment: provider error");
                }
            }
        }
        let status = match repo::apply_enrichment_result(&self.pool, id, merged).await {
            Ok(s) => s,
            Err(e) => {
                warn!(%id, error = %e, "enrichment: apply_result failed");
                return;
            }
        };
        self.broker
            .publish(CatalogueEvent::EnrichmentProgress { id, status });
    }

    /// Scan the DB for stale `pending` rows and process them.
    ///
    /// Bounded to 128 rows per sweep to keep the runner responsive to
    /// fresh queue deliveries; the next tick picks up any remainder.
    ///
    /// NOTE on re-entry: the sweep keys off `(status = 'pending', created_at
    /// older than 30 s)`. Because the single-task `tokio::select!` serialises
    /// sweep ticks with `process_one`, an in-flight call can't run concurrently
    /// with its own sweep pick. A long `process_one` (>30 s) that later fails
    /// without transitioning the row out of `pending` — e.g. a crashed process
    /// or an error path that skipped `apply_enrichment_result` — may be re-picked
    /// by a subsequent sweep. Adding `enriched_at IS NULL` to the predicate
    /// would not change this: `mark_enrichment_start` nulls `enriched_at` at
    /// the start of every attempt, so the extra clause is always true for
    /// genuinely in-flight rows. The correct guard would be a `last_attempt_at`
    /// column (deferred — out of scope here). Meanwhile repeats are wasteful
    /// but safe: the provider chain is idempotent and persistence uses
    /// `COALESCE` + the operator-lock check, so re-running on the same row
    /// produces the same merged state.
    async fn sweep(&self) {
        let rows: Vec<Uuid> = match sqlx::query_scalar!(
            r#"SELECT id FROM ip_catalogue
               WHERE enrichment_status = 'pending'
                 AND created_at < NOW() - INTERVAL '30 seconds'
               LIMIT 128"#
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "enrichment sweep query failed");
                return;
            }
        };
        for id in rows {
            self.process_one(id).await;
        }
    }
}
