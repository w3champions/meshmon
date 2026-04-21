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

use super::dispatch::{DispatchOutcome, PairDispatcher, PendingPair};
use super::events::{NOTIFY_CHANNEL, PAIR_SETTLED_CHANNEL};
use super::model::{CampaignState, MeasurementKind, PairResolutionState};
use super::repo::{self, RepoError};
use crate::metrics;
use crate::registry::AgentRegistry;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Single-instance campaign scheduler. Instantiate with [`Scheduler::new`]
/// and drive with [`Scheduler::run`] inside a `tokio::spawn`.
///
/// The per-destination token bucket lives on the dispatcher (see
/// [`super::rpc_dispatcher::RpcDispatcher`]); the scheduler only drives
/// claim → dispatch → revert and never second-guesses the dispatcher's
/// throttling decisions.
pub struct Scheduler {
    pool: PgPool,
    registry: Arc<AgentRegistry>,
    dispatcher: Arc<dyn PairDispatcher>,
    tick: Duration,
    chunk_size: i64,
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
        max_pair_attempts: i16,
        target_active_window: Duration,
    ) -> Self {
        Self {
            pool,
            registry,
            dispatcher,
            tick: Duration::from_millis(tick_ms as u64),
            chunk_size,
            max_pair_attempts,
            target_active_window,
        }
    }

    /// Main loop. Runs until `cancel` fires.
    ///
    /// Opens a dedicated [`PgListener`] subscribed to both
    /// [`NOTIFY_CHANNEL`] (lifecycle transitions) and
    /// [`PAIR_SETTLED_CHANNEL`] (dispatch-writer settlements). Either
    /// channel wakes the loop sooner than the periodic tick; the `recv`
    /// arm does not distinguish channels since any wake triggers a
    /// single `tick_once`. On listener failure, falls back to a
    /// tick-only loop so a transient listener outage never grounds
    /// dispatch permanently.
    pub async fn run(self, cancel: CancellationToken) {
        info!(
            tick_ms = self.tick.as_millis() as u64,
            chunk_size = self.chunk_size,
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
        if let Err(e) = listener
            .listen_all([NOTIFY_CHANNEL, PAIR_SETTLED_CHANNEL])
            .await
        {
            warn!(
                error = %e,
                "scheduler: failed to subscribe to NOTIFY channels; falling back to periodic tick only"
            );
            self.tick_only_loop(cancel).await;
            return;
        }

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

            if let Err(e) = self.tick_once(&mut cursor).await {
                warn!(error = %e, "scheduler: tick failed");
            }
        }
    }

    async fn tick_only_loop(&self, cancel: CancellationToken) {
        let mut cursor: usize = 0;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sleep(self.tick) => {}
            }
            if let Err(e) = self.tick_once(&mut cursor).await {
                warn!(error = %e, "scheduler (tick-only): tick failed");
            }
        }
    }

    async fn tick_once(&self, cursor: &mut usize) -> Result<(), RepoError> {
        // Stopwatch around the dispatch body. We record the histogram
        // whether the inner loop returns Ok or Err so failed ticks still
        // show up in SLOs; then sample the gauges once per tick.
        let started = Instant::now();
        let result = self.tick_once_inner(cursor).await;
        metrics::scheduler_tick_seconds().record(started.elapsed().as_secs_f64());
        self.sample_metrics().await;
        result
    }

    /// One-shot snapshot of campaign/pair counts + reuse ratio. Any error
    /// is swallowed with a warn — a tick that dispatched work should not
    /// fail because the aggregate query misbehaved.
    ///
    /// Postgres `GROUP BY state` omits zero-count rows. To prevent stale
    /// gauge readings after a state drains empty, we iterate over the
    /// full enum and set every label — defaulting missing ones to 0.
    async fn sample_metrics(&self) {
        match repo::metrics_snapshot(&self.pool).await {
            Ok(snap) => {
                for state in CampaignState::ALL {
                    let n = snap
                        .campaigns
                        .iter()
                        .find(|(s, _)| s == state)
                        .map(|(_, n)| *n)
                        .unwrap_or(0);
                    metrics::campaigns_total(state.as_str()).set(n as f64);
                }
                for state in PairResolutionState::ALL {
                    let n = snap
                        .pairs
                        .iter()
                        .find(|(s, _)| s == state)
                        .map(|(_, n)| *n)
                        .unwrap_or(0);
                    metrics::campaign_pairs_total(state.as_str()).set(n as f64);
                }
                metrics::campaign_reuse_ratio().set(snap.reuse_ratio);
            }
            Err(e) => {
                warn!(error = %e, "scheduler: metrics snapshot failed");
            }
        }
    }

    async fn tick_once_inner(&self, cursor: &mut usize) -> Result<(), RepoError> {
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
                let dispatched = self.dispatch_for_campaign(c_id, &agent.id).await?;
                if dispatched {
                    *cursor = c_idx;
                    break;
                }
            }
        }

        // Skip pairs for source agents that are not currently active.
        // Without this sweep, a campaign targeting an offline agent
        // would stay in `running` forever because `maybe_complete` only
        // fires when every pair is terminal. The registry's activity
        // window is on the order of minutes, so by the time an agent
        // is missing from `active_agents` it has been silent long
        // enough to declare its pairs stuck.
        let active_agent_ids: Vec<String> = active_agents.iter().map(|a| a.id.clone()).collect();
        let skipped_offline = repo::skip_pending_for_inactive_sources(
            &self.pool,
            &active_agent_ids,
            &active_campaigns,
        )
        .await?;
        if skipped_offline > 0 {
            debug!(
                skipped = skipped_offline,
                "scheduler: skipped pairs for offline source agents"
            );
        }

        // After a batch settles (or agent-offline sweep skips), complete
        // any campaigns whose pairs are all terminal.
        for c_id in active_campaigns {
            let _ = repo::maybe_complete(&self.pool, c_id).await?;
        }

        // Safety-net sweep for max-attempts-exceeded pairs.
        let _ = repo::expire_stale_attempts(&self.pool, self.max_pair_attempts).await?;

        Ok(())
    }

    /// Returns `true` if work was dispatched for this campaign + agent.
    async fn dispatch_for_campaign(
        &self,
        campaign_id: Uuid,
        agent_id: &str,
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

        // Reuse lookup. Two structural bypasses:
        //   - `force_measurement` short-circuits at the scheduler level.
        //   - `resolve_reuse` itself filters `cp.kind='campaign'`, so
        //     detail rows never match. No extra kind-gate needed here.
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

        // Per-destination rate limiting is the dispatcher's responsibility
        // (see `rpc_dispatcher.rs`); the scheduler only claims pairs and
        // reverts what the dispatcher refuses.
        //
        // Atomicity gap: `take_pending_batch` commits the
        // `pending → dispatched` flip in its own transaction, so a
        // process panic/kill between that commit and the revert below
        // leaves the refused subset stranded in `dispatched`.
        // `expire_stale_attempts` only targets `pending` rows, so
        // recovery surfaces are: operator `force_pair`,
        // `apply_edit{force_measurement=true}` (which resets every
        // non-pending pair including `dispatched`), or a process
        // restart followed by adding a `dispatched`-TTL sweeper. Tick
        // panics are logged, so the failure is observable.

        // Group the batch by kind. `rpc_dispatcher::build_request` reads
        // the wire `MeasurementKind` and `probe_count` from the head
        // pair, so each call must be homogeneous. The scheduler picks
        // kind-specific `probe_count` here:
        //   - Campaign    → campaigns.probe_count (baseline cadence)
        //   - DetailPing  → campaigns.probe_count_detail (high-fidelity)
        //   - DetailMtr   → 1 (MTR spec: single round)
        let mut by_kind: std::collections::HashMap<MeasurementKind, Vec<PendingPair>> =
            std::collections::HashMap::new();
        for p in batch {
            let dest = match p.destination_ip {
                sqlx::types::ipnetwork::IpNetwork::V4(n) => std::net::IpAddr::V4(n.ip()),
                sqlx::types::ipnetwork::IpNetwork::V6(n) => std::net::IpAddr::V6(n.ip()),
            };
            let probe_count = match p.kind {
                MeasurementKind::Campaign => camp.probe_count,
                MeasurementKind::DetailPing => camp.probe_count_detail,
                MeasurementKind::DetailMtr => 1,
            };
            // Detail dispatches MUST bypass any agent-side cache to
            // produce the promised high-fidelity probes — an operator
            // hitting `/detail` on a campaign created with the default
            // `force_measurement=false` would otherwise get
            // agent-cached results back. Kind-based override here
            // keeps the campaign's own `force_measurement` knob
            // exclusively about baseline dispatch.
            let force_measurement = match p.kind {
                MeasurementKind::Campaign => camp.force_measurement,
                MeasurementKind::DetailPing | MeasurementKind::DetailMtr => true,
            };
            by_kind.entry(p.kind).or_default().push(PendingPair {
                pair_id: p.id,
                campaign_id,
                source_agent_id: p.source_agent_id,
                destination_ip: dest,
                probe_count,
                timeout_ms: camp.timeout_ms,
                probe_stagger_ms: camp.probe_stagger_ms,
                force_measurement,
                protocol: camp.protocol,
                kind: p.kind,
            });
        }

        // Issue one dispatch per kind group, aggregating outcomes so the
        // existing revert/rate-limit bookkeeping runs exactly once for
        // the whole tick batch.
        let mut aggregate = DispatchOutcome::default();
        for (_kind, group) in by_kind {
            if group.is_empty() {
                continue;
            }
            let outcome = self.dispatcher.dispatch(agent_id, group).await;
            aggregate.dispatched += outcome.dispatched;
            aggregate.rejected_ids.extend(outcome.rejected_ids);
            aggregate.rate_limited_ids.extend(outcome.rate_limited_ids);
            if aggregate.skipped_reason.is_none() {
                aggregate.skipped_reason = outcome.skipped_reason;
            }
        }

        // Both revert branches gate on `resolution_state = 'dispatched'`
        // — the same predicate the writer uses. Without it, an operator
        // `force_pair` or `apply_edit{force_measurement=true}` that
        // lands between batch claim and dispatcher completion would
        // get clobbered back to `pending` by these UPDATEs, losing
        // the reset. With the gate, concurrent resets survive and the
        // revert becomes a silent no-op for those rows.

        // Rate-limited pairs: revert AND decrement attempt_count so a
        // throttling decision made before the RPC does not burn retry
        // budget. Without this, a high-traffic destination would exhaust
        // its per-pair attempt budget after `max_pair_attempts`
        // consecutive rate-limited ticks and get expired.
        if !aggregate.rate_limited_ids.is_empty() {
            sqlx::query!(
                "UPDATE campaign_pairs
                    SET resolution_state = 'pending',
                        dispatched_at    = NULL,
                        attempt_count    = GREATEST(0, attempt_count - 1)
                  WHERE id = ANY($1::bigint[])
                    AND resolution_state = 'dispatched'",
                &aggregate.rate_limited_ids as &[i64],
            )
            .execute(&self.pool)
            .await
            .map_err(RepoError::from)?;
        }

        if !aggregate.rejected_ids.is_empty() {
            // Dispatcher refused these — revert to `pending` so a
            // subsequent tick can retry. `take_pending_batch` already
            // bumped `attempt_count`, so the retry budget counts down
            // naturally; `expire_stale_attempts` eventually skips any
            // pair the dispatcher keeps rejecting.
            sqlx::query!(
                "UPDATE campaign_pairs
                    SET resolution_state = 'pending',
                        dispatched_at    = NULL
                  WHERE id = ANY($1::bigint[])
                    AND resolution_state = 'dispatched'",
                &aggregate.rejected_ids as &[i64],
            )
            .execute(&self.pool)
            .await
            .map_err(RepoError::from)?;
            debug!(
                agent_id,
                rejected = aggregate.rejected_ids.len(),
                reason = aggregate.skipped_reason.as_deref().unwrap_or(""),
                "scheduler: dispatcher rejected pairs"
            );
        }
        Ok(true)
    }
}
