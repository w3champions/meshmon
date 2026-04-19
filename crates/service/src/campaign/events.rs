//! NOTIFY channel name used by the measurement-campaign scheduler.
//!
//! The full publisher helper (`notify_state_changed`) is added in the
//! events module proper; for now the repo only needs the channel name
//! so it can coordinate with the migration trigger.

/// Postgres NOTIFY channel name. Both the migration trigger function
/// (`measurement_campaigns_notify`) and the scheduler's listener
/// reference this exact string — never rename without touching both
/// sides in the same commit.
pub const NOTIFY_CHANNEL: &str = "campaign_state_changed";
