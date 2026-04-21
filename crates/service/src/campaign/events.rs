//! NOTIFY channel names for the campaign subsystem.
//!
//! Three channels drive wake-ups:
//!
//! * [`NOTIFY_CHANNEL`] (`campaign_state_changed`) — fired by the
//!   migration trigger on every INSERT / UPDATE OF state on
//!   `measurement_campaigns`. Lets the scheduler pick up newly-started
//!   or operator-stopped campaigns without waiting for the next tick.
//! * [`PAIR_SETTLED_CHANNEL`] (`campaign_pair_settled`) — fired by the
//!   dispatch writer on every successful settle (see
//!   [`super::writer`]). Lets the scheduler run `maybe_complete`
//!   promptly after a batch lands instead of waiting for the next
//!   500 ms tick.
//! * [`EVALUATED_CHANNEL`] (`campaign_evaluated`) — fired by the
//!   `campaign_evaluations_notify` trigger on INSERT / UPDATE of
//!   `campaign_evaluations`. Lets the SSE listener fan `evaluated`
//!   frames out to subscribers on peer instances so clients not
//!   connected to the `/evaluate` handler still invalidate their
//!   evaluation cache.
//!
//! All payloads are the campaign UUID as a text string, well under
//! `pg_notify`'s 8000-byte cap.

/// Postgres NOTIFY channel for campaign lifecycle changes. Both the
/// migration trigger function and the scheduler's listener reference
/// this exact string — never rename without touching both sides in one
/// commit.
pub const NOTIFY_CHANNEL: &str = "campaign_state_changed";

/// Second NOTIFY channel (T45) — fired by the dispatch writer on every
/// settled pair. Payload is the campaign UUID as a text string. Lets
/// the scheduler run `maybe_complete` promptly after a batch lands
/// instead of waiting for the next tick.
pub const PAIR_SETTLED_CHANNEL: &str = "campaign_pair_settled";

/// Third NOTIFY channel — fired by the `campaign_evaluations_notify`
/// trigger on every INSERT / UPDATE of `campaign_evaluations`. Payload
/// is the campaign UUID as a text string. The SSE listener translates
/// it to a `CampaignStreamEvent::Evaluated` broadcast so peer
/// instances' SSE subscribers re-fetch their `/evaluation` cache.
pub const EVALUATED_CHANNEL: &str = "campaign_evaluated";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_channel_name_is_stable() {
        // Renaming this channel requires touching the migration trigger
        // and the scheduler listener in the same commit — this test
        // exists to make that coupling explicit.
        assert_eq!(NOTIFY_CHANNEL, "campaign_state_changed");
    }

    #[test]
    fn pair_settled_channel_name_is_stable() {
        // The writer's `pg_notify` call and the scheduler's listener
        // both reference this string. Renaming requires touching both
        // sides in one commit.
        assert_eq!(PAIR_SETTLED_CHANNEL, "campaign_pair_settled");
    }

    #[test]
    fn evaluated_channel_name_is_stable() {
        // The `campaign_evaluations_notify` trigger and the SSE listener
        // both reference this string. Renaming requires touching the
        // migration and the listener in one commit.
        assert_eq!(EVALUATED_CHANNEL, "campaign_evaluated");
    }
}
