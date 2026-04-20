//! NOTIFY channel name. The scheduler `PgListener::listen(NOTIFY_CHANNEL)`
//! wakes on any campaign state change; the DB trigger in migration
//! `20260420120000_campaigns.up.sql` fires on INSERT or UPDATE OF state
//! of `measurement_campaigns`.

/// Postgres NOTIFY channel name. Both the migration trigger function
/// and the scheduler's listener reference this exact string — never
/// rename without touching both sides in one commit.
pub const NOTIFY_CHANNEL: &str = "campaign_state_changed";

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
}
