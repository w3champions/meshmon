//! NOTIFY channel glue. The scheduler `PgListener::listen(NOTIFY_CHANNEL)`
//! wakes on any campaign state change; the DB trigger in migration
//! `20260420120000_campaigns.up.sql` fires on INSERT or UPDATE OF state
//! of `measurement_campaigns`.

use sqlx::PgExecutor;
use uuid::Uuid;

/// Postgres NOTIFY channel name. Both the migration trigger function
/// and the scheduler's listener reference this exact string — never
/// rename without touching both sides in one commit.
pub const NOTIFY_CHANNEL: &str = "campaign_state_changed";

/// Manually fan out a state-changed NOTIFY. Called after lifecycle
/// transitions that happen inside a single transaction together with
/// pair mutations (e.g. `repo::stop`, `repo::apply_edit`) so the
/// scheduler wakes once the commit is durable.
///
/// The `AFTER UPDATE OF state` trigger fires automatically on
/// single-statement updates; this helper is for the rare path where a
/// composite transition wants an explicit notify (e.g. after inserting
/// new pairs into a Running campaign in `apply_edit`).
pub async fn notify_state_changed<'e, E>(executor: E, campaign_id: Uuid) -> Result<(), sqlx::Error>
where
    E: PgExecutor<'e>,
{
    sqlx::query("SELECT pg_notify($1, $2::text)")
        .bind(NOTIFY_CHANNEL)
        .bind(campaign_id)
        .execute(executor)
        .await
        .map(|_| ())
}

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
