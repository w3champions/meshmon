//! Service-to-agent command surface.
//!
//! The `AgentCommand` tonic service has two methods:
//!   * `RefreshConfig` — forces an early config/targets refresh.
//!   * `RunMeasurementBatch` — server-streams one-off measurement
//!     results for a batch of campaign pairs.
//!
//! The prober itself is pluggable via [`CampaignProber`]. The default
//! [`StubProber`] lets transport-level tests run without the real
//! trippy-backed prober; production deployments swap it for the real
//! one at construction time.

pub mod measurements;

pub use measurements::{AgentCommandService, CampaignProber, StubProber};
