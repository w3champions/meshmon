//! NOTIFY channel names for the campaign subsystem.
//!
//! Two channels drive the scheduler's wakeups:
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
//!
//! Both payloads are the campaign UUID as a text string, well under
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
}
