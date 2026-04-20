//! Dedicated `PgListener` task that translates campaign NOTIFY wake-ups
//! into broadcast publishes on the [`CampaignBroker`].
//!
//! This is the SSE fan-out side of the two NOTIFY channels the scheduler
//! also watches. PostgreSQL delivers every `NOTIFY` payload to every
//! `LISTEN`ing connection independently, so this listener coexists with
//! [`super::scheduler::Scheduler`]'s listener without any coordination.
//!
//! Behaviour summary:
//!
//! - On startup, opens a dedicated `PgListener` and subscribes to
//!   [`super::events::NOTIFY_CHANNEL`] and
//!   [`super::events::PAIR_SETTLED_CHANNEL`].
//! - For every `campaign_state_changed` notification, parses the
//!   payload as a campaign UUID, reads the current `state` column, and
//!   publishes [`CampaignStreamEvent::StateChanged`]. A deleted row is
//!   logged at `warn!` and skipped (the trigger fires on UPDATE, not
//!   DELETE, but a concurrent delete between the UPDATE commit and our
//!   read is possible).
//! - For every `campaign_pair_settled` notification, parses the
//!   payload as a campaign UUID and publishes
//!   [`CampaignStreamEvent::PairSettled`]. The SSE consumer re-fetches
//!   the affected pairs through the existing pairs endpoint.
//! - On any listener error (connect, subscribe, recv), logs and
//!   reconnects with capped exponential backoff (1 s → 30 s). The
//!   broker stays alive across reconnect attempts so existing SSE
//!   subscribers never have to re-open their streams.
//! - Exits promptly when the supplied [`CancellationToken`] fires.

use super::broker::{CampaignBroker, CampaignStreamEvent};
use super::events::{NOTIFY_CHANNEL, PAIR_SETTLED_CHANNEL};
use super::model::CampaignState;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Initial reconnect backoff. Doubles on every consecutive failure and is
/// capped at [`BACKOFF_MAX`]. Reset back to this value after any session
/// that successfully received at least one notification, so a long-lived
/// listener that eventually trips over a routine PG connection recycle
/// reconnects promptly instead of waiting out a carried-over 30 s delay
/// accumulated hours earlier.
const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// Upper bound on reconnect backoff. Matches the scheduler's tick fallback
/// order of magnitude so a listener outage cannot leave subscribers idle
/// longer than the operator's existing refresh cadence.
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Spawn the campaign SSE listener task.
///
/// The returned [`JoinHandle`] lets the caller await shutdown during the
/// main-process drain phase. The task exits when `cancel` fires; it
/// otherwise runs forever, reconnecting on any listener failure.
pub fn spawn_campaign_listener(
    pool: PgPool,
    broker: CampaignBroker,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(run(pool, broker, cancel))
}

/// The listener loop. Keeps reconnecting on failure so a transient DB
/// blip never grounds SSE fan-out permanently; exits only on cancel.
async fn run(pool: PgPool, broker: CampaignBroker, cancel: CancellationToken) {
    info!("campaign SSE listener starting");
    // Reconnect backoff — doubles on every consecutive failure, capped at
    // `BACKOFF_MAX`. Reset to `BACKOFF_INITIAL` whenever the preceding
    // session proved healthy (at least one notification successfully
    // received) so a listener that has survived for hours doesn't wait
    // out an ancient 30 s delay after a routine PG connection recycle.
    let mut backoff = BACKOFF_INITIAL;

    loop {
        if cancel.is_cancelled() {
            break;
        }
        match session(&pool, &broker, &cancel).await {
            SessionOutcome::Cancelled => break,
            SessionOutcome::Failed { had_activity } => {
                // A session that delivered at least one notification is
                // evidence the DB was reachable and our subscription was
                // live; treat the next reconnect as a fresh attempt.
                if had_activity {
                    backoff = BACKOFF_INITIAL;
                }
                // Backoff before reconnecting. Observe the cancel token
                // *during* the sleep so shutdown doesn't have to wait
                // out the full delay.
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(backoff) => {}
                }
                if !had_activity {
                    backoff = std::cmp::min(backoff.saturating_mul(2), BACKOFF_MAX);
                }
            }
        }
    }
    info!("campaign SSE listener shutting down");
}

/// Outcome of one listener session. Drives the outer reconnect policy.
///
/// `PgListener::recv()` only returns `Ok(_) | Err(_)`, so every session
/// exits either via the cancel branch (shutdown) or via an error path.
/// A separate clean-end variant would be unreachable today.
enum SessionOutcome {
    /// Shutdown was requested — terminate the loop entirely.
    Cancelled,
    /// Listener hit an error (connect, subscribe, or recv). Reconnect
    /// after the backoff delay. `had_activity` is `true` when the
    /// session successfully received at least one notification before
    /// failing — in that case the outer loop resets the backoff to
    /// `BACKOFF_INITIAL` so routine connection recycles don't carry
    /// over hours-old 30 s delays.
    Failed { had_activity: bool },
}

/// One lifetime of the `PgListener`. Returns an outcome that tells the
/// outer loop whether to sleep before the next attempt.
async fn session(
    pool: &PgPool,
    broker: &CampaignBroker,
    cancel: &CancellationToken,
) -> SessionOutcome {
    let mut listener = match PgListener::connect_with(pool).await {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "campaign SSE listener: failed to open PgListener");
            return SessionOutcome::Failed {
                had_activity: false,
            };
        }
    };

    if let Err(e) = listener
        .listen_all([NOTIFY_CHANNEL, PAIR_SETTLED_CHANNEL])
        .await
    {
        warn!(
            error = %e,
            "campaign SSE listener: failed to subscribe to NOTIFY channels"
        );
        return SessionOutcome::Failed {
            had_activity: false,
        };
    }

    info!("campaign SSE listener: subscribed to NOTIFY channels");

    // `had_activity` flips to `true` on the first successful `recv()`.
    // "At least one notification received" is the least ambiguous
    // indicator that the session was healthy: the connect+subscribe
    // pair could theoretically succeed against a half-broken DB, but a
    // delivered NOTIFY proves the pipe was round-trip functional.
    let mut had_activity = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return SessionOutcome::Cancelled,
            recv_result = listener.recv() => match recv_result {
                Ok(notification) => {
                    had_activity = true;
                    handle_notification(pool, broker, &notification).await;
                }
                Err(e) => {
                    warn!(error = %e, "campaign SSE listener: recv error; reconnecting");
                    return SessionOutcome::Failed { had_activity };
                }
            }
        }
    }
}

/// Dispatch on `notification.channel()` and publish the corresponding
/// broker event. Malformed payloads are logged and skipped rather than
/// propagated.
async fn handle_notification(
    pool: &PgPool,
    broker: &CampaignBroker,
    notification: &sqlx::postgres::PgNotification,
) {
    let channel = notification.channel();
    let payload = notification.payload();
    debug!(channel, payload, "campaign SSE listener: notify received");

    let campaign_id = match Uuid::parse_str(payload) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                channel,
                payload,
                error = %e,
                "campaign SSE listener: malformed UUID payload; dropping notification"
            );
            return;
        }
    };

    match channel {
        NOTIFY_CHANNEL => {
            // Read the current state. A concurrent DELETE between the
            // trigger's commit and this read is rare (the trigger fires
            // on INSERT/UPDATE, not DELETE) but not impossible — log and
            // skip rather than spam zero-row errors.
            //
            // TODO(follow-up): emit transition state from the NOTIFY payload
            // instead of a re-query, so rapid draft→running→stopped sequences
            // don't collapse into two "stopped" events. Frontend currently
            // invalidates by id only so the race is cosmetic, but the stream
            // contract documents per-transition semantics.
            let row = sqlx::query_scalar!(
                r#"SELECT state AS "state: CampaignState" FROM measurement_campaigns WHERE id = $1"#,
                campaign_id
            )
            .fetch_optional(pool)
            .await;
            match row {
                Ok(Some(state)) => {
                    broker.publish(CampaignStreamEvent::StateChanged { campaign_id, state });
                }
                Ok(None) => {
                    warn!(
                        %campaign_id,
                        "campaign SSE listener: state_changed notify for missing campaign; skipping"
                    );
                }
                Err(e) => {
                    error!(
                        %campaign_id,
                        error = %e,
                        "campaign SSE listener: failed to read state column; dropping notification"
                    );
                }
            }
        }
        PAIR_SETTLED_CHANNEL => {
            broker.publish(CampaignStreamEvent::PairSettled { campaign_id });
        }
        other => {
            warn!(
                channel = other,
                "campaign SSE listener: notification on unexpected channel; ignoring"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Smoke test — the real listener behaviour is exercised end-to-end in
    // `tests/campaigns_sse_test.rs` against a real Postgres. Here we only
    // confirm that the spawn surface returns a usable `JoinHandle` and
    // exits promptly on cancel without touching a pool.
    #[tokio::test]
    async fn spawn_returns_join_handle_and_cancels_cleanly() {
        // A pool that will never successfully connect: the listener
        // bounces between `PgListener::connect_with` errors and the
        // backoff sleep. Cancellation during the sleep must still exit
        // within a reasonable deadline.
        let pool = PgPool::connect_lazy("postgres://unreachable@127.0.0.1:1/none")
            .expect("build lazy pool");
        let broker = CampaignBroker::default();
        let cancel = CancellationToken::new();
        let handle: JoinHandle<()> = spawn_campaign_listener(pool, broker, cancel.clone());

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("listener task did not exit within 5s")
            .expect("listener task panicked");
    }
}
