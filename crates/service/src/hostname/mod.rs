//! IP → hostname reverse-DNS cache.
//!
//! The session-scoped SSE broadcaster diverges from
//! `catalogue::events::CatalogueBroker`'s broadcast shape because every
//! session's requests trigger lookups whose events must flow only to the
//! session that caused them.
mod ip_canon;

pub use ip_canon::canonicalize;
