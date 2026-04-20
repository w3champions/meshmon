//! Legacy shim — the full `AgentCommand` surface now lives in
//! [`crate::command::AgentCommandService`]. This module re-exports the
//! replacement types so external imports that used to reach for
//! `command_service::` keep working.

pub use crate::command::{AgentCommandService, CampaignProber, StubProber};
