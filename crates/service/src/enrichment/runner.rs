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
    facets::FacetsCache,
    model::EnrichmentStatus,
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
    /// Facets cache shared with the HTTP handlers. The runner calls
    /// [`FacetsCache::invalidate`] after every terminal enrichment
    /// transition (`Enriched` / `Failed`) so the filter rail reflects
    /// the new `country_code`, `asn`, `network_operator`, and
    /// `enrichment_status` values without waiting for the TTL.
    facets_cache: Arc<FacetsCache>,
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
    /// - `facets_cache` — shared cache invalidated on every terminal
    ///   enrichment transition so the filter rail stays fresh.
    pub fn new(
        pool: PgPool,
        chain: Vec<Arc<dyn EnrichmentProvider>>,
        broker: CatalogueBroker,
        rx: mpsc::Receiver<Uuid>,
        sweep_interval: Duration,
        facets_cache: Arc<FacetsCache>,
    ) -> Self {
        Self {
            pool,
            chain,
            broker,
            rx,
            sweep_interval,
            facets_cache,
        }
    }

    /// Drive the runner until the queue sender is dropped.
    ///
    /// Both branches — queue-drain and periodic sweep — share one
    /// `tokio::select!`. The select is *unbiased* on purpose: a `biased`
    /// select that preferred the queue would starve the sweep whenever
    /// the queue stayed continuously non-empty (sustained paste load),
    /// and rows dropped by `enqueue` backpressure would never be
    /// recovered. Unbiased scheduling means tokio picks randomly when
    /// both are ready, so the sweep fires within a bounded window
    /// (~2 × `sweep_interval` worst case under continuous queue
    /// traffic). Concurrent execution of both branches is not a
    /// correctness hazard: `process_one` is idempotent (the repo's
    /// `COALESCE`-plus-lock-re-check `UPDATE` tolerates being re-run
    /// on the same row) and the select itself serialises the two
    /// branches within this task.
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
        // `saw_retryable_error` tracks whether any provider returned a
        // retryable failure (rate-limited / transient). If every provider
        // in the chain errored retryably *and* produced no data, we must
        // leave the row in `pending` — otherwise the sweep will never
        // pick it up again and a brief upstream outage permanently
        // strands rows in `failed`. Terminal errors (Unauthorized,
        // NotFound, Permanent) don't set this: they represent "retrying
        // won't help" states.
        let mut saw_retryable_error = false;
        for provider in &self.chain {
            // Skip providers whose entire `supported()` set is already
            // settled (filled by an earlier provider or locked by the
            // operator). Without this, a chain like ipgeo → rdap makes
            // an RDAP call for every row even when ipgeo already filled
            // ASN and NetworkOperator — wasted quota and latency.
            if !merged.needs_provider(provider.supported(), &locked) {
                continue;
            }
            match provider.lookup(entry.ip).await {
                Ok(res) => merged.apply(provider.id(), res, &locked),
                Err(e) => {
                    if e.is_retryable() {
                        saw_retryable_error = true;
                    }
                    warn!(%id, provider = provider.id(), error = %e, "enrichment: provider error");
                }
            }
        }
        // Pick the terminal status the DB will record when `merged` is
        // empty: `Pending` keeps the row in the sweep's queue for a
        // retry, `Failed` is the end state when every provider gave a
        // terminal verdict. `apply_enrichment_result` ignores this when
        // at least one provider populated a field (the row always
        // becomes `Enriched` in that case).
        let empty_status = if saw_retryable_error {
            EnrichmentStatus::Pending
        } else {
            EnrichmentStatus::Failed
        };
        let status = match repo::apply_enrichment_result(&self.pool, id, merged, empty_status).await
        {
            // `None` means the row was concurrently deleted between
            // our read and write — skip the progress broadcast rather
            // than emit a ghost `EnrichmentProgress` for a gone row.
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(e) => {
                warn!(%id, error = %e, "enrichment: apply_result failed");
                return;
            }
        };
        self.broker
            .publish(CatalogueEvent::EnrichmentProgress { id, status });
        // Invalidate the facets cache on terminal transitions only.
        // `Enriched` and `Failed` both settle the row's `enrichment_status`
        // bucket and may have filled `country_code`, `asn`, or
        // `network_operator` — all of which drive facet counts. `Pending`
        // means the row will be retried; its counts haven't changed yet and
        // we skip the invalidation to avoid a spurious extra DB round-trip.
        if matches!(
            status,
            EnrichmentStatus::Enriched | EnrichmentStatus::Failed
        ) {
            self.facets_cache.invalidate().await;
        }
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
               ORDER BY created_at ASC, id ASC
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
