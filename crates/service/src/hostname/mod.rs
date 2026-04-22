//! IP → hostname reverse-DNS cache.
//!
//! The session-scoped SSE broadcaster diverges from
//! `catalogue::events::CatalogueBroker`'s broadcast shape because every
//! session's requests trigger lookups whose events must flow only to the
//! session that caused them.
mod backend;
pub(crate) mod handlers;
mod ip_canon;
mod refresh_limit;
mod repo;
mod resolver;
mod sse;

pub use backend::{HickoryBackend, LookupOutcome, ResolverBackend};
pub use ip_canon::canonicalize;
pub use refresh_limit::HostnameRefreshLimiter;
pub use repo::{hostnames_for, record_negative, record_positive};
pub use resolver::Resolver;
pub use sse::{HostnameBroadcaster, HostnameEvent, SessionHandle, SessionId};

#[cfg(test)]
pub(crate) use backend::test_support;
