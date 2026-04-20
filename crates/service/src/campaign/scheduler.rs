//! Fair round-robin campaign scheduler.
//!
//! - One `tokio::spawn`ed task per service instance.
//! - LISTEN/NOTIFY wake on `campaign_state_changed` plus a configurable
//!   tick fallback (default 500 ms).
//! - Round-robin cursor across active campaigns at batch granularity so
//!   every running campaign gets fair share of each agent's dispatch
//!   budget.
//!
//! The dispatcher is injected via the [`PairDispatcher`] trait so tests
//! can drive the loop with stub implementations (see `dispatch.rs`). T45
//! plugs in the real RPC dispatcher.

use super::dispatch::{PairDispatcher, PendingPair};
use super::events::NOTIFY_CHANNEL;
use super::model::CampaignState;
use super::repo::{self, RepoError};
use crate::metrics;
use crate::registry::AgentRegistry;
use moka::future::Cache;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Per-destination token bucket configuration. Simple leaky-bucket of
/// capacity `per_destination_rps`; refills once per second.
#[derive(Debug, Clone)]
struct Bucket {
    /// Tokens remaining in the current second.
    remaining: u32,
    /// When the bucket last refilled.
    refilled_at: Instant,
    /// Capacity (tokens per second).
    capacity: u32,
}

impl Bucket {
    fn new(capacity: u32) -> Self {
        Self {
            remaining: capacity,
            refilled_at: Instant::now(),
            capacity,
        }
    }

    /// Try to draw `n` tokens. Returns the number actually drawn (0..=n).
    /// Refills to full on every clock second.
    fn try_take(&mut self, n: u32) -> u32 {
        let now = Instant::now();
        if now.duration_since(self.refilled_at) >= Duration::from_secs(1) {
            self.remaining = self.capacity;
            self.refilled_at = now;
        }
        let drawn = n.min(self.remaining);
        self.remaining -= drawn;
        drawn
    }
}

/// Single-instance campaign scheduler. Instantiate with [`Scheduler::new`]
/// and drive with [`Scheduler::run`] inside a `tokio::spawn`.
pub struct Scheduler {
    pool: PgPool,
    registry: Arc<AgentRegistry>,
    dispatcher: Arc<dyn PairDispatcher>,
    tick: Duration,
    chunk_size: i64,
    per_destination_rps: u32,
    max_pair_attempts: i16,
    target_active_window: Duration,
}

impl Scheduler {
    /// Construct a scheduler with every knob the tick loop needs.
    ///
    /// - `tick_ms` — tick fallback cadence; NOTIFY wakes the loop sooner
    ///   when the DB trigger fires.
    /// - `chunk_size` — maximum pairs claimed per `(agent, campaign)` per
    ///   tick (see `repo::take_pending_batch`).
    /// - `per_destination_rps` — per-destination-IP token bucket cap.
    /// - `max_pair_attempts` — sweep threshold for `pending` pairs the
    ///   scheduler gives up on (see `repo::expire_stale_attempts`).
    /// - `target_active_window` — agents with `last_seen_at` newer than
    ///   this window are eligible to receive dispatches.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        registry: Arc<AgentRegistry>,
        dispatcher: Arc<dyn PairDispatcher>,
        tick_ms: u32,
        chunk_size: i64,
        per_destination_rps: u32,
        max_pair_attempts: i16,
        target_active_window: Duration,
    ) -> Self {
        Self {
            pool,
            registry,
            dispatcher,
            tick: Duration::from_millis(tick_ms as u64),
            chunk_size,
            per_destination_rps,
            max_pair_attempts,
            target_active_window,
        }
    }

    /// Main loop. Runs until `cancel` fires.
    ///
    /// Opens a dedicated [`PgListener`] on `NOTIFY_CHANNEL`. On listener
    /// failure falls back to a tick-only loop so a transient listener
    /// outage never grounds dispatch permanently.
    pub async fn run(self, cancel: CancellationToken) {
        info!(
            tick_ms = self.tick.as_millis() as u64,
            chunk_size = self.chunk_size,
            rps = self.per_destination_rps,
            "campaign scheduler starting"
        );

        let mut listener = match PgListener::connect_with(&self.pool).await {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    error = %e,
                    "scheduler: failed to open PgListener; falling back to periodic tick only"
                );
                self.tick_only_loop(cancel).await;
                return;
            }
        };
        if let Err(e) = listener.listen(NOTIFY_CHANNEL).await {
            warn!(
                error = %e,
                "scheduler: failed to subscribe to NOTIFY; falling back to periodic tick only"
            );
            self.tick_only_loop(cancel).await;
            return;
        }

        let buckets: Cache<IpAddr, Arc<tokio::sync::Mutex<Bucket>>> = Cache::builder()
            .time_to_idle(Duration::from_secs(60))
            .build();

        // Round-robin cursor; preserved across ticks so successive ticks
        // interleave batches between active campaigns.
        let mut cursor: usize = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("campaign scheduler shutting down");
                    return;
                }
                recv = listener.try_recv() => {
                    match recv {
                        Ok(Some(n)) => debug!(
                            channel = n.channel(),
                            payload = n.payload(),
                            "notify received",
                        ),
                        Ok(None) => {
                            warn!("scheduler: PgListener closed; switching to tick-only");
                            self.tick_only_loop(cancel).await;
                            return;
                        }
                        Err(e) => warn!(error = %e, "scheduler: listener recv error"),
                    }
                }
                _ = sleep(self.tick) => {}
            }

            if let Err(e) = self.tick_once(&buckets, &mut cursor).await {
                warn!(error = %e, "scheduler: tick failed");
            }
        }
    }

    async fn tick_only_loop(&self, cancel: CancellationToken) {
        let buckets: Cache<IpAddr, Arc<tokio::sync::Mutex<Bucket>>> = Cache::builder()
            .time_to_idle(Duration::from_secs(60))
            .build();
        let mut cursor: usize = 0;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sleep(self.tick) => {}
            }
            if let Err(e) = self.tick_once(&buckets, &mut cursor).await {
                warn!(error = %e, "scheduler (tick-only): tick failed");
            }
        }
    }

    async fn tick_once(
        &self,
        buckets: &Cache<IpAddr, Arc<tokio::sync::Mutex<Bucket>>>,
        cursor: &mut usize,
    ) -> Result<(), RepoError> {
        // Stopwatch around the dispatch body. We record the histogram
        // whether the inner loop returns Ok or Err so failed ticks still
        // show up in SLOs; then sample the gauges once per tick.
        let started = Instant::now();
        let result = self.tick_once_inner(buckets, cursor).await;
        metrics::scheduler_tick_seconds().record(started.elapsed().as_secs_f64());
        self.sample_metrics().await;
        result
    }

    /// One-shot snapshot of campaign/pair counts + reuse ratio. Any error
    /// is swallowed with a warn — a tick that dispatched work should not
    /// fail because the aggregate query misbehaved.
    async fn sample_metrics(&self) {
        match repo::metrics_snapshot(&self.pool).await {
            Ok(snap) => {
                for (state, n) in &snap.campaigns {
                    metrics::campaigns_total(state.as_str()).set(*n as f64);
                }
                for (state, n) in &snap.pairs {
                    metrics::campaign_pairs_total(state.as_str()).set(*n as f64);
                }
                metrics::campaign_reuse_ratio().set(snap.reuse_ratio);
            }
            Err(e) => {
                warn!(error = %e, "scheduler: metrics snapshot failed");
            }
        }
    }

    async fn tick_once_inner(
        &self,
        buckets: &Cache<IpAddr, Arc<tokio::sync::Mutex<Bucket>>>,
        cursor: &mut usize,
    ) -> Result<(), RepoError> {
        // Reload active campaigns (started_at ASC for stable rotation).
        let active_campaigns = repo::active_campaigns(&self.pool).await?;
        if active_campaigns.is_empty() {
            // Cursor is irrelevant while empty; keep it for when new
            // campaigns arrive. Sweep stale attempts even on an empty tick.
            let swept = repo::expire_stale_attempts(&self.pool, self.max_pair_attempts).await?;
            if swept > 0 {
                debug!(swept, "scheduler: swept stale attempts");
            }
            return Ok(());
        }

        // Active agents = registry targets with last_seen_at within window.
        // `active_targets("")` — empty sentinel excludes no one so every
        // agent is returned.
        let active_snapshot = self.registry.snapshot();
        let active_agents = active_snapshot.active_targets("", self.target_active_window);

        let len = active_campaigns.len();
        for agent in &active_agents {
            // Start one-past-cursor so the rotation advances between ticks
            // even when the first campaign keeps returning work.
            for step in 1..=len {
                let c_idx = (*cursor + step) % len;
                let c_id = active_campaigns[c_idx];
                let dispatched = self.dispatch_for_campaign(c_id, &agent.id, buckets).await?;
                if dispatched {
                    *cursor = c_idx;
                    break;
                }
            }
        }

        // After a batch settles, complete any campaigns whose pairs are
        // all terminal.
        for c_id in active_campaigns {
            let _ = repo::maybe_complete(&self.pool, c_id).await?;
        }

        // Safety-net sweep.
        let _ = repo::expire_stale_attempts(&self.pool, self.max_pair_attempts).await?;

        Ok(())
    }

    /// Returns `true` if work was dispatched for this campaign + agent.
    async fn dispatch_for_campaign(
        &self,
        campaign_id: Uuid,
        agent_id: &str,
        buckets: &Cache<IpAddr, Arc<tokio::sync::Mutex<Bucket>>>,
    ) -> Result<bool, RepoError> {
        // Read campaign row for the knobs (probe_count etc.) without pair join.
        let camp = match repo::get_raw_for_scheduler(&self.pool, campaign_id).await? {
            Some(c) if c.state == CampaignState::Running => c,
            _ => return Ok(false),
        };

        // Pull a batch of pending pairs for (campaign, agent).
        let mut batch =
            repo::take_pending_batch(&self.pool, campaign_id, agent_id, self.chunk_size).await?;
        if batch.is_empty() {
            return Ok(false);
        }

        // Reuse lookup (skipped entirely when campaign.force_measurement).
        if !camp.force_measurement {
            let decisions = repo::resolve_reuse(&self.pool, &batch, camp.protocol).await?;
            if !decisions.is_empty() {
                repo::apply_reuse(&self.pool, &decisions).await?;
                // Filter the batch to drop reused pairs before dispatching.
                let reused_ids: std::collections::HashSet<i64> =
                    decisions.iter().map(|(pair_id, _)| *pair_id).collect();
                batch.retain(|p| !reused_ids.contains(&p.id));
                if batch.is_empty() {
                    return Ok(true);
                }
            }
        }

        // Per-destination rate limit.
        let mut allowed: Vec<PendingPair> = Vec::with_capacity(batch.len());
        for p in batch {
            let dest: IpAddr = match p.destination_ip {
                sqlx::types::ipnetwork::IpNetwork::V4(n) => IpAddr::V4(n.ip()),
                sqlx::types::ipnetwork::IpNetwork::V6(n) => IpAddr::V6(n.ip()),
            };
            let bucket = buckets
                .get_with(dest, async {
                    Arc::new(tokio::sync::Mutex::new(Bucket::new(
                        self.per_destination_rps,
                    )))
                })
                .await;
            let drawn = {
                let mut guard = bucket.lock().await;
                guard.try_take(1)
            };
            if drawn == 1 {
                allowed.push(PendingPair {
                    pair_id: p.id,
                    campaign_id,
                    source_agent_id: p.source_agent_id.clone(),
                    destination_ip: dest,
                    probe_count: camp.probe_count,
                    timeout_ms: camp.timeout_ms,
                    probe_stagger_ms: camp.probe_stagger_ms,
                    force_measurement: camp.force_measurement,
                    protocol: camp.protocol,
                });
            } else {
                // Rate-limit hit: put the pair back to pending so a later
                // tick retries. `take_pending_batch` flipped it to
                // dispatched and bumped attempt_count; revert both.
                sqlx::query!(
                    "UPDATE campaign_pairs
                        SET resolution_state = 'pending',
                            dispatched_at    = NULL,
                            attempt_count    = GREATEST(0, attempt_count - 1)
                      WHERE id = $1",
                    p.id
                )
                .execute(&self.pool)
                .await
                .map_err(RepoError::from)?;
            }
        }

        if allowed.is_empty() {
            // We did useful work (reuse settlements or rate-limit backoff).
            // Report `true` so the caller anchors the cursor on this
            // campaign — subsequent agents in this tick (and the next
            // tick) rotate past it via `(cursor + step) % len`.
            return Ok(true);
        }

        let _ = self.dispatcher.dispatch(agent_id, allowed).await;
        Ok(true)
    }
}
